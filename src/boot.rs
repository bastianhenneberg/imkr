//! Sessions back after a reboot without anybody attaching (#9619). On a
//! machine nobody sits at, `client::run` never runs after a reboot, so
//! `revive_sessions` alone brings nothing back. With `revive_at_boot` on,
//! imkr keeps a systemd user unit that runs `imkr boot run` when the user's
//! systemd starts — at boot only with linger, otherwise at the first login.
//! What it brings back is what `revive_sessions` brings back: the sessions
//! that ran when the machine went down and stopped with it, layout and
//! shells, never closed ones, and at boot never agents (`ENV`): the resume
//! command is not even typed.
//!
//! The unit is the setting's mirror: written while it is on, removed (only
//! ours, recognised by `MARK`) while it is off, by the server whenever it
//! applies the config (`sync`). Nothing calls `systemctl`: the user's
//! systemd reads the unit and its `default.target.wants` link when it
//! starts, which is the only time it matters.
//!
//! macOS (launchd) is a named gap: `gaps` says so instead of the setting
//! doing nothing quietly.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

pub const UNIT: &str = "imkr-revive.service";

/// Set for a server `imkr boot run` starts: it brings back the layout and
/// the shells, but types no agent's resume command, whatever `resume`
/// says — nobody is there to see an agent start (#9619).
pub const ENV: &str = "IMKR_AT_BOOT";

static AT_BOOT: AtomicBool = AtomicBool::new(false);

/// Takes `ENV` out of this server's environment, so its panes do not
/// inherit it, and remembers it for `at_boot`. First thing in the server,
/// before any thread starts.
pub fn take_env() {
    if std::env::var_os(ENV).is_some() {
        AT_BOOT.store(true, Ordering::Relaxed);
        // SAFETY: called before the server starts any thread.
        unsafe { std::env::remove_var(ENV) };
    }
}

/// This server was started at boot, see `ENV`.
pub fn at_boot() -> bool {
    AT_BOOT.load(Ordering::Relaxed)
}

/// The unit's first line. Only a unit starting with it is imkr's to
/// rewrite or remove.
const MARK: &str = "# Written by imkr for the setting revive_at_boot";

/// Where the user's own systemd units live: `$XDG_CONFIG_HOME/systemd/user`.
pub fn unit_dir() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(config.join("systemd/user"))
}

/// The unit for the imkr binary at `exe`. `RemainAfterExit` and
/// `KillMode=process`: the servers it starts outlive `imkr boot run` in the
/// unit's cgroup, and stopping the unit does not end them.
pub fn unit_text(exe: &Path) -> String {
    format!(
        "{MARK}; imkr removes it when the setting is off.
[Unit]
Description=imkr: bring back the sessions that ran before the reboot

[Service]
Type=oneshot
RemainAfterExit=yes
KillMode=process
WorkingDirectory=%h
ExecStart=\"{}\" boot run

[Install]
WantedBy=default.target
",
        exe.display()
    )
}

/// The binary running now, as the unit should name it: after `imkr update`
/// replaced the file, `/proc/self/exe` ends in ` (deleted)`.
fn own_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let text = exe.to_string_lossy();
    Ok(match text.strip_suffix(" (deleted)") {
        Some(path) => PathBuf::from(path),
        None => exe,
    })
}

/// What changed on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Installed(PathBuf),
    Removed(PathBuf),
}

/// Brings the unit in line with the setting, in the user's unit folder.
pub fn sync(on: bool) -> Result<Option<Change>> {
    sync_in(&unit_dir()?, on, &own_exe()?)
}

