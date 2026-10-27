//! imkr events in AI Brain's shared inbox (#536).
//!
//! When an agent waits for an answer, is done, gave up, ran its account dry
//! or a contact wrote, that reaches the user as a bell in the terminal and,
//! with `[push]`, on the phone. Both pass. This takes the same event to AI
//! Brain's inbox, where it stays until somebody ticks it off — **on top of
//! the bell and ntfy, not instead**.
//!
//! **imkr never talks to AI Brain itself.** It talks to the harness on this
//! machine, which holds the sign-in — the same way as the messages between
//! people's imkrs (`contacts`, #495). Without a harness nothing happens and
//! nothing is run.
//!
//! **Ticking off comes back.** imkr keeps its own ✦ ("something happened
//! here and you have not seen it"). Whoever ticks the line off in the inbox
//! has seen it, so a ✦ left in the terminal afterwards would be a second
//! truth. `read()` fetches what was ticked off, clears the ✦ in the session
//! it belongs to, and only then confirms it.
//!
//! **At least once, not exactly once.** A line comes again until it is
//! confirmed. Clearing the same ✦ twice costs nothing, leaving it once does.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::layout::PaneId;
use crate::protocol::{self, Request};

/// How many ids of ticked-off events are remembered against doubles: the
/// harness hands one out again until it is confirmed.
const SEEN: usize = 500;
/// The longest text sent along, in bytes. AI Brain cuts at the same place;
/// it is said here too so the limit holds without the other side.
const MAX_TEXT: usize = 8 * 1024;
/// Nothing may hold up a tick: both calls go through `timeout`.
const TIMEOUT: &str = "30";

/// What happened, as the inbox gets it. The fields of `push::Event` that
/// have an address in them, plus what is shown.
#[derive(Clone, Debug, Default)]
pub struct Note {
    /// `blocked`, `done`, `failed`, `account` or `message`.
    pub kind: String,
    pub machine: String,
    pub session: String,
    pub pane: PaneId,
    pub window: u8,
    /// The agent, e.g. `claude`.
    pub agent: String,
    /// The line in the inbox, e.g. `claude needs you`.
    pub title: String,
    /// What it is about: the task, the question, the error.
    pub text: String,
    pub link: String,
}

#[derive(Debug, Deserialize)]
struct Read {
    id: u64,
    #[serde(default)]
    sitzung: Option<String>,
    #[serde(default)]
    pane: Option<PaneId>,
}

#[derive(Debug, Deserialize)]
struct Fetched {
    #[serde(default)]
    gelesen: Vec<Read>,
}

fn path(name: &str) -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join(name))
}

/// Whether a harness is set up here: its program and its connection.
///
/// Both, not one: the program alone is on every machine that ever ran the
/// installer, and a connection file without the program cannot be used.
/// Asked before every report, so a machine without AI Brain never starts a
/// process.
///
/// **A test world never reports.** Its fake agents would otherwise fill the
/// real inbox of the person running the test — the same reason the
/// contacts' fetch leaves test servers alone.
pub fn connected() -> bool {
    protocol::test_socket().is_none()
        && crate::contacts::brain_installed()
        && connection().is_some()
}

/// The harness' connection file, where it keeps it.
///
/// `XDG_CONFIG_HOME` first, as the harness does; the directory keeps the
/// name `ai-brain-cli` from before the program was renamed.
fn connection() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    let file = base.join("ai-brain-cli").join("connection.json");
    file.is_file().then_some(file)
}

/// Runs `ai-brain-harness meldung ARGS` with `input` on stdin; its output.
fn cli(args: &[&str], input: &str) -> Result<String> {
    let program = crate::contacts::brain_program();
    let mut child = std::process::Command::new("timeout")
        .args([TIMEOUT, program, "meldung"])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        match out.status.code() {
            Some(127) => bail!("ai-brain-harness is not installed here"),
            Some(124) => bail!("{program} did not answer within {TIMEOUT} s"),
            _ => bail!("{}", why.trim()),
        }
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Puts `note` in AI Brain's inbox, in the background: the event loop never
/// waits on a process or the network, the same as `push::send`. Nothing
/// happens without a harness. Failures go to the server's log.
pub fn report(note: Note) {
    if !connected() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = send(&note) {
            eprintln!("imkr event to AI Brain: {e:#}");
        }
    });
}

/// The one call, so the thread above stays about threading.
fn send(note: &Note) -> Result<String> {
    let args = args_for(note);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cli(&args, &cut(&note.text, MAX_TEXT))
}

