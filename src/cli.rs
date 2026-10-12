//! Commands for scripts and agents: `imkr status`, `list`, `send`, `new`,
//! `notify` and `hooks`. They talk to the running server over its socket.

use std::io::Read;

use anyhow::{Context, Result, bail};

use crate::agent::AgentState;
use crate::client;
use crate::layout::PaneId;
use crate::protocol::{self, RenameTarget, Request};

/// `imkr status <working|blocked|idle|clear> [--pane N]`
///
/// Meant for agent hooks, so it stays quiet and succeeds when it runs
/// outside imkr or the server is gone: a hook must never break the agent.
pub fn status(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?;
    let [state] = rest.as_slice() else {
        bail!("usage: imkr status <working|blocked|idle|clear> [--pane N]");
    };
    let state = match state.as_str() {
        "working" => Some(AgentState::Working),
        "blocked" => Some(AgentState::Blocked),
        "idle" => Some(AgentState::Idle),
        "clear" => None,
        other => bail!("unknown state {other:?}, use working, blocked, idle or clear"),
    };
    let Some(pane) = pane else {
        return Ok(());
    };
    // Agent hooks pass what happened as JSON on stdin.
    let hook = read_hook_input();
    if hook.as_ref().is_some_and(|hook| ignore_hook(hook, state)) {
        return Ok(());
    }
    let session = hook
        .as_ref()
        .and_then(|hook| hook["session_id"].as_str())
        .map(str::to_owned);
    let _ = client::request(Request::Status {
        pane,
        state,
        session,
    });
    Ok(())
}

/// The JSON a hook got on stdin, `None` when stdin is a terminal or holds
/// no JSON.
fn read_hook_input() -> Option<serde_json::Value> {
    // SAFETY: isatty only looks at the file descriptor.
    if unsafe { libc::isatty(0) } == 1 {
        return None;
    }
    let mut text = String::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}

/// Hook events that must not change the pane's state: a subagent finishing
/// is not the agent finishing, so it must not make the pane look done.
fn ignore_hook(hook: &serde_json::Value, state: Option<AgentState>) -> bool {
    if hook["hook_event_name"] == "SubagentStop" {
        return true;
    }
    let subagent = hook.get("agent_id").is_some_and(|id| !id.is_null());
    subagent && matches!(state, Some(AgentState::Idle) | None)
}

/// `imkr ls`: the running sessions with their panes and agents.
pub fn ls(args: &[String]) -> Result<()> {
    let sessions = client::local_sessions();
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!("no sessions running");
        return Ok(());
    }
    let current = protocol::session_name();
    for info in sessions {
        let marker = if info.name == current { "*" } else { " " };
        let mut agents = String::new();
        for state in [
            AgentState::Blocked,
            AgentState::Done,
            AgentState::Working,
            AgentState::Idle,
        ] {
            if let Some(count) = info.agents.get(state.name()).filter(|&&n| n > 0) {
                agents.push_str(&format!("  {} {count} {}", state.symbol(), state.name()));
            }
        }
        println!(
            "{marker} {:<16} {} panes in {} windows{agents}",
            info.name, info.panes, info.windows
        );
    }
    Ok(())
}

/// `imkr list [--json]`
pub fn list(args: &[String]) -> Result<()> {
    let value = client::request(Request::List)?;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!(
        "{:<5} {:<3} {:<8} {:<26} {:<8} DIRECTORY",
        "PANE", "WIN", "", "NAME", "STATE"
    );
    for pane in value.as_array().into_iter().flatten() {
        let text = |key: &str| pane[key].as_str().unwrap_or("-").to_owned();
        let mut flags = Vec::new();
        if pane["focused"].as_bool() == Some(true) {
            flags.push("focused");
        }
        if pane["floating"].as_bool() == Some(true) {
            flags.push("float");
        }
        println!(
            "{:<5} {:<3} {:<8} {:<26} {:<8} {}",
            pane["pane"].to_string(),
            pane["workspace"].to_string(),
            flags.join(","),
            match pane["name"].as_str() {
                Some(name) => format!("{name} ({})", text("program")),
                None => text("program"),
            },
            text("state"),
            text("cwd"),
        );
    }
    Ok(())
}

