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
        _ => what,
    };
    let title = format!("{agent} · {}", label(action, "needs attention"));
    let identity = name
        .filter(|n| !n.trim().is_empty())
        .map(|n| label(n, "bee"))
        .unwrap_or_else(|| format!("{} / bee {pane}", label(session, "imkr")));
    let mut body = format!("{} · {identity}", label(project.unwrap_or(""), "imkr"));
    if kind == "failed" && what != "stopped with an error" {
        body.push_str(&format!(" · {}", label(what, "error")));
    }
    (title, body)
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
}
