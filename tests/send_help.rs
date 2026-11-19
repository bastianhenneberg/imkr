//! `imkr send --help` typed the text `--help` into the caller's own pane
//! and pressed Enter (bug #1526): `--help` was caught only as the sole
//! argument, and `send` falls back to `$IMKR_PANE`, which is set in
//! every pane. In an agent's pane that arrives as a message from the
//! user. These tests run the real binary inside a session's
//! environment, against a stand-in socket that records every byte that
//! reaches it: asking for help must not reach the session at all.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A world of its own, with a socket that takes every connection and
/// keeps what arrives, and answers nothing.
struct World {
    dir: PathBuf,
    arrived: Arc<Mutex<Vec<String>>>,
}

impl World {
    fn new(name: &str) -> World {
        // Short: socket paths end at 108 bytes.
        // Numbered: tests run in parallel and may share a name.
        static WORLDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = WORLDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/imkr-help-{}-{n}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("run")).unwrap();
        let notify = dir.join("bin/notify-send");
        std::fs::write(&notify, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&notify, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("config.toml"), "notifications = \"off\"\n").unwrap();
        let listener = UnixListener::bind(dir.join("s.sock")).unwrap();
        let arrived = Arc::new(Mutex::new(Vec::new()));
        let keep = arrived.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let keep = keep.clone();
                std::thread::spawn(move || {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(500)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                    }
                    keep.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&bytes).into_owned());
                });
            }
        });
        World { dir, arrived }
    }

    /// `imkr ARGS` as an agent in pane 0 of session `own` runs it.
    fn imkr(&self, args: &[&str]) -> Output {
        let dir = &self.dir;
        Command::new(env!("CARGO_BIN_EXE_imkr"))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", dir.join("bin").display()),
            )
            .env("HOME", dir.join("home"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
            .env("XDG_RUNTIME_DIR", dir.join("run"))
            .env("IMKR_CONFIG", dir.join("config.toml"))
            .env("IMKR_SOCKET", dir.join("s.sock"))
            .env("IMKR_SESSION", "own")
            .env("IMKR_PANE", "0")
            .env("IMKR", "1")
            .env("IMKR_HOSTNAME", "help-test")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    /// Everything that reached the session, once the connections ended.
    fn arrived(&self) -> Vec<String> {
        std::thread::sleep(Duration::from_millis(700));
        self.arrived.lock().unwrap().clone()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr)
}

fn shows_help(command: &str, args: &[&str]) {
    let world = World::new(command);
    let output = world.imkr(args);
    let arrived = world.arrived();
    assert!(
        arrived.is_empty(),
        "`imkr {}` reached the session: {arrived:?}",
        args.join(" ")
    );
    assert!(
        output.status.success(),
        "`imkr {}` failed: {}",
        args.join(" "),
        text(&output)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("usage: imkr {command}")),
        "`imkr {}` printed no usage: {}",
        args.join(" "),
        text(&output)
    );
}

/// The case of bug #1526: not one byte for the session, so nothing can
/// be typed into the pane — the usage instead.
#[test]
fn send_help_types_nothing_and_shows_the_usage() {
    shows_help("send", &["send", "--help"]);
    shows_help("send", &["send", "-h"]);
    shows_help("send", &["send", "--pane", "3", "--help"]);
}

/// The other commands with options of their own: asking them for help
/// reads no pane, sends no message, starts no session.
#[test]
fn other_commands_show_their_usage_too() {
    shows_help("msg", &["msg", "--help"]);
    shows_help("read", &["read", "--help"]);
    shows_help("spawn", &["spawn", "--help"]);
    shows_help("keeper", &["keeper", "--help"]);
}

