//! `tmux` for programs that drive tmux, such as Claude Code's agent teams:
//! with the setting on, panes find this as `tmux` on their PATH, and its
//! commands open, type into, read and close imkr panes. Pane ids are
//! imkr's, `%3` is pane 3.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::client;
use crate::layout::PaneId;
use crate::protocol::{RenameTarget, Request};

/// The tmux version it says it is: new enough for what programs check.
const VERSION: &str = "tmux 3.4";

/// Runs `tmux args…` (argv after the program name), commands separated by
/// `;`. The exit code.
pub fn run(args: &[String]) -> i32 {
    let mut rest: &[String] = args;
    // Global options: sockets and configs are imkr's own.
    while let Some(first) = rest.first() {
        match first.as_str() {
            "-V" => {
                println!("{VERSION}");
                return 0;
            }
            "-L" | "-S" | "-f" | "-T" => rest = rest.get(2..).unwrap_or_default(),
            flag if flag.starts_with('-') && flag.len() > 1 && !flag.starts_with("--") => {
                rest = &rest[1..]
            }
            _ => break,
        }
    }
    for command in rest.split(|arg| arg == ";" || arg == "\\;") {
        if command.is_empty() {
            continue;
        }
        if let Err(e) = run_one(command) {
            eprintln!("tmux (imkr): {e:#}");
            return 1;
        }
    }
    0
}

/// A pane as `list` tells it, for targets and formats.
#[derive(Clone, Debug, Default)]
struct PaneInfo {
    id: PaneId,
    window: u8,
    window_name: String,
    program: String,
    cwd: String,
}

fn panes() -> Result<Vec<PaneInfo>> {
    let list = client::request(Request::List)?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| PaneInfo {
            id: p["pane"].as_u64().unwrap_or(0) as PaneId,
            window: p["workspace"].as_u64().unwrap_or(0) as u8,
            window_name: p["workspace_name"].as_str().unwrap_or_default().to_owned(),
            program: p["name"]
                .as_str()
                .or(p["program"].as_str())
                .unwrap_or_default()
                .to_owned(),
            cwd: p["cwd"].as_str().unwrap_or_default().to_owned(),
        })
        .collect())
}

/// The pane `target` means: `%N`, `session:window.pane` (the window's
/// first pane), `:N`; without one the pane this runs in.
fn target_pane(target: Option<&str>, all: &[PaneInfo]) -> Result<PaneInfo> {
    let own = || {
        std::env::var("TMUX_PANE")
            .ok()
            .and_then(|p| p.trim_start_matches('%').parse::<PaneId>().ok())
    };
    let id = match target {
        None | Some("") => own(),
        Some(t) => match t.rsplit_once('%') {
            Some((_, id)) => id.parse().ok(),
            None => {
                // `session:window.pane` or `:window`: the window counts.
                let window = t
                    .split_once(':')
                    .map_or(t, |(_, w)| w)
                    .split('.')
                    .next()
                    .and_then(|w| w.parse::<u8>().ok());
                match window {
                    Some(n) => all.iter().find(|p| p.window == n).map(|p| p.id),
                    None => own(),
                }
            }
        },
    };
    let id = id.context("no target pane: give -t %N")?;
    all.iter()
        .find(|p| p.id == id)
        .cloned()
        .with_context(|| format!("no pane %{id}"))
}

