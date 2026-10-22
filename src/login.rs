//! Watching the Claude logins of the accounts. A login is an access key
//! that lasts some hours and a refresh token that lasts about 30 days from
//! `/login`: renewing the key does not move that end (seen 2026-10-20), so
//! no renewal keeps a login alive for good. Claude Code empties a login
//! when the server refuses its refresh token. One imkr per machine (the one
//! holding `renew.lock`) says once when a login ends soon and when one is
//! logged out, with the command that logs it in again.
//!
//! imkr never renews a login itself: the refresh token changes with every
//! renewal, and a Claude still holding the old one would be logged out.
//! When a fresh key is needed (`Ctrl+B s u`), Claude Code renews it,
//! answering one word, never next to a Claude running in the same account,
//! and never twice at once (a lock per account).

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::account::{Account, Store};

/// How long before its end a login is said to end soon.
pub const WARN_BEFORE: u64 = 3 * 24 * 3600;

/// The Claude login file of `account`, as JSON.
fn read_login(store: &Store, account: &Account) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(store.claude_credentials(account)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the access key of a login is out of date at `now` (seconds).
pub fn key_expired(login: &serde_json::Value, now: u64) -> bool {
    login["claudeAiOauth"]["expiresAt"].as_u64().unwrap_or(0) / 1000 <= now
}

/// When a good login ends (seconds), if within `WARN_BEFORE` of `now`.
pub fn ends_soon(login: &serde_json::Value, now: u64) -> Option<u64> {
    if crate::account::login_spent(login, now) {
        return None;
    }
    let ends = login["claudeAiOauth"]["refreshTokenExpiresAt"]
        .as_u64()
        .unwrap_or(0)
        / 1000;
    (ends > 0 && ends <= now + WARN_BEFORE).then_some(ends)
}

/// The Claude subscriptions here, `standard` too.
fn claude_logins(store: &Store) -> Vec<Account> {
    store
        .listed("claude")
        .into_iter()
        .filter(|a| a.url.is_none())
        .collect()
}

/// The Claude accounts whose login is there but spent.
pub fn dead(store: &Store) -> Vec<Account> {
    claude_logins(store)
        .into_iter()
        .filter(|a| store.login_dead(a))
        .collect()
}

/// The Claude accounts whose login ends soon, with when.
pub fn ending(store: &Store) -> Vec<(Account, u64)> {
    let now = crate::persist::now();
    claude_logins(store)
        .into_iter()
        .filter_map(|a| {
            let ends = ends_soon(&read_login(store, &a)?, now)?;
            Some((a, ends))
        })
        .collect()
}

/// When the Claude login of `account` ends, if soon (`WARN_BEFORE`).
pub fn login_ends_soon(store: &Store, account: &Account) -> Option<u64> {
    if account.url.is_some() || account.tool != "claude" {
        return None;
    }
    ends_soon(&read_login(store, account)?, crate::persist::now())
}

/// Has Claude Code renew the access key of `account` when `wanted` says so
/// of its login, read again under the account's lock, so that a renewal
/// another imkr made meanwhile is not made twice. Not while a Claude runs
/// in the account: that one renews it. True when the key is new.
pub fn renew(
    store: &Store,
    account: &Account,
    wanted: impl Fn(&serde_json::Value, u64) -> bool,
) -> bool {
    let Some(_lock) = lock(store, account) else {
        return false;
    };
    let now = crate::persist::now();
    if !read_login(store, account).is_some_and(|login| wanted(&login, now)) {
        return false;
    }
    let dir = store
        .dir_of(account)
        .unwrap_or_else(|| store.usual_dir("claude"));
    if claude_runs_in(account, &dir) {
        return false;
    }
    let mut claude = Command::new("timeout");
    claude
        .args([
            "90",
            "claude",
            "-p",
            "Answer with one word: ok",
            "--model",
            "haiku",
        ])
        .args([
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
        ])
        .args(["--setting-sources", ""])
        .current_dir(std::env::temp_dir())
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !account.is_standard() {
        claude.env("CLAUDE_CONFIG_DIR", &dir);
    }
    let _ = claude.status();
    read_login(store, account).is_some_and(|login| !key_expired(&login, crate::persist::now()))
}

/// Waits for and holds the renewal lock of `account`, until dropped.
fn lock(store: &Store, account: &Account) -> Option<File> {
    let path = locks(store).join(format!("{}-{}.lock", account.tool, account.name));
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = File::create(path).ok()?;
    // SAFETY: flock on a descriptor we own; released when `file` closes.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0).then_some(file)
}

/// The lock that makes one imkr on this machine the one watching the
/// logins, while it holds it; `None` when another one does.
pub fn take_renewer(store: &Store) -> Option<File> {
    let path = locks(store).join("renew.lock");
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = File::create(path).ok()?;
    // SAFETY: flock on a descriptor we own; released when `file` closes.
    (unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(file)
}

fn locks(store: &Store) -> PathBuf {
    store.root.join("locks")
}

/// Whether a Claude process uses `account` now: one with its
/// `CLAUDE_CONFIG_DIR`, or for `standard` one without any.
pub fn claude_runs_in(account: &Account, dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let wanted = format!("CLAUDE_CONFIG_DIR={}", dir.display());
    entries.flatten().any(|entry| {
        let path = entry.path();
        let Ok(argv) = std::fs::read(path.join("cmdline")) else {
            return false;
        };
        let program = argv.split(|&b| b == 0).next().unwrap_or_default();
        let is_claude = std::path::Path::new(std::str::from_utf8(program).unwrap_or_default())
            .file_name()
            .is_some_and(|name| name == "claude")
            || argv.windows(7).any(|w| w == b"/claude");
        // The helper of the Chrome extension runs for good and asks no model.
        if !is_claude || argv.windows(18).any(|w| w == b"chrome-native-host") {
            return false;
        }
        let Ok(env) = std::fs::read(path.join("environ")) else {
            return false;
        };
        let vars: Vec<&[u8]> = env.split(|&b| b == 0).collect();
        let configured = vars.iter().find(|v| v.starts_with(b"CLAUDE_CONFIG_DIR="));
        match configured {
            Some(var) => *var == wanted.as_bytes(),
            None => account.is_standard(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(refresh: &str, key_ends: u64, refresh_ends: u64) -> serde_json::Value {
        serde_json::json!({"claudeAiOauth": {
            "accessToken": "a", "refreshToken": refresh,
            "expiresAt": key_ends * 1000,
            "refreshTokenExpiresAt": refresh_ends * 1000}})
    }

    #[test]
    fn said_to_end_soon_three_days_ahead() {
        let day = 24 * 3600;
        let now = 100 * day;
        assert_eq!(
            ends_soon(&login("r", now, now + 2 * day), now),
            Some(now + 2 * day)
        );
        assert_eq!(ends_soon(&login("r", now, now + 5 * day), now), None);
        // Logged out already: that is said otherwise.
        assert_eq!(ends_soon(&login("", now, now + 2 * day), now), None);
        assert_eq!(ends_soon(&login("r", now, now - day), now), None);
        assert_eq!(ends_soon(&login("r", now, 0), now), None);
    }

    #[test]
    fn one_renewer_per_machine() {
        let base = std::env::temp_dir().join(format!("imkr-renewer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = Store {
            root: base.join("accounts"),
            home: base.join("home"),
        };
        let first = take_renewer(&store);
        assert!(first.is_some());
        assert!(take_renewer(&store).is_none());
        drop(first);
        // A child another test forks right then shares the lock until it
        // runs its program: free again a moment later.
        let again = (0..50).find_map(|_| {
            take_renewer(&store).or_else(|| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                None
            })
        });
        assert!(again.is_some());
        let _ = std::fs::remove_dir_all(&base);
    }
}