/// `imkr send [--pane N] [--no-enter] <text>...`
pub fn send(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let enter = !take_flag(&mut rest, "--no-enter");
    if rest.is_empty() {
        bail!("usage: imkr send [--pane N] [--no-enter] <text>...");
    }
    client::request(Request::Send {
        pane,
        text: rest.join(" "),
        enter,
    })?;
    Ok(())
}

/// `imkr notify [--title T] [--pane N] <text>...`
///
/// Shows `text` as a notification, the way the settings say, and in the
/// control bar. Without a title it is titled after the pane it runs in.
pub fn notify(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let title = take_value(&mut rest, "--title")?;
    let pane = take_pane(&mut rest)?;
    if rest.is_empty() {
        bail!("usage: imkr notify [--title T] [--pane N] <text>...");
    }
    client::request(Request::Notify {
        text: rest.join(" "),
        title,
        pane,
    })?;
    Ok(())
}

/// `imkr read [--pane N] [--lines N]`: what is on a pane's screen, or its
/// last N lines with the history.
pub fn read(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let lines = take_value(&mut rest, "--lines")?
        .map(|n| n.parse::<usize>().context("--lines takes a number"))
        .transpose()?;
    let value = client::request(Request::Read { pane, lines })?;
    println!("{}", value["text"].as_str().unwrap_or_default());
    Ok(())
}

/// `imkr wait [--pane N] --until <working|blocked|idle|done|ready> [--timeout S]`
///
/// Waits until the agent in a pane reaches a state, for scripts and agents
/// that hand work to another agent. `ready` means idle or done. Exits with
/// an error on timeout, or when the pane is gone.
pub fn wait(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let until = take_value(&mut rest, "--until")?.context(
        "usage: imkr wait [--pane N] --until <working|blocked|idle|done|ready> [--timeout S]",
    )?;
    let wanted: &[&str] = match until.as_str() {
        "ready" => &["idle", "done"],
        "working" => &["working"],
        "blocked" => &["blocked"],
        "idle" => &["idle"],
        "done" => &["done"],
        other => bail!("unknown state {other:?}, use working, blocked, idle, done or ready"),
    };
    let timeout = take_value(&mut rest, "--timeout")?
        .map(|s| s.parse::<f64>().context("--timeout takes seconds"))
        .transpose()?
        .map(std::time::Duration::from_secs_f64);

    let start = std::time::Instant::now();
    loop {
        let panes = client::request(Request::List)?;
        let Some(entry) = panes
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["pane"].as_u64() == Some(pane as u64))
        else {
            bail!("pane {pane} is gone");
        };
        if let Some(state) = entry["state"].as_str()
            && wanted.contains(&state)
        {
            println!("{state}");
            return Ok(());
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            bail!("pane {pane} did not become {until} in time");
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// `imkr rename [--pane N | --window N] [--clear | <name>...]`
///
/// Without a target it names the pane it runs in, so an agent can label its
/// own pane.
pub fn rename(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let clear = take_flag(&mut rest, "--clear");
    let workspace = take_window(&mut rest)?;
    let target = match workspace {
        Some(n) => RenameTarget::Workspace(n),
        None => RenameTarget::Pane(take_pane(&mut rest)?.context("which pane? use --pane N")?),
    };
    let name = if clear {
        None
    } else if rest.is_empty() {
        bail!("usage: imkr rename [--pane N | --window N] [--clear | <name>...]");
    } else {
        Some(rest.join(" "))
    };
    client::request(Request::Rename { target, name })?;
    Ok(())
}

/// `imkr popup [--name N] [--title T] [-- command args...]`: shows the
/// popup `N`, or hides it if it is shown; the first time it starts the
/// command (the shell without one) in this directory.
pub fn popup(args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    };
    let mut options = options;
    let title = take_value(&mut options, "--title")?;
    let name = take_value(&mut options, "--name")?
        .or_else(|| command.first().cloned())
        .unwrap_or_else(|| "shell".into());
    if let Some(unknown) = options.first() {
        bail!("unknown option {unknown:?}, put the command after --");
    }
    client::request(Request::Popup {
        name,
        title,
        command,
        cwd: std::env::current_dir().ok(),
    })?;
    Ok(())
}

/// `imkr move-window [--window N] [--to] SESSION`: moves a window with its
/// running panes to another session, started if needed.
pub fn move_window(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let workspace = take_window(&mut rest)?;
    let to = match (take_value(&mut rest, "--to")?, rest.as_slice()) {
        (Some(to), []) => to,
        (None, [to]) => to.clone(),
        _ => bail!("usage: imkr move-window [--window N] <session>"),
    };
    let reply = client::request(Request::MoveWindow { workspace, to })?;
    println!(
        "window {} is now window {} of session {}",
        reply["window"],
        reply["as"],
        reply["to"].as_str().unwrap_or_default()
    );
    Ok(())
}

/// `imkr new [--float] [--window N] [-- command args...]`
pub fn new(args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    };
    let mut options = options;
    let float = take_flag(&mut options, "--float");
    let workspace = take_window(&mut options)?;
    if let Some(unknown) = options.first() {
        bail!("unknown option {unknown:?}, put the command after --");
    }
    let value = client::request(Request::New {
        command,
        float,
        workspace,
        cwd: std::env::current_dir().ok(),
    })?;
    println!("{}", value["pane"]);
    Ok(())
}