/// `sync` in `dir`, for the binary `exe`. Leaves a unit of the same name
/// that imkr did not write alone, either way.
pub fn sync_in(dir: &Path, on: bool, exe: &Path) -> Result<Option<Change>> {
    let path = dir.join(UNIT);
    let wants = dir.join("default.target.wants").join(UNIT);
    let there = fs::read_to_string(&path).ok();
    let ours = there.as_deref().is_none_or(|t| t.starts_with(MARK));
    if !ours {
        return Ok(None);
    }
    if on {
        let text = unit_text(exe);
        let linked = fs::read_link(&wants).is_ok_and(|to| to == path);
        if there.as_deref() == Some(text.as_str()) && linked {
            return Ok(None);
        }
        fs::create_dir_all(wants.parent().unwrap_or(dir))?;
        fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        if !linked {
            let _ = fs::remove_file(&wants);
            std::os::unix::fs::symlink(&path, &wants)
                .with_context(|| format!("cannot link {}", wants.display()))?;
        }
        Ok(Some(Change::Installed(path)))
    } else {
        let link = fs::symlink_metadata(&wants).is_ok();
        if there.is_none() && !link {
            return Ok(None);
        }
        if link {
            fs::remove_file(&wants)?;
        }
        if there.is_some() {
            fs::remove_file(&path)?;
        }
        Ok(Some(Change::Removed(path)))
    }
}

/// What this machine lacks for the unit to run at boot, each as a sentence
/// with what to do. Empty: it will.
pub fn gaps() -> Vec<String> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    gaps_in(Path::new("/"), &user)
}

/// `gaps` against a filesystem rooted at `root`.
pub fn gaps_in(root: &Path, user: &str) -> Vec<String> {
    if !cfg!(target_os = "linux") {
        return vec![
            "only Linux with systemd so far: on macOS nothing starts the sessions at boot (no launchd agent yet)"
                .to_owned(),
        ];
    }
    // sd_booted(): systemd is the init system.
    if !root.join("run/systemd/system").is_dir() {
        return vec![
            "this machine does not run systemd: nothing starts the sessions at boot".to_owned(),
        ];
    }
    let mut gaps = Vec::new();
    if user.is_empty() || !root.join("var/lib/systemd/linger").join(user).exists() {
        gaps.push(
            "linger is off: the sessions come back at the first login, not at boot — `loginctl enable-linger` turns it on"
                .to_owned(),
        );
    }
    gaps
}

/// The settings row's value: on/off, and what is missing for "on".
pub fn shown(on: bool) -> String {
    if !on {
        return "off".to_owned();
    }
    let gaps = gaps();
    match gaps.first() {
        None => "on".to_owned(),
        Some(gap) if gap.starts_with("linger") => "on · no linger".to_owned(),
        Some(_) => "on · cannot (see imkr boot)".to_owned(),
    }
}

/// One line for the flash after the setting changed or the server applied it.
pub fn told(on: bool, change: &Result<Option<Change>>) -> Option<String> {
    let gaps = gaps();
    match change {
        Err(e) => Some(format!("sessions at boot: {e:#}")),
        Ok(Some(Change::Installed(path))) => Some(match gaps.first() {
            None => format!("sessions come back at boot: {}", crate::app::tilde(path)),
            Some(gap) => format!("sessions at boot: {gap}"),
        }),
        Ok(Some(Change::Removed(_))) => Some("sessions no longer come back at boot".to_owned()),
        Ok(None) if on => gaps.first().map(|gap| format!("sessions at boot: {gap}")),
        Ok(None) => None,
    }
}

/// `imkr boot [status | run]`.
pub fn main(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("status") => status(),
        Some("run") => run(),
        Some(other) => anyhow::bail!("unknown `imkr boot {other}`, see `imkr help`"),
    }
}

/// What the unit runs: the stopped sessions back, as `imkr` after a reboot
/// would. Prints to the journal what it did.
fn run() -> Result<()> {
    let (config, _) = crate::config::Config::load();
    if !config.revive_at_boot {
        println!("revive_at_boot is off, nothing to bring back");
        return Ok(());
    }
    // The servers started from here inherit it and start no agents.
    // SAFETY: no other thread runs.
    unsafe { std::env::set_var(ENV, "1") };
    let report = crate::client::revive_stopped();
    if report.is_empty() {
        println!("no session stopped with the machine");
    }
    for line in report {
        println!("{line}");
    }
    Ok(())
}