/// `format` with tmux's `#{…}` filled in for `pane`.
fn expand(format: &str, pane: &PaneInfo) -> String {
    let session = crate::protocol::session_name();
    let value = |key: &str| -> String {
        match key {
            "pane_id" => format!("%{}", pane.id),
            "pane_index" => pane.id.to_string(),
            "window_index" => pane.window.to_string(),
            "window_id" => format!("@{}", pane.window),
            "window_name" => pane.window_name.clone(),
            "session_name" => session.clone(),
            "session_id" => "$0".into(),
            "pane_current_path" => pane.cwd.clone(),
            "pane_current_command" | "pane_title" => pane.program.clone(),
            "pane_active" | "window_active" => "1".into(),
            "pane_width" => "80".into(),
            "pane_height" => "24".into(),
            _ => String::new(),
        }
    };
    let mut out = String::new();
    let mut rest = format;
    while let Some(start) = rest.find("#{") {
        out.push_str(&rest[..start]);
        match rest[start..].find('}') {
            Some(end) => {
                out.push_str(&value(&rest[start + 2..start + end]));
                rest = &rest[start + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.replace("#S", &session)
        .replace("#D", &format!("%{}", pane.id))
        .replace("#I", &pane.window.to_string())
        .replace("#P", &pane.id.to_string())
        .replace("#W", &pane.window_name)
}

/// The bytes tmux's key name `key` sends, `None` for text to send as it is.
fn key_bytes(key: &str) -> Option<String> {
    let named = match key {
        "Enter" | "C-m" | "KPEnter" => "\r",
        "Escape" | "C-[" => "\x1b",
        "Tab" | "C-i" => "\t",
        "BTab" => "\x1b[Z",
        "Space" => " ",
        "BSpace" => "\x7f",
        "Up" => "\x1b[A",
        "Down" => "\x1b[B",
        "Right" => "\x1b[C",
        "Left" => "\x1b[D",
        "Home" => "\x1b[H",
        "End" => "\x1b[F",
        "PageUp" | "PPage" => "\x1b[5~",
        "PageDown" | "NPage" => "\x1b[6~",
        "DC" | "Delete" => "\x1b[3~",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_owned());
    }
    // C-x and ^x: the control character.
    let letter = key
        .strip_prefix("C-")
        .or_else(|| key.strip_prefix('^'))
        .filter(|rest| rest.chars().count() == 1)?
        .chars()
        .next()?
        .to_ascii_lowercase();
    letter
        .is_ascii_lowercase()
        .then(|| char::from(letter as u8 - b'a' + 1).to_string())
}

/// Options of a command: flags that take a value, `-x value`, in `values`;
/// the others are on or off. The rest after them is the command's.
struct Options {
    flags: Vec<char>,
    values: Vec<(char, String)>,
    rest: Vec<String>,
}

impl Options {
    fn parse(args: &[String], with_value: &str) -> Self {
        let mut options = Options {
            flags: Vec::new(),
            values: Vec::new(),
            rest: Vec::new(),
        };
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--" {
                options.rest.extend_from_slice(&args[i + 1..]);
                break;
            }
            if !arg.starts_with('-') || arg.len() < 2 {
                options.rest.extend_from_slice(&args[i..]);
                break;
            }
            let chars: Vec<char> = arg[1..].chars().collect();
            for (j, &c) in chars.iter().enumerate() {
                if with_value.contains(c) {
                    let inline: String = chars[j + 1..].iter().collect();
                    let value = if inline.is_empty() {
                        i += 1;
                        args.get(i).cloned().unwrap_or_default()
                    } else {
                        inline
                    };
                    options.values.push((c, value));
                    break;
                }
                options.flags.push(c);
            }
            i += 1;
        }
        options
    }

    fn has(&self, flag: char) -> bool {
        self.flags.contains(&flag)
    }

    fn value(&self, flag: char) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(c, _)| *c == flag)
            .map(|(_, v)| v.as_str())
    }

    /// The shell command at the end, with `-e VAR=value` in front, as
    /// imkr runs it.
    fn command(&self) -> Vec<String> {
        let env: Vec<&str> = self
            .values
            .iter()
            .filter(|(c, _)| *c == 'e')
            .map(|(_, v)| v.as_str())
            .collect();
        if self.rest.is_empty() && env.is_empty() {
            return Vec::new();
        }
        let shell = if self.rest.is_empty() {
            "exec \"${SHELL:-sh}\"".to_owned()
        } else {
            self.rest.join(" ")
        };
        let mut command = vec!["env".to_owned()];
        command.extend(env.iter().map(|e| (*e).to_owned()));
        command.extend(["sh".into(), "-c".into(), shell]);
        command
    }
}