/// The Claude Code events that report the agent state, with the matcher
/// and the state each one sets. Waiting for the user is told by the
/// permission request and, with permissions skipped, by the notification
/// that a question or approval waits.
const HOOKS: &[(&str, &str, &str)] = &[
    ("SessionStart", "*", "idle"),
    ("UserPromptSubmit", "*", "working"),
    ("PreToolUse", "*", "working"),
    ("PermissionRequest", "*", "blocked"),
    (
        "Notification",
        "permission_prompt|elicitation_dialog",
        "blocked",
    ),
    ("Stop", "*", "idle"),
    ("SessionEnd", "*", "clear"),
];

/// The script the hooks run: finds imkr even where `~/.cargo/bin` is only
/// on the PATH of interactive shells, and does nothing outside imkr.
const HOOK_SCRIPT: &str = r#"#!/bin/sh
# imkr: reports Claude Code's state (working, blocked, idle) to the imkr
# pane it runs in. Installed by `imkr hooks --install`. Does nothing outside imkr.

[ -n "${IMKR_PANE:-}" ] || exit 0
bin="$(command -v imkr 2>/dev/null || echo "$HOME/.cargo/bin/imkr")"
[ -x "$bin" ] || bin="$HOME/.local/bin/imkr"
[ -x "$bin" ] || exit 0
exec "$bin" status "${1:-}"
"#;

/// `imkr hooks [--install]`: the Claude Code hook config that reports the
/// agent state; `--install` puts it into `~/.claude` right away.
pub fn hooks(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--install") {
        return install_hooks();
    }
    let mut config = serde_json::Map::new();
    for &(event, matcher, state) in HOOKS {
        config.insert(
            event.to_owned(),
            serde_json::json!([{ "matcher": matcher, "hooks": [{
                "type": "command",
                "command": format!("imkr status {state}"),
                "timeout": 10,
            }]}]),
        );
    }
    let config = serde_json::json!({ "hooks": config });
    println!("{}", serde_json::to_string_pretty(&config)?);
    eprintln!(
        "\nAdd this to ~/.claude/settings.json (merge it with hooks you already have),\n\
         or let `imkr hooks --install` do it. Outside imkr the hooks do nothing."
    );
    Ok(())
}