/// `imkr boot status`: the setting, the unit, what is missing, and which
/// sessions the next boot would bring back.
fn status() -> Result<()> {
    let (config, _) = crate::config::Config::load();
    let on = config.revive_at_boot;
    println!(
        "revive_at_boot: {} (settings menu: Restart sessions at boot, without a client)",
        if on { "on" } else { "off" }
    );
    let path = unit_dir()?.join(UNIT);
    let text = fs::read_to_string(&path).ok();
    match (&text, on) {
        (Some(t), _) if !t.starts_with(MARK) => {
            println!(
                "unit: {} is not imkr's, left alone",
                crate::app::tilde(&path)
            )
        }
        (Some(_), _) => println!("unit: {}", crate::app::tilde(&path)),
        (None, true) => println!(
            "unit: not written yet, a running imkr server writes it (or turn the setting off and on)"
        ),
        (None, false) => println!("unit: none"),
    }
    for gap in gaps() {
        println!("missing: {gap}");
    }
    let names = crate::persist::were_running();
    if names.is_empty() {
        println!("ran last: nothing noted");
    } else {
        println!("ran last: {}", names.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imkr-boot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_unit_follows_the_setting() {
        let dir = scratch("follow");
        let exe = Path::new("/opt/imkr/bin/imkr");
        let path = dir.join(UNIT);
        let wants = dir.join("default.target.wants").join(UNIT);
        assert_eq!(
            sync_in(&dir, false, exe).unwrap(),
            None,
            "off: nothing to do"
        );
        assert_eq!(
            sync_in(&dir, true, exe).unwrap(),
            Some(Change::Installed(path.clone()))
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("ExecStart=\"/opt/imkr/bin/imkr\" boot run"),
            "{text}"
        );
        assert!(text.contains("WantedBy=default.target"));
        assert!(
            text.contains("KillMode=process"),
            "the servers must outlive it"
        );
        assert_eq!(fs::read_link(&wants).unwrap(), path);
        // The same again changes nothing: the server applies it often.
        assert_eq!(sync_in(&dir, true, exe).unwrap(), None);
        // A new binary (an update moved it) is written again.
        let moved = Path::new("/home/u/.cargo/bin/imkr");
        assert!(sync_in(&dir, true, moved).unwrap().is_some());
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains(".cargo/bin/imkr")
        );
        assert_eq!(
            sync_in(&dir, false, exe).unwrap(),
            Some(Change::Removed(path.clone()))
        );
        assert!(!path.exists() && fs::symlink_metadata(&wants).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_unit_imkr_did_not_write_is_left_alone() {
        let dir = scratch("foreign");
        let path = dir.join(UNIT);
        fs::write(&path, "[Service]\nExecStart=/usr/bin/true\n").unwrap();
        let exe = Path::new("/usr/bin/imkr");
        assert_eq!(sync_in(&dir, true, exe).unwrap(), None);
        assert_eq!(sync_in(&dir, false, exe).unwrap(), None);
        assert!(fs::read_to_string(&path).unwrap().contains("/usr/bin/true"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The briefing's case: codingmachine had the sessions noted and
    /// nothing started them. Missing pieces are said, not swallowed.
    #[test]
    fn missing_systemd_or_linger_is_said() {
        let root = scratch("gaps");
        if cfg!(target_os = "linux") {
            let gaps = gaps_in(&root, "bastian");
            assert!(gaps[0].contains("does not run systemd"), "{gaps:?}");
            fs::create_dir_all(root.join("run/systemd/system")).unwrap();
            let gaps = gaps_in(&root, "bastian");
            assert!(gaps[0].contains("loginctl enable-linger"), "{gaps:?}");
            fs::create_dir_all(root.join("var/lib/systemd/linger")).unwrap();
            fs::write(root.join("var/lib/systemd/linger/bastian"), "").unwrap();
            assert!(gaps_in(&root, "bastian").is_empty());
            assert!(!gaps_in(&root, "").is_empty(), "no user, no linger");
        } else {
            assert!(gaps_in(&root, "bastian")[0].contains("macOS"));
        }
        let _ = fs::remove_dir_all(&root);
    }
}