/// What `meldung senden` is called with.
///
/// Its own function because the order matters and nothing else shows it:
/// everything that is not a flag or a flag's value is the title, so the
/// title goes **last** — a flag put after it would be read as part of it.
fn args_for(note: &Note) -> Vec<String> {
    let mut args = vec![
        "senden".to_owned(),
        "--art".to_owned(),
        note.kind.clone(),
        "--rechner".to_owned(),
        note.machine.clone(),
        "--sitzung".to_owned(),
        note.session.clone(),
        "--pane".to_owned(),
        note.pane.to_string(),
        "--fenster".to_owned(),
        note.window.to_string(),
    ];
    if !note.agent.is_empty() {
        args.extend(["--agent".to_owned(), note.agent.clone()]);
    }
    if !note.link.is_empty() {
        args.extend(["--link".to_owned(), note.link.clone()]);
    }
    args.push(note.title.clone());
    args
}

/// `text` cut to `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Takes in what was ticked off in AI Brain's inbox: clears the ✦ of the
/// pane it belongs to, then confirms it. How many came.
///
/// Only with a harness and only with the setting on, so machines without
/// AI Brain never run a process every half minute. The setting is read from
/// the file and not from a server's copy: this runs in each session's
/// server, and switching it off should stop all of them without a restart.
/// What that costs is a ✦ of a line ticked off after switching off — and
/// looking at the pane clears that too.
///
/// One fetch per machine at a time: every session's server asks, and two at
/// once would both get the same unconfirmed line — harmless by itself, but
/// the second would confirm what the first had not applied yet.
pub fn read() -> Result<usize> {
    if !connected() || !crate::config::Config::load().0.brain_inbox {
        return Ok(0);
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path("imkr-brain.lock")?)?;
    // SAFETY: flock on a descriptor we own; released when `lock` closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(0);
    }
    let out = cli(&["gelesen", "--json"], "")?;
    let fetched = serde_json::from_str::<Fetched>(&out)
        .context("ai-brain-harness answered no list of ticked-off events")?
        .gelesen;
    let count = fetched.len();
    let mut seen: Vec<u64> = read_file("imkr-brain-seen.json");
    for note in fetched {
        let id = note.id;
        if !seen.contains(&id)
            && let (Some(session), Some(pane)) = (note.sitzung, note.pane)
        {
            // An event whose session is gone is confirmed all the same: it
            // would come again forever, and there is no ✦ left to clear.
            if let Err(e) = clear(&session, pane) {
                eprintln!("seen in AI Brain, pane {pane} of {session}: {e:#}");
            }
        }
        seen.push(id);
        if seen.len() > SEEN {
            seen.remove(0);
        }
        let _ = write_file("imkr-brain-seen.json", &seen);
        let _ = cli(&["bestaetigen", &id.to_string()], "");
    }
    Ok(count)
}

/// Tells the session that pane `pane` has been seen.
fn clear(session: &str, pane: PaneId) -> Result<()> {
    let socket = protocol::session_socket(session)?;
    crate::client::request_at(&socket, Request::Seen { pane })?;
    Ok(())
}

fn read_file<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    path(name)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_file<T: serde::Serialize>(name: &str, value: &T) -> Result<()> {
    std::fs::write(path(name)?, serde_json::to_vec(value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_on_a_character_boundary() {
        assert_eq!(cut("abc", 10), "abc");
        // „ä" is two bytes: cutting at 3 must not split it.
        assert_eq!(cut("aäb", 3), "aä");
        assert_eq!(cut("aäb", 2), "a");
    }

    fn note() -> Note {
        Note {
            kind: "blocked".into(),
            machine: "codingmachine".into(),
            session: "imkr".into(),
            pane: 3,
            window: 1,
            title: "claude needs you".into(),
            ..Note::default()
        }
    }

    #[test]
    fn the_title_is_the_last_argument() {
        let args = args_for(&note());
        assert_eq!(args.last().unwrap(), "claude needs you");
        assert_eq!(
            args,
            [
                "senden",
                "--art",
                "blocked",
                "--rechner",
                "codingmachine",
                "--sitzung",
                "imkr",
                "--pane",
                "3",
                "--fenster",
                "1",
                "claude needs you",
            ]
        );
    }

    #[test]
    fn what_is_empty_is_left_out() {
        // An empty `--agent` would become the title's first word: the
        // harness takes everything that is not a flag's value as title.
        let mut note = note();
        note.link = "http://x".into();
        let args = args_for(&note);
        assert!(!args.iter().any(|a| a == "--agent"));
        assert_eq!(
            args[args.len() - 3..],
            ["--link", "http://x", "claude needs you"]
        );
    }
}