/// Writes the hook script to `~/.claude/hooks/imkr-agent-state.sh` and
/// puts its hooks into `~/.claude/settings.json`, replacing imkr's own from
/// before and keeping every other hook. The settings are backed up first.
fn install_hooks() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let home = std::path::PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let claude = home.join(".claude");
    let script = claude.join("hooks/imkr-agent-state.sh");
    std::fs::create_dir_all(script.parent().expect("has a parent"))?;
    std::fs::write(&script, HOOK_SCRIPT)?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;

    let path = claude.join("settings.json");
    let mut settings: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(text) => {
            std::fs::write(claude.join("settings.json.bak-imkr"), &text)?;
            serde_json::from_str(&text)
                .with_context(|| format!("cannot read {}", path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    let hooks = settings
        .as_object_mut()
        .context("settings.json is no JSON object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("\"hooks\" in settings.json is no object")?;
    let ours =
        |command: &str| command.contains("imkr-agent-state") || command.starts_with("imkr status");
    // Out with imkr's hooks from before, whatever they looked like.
    for groups in hooks.values_mut() {
        if let Some(groups) = groups.as_array_mut() {
            for group in groups.iter_mut() {
                if let Some(list) = group["hooks"].as_array_mut() {
                    list.retain(|h| !h["command"].as_str().is_some_and(ours));
                }
            }
            groups.retain(|g| g["hooks"].as_array().is_none_or(|list| !list.is_empty()));
        }
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    for &(event, matcher, state) in HOOKS {
        let entry = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
        if let Some(groups) = entry.as_array_mut() {
            groups.push(serde_json::json!({ "matcher": matcher, "hooks": [{
                "type": "command",
                "command": format!("sh '{}' {state}", script.display()),
                "timeout": 10,
            }]}));
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&settings)? + "\n")?;
    println!(
        "hooks installed: {} and {} (backup: settings.json.bak-imkr)",
        script.display(),
        path.display()
    );
    println!("Claude Code sessions started from now on report their state to imkr.");
    Ok(())
}

/// Removes `--pane N` from `args`, falling back to `$IMKR_PANE`, the pane
/// the command runs in.
fn take_pane(args: &mut Vec<String>) -> Result<Option<PaneId>> {
    let value = take_value(args, "--pane")?.or_else(|| std::env::var("IMKR_PANE").ok());
    value
        .map(|v| {
            v.parse()
                .with_context(|| format!("{v:?} is not a pane number"))
        })
        .transpose()
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// `--window N`, or `--workspace N` as it was called before.
fn take_window(args: &mut Vec<String>) -> Result<Option<u8>> {
    let mut window = None;
    for option in ["--window", "--workspace"] {
        if let Some(n) = take_value(args, option)? {
            window = Some(
                n.parse::<u8>()
                    .with_context(|| format!("{option} takes a number from 1 to 9"))?,
            );
        }
    }
    Ok(window)
}

fn take_value(args: &mut Vec<String>, option: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == option) else {
        return Ok(None);
    };
    if i + 1 >= args.len() {
        bail!("{option} needs a value");
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

/// `imkr rename-session NAME`: renames the session this command is
/// for, `-s` picks another.
pub fn rename_session(args: &[String]) -> Result<()> {
    let [name] = args else {
        bail!("usage: imkr rename-session NAME");
    };
    client::request(Request::RenameSession { name: name.clone() })?;
    println!("renamed to {name}");
    Ok(())
}

/// `imkr save [NAME]`: every running session's layout in one save
/// file, `last` without a name.
pub fn save(args: &[String]) -> Result<()> {
    let name = match args {
        [] => crate::persist::LAST,
        [name] => name.as_str(),
        _ => bail!("usage: imkr save [NAME]"),
    };
    let previous = crate::persist::load_hive(name).ok().flatten();
    let hive = client::snapshot_everywhere(None, previous.as_ref())?;
    if hive.sessions.is_empty() && hive.machines.is_empty() {
        bail!("no session is running");
    }
    let path = crate::persist::write_hive(name, &hive)?;
    println!("hive {name}: saved to {}", path.display());
    print_hive(&hive);
    if name != crate::persist::LAST {
        let to = client::push_hive(name, &hive);
        if !to.is_empty() {
            println!("  given to {}", to.join(", "));
        }
    }
    Ok(())
}

/// What a hive holds, machine by machine.
fn print_hive(hive: &crate::persist::Hive) {
    let names = |sessions: &std::collections::BTreeMap<String, crate::persist::HiveSession>| {
        sessions.keys().cloned().collect::<Vec<_>>().join(", ")
    };
    let here = if hive.machine.is_empty() {
        "here"
    } else {
        &hive.machine
    };
    println!("  ⌂ {here}: {}", names(&hive.sessions));
    for machine in &hive.machines {
        println!("  ⇄ {}: {}", machine.name, names(&machine.sessions));
    }
}

/// `imkr restore [NAME]`: starts the sessions of a save file that do
/// not run, `last` without a name. `--list` shows the save files.
pub fn restore(args: &[String]) -> Result<()> {
    match args {
        [flag] if flag == "--list" => {
            for name in crate::persist::saves() {
                let Ok(Some(hive)) = crate::persist::load_hive(&name) else {
                    continue;
                };
                let age = |saved_at: u64| {
                    let age = crate::persist::now().saturating_sub(saved_at);
                    crate::sidebar::age(std::time::Duration::from_secs(age))
                };
                let mut sessions: Vec<String> = hive
                    .sessions
                    .iter()
                    .map(|(session, saved)| format!("{session} ({} ago)", age(saved.saved_at)))
                    .collect();
                for machine in &hive.machines {
                    for (session, saved) in &machine.sessions {
                        sessions.push(format!(
                            "⇄ {}:{session} ({} ago)",
                            machine.name,
                            age(saved.saved_at)
                        ));
                    }
                }
                println!("{name:<12} {}", sessions.join(", "));
            }
            Ok(())
        }
        _ if !args.iter().any(|a| a == "--list") => {
            let mut rest = args.to_vec();
            let from = take_value(&mut rest, "--from")?;
            let take = take_value(&mut rest, "--take")?;
            let name = match rest.as_slice() {
                [] => crate::persist::LAST,
                [name] => name.as_str(),
                _ => bail!("usage: imkr restore [NAME] [--from HOST] [--take MACHINE]"),
            };
            for line in client::restore_hive(name, from.as_deref(), take.as_deref())? {
                println!("{line}");
            }
            println!("switch with Ctrl+B S, or attach with `imkr -s NAME`");
            Ok(())
        }
        _ => bail!("usage: imkr restore [NAME | --list] [--from HOST] [--take MACHINE]"),
    }
}

/// `imkr hive <ls | save | restore | rm | rename>`: hives, saved groups
/// of sessions. `save` and `restore` also work without `hive`.
pub fn hive(args: &[String]) -> Result<()> {
    const USAGE: &str =
        "usage: imkr hive <ls | save [NAME] | restore [NAME] | rm NAME | rename OLD NEW>";
    match args {
        [] => bail!(USAGE),
        [cmd] if cmd == "ls" => restore(&["--list".to_owned()]),
        [cmd, rest @ ..] if cmd == "save" => save(rest),
        // Hives between machines: all shared ones out as JSON, and in from
        // stdin, each only if newer.
        [cmd] if cmd == "export" => {
            println!(
                "{}",
                serde_json::to_string(&crate::persist::shared_hives())?
            );
            Ok(())
        }
        [cmd] if cmd == "import" => {
            let hives = serde_json::from_reader(std::io::stdin())?;
            let taken = crate::persist::import_hives(hives)?;
            if !taken.is_empty() {
                eprintln!("took {}", taken.join(", "));
            }
            Ok(())
        }
        // Another machine bringing a hive back: start these sessions here
        // (JSON on stdin), print those started.
        [cmd] if cmd == "start" => {
            let sessions = serde_json::from_reader(std::io::stdin())?;
            println!(
                "{}",
                serde_json::to_string(&client::restore_sessions(sessions)?)?
            );
            Ok(())
        }
        // For a hive saved elsewhere: this machine's sessions as JSON.
        [cmd] if cmd == "snapshot" => {
            println!("{}", serde_json::to_string(&client::snapshot_all()?)?);
            Ok(())
        }
        [cmd, rest @ ..] if cmd == "restore" => restore(rest),
        [cmd, name] if cmd == "rm" => {
            crate::persist::delete_hive(name)?;
            println!("hive {name} deleted");
            Ok(())
        }
        [cmd, old, name] if cmd == "rename" => {
            crate::persist::rename_hive(old, name)?;
            println!("hive {old} is now {name}");
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

/// `imkr plugins`: the plugins and what is wrong with broken files.
pub fn plugins() -> Result<()> {
    let (plugins, broken) = crate::plugin::load();
    if let Some(dir) = crate::plugin::dir() {
        println!("plugins in {}", dir.display());
    }
    if plugins.is_empty() && broken.is_empty() {
        println!("none yet");
    }
    for plugin in &plugins {
        let key = plugin.key.map(|k| format!("^B p {k}")).unwrap_or_default();
        println!("  {:<16} {:<10} {}", plugin.name, key, plugin.command);
    }
    for file in &broken {
        println!("  {:<16} broken: {}", file.name, file.problem);
    }
    Ok(())
}

/// `imkr hosts [add NAME | rm NAME]`: the other machines this one shows,
/// from the settings and remembered.
pub fn hosts(args: &[String]) -> Result<()> {
    match args {
        [] => {
            let config = crate::config::Config::load().0.hosts;
            for host in &config {
                println!("{host:<24} settings");
            }
            for host in client::remembered_hosts() {
                if !config.contains(&host) {
                    println!("{host:<24} remembered");
                }
            }
            Ok(())
        }
        [cmd, name] if cmd == "add" => {
            if *name != crate::protocol::machine_name() {
                client::remember_host(name);
            }
            Ok(())
        }
        [cmd, name] if cmd == "rm" => {
            if !client::forget_host(name) {
                bail!("{name} is not remembered; hosts from the settings go there");
            }
            Ok(())
        }
        _ => bail!("usage: imkr hosts [add NAME | rm NAME]"),
    }
}

/// `imkr update`: the running server becomes this binary, keeping every
/// pane and the attached client, see `upgrade`.
pub fn update() -> Result<()> {
    update_to(std::env::current_exe()?)
}

/// `imkr upgrade` has to name the path itself: once the binary has been
/// replaced, `current_exe()` reads back as `… (deleted)`, because the running
/// process still holds the old, now unlinked inode.
pub fn update_to(exe: std::path::PathBuf) -> Result<()> {
    let reply = client::request(Request::Upgrade { exe: exe.clone() })?;
    let panes = reply["panes"].as_u64().unwrap_or(0);
    // The new server takes the socket over; wait until it answers.
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if client::request(Request::List).is_ok() {
            println!("updated to {} ({panes} panes kept)", exe.display());
            return Ok(());
        }
    }
    anyhow::bail!("the server did not come back, see its log next to the socket")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_are_taken_out() {
        let mut a = args(&["--pane", "4", "hello", "--no-enter", "world"]);
        assert_eq!(take_value(&mut a, "--pane").unwrap(), Some("4".into()));
        assert!(take_flag(&mut a, "--no-enter"));
        assert_eq!(a, args(&["hello", "world"]));
        assert!(take_value(&mut args(&["--pane"]), "--pane").is_err());
    }

    #[test]
    fn subagents_do_not_finish_the_agent() {
        let sub = serde_json::json!({"hook_event_name": "Stop", "agent_id": "a1"});
        assert!(ignore_hook(&sub, Some(AgentState::Idle)));
        assert!(!ignore_hook(&sub, Some(AgentState::Working)));
        let main = serde_json::json!({"hook_event_name": "Stop", "session_id": "s"});
        assert!(!ignore_hook(&main, Some(AgentState::Idle)));
        let stop = serde_json::json!({"hook_event_name": "SubagentStop"});
        assert!(ignore_hook(&stop, Some(AgentState::Working)));
    }

    #[test]
    fn status_outside_imkr_is_a_quiet_no_op() {
        // No --pane and, in the test environment, no IMKR_PANE.
        if std::env::var_os("IMKR_PANE").is_none() {
            assert!(status(&args(&["working"])).is_ok());
        }
        assert!(status(&args(&["sleeping"])).is_err());
    }
}
