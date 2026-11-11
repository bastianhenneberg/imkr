//! Presentation and ownership for imkr's automatic desktop alerts only.
use std::{
    collections::HashMap,
    fs::File,
    os::fd::AsRawFd,
    sync::{Mutex, OnceLock},
};

/// Compact plain text: never use terminal titles or screen excerpts here.
pub fn label(value: &str, fallback: &str) -> String {
    let text: String = value.chars().filter(|c| !c.is_control()).collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.is_empty() { fallback } else { &text };
    // Escape notification markup, independently of shell quoting (argv is direct).
    let short: String = text.chars().take(64).collect();
    let short = if text.chars().count() > 64 {
        format!("{short}…")
    } else {
        short
    };
    short
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn event(
    kind: &str,
    what: &str,
    program: Option<&str>,
    name: Option<&str>,
    project: Option<&str>,
    session: &str,
    pane: usize,
) -> (String, String) {
    let agent = match program {
        Some("claude") => "Claude",
        Some("codex") => "Codex",
        _ => "Agent",
    };
    let action = match kind {
        "done" => "finished its turn",
        "blocked" if what == "needs you" => "is waiting for your answer",
        "failed" => "stopped with an error",
        "delivery" => "message delivery delayed",
        _ => what,
    };
    let title = format!("{agent} · {}", label(action, "needs attention"));
    let identity = name
        .filter(|n| !n.trim().is_empty())
        .map(|n| label(n, "bee"))
        .unwrap_or_else(|| format!("{} / bee {pane}", label(session, "imkr")));
    let mut body = format!("{} · {identity}", label(project.unwrap_or(""), "imkr"));
    if matches!(kind, "failed" | "delivery") && what != "stopped with an error" {
        body.push_str(&format!(" · {}", label(what, "error")));
    }
    (title, body)
}

/// The existing transferred mail log is enough to suppress a desktop replay
/// after live update, without changing delivery, sender feedback, push or OSC.
/// `Again` records a deliberate new attempt and must permit a fresh warning.
pub fn delivery_warning_is_new(notes: &[crate::protocol::MailNote], pane: usize, seq: u64) -> bool {
    use crate::protocol::MailWhat;
    if seq == 0 {
        return true;
    } // No reliable identity: preserve attention.
    notes
        .iter()
        .rev()
        .filter(|note| !note.sent && note.pane == pane && note.about == Some(seq))
        .find_map(|note| match note.what {
            MailWhat::Overdue => Some(false),
            MailWhat::Again => Some(true),
            _ => None,
        })
        .unwrap_or(true)
}

/// Selection markers and wrapping are presentation changes, not new questions.
/// The fingerprint stays private; it is never a popup body or transport payload.
pub fn question_identity(text: &str) -> String {
    text.lines()
        .map(|line| line.trim().trim_start_matches(['›', '>', ' ']))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fixed collection window; later events cannot postpone a waiting question.
pub const BURST: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Source {
    pub host: String,
    pub session: String,
    pub pane: usize,
}

#[derive(Clone)]
pub struct Alert {
    pub source: Source,
    pub kind: String,
    pub detail: String,
    pub title: String,
    pub body: String,
}

pub struct Notice {
    pub title: String,
    pub body: String,
    pub urgency: &'static str,
}

struct Batch {
    since: std::time::Instant,
    entries: std::collections::BTreeMap<Source, Alert>,
}

/// Per-server desktop collection, scoped by remote host and originating session.
/// Remote producer ownership remains separate: batching cannot fix duplicates
/// produced by independent servers.
#[derive(Default)]
pub struct Queue {
    states: HashMap<Source, (String, String)>,
    delivered: HashMap<(Source, String), String>,
    batches: std::collections::BTreeMap<(u8, String, String, String), Batch>,
}

impl Queue {
    pub fn knows(&self, source: &Source) -> bool {
        self.states.contains_key(source)
    }

    /// Returns true for a changed question/error while its state stays the same.
    /// A real recovery opens a new episode even if the next question is identical.
    pub fn observe(&mut self, source: Source, state: &str, fingerprint: String) -> bool {
        let current = (state.to_owned(), fingerprint);
        let previous = self.states.insert(source.clone(), current.clone());
        let changed = previous.as_ref().is_some_and(|old| old != &current);
        if changed {
            self.delivered.retain(|(key, _), _| key != &source);
        }
        // A question answered inside the collection window is no longer waiting.
        // Keep failures: recovery must not hide an error that actually happened.
        if state != "blocked" {
            for ((_, _, _, kind), batch) in &mut self.batches {
                if kind == "blocked"
                    && batch
                        .entries
                        .get(&source)
                        .is_some_and(|alert| alert.detail == "needs you")
                {
                    batch.entries.remove(&source);
                }
            }
            self.batches.retain(|_, batch| !batch.entries.is_empty());
        }
        changed
            && previous.is_some_and(|old| old.0 == state)
            && matches!(state, "blocked" | "failed")
    }

    pub fn submit(&mut self, alert: Alert, now: std::time::Instant) {
        let key = (alert.source.clone(), alert.kind.clone());
        if self.delivered.get(&key) == Some(&alert.detail) {
            return;
        }
        self.delivered.insert(key, alert.detail.clone());
        let rank = match alert.kind.as_str() {
            "failed" => 0,
            "blocked" | "delivery" => 1,
            "account" => 2,
            _ => 3,
        };
        let group = (
            rank,
            alert.source.host.clone(),
            alert.source.session.clone(),
            alert.kind.clone(),
        );
        self.batches
            .entry(group)
            .or_insert_with(|| Batch {
                since: now,
                entries: Default::default(),
            })
            .entries
            .insert(alert.source.clone(), alert);
    }

    pub fn drain(&mut self, now: std::time::Instant) -> Vec<Notice> {
        let ready: Vec<_> = self
            .batches
            .iter()
            .filter(|(_, batch)| now.saturating_duration_since(batch.since) >= BURST)
            .map(|(key, _)| key.clone())
            .collect();
        ready
            .into_iter()
            .flat_map(|key| {
                let batch = self.batches.remove(&key).unwrap();
                let urgency = match key.3.as_str() {
                    "failed" => "critical",
                    "done" => "low",
                    _ => "normal",
                };
                // Bound every popup while retaining every identity, even for large fleets.
                let entries: Vec<_> = batch.entries.into_values().collect();
                entries
                    .chunks(15)
                    .map(|chunk| {
                        let first = &chunk[0];
                        let title = if chunk.len() == 1 {
                            first.title.clone()
                        } else {
                            let action = match key.3.as_str() {
                                "failed" => "stopped with an error",
                                "blocked" => "need your attention",
                                "done" => "finished their turns",
                                "delivery" => "have delivery delays",
                                _ => "have account updates",
                            };
                            format!("{} bees {action}", chunk.len())
                        };
                        let body = if chunk.len() == 1 {
                            format!("{} · {}", label(&key.2, "imkr"), first.body)
                        } else {
                            let origin = if key.1.is_empty() {
                                label(&key.2, "imkr")
                            } else {
                                format!("{} · {}", label(&key.1, "remote"), label(&key.2, "imkr"))
                            };
                            format!(
                                "{origin}\n{}",
                                chunk
                                    .iter()
                                    .map(|a| format!("{}: {}", a.title, a.body))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            )
                        };
                        Notice {
                            title,
                            body,
                            urgency,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// A disappearing agent can be replaced in the same pane. Do not carry
    /// its duplicate identity into the replacement or across a closed session.
    pub fn retain_sources(&mut self, host: &str, live: &std::collections::HashSet<Source>) {
        let keep = |source: &Source| source.host != host || live.contains(source);
        self.states.retain(|source, _| keep(source));
        self.delivered.retain(|(source, _), _| keep(source));
        for ((_, _, _, kind), batch) in &mut self.batches {
            if kind == "blocked" {
                batch
                    .entries
                    .retain(|source, alert| keep(source) || alert.detail != "needs you");
            }
        }
        self.batches.retain(|_, batch| !batch.entries.is_empty());
    }

    pub fn finish(&mut self, now: std::time::Instant) -> Vec<Notice> {
        // Local agents end with their server; remote agents can still wait.
        // Delivery warnings and errors remain meaningful after a local exit.
        for ((_, host, _, kind), batch) in &mut self.batches {
            if host.is_empty() && kind == "blocked" {
                batch.entries.retain(|_, alert| alert.detail != "needs you");
            }
        }
        self.batches.retain(|_, batch| !batch.entries.is_empty());
        self.drain(now + BURST)
    }

    pub fn discard_pending(&mut self) {
        self.batches.clear();
    }
}

/// Only automatic System alerts use this sender; manual text and OSC keep the
/// original generic sender. No shell, desktop focus change, or settings write.
fn command(notice: &Notice) -> std::process::Command {
    let mut command = std::process::Command::new("notify-send");
    command
        .args([
            "--app-name=imkr",
            &format!("--urgency={}", notice.urgency),
            "--",
            &notice.title,
            &notice.body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

pub fn send(notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    // One short-lived worker per flush preserves priority at the actual command
    // boundary, rather than racing three independent notify-send children.
    std::thread::spawn(move || {
        for notice in notices {
            let _ = command(&notice).status();
        }
    });
}

/// Start remaining commands before a graceful server exit can end its threads.
/// The children can finish independently; shutdown never waits on the desktop.
pub fn send_before_exit(notices: Vec<Notice>) {
    for notice in notices {
        if let Ok(mut child) = command(&notice).spawn() {
            std::thread::spawn(move || child.wait());
        }
    }
}

pub fn remote_alert(
    host: &str,
    session: &str,
    agent: &crate::protocol::AgentInfo,
    kind: &str,
    what: &str,
) -> Alert {
    let (title, body) = event(
        kind,
        what,
        Some(&agent.program),
        (!matches!(agent.program.as_str(), "claude" | "codex")).then_some(agent.program.as_str()),
        Some(&agent.dir),
        session,
        agent.pane,
    );
    Alert {
        source: Source {
            host: host.into(),
            session: session.into(),
            pane: agent.pane,
        },
        kind: kind.into(),
        detail: what.into(),
        title,
        body: format!("{} · {body}", label(host, "remote")),
    }
}

fn lock(path: &std::path::Path) -> std::io::Result<Option<File>> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    // One notifier for each remote host on this desktop; the fd holds ownership.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(file))
    } else {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }
}

pub fn remote_owner(host: &str) -> bool {
    static OWNED: OnceLock<Mutex<HashMap<String, File>>> = OnceLock::new();
    let mut owned = OWNED.get_or_init(Default::default).lock().unwrap();
    if owned.contains_key(host) {
        return true;
    }
    let Ok(dir) = crate::persist::state_dir() else {
        return true;
    };
    let dir = dir.join("desktop-notifiers");
    if std::fs::create_dir_all(&dir).is_err() {
        return true;
    }
    let key = host
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let file = match lock(&dir.join(key)) {
        Ok(Some(file)) => file,
        Ok(None) => return false,
        // Failure to coordinate must not silence a question or error.
        Err(_) => return true,
    };
    owned.insert(host.to_owned(), file);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_events_are_specific_and_do_not_assume_claude() {
        for (kind, what, expected) in [
            ("done", "is done", "finished its turn"),
            ("blocked", "needs you", "is waiting for your answer"),
            ("failed", "stopped: limit reached", "stopped with an error"),
        ] {
            let (title, body) = event(
                kind,
                what,
                Some("codex"),
                Some("security"),
                Some("imkr"),
                "work",
                2,
            );
            assert_eq!(title, format!("Codex · {expected}"));
            assert!(body.starts_with("imkr · security"));
        }
        assert!(
            event("blocked", "needs you", Some("claude"), None, None, "", 0)
                .0
                .starts_with("Claude")
        );
        assert_eq!(
            event("done", "is done", None, None, None, "", 0).1,
            "imkr · imkr / bee 0"
        );
        assert_eq!(label("\x1b\n<b>&\r", "x"), "&lt;b&gt;&amp;");
        assert!(label(&"界".repeat(200), "x").chars().count() <= 65);
    }
    #[test]
    fn remote_notifier_has_one_owner_and_failover() {
        let path = std::env::temp_dir().join(format!("imkr-desktop-lock-{}", std::process::id()));
        let first = lock(&path).unwrap().unwrap();
        for _ in 0..15 {
            assert!(lock(&path).unwrap().is_none());
        }
        drop(first);
        assert!(lock(&path).unwrap().is_some());
        std::fs::remove_file(path).unwrap();
    }
    fn alert(pane: usize, kind: &str) -> Alert {
        Alert {
            source: Source {
                host: String::new(),
                session: "review".into(),
                pane,
            },
            kind: kind.into(),
            detail: if kind == "blocked" {
                "needs you".into()
            } else {
                kind.into()
            },
            title: format!("Codex · {kind}"),
            body: format!("imkr · bee {pane}"),
        }
    }

    #[test]
    fn bursts_have_a_fixed_deadline_and_keep_all_fifteen_identities() {
        let mut queue = Queue::default();
        let now = std::time::Instant::now();
        for id in 0..15 {
            let event = alert(id, "blocked");
            queue.observe(event.source.clone(), "blocked", "question A".into());
            queue.submit(event.clone(), now);
            queue.submit(event, now + BURST / 2);
        }
        assert!(
            queue
                .drain(now + BURST - std::time::Duration::from_nanos(1))
                .is_empty()
        );
        let notices = queue.drain(now + BURST);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "15 bees need your attention");
        assert_eq!(notices[0].urgency, "normal");
        assert_eq!(notices[0].body.lines().count(), 16);
        for id in 0..15 {
            assert!(notices[0].body.contains(&format!("bee {id}")));
        }
        queue.submit(alert(0, "blocked"), now + BURST * 2);
        assert!(queue.drain(now + BURST * 3).is_empty());
    }

    #[test]
    fn recovery_changed_question_and_replacement_rearm_without_crossing_sessions() {
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        let first = alert(0, "blocked");
        let source = first.source.clone();
        assert!(!queue.observe(source.clone(), "blocked", "question A".into()));
        queue.submit(first.clone(), now);
        assert_eq!(queue.drain(now + BURST).len(), 1);
        assert!(!queue.observe(source.clone(), "blocked", "question A".into()));
        queue.submit(first.clone(), now + BURST);
        assert!(queue.drain(now + BURST * 2).is_empty());
        assert!(queue.observe(source.clone(), "blocked", "question B".into()));
        queue.submit(first.clone(), now + BURST * 2);
        assert_eq!(queue.drain(now + BURST * 3).len(), 1);
        queue.observe(source.clone(), "working", String::new());
        queue.observe(source.clone(), "blocked", "question B".into());
        queue.submit(first.clone(), now + BURST * 3);
        let mut other = first.clone();
        other.source.session = "other session".into();
        queue.submit(other, now + BURST * 3);
        assert_eq!(queue.drain(now + BURST * 4).len(), 2);
        queue.retain_sources("", &Default::default());
        queue.submit(first, now + BURST * 4);
        assert_eq!(queue.drain(now + BURST * 5).len(), 1);
    }

    #[test]
    fn errors_questions_and_completions_remain_separate_and_priority_ordered() {
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        for id in 0..15 {
            let kind = match id % 3 {
                0 => "done",
                1 => "blocked",
                _ => "failed",
            };
            queue.submit(alert(id, kind), now);
        }
        let notices = queue.drain(now + BURST);
        assert_eq!(notices.len(), 3);
        assert_eq!(
            notices.iter().map(|n| n.urgency).collect::<Vec<_>>(),
            ["critical", "normal", "low"]
        );
        for notice in notices {
            assert!(notice.title.starts_with("5 bees"));
        }
    }

    #[test]
    fn answering_cancels_pending_question_but_recovery_does_not_hide_errors() {
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        let event = alert(0, "blocked");
        queue.submit(event.clone(), now);
        queue.observe(event.source.clone(), "working", String::new());
        assert!(queue.drain(now + BURST).is_empty());
        queue.submit(alert(0, "failed"), now);
        queue.observe(event.source, "idle", String::new());
        assert_eq!(queue.drain(now + BURST)[0].urgency, "critical");
    }

    #[test]
    fn moving_selection_is_not_a_new_question() {
        let question = "Which approach?\n › 1. First\n   2. Second";
        let moved = "Which approach?\n   1. First\n › 2. Second";
        assert_eq!(question_identity(question), question_identity(moved));
        assert_ne!(
            question_identity(question),
            question_identity("Which project?\n1. First\n2. Second")
        );
    }
    #[test]
    fn delivery_attention_is_preserved_even_when_its_agent_disappears() {
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        let mut warning = alert(0, "blocked");
        warning.detail = "mail could not reach its agent".into();
        queue.submit(warning.clone(), now);
        queue.observe(warning.source, "absent", String::new());
        queue.retain_sources("", &Default::default());
        assert_eq!(queue.drain(now + BURST).len(), 1);
    }
    #[test]
    fn shutdown_flushes_failures_without_claiming_ended_local_agents_still_wait() {
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        queue.submit(alert(0, "blocked"), now);
        queue.submit(alert(1, "failed"), now);
        let notices = queue.finish(now);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].urgency, "critical");
    }
    #[test]
    fn overdue_desktop_replay_uses_message_identity_and_explicit_redelivery_episode() {
        use crate::protocol::{MailNote, MailWhat};
        let note = |pane, about, what| MailNote {
            at: 1000,
            seq: 50,
            pane,
            peer: String::new(),
            sent: false,
            text: String::new(),
            what,
            about: Some(about),
            why: None,
        };
        let mut notes = vec![note(0, 7, MailWhat::Overdue)];
        assert!(!delivery_warning_is_new(&notes, 0, 7));
        assert!(delivery_warning_is_new(&notes, 0, 8));
        assert!(delivery_warning_is_new(&notes, 1, 7));
        // A fresh Held note after update is not a new delivery episode.
        notes.push(note(0, 7, MailWhat::Held));
        assert!(!delivery_warning_is_new(&notes, 0, 7));
        notes.push(note(0, 7, MailWhat::Again));
        assert!(delivery_warning_is_new(&notes, 0, 7));
        notes.push(note(0, 7, MailWhat::Overdue));
        assert!(!delivery_warning_is_new(&notes, 0, 7));
        assert!(delivery_warning_is_new(&[], 0, 7));
        assert!(delivery_warning_is_new(&notes, 0, 0));
    }

    #[test]
    fn delivery_warning_is_an_actual_delay_not_a_question_for_the_user() {
        let (title, body) = event(
            "delivery",
            "working",
            Some("codex"),
            Some("worker"),
            Some("imkr"),
            "review",
            0,
        );
        assert_eq!(title, "Codex · message delivery delayed");
        assert_eq!(body, "imkr · worker · working");
        let now = std::time::Instant::now();
        let mut queue = Queue::default();
        for seq in [7, 8] {
            let mut warning = alert(0, "delivery");
            warning.detail = format!("mail:{seq}");
            queue.submit(warning, now + BURST * (seq - 7));
            assert_eq!(queue.drain(now + BURST * (seq - 6)).len(), 1);
        }
    }
}