/// A word that looks like an option and is none is almost always a typo
/// and almost never the message: refused, nothing typed.
#[test]
fn an_unknown_option_is_refused_not_typed() {
    for args in [
        &["send", "--pain", "3", "hello"][..],
        &["send", "--pane", "3", "--keys", "Enter"],
        &["send", "--pane", "3", "hello", "--no-entr"],
        &["msg", "own/0", "--wiat", "hello"],
    ] {
        let world = World::new("unknown");
        let output = world.imkr(args);
        let arrived = world.arrived();
        assert!(
            arrived.is_empty(),
            "`imkr {}` reached the session: {arrived:?}",
            args.join(" ")
        );
        assert!(
            !output.status.success(),
            "`imkr {}` was not refused: {}",
            args.join(" "),
            text(&output)
        );
        assert!(
            text(&output).contains("unknown option"),
            "`imkr {}` does not say why: {}",
            args.join(" "),
            text(&output)
        );
    }
}

/// A message that really begins with `--` goes after `--`; from there
/// on everything is text, options included.
#[test]
fn after_a_double_dash_everything_is_text() {
    let world = World::new("dashdash");
    let output = world.imkr(&["send", "--pane", "3", "--", "--help", "--no-enter"]);
    let arrived = world.arrived().join("\n");
    // The stand-in answers nothing, so the command itself fails; what
    // counts is what it asked the session to type.
    assert!(
        arrived.contains(r#""text":"--help --no-enter""#),
        "not sent as text: {arrived} / {}",
        text(&output)
    );
    assert!(
        arrived.contains(r#""enter":true"#),
        "--no-enter after -- was taken as an option: {arrived}"
    );
}

/// Only a word that looks like an option is refused: dashes in front of
/// text are text — a file with front matter, a quoted sentence.
#[test]
fn text_that_only_begins_with_dashes_is_sent() {
    for (args, sent) in [
        (
            &["send", "--pane", "3", "---\ntitle: x"][..],
            r#""text":"---\ntitle: x""#,
        ),
        (
            &["send", "--pane", "3", "--so geht es"],
            r#""text":"--so geht es""#,
        ),
        (&["send", "--pane", "3", "a", "-", "b"], r#""text":"a - b""#),
    ] {
        let world = World::new("dashes");
        let output = world.imkr(args);
        let arrived = world.arrived().join("\n");
        assert!(
            arrived.contains(sent),
            "`imkr {}` did not send {sent}: {arrived} / {}",
            args.join(" "),
            text(&output)
        );
    }
}

/// The cause of bug #1526: `send` filled a missing `--pane` from
/// `$IMKR_PANE`, which every pane has — so a forgotten `--pane` typed
/// the text into the caller's own pane, with Enter. Nobody used that on
/// purpose (counted: code, probes, docs, skills, hooks — all name the
/// pane). Now refused, and the refusal says what changed, so whoever
/// meets an old call reads why it stopped working.
#[test]
fn send_without_a_pane_is_refused_and_says_why() {
    let world = World::new("nopane");
    let output = world.imkr(&["send", "hello"]);
    let arrived = world.arrived();
    assert!(arrived.is_empty(), "reached the session: {arrived:?}");
    assert!(!output.status.success(), "not refused: {}", text(&output));
    let said = text(&output);
    for words in ["--pane", "IMKR_PANE", "own pane", "0.21.114", "--pane 0"] {
        assert!(said.contains(words), "does not say {words:?}: {said}");
    }
}

/// `take_value` takes the next argument as the value, so `--pane --help`
/// would read `--help` as a pane number; the help is caught first.
#[test]
fn pane_followed_by_help_shows_the_help() {
    shows_help("send", &["send", "--pane", "--help"]);
}

/// `take_flag` compares whole arguments: a bare word `--no-enter` is the
/// option, a quoted text that contains it is text and stays whole.
#[test]
fn a_quoted_text_keeps_an_option_word_inside_it() {
    let world = World::new("quoted");
    let output = world.imkr(&["send", "--pane", "3", "say --no-enter to it"]);
    let arrived = world.arrived().join("\n");
    assert!(
        arrived.contains(r#""text":"say --no-enter to it","enter":true"#),
        "the quoted text was cut: {arrived} / {}",
        text(&output)
    );
}