fn run_one(command: &[String]) -> Result<()> {
    let (name, args) = command.split_first().expect("not empty");
    match name.as_str() {
        "split-window" | "splitw" => {
            let o = Options::parse(args, "tcFlpe");
            let all = panes()?;
            let at = target_pane(o.value('t'), &all)?;
            let reply = client::request(Request::New {
                command: o.command(),
                float: false,
                workspace: Some(at.window),
                cwd: o
                    .value('c')
                    .map(PathBuf::from)
                    .or(Some(at.cwd.clone().into())),
            })?;
            print_new(&o, reply, at.window)
        }
        "new-window" | "neww" => {
            let o = Options::parse(args, "tcFnSe");
            let all = panes()?;
            let window = (1..=9)
                .find(|n| !all.iter().any(|p| p.window == *n))
                .context("all 9 windows are in use")?;
            let reply = client::request(Request::New {
                command: o.command(),
                float: false,
                workspace: Some(window),
                cwd: o.value('c').map(PathBuf::from),
            })?;
            if let Some(name) = o.value('n') {
                client::request(Request::Rename {
                    target: RenameTarget::Workspace(window),
                    name: Some(name.to_owned()),
                })?;
            }
            print_new(&o, reply, window)
        }
        "send-keys" | "send" => {
            let o = Options::parse(args, "tN");
            let at = target_pane(o.value('t'), &panes()?)?;
            let text: String = if o.has('l') {
                o.rest.join(" ")
            } else {
                o.rest
                    .iter()
                    .map(|key| key_bytes(key).unwrap_or_else(|| key.clone()))
                    .collect()
            };
            client::request(Request::Send {
                pane: at.id,
                text,
                enter: false,
            })?;
            Ok(())
        }
        "capture-pane" | "capturep" => {
            let o = Options::parse(args, "tSEb");
            let at = target_pane(o.value('t'), &panes()?)?;
            // `-S -N`: N lines of history over the screen.
            let lines = o
                .value('S')
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|s| *s < 0)
                .map(|s| s.unsigned_abs() as usize + 200);
            let reply = client::request(Request::Read { pane: at.id, lines })?;
            println!("{}", reply["text"].as_str().unwrap_or_default());
            Ok(())
        }
        "kill-pane" | "killp" => {
            let o = Options::parse(args, "t");
            let at = target_pane(o.value('t'), &panes()?)?;
            client::request(Request::Kill { pane: at.id })?;
            Ok(())
        }
        "display-message" | "display" => {
            let o = Options::parse(args, "tcF");
            let at = target_pane(o.value('t'), &panes()?)?;
            let format = o.value('F').map(str::to_owned).unwrap_or(o.rest.join(" "));
            let text = expand(&format, &at);
            if o.has('p') {
                println!("{text}");
            } else {
                client::request(Request::Flash { text })?;
            }
            Ok(())
        }
        "list-panes" | "lsp" | "list-windows" | "lsw" => {
            let windows = name.starts_with("list-w") || name == "lsw";
            let o = Options::parse(args, "tFf");
            let all = panes()?;
            let format = o.value('F').unwrap_or(if windows {
                "#{window_index}: #{window_name}"
            } else {
                "#{pane_index}: #{pane_id}"
            });
            let window = (!o.has('a') && !o.has('s') && !windows)
                .then(|| target_pane(o.value('t'), &all).map(|p| p.window))
                .transpose()?;
            let mut seen = Vec::new();
            for pane in all.iter().filter(|p| window.is_none_or(|w| p.window == w)) {
                if windows {
                    if seen.contains(&pane.window) {
                        continue;
                    }
                    seen.push(pane.window);
                }
                println!("{}", expand(format, pane));
            }
            Ok(())
        }
        "select-pane" | "selectp" => {
            let o = Options::parse(args, "tT");
            let at = target_pane(o.value('t'), &panes()?)?;
            if let Some(title) = o.value('T') {
                client::request(Request::Rename {
                    target: RenameTarget::Pane(at.id),
                    name: Some(title.to_owned()),
                })?;
            }
            Ok(())
        }
        "rename-window" | "renamew" => {
            let o = Options::parse(args, "t");
            let at = target_pane(o.value('t'), &panes()?)?;
            client::request(Request::Rename {
                target: RenameTarget::Workspace(at.window),
                name: o.rest.first().cloned(),
            })?;
            Ok(())
        }
        "has-session" | "has" => Ok(()),
        // What only changes looks, or tmux's own state: done.
        "select-layout" | "selectl" | "resize-pane" | "resizep" | "set-option" | "set"
        | "set-window-option" | "setw" | "refresh-client" | "refresh" | "select-window"
        | "selectw" | "wait-for" | "wait" | "source-file" | "source" | "set-environment"
        | "setenv" => Ok(()),
        "kill-session" | "kill-server" | "kill-window" | "killw" => {
            bail!("{name} is not done through imkr's tmux; close it in imkr")
        }
        other => bail!("{other} is not known to imkr's tmux"),
    }
}

/// What `split-window` and `new-window` print with `-P`.
fn print_new(o: &Options, reply: serde_json::Value, window: u8) -> Result<()> {
    if o.has('P') {
        let pane = PaneInfo {
            id: reply["pane"].as_u64().unwrap_or(0) as PaneId,
            window,
            ..Default::default()
        };
        let format = o
            .value('F')
            .unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
        println!("{}", expand(format, &pane));
    }
    Ok(())
}

/// Where the `tmux` that panes find lives: a link to this binary, made
/// again whenever the server starts.
pub fn shim_dir() -> Result<PathBuf> {
    let dir = crate::persist::state_dir()?.join("tmux-shim");
    std::fs::create_dir_all(&dir)?;
    let link = dir.join("tmux");
    let exe = std::env::current_exe()?;
    if std::fs::read_link(&link).ok().as_deref() != Some(exe.as_path()) {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_and_the_command_after_them() {
        let o = Options::parse(
            &args(&[
                "-h",
                "-dP",
                "-F#{pane_id}",
                "-t",
                "%3",
                "-e",
                "A=1",
                "claude",
                "--x",
            ]),
            "tcFlpe",
        );
        assert!(o.has('h') && o.has('d') && o.has('P'));
        assert_eq!(o.value('F'), Some("#{pane_id}"));
        assert_eq!(o.value('t'), Some("%3"));
        assert_eq!(o.command(), args(&["env", "A=1", "sh", "-c", "claude --x"]));
        assert!(Options::parse(&args(&["-h"]), "t").command().is_empty());
    }

    #[test]
    fn targets_formats_and_keys() {
        let all = vec![
            PaneInfo {
                id: 1,
                window: 1,
                ..Default::default()
            },
            PaneInfo {
                id: 4,
                window: 2,
                window_name: "api".into(),
                ..Default::default()
            },
        ];
        assert_eq!(target_pane(Some("%4"), &all).unwrap().id, 4);
        assert_eq!(target_pane(Some("work:2.0"), &all).unwrap().id, 4);
        assert_eq!(target_pane(Some(":1"), &all).unwrap().id, 1);
        assert!(target_pane(Some("%9"), &all).is_err());
        assert_eq!(
            expand("#{pane_id} in #{window_index} #W", &all[1]),
            "%4 in 2 api"
        );
        assert_eq!(key_bytes("Enter").as_deref(), Some("\r"));
        assert_eq!(key_bytes("C-c").as_deref(), Some("\x03"));
        assert_eq!(key_bytes("hello"), None);
    }
}
