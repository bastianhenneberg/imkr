//! Commands for scripts and agents: `imkr status`, `list`, `send`, `new`,
//! `notify` and `hooks`. They talk to the running server over its socket.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

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
    let field = |name: &str| {
        hook.as_ref()
            .and_then(|hook| hook[name].as_str())
            .map(str::to_owned)
    };
    let _ = client::request(Request::Status {
        pane,
        state,
        session: field("session_id"),
        transcript: field("transcript_path").map(std::path::PathBuf::from),
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

/// `imkr ls`: the running sessions with their panes and agents, then
/// those that stopped or were closed and can come back.
/// `imkr dirs [DIR]`: the directories in `DIR` as typed (`~` is the home
/// here), or in the one on stdin, as a JSON list, for another machine
/// completing a session's directory. Nothing when it cannot be read.
pub fn dirs(parent: Option<&str>) -> Result<()> {
    let parent = match parent {
        Some(parent) => parent.to_owned(),
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text.trim_end_matches('\n').to_owned()
        }
    };
    let names = crate::template::subdirs(&parent).unwrap_or_default();
    println!("{}", serde_json::to_string(&names)?);
    Ok(())
}

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
        if let Some(label) = info.dormant_label() {
            println!(
                "◌ {:<16} {label}, {} panes in {} windows · `imkr revive {}` brings it back",
                info.name, info.panes, info.windows, info.name
            );
            continue;
        }
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
        if let Some(quiet) = info.quiet_for().filter(|&q| q >= 60) {
            let ago = crate::sidebar::age(std::time::Duration::from_secs(quiet));
            agents.push_str(&format!(" · {ago} ago"));
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

/// `imkr send [--pane N] [--no-enter] [--stdin] <text>...`, with
/// `--stdin` the text as it comes, e.g. an answer sent through ssh.
pub fn send(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let enter = !take_flag(&mut rest, "--no-enter");
    let text = if take_flag(&mut rest, "--stdin") {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else {
        rest.join(" ")
    };
    if text.is_empty() && !enter {
        bail!("usage: imkr send [--pane N] [--no-enter] [--stdin] <text>...");
    }
    client::request(Request::Send { pane, text, enter })?;
    Ok(())
}

/// This pane's address for answers: `machine:session/pane`, with the
/// program in front when it is known (`claude@laptop:imkr/3`).
fn own_address() -> String {
    let machine = protocol::machine_name();
    let Some(pane) = std::env::var("IMKR_PANE").ok() else {
        return format!("the user at {machine}");
    };
    let session = protocol::session_name();
    let program = client::request(Request::List).ok().and_then(|panes| {
        panes
                .as_array()?
                .iter()
                .find(|p| p["pane"].as_u64().map(|n| n.to_string()).as_deref() == Some(&pane))?
                ["program"]
                .as_str()
                .map(str::to_owned)
    });
    match program {
        Some(program) => format!("{program}@{machine}:{session}/{pane}"),
        None => format!("{machine}:{session}/{pane}"),
    }
}

/// `machine:session/who`, `session/who` or `who`, the machine left out
/// when it is this one.
fn parse_address(to: &str) -> Result<(Option<String>, Option<String>, String)> {
    let to = to.rsplit_once('@').map_or(to, |(_, at)| at);
    let (machine, rest) = match to.split_once(':') {
        Some((machine, rest)) => (Some(machine.to_owned()), rest),
        None => (None, to),
    };
    let (session, who) = match rest.split_once('/') {
        Some((session, who)) => (Some(session.to_owned()), who.to_owned()),
        None if machine.is_some() => bail!("{to}: say the pane, e.g. {to}/claude"),
        None => (None, rest.to_owned()),
    };
    if who.is_empty() {
        bail!("{to}: say the pane, a number, name or program");
    }
    let machine = machine.filter(|m| *m != protocol::machine_name());
    Ok((machine, session, who))
}

/// The working directory of the pane `address` means (as for `msg`): on
/// which machine (`None`: this one) and where.
fn pane_dir(address: &str) -> Result<(Option<String>, String)> {
    let (machine, session, who) = parse_address(address)?;
    let panes: serde_json::Value = match &machine {
        Some(host) => {
            let session = session
                .clone()
                .unwrap_or_else(|| protocol::DEFAULT_SESSION.to_owned());
            let text = client::remote_output(host, &["-s", &session, "list", "--json"], "")?;
            serde_json::from_str(&text).context("no pane list from there")?
        }
        None => match &session {
            Some(name) => {
                if !protocol::sessions().contains(name) {
                    bail!("no session {name} here");
                }
                client::request_at(&protocol::session_socket(name)?, Request::List)?
            }
            None => client::request(Request::List)?,
        },
    };
    let wanted = who.to_lowercase();
    let found = panes.as_array().into_iter().flatten().find(|p| {
        p["pane"].as_u64().map(|n| n.to_string()) == Some(who.clone())
            || p["name"].as_str().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
            || p["program"].as_str().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
    });
    let dir = found
        .and_then(|p| p["cwd"].as_str())
        .with_context(|| format!("no pane {address} with a directory"))?;
    Ok((machine, dir.to_owned()))
}

/// `imkr cp SOURCE… DEST`: copies files like scp; any of them may be
/// `machine:path`, or `@pane` (`@3`, `@api/claude`, `@server:api/claude`)
/// for the directory that pane works in, with `@pane/sub/path` below it.
pub fn cp(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: imkr cp SOURCE… DEST   (machine:path, or @pane for its directory)");
    }
    let mut remote = false;
    let mut resolved = Vec::new();
    for arg in args {
        let Some(address) = arg.strip_prefix('@') else {
            remote |= arg
                .split_once(':')
                .is_some_and(|(host, _)| !host.contains('/'));
            resolved.push(arg.clone());
            continue;
        };
        // `@api/claude:sub/path`: below the pane's directory.
        let (address, below) = match address.split_once("::") {
            Some((address, below)) => (address, Some(below)),
            None => (address, None),
        };
        let (machine, dir) = pane_dir(address)?;
        let path = match below {
            Some(below) => format!("{}/{below}", dir.trim_end_matches('/')),
            None => format!("{}/", dir.trim_end_matches('/')),
        };
        match machine {
            Some(host) => {
                remote = true;
                resolved.push(format!("{host}:{path}"));
            }
            None => resolved.push(path),
        }
    }
    let program = if remote { "scp" } else { "cp" };
    let mut command = std::process::Command::new(program);
    command.arg("-rp").args(&resolved);
    eprintln!("{program} -rp {}", resolved.join(" "));
    let status = command
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} failed");
    }
    Ok(())
}

/// `imkr msg [--wait] <to> <text>...`
///
/// Sends a message to an agent, here or on another machine. The server
/// types it in once the agent is idle, never into a question, with this
/// pane's address to answer to. `--stdin` reads the text, `--from` keeps
/// the sender when the message is passed on over ssh.
pub fn msg(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: imkr msg [--wait] <to> <text>...";
    let mut rest = args.to_vec();
    let wait = take_flag(&mut rest, "--wait");
    let stdin = take_flag(&mut rest, "--stdin");
    let from = match take_value(&mut rest, "--from")? {
        Some(from) => from,
        None => own_address(),
    };
    let Some((to, words)) = rest.split_first() else {
        bail!(USAGE);
    };
    let text = if stdin {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else {
        words.join(" ")
    };
    if text.trim().is_empty() {
        bail!(USAGE);
    }
    let (machine, session, who) = parse_address(to)?;
    if let Some(machine) = machine {
        let session = session.unwrap_or_else(|| protocol::DEFAULT_SESSION.to_owned());
        let mut args = vec!["-s", &session, "msg", "--stdin", "--from", &from];
        if wait {
            args.push("--wait");
        }
        args.push(&who);
        print!("{}", client::remote_output(&machine, &args, &text)?);
        return Ok(());
    }
    let socket = match &session {
        Some(session) => protocol::session_socket(session)?,
        None => protocol::socket_path()?,
    };
    let reply = client::request_at(
        &socket,
        Request::Message {
            to: who.clone(),
            text,
            from,
        },
    )?;
    let pane = reply["pane"].as_u64().unwrap_or_default() as PaneId;
    if reply["queued"].as_bool() == Some(true) {
        println!("queued for pane {pane}: it is busy or asking something, it gets it when idle");
    } else {
        println!("delivered to pane {pane}");
    }
    if wait {
        wait_for_answer(&socket, pane)?;
    }
    Ok(())
}

/// Waits until the agent in `pane` has taken up a message and finished,
/// then prints the end of its screen.
fn wait_for_answer(socket: &std::path::Path, pane: PaneId) -> Result<()> {
    let state = |socket: &std::path::Path| -> Result<Option<String>> {
        let panes = client::request_at(socket, Request::List)?;
        let entry = panes
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["pane"].as_u64() == Some(pane as u64))
            .with_context(|| format!("pane {pane} is gone"))?;
        Ok(entry["state"].as_str().map(str::to_owned))
    };
    let start = std::time::Instant::now();
    let mut started = false;
    loop {
        let now = state(socket)?;
        match now.as_deref() {
            Some("working") => started = true,
            Some("blocked") if started => {
                println!("pane {pane} is asking something, see it with `imkr read --pane {pane}`");
                break;
            }
            // Never an agent, or done with it.
            None if start.elapsed() > std::time::Duration::from_secs(5) => break,
            Some("idle" | "done") if started => break,
            _ => {}
        }
        if start.elapsed() > std::time::Duration::from_secs(30 * 60) {
            bail!("pane {pane} did not finish in 30 minutes");
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let screen = client::request_at(
        socket,
        Request::Read {
            pane,
            lines: Some(40),
        },
    )?;
    println!("{}", screen["text"].as_str().unwrap_or_default());
    Ok(())
}

/// `imkr agents [--all] [--json]`: every agent in every session on this
/// machine, with `--all` also on the other machines known, each with its
/// address for `imkr msg`.
pub fn agents(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let json = take_flag(&mut rest, "--json");
    let all = take_flag(&mut rest, "--all");
    if !rest.is_empty() {
        bail!("usage: imkr agents [--all] [--json]");
    }
    let here = protocol::machine_name();
    let mut machines = vec![(here.clone(), Ok(client::local_sessions()))];
    if all {
        for host in client::known_hosts() {
            let sessions = client::remote_sessions(&host);
            machines.push((host, sessions));
        }
    }
    let mut out = Vec::new();
    for (machine, sessions) in machines {
        let sessions = match sessions {
            Ok(sessions) => sessions,
            Err(e) => {
                if !json {
                    println!("{machine}: offline ({e:#})");
                }
                continue;
            }
        };
        for session in sessions {
            for agent in session.agent_panes {
                let address = format!("{machine}:{}/{}", session.name, agent.pane);
                if json {
                    out.push(serde_json::json!({
                        "address": address,
                        "machine": machine,
                        "session": session.name,
                        "pane": agent.pane,
                        "program": agent.program,
                        "state": agent.state,
                        "window": agent.workspace,
                        "window_name": agent.dir,
                        "meta": agent.meta,
                    }));
                } else {
                    let at = format!("{} {} › {}", agent.workspace, agent.dir, agent.program);
                    let meta = if agent.meta.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", agent.meta)
                    };
                    println!("{address:<24} {:<8} {at}{meta}", agent.state);
                }
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    }
    Ok(())
}

/// What `subscribe` compares: per (session, pane) its state (empty
/// without an agent), program and window.
type Snapshot = std::collections::BTreeMap<(String, PaneId), (String, String, u64)>;

/// The panes of `sessions` (all running here when empty) as they are now.
/// Sessions that do not answer are left out.
fn snapshot(sessions: &[String]) -> Snapshot {
    let names = if sessions.is_empty() {
        protocol::sessions()
    } else {
        sessions.to_vec()
    };
    let mut out = Snapshot::new();
    for name in names {
        let Ok(reply) = protocol::session_socket(&name)
            .and_then(|path| client::request_at(&path, Request::List))
        else {
            continue;
        };
        for pane in reply.as_array().into_iter().flatten() {
            let Some(id) = pane["pane"].as_u64() else {
                continue;
            };
            let program = pane["name"]
                .as_str()
                .or(pane["program"].as_str())
                .unwrap_or_default();
            out.insert(
                (name.clone(), id as PaneId),
                (
                    pane["state"].as_str().unwrap_or_default().to_owned(),
                    program.to_owned(),
                    pane["workspace"].as_u64().unwrap_or(0),
                ),
            );
        }
    }
    out
}

/// What happened between `old` and `new`: panes opened and closed, agent
/// states changed, panes moved to another window.
fn changes(old: &Snapshot, new: &Snapshot) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let event = |kind: &str,
                 (session, pane): &(String, PaneId),
                 (state, program, window): &(String, String, u64)| {
        serde_json::json!({
            "event": kind,
            "session": session,
            "pane": pane,
            "program": program,
            "window": window,
            "state": if state.is_empty() { None } else { Some(state) },
        })
    };
    for (key, now) in new {
        match old.get(key) {
            None => out.push(event("pane-opened", key, now)),
            Some(was) => {
                if was.0 != now.0 {
                    let mut e = event("state", key, now);
                    e["from"] =
                        serde_json::json!(if was.0.is_empty() { None } else { Some(&was.0) });
                    out.push(e);
                }
                if was.2 != now.2 {
                    let mut e = event("pane-moved", key, now);
                    e["from_window"] = serde_json::json!(was.2);
                    out.push(e);
                }
            }
        }
    }
    for (key, was) in old {
        if !new.contains_key(key) {
            out.push(event("pane-closed", key, was));
        }
    }
    out
}

/// `imkr subscribe [-s NAME …] [--snapshot]`: what changes in the sessions
/// here, as one JSON object per line, until stdin's reader goes away.
/// `--snapshot` first lists every pane as `pane-opened`.
pub fn subscribe(args: &[String], only: Option<String>) -> Result<()> {
    use std::io::Write;
    let mut rest = args.to_vec();
    let with_snapshot = take_flag(&mut rest, "--snapshot");
    if let Some(unknown) = rest.first() {
        bail!("unknown option {unknown:?}; usage: imkr subscribe [--snapshot]");
    }
    let sessions: Vec<String> = only.into_iter().collect();
    let mut last = if with_snapshot {
        Snapshot::new()
    } else {
        snapshot(&sessions)
    };
    let mut out = std::io::stdout().lock();
    loop {
        let now = snapshot(&sessions);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for mut change in changes(&last, &now) {
            change["time"] = serde_json::json!(time);
            // The reader is gone: done.
            if writeln!(out, "{change}")
                .and_then(|()| out.flush())
                .is_err()
            {
                return Ok(());
            }
        }
        last = now;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// The guide for an agent in an imkr pane, as `imkr skill` prints it and
/// `--install` puts it into Claude Code's skills.
const SKILL: &str = include_str!("../skills/imkr/SKILL.md");

/// `imkr skill [--install]`
pub fn skill(args: &[String]) -> Result<()> {
    match args {
        [] => {
            print!("{SKILL}");
            Ok(())
        }
        [install] if install == "--install" => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            let dir = std::path::PathBuf::from(home).join(".claude/skills/imkr");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("SKILL.md");
            std::fs::write(&path, SKILL)?;
            println!("installed {}", path.display());
            Ok(())
        }
        _ => bail!("usage: imkr skill [--install]"),
    }
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
    if !args.iter().any(|a| a == "--until") {
        return wait_command(args);
    }
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

/// Shell integration (OSC 133) for zsh: marks where prompts, commands and
/// their output are, and how commands ended. Only inside imkr.
const ZSH_INTEGRATION: &str = r#"# imkr shell integration: `eval "$(imkr shell-integration zsh)"` in ~/.zshrc
if [[ -n "$IMKR" ]]; then
  _imkr_precmd() { local code=$?; print -n "\e]133;D;$code\a\e]133;A\a"; }
  _imkr_preexec() { print -n "\e]133;C\a"; }
  precmd_functions+=(_imkr_precmd)
  preexec_functions+=(_imkr_preexec)
fi
"#;

/// The same for bash, with a DEBUG trap for the start of a command.
const BASH_INTEGRATION: &str = r#"# imkr shell integration: `eval "$(imkr shell-integration bash)"` in ~/.bashrc
if [[ -n "$IMKR" ]]; then
  _imkr_ran=
  _imkr_preexec() {
    [[ -n "$_imkr_ran" || "$BASH_COMMAND" == _imkr_prompt* ]] && return
    _imkr_ran=1; printf '\e]133;C\a'
  }
  _imkr_prompt() { local code=$?; [[ -n "$_imkr_ran" ]] && printf '\e]133;D;%s\a' "$code"; _imkr_ran=; printf '\e]133;A\a'; }
  trap _imkr_preexec DEBUG
  PROMPT_COMMAND="_imkr_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#;

/// `imkr shell-integration zsh|bash`: what to eval in the shell's rc file.
pub fn shell_integration(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("zsh") => print!("{ZSH_INTEGRATION}"),
        Some("bash") => print!("{BASH_INTEGRATION}"),
        _ => bail!("usage: imkr shell-integration zsh|bash"),
    }
    Ok(())
}

/// The last command of `pane` as its shell told it, failing with how to
/// set the integration up when the shell tells nothing.
fn last_command(pane: PaneId) -> Result<serde_json::Value> {
    let last = client::request(Request::LastCommand { pane })?;
    if last["integration"] != serde_json::json!(true) {
        bail!(
            "the shell in pane {pane} has no imkr integration yet: add\n  \
             eval \"$(imkr shell-integration zsh)\"   (or bash)\n\
             to its rc file and start a new shell"
        );
    }
    Ok(last)
}

/// Waits until the shell of `pane` finished more than `finished` commands
/// and none runs, then prints the last one's output and exits with its
/// code. Gives up after `timeout`.
fn finish(pane: PaneId, finished: u64, timeout: Option<std::time::Duration>) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        let last = last_command(pane)?;
        let done = last["finished"].as_u64().unwrap_or(0) > finished
            && last["running"] != serde_json::json!(true);
        if done {
            let output = last["output"].as_str().unwrap_or_default();
            if !output.is_empty() {
                println!("{output}");
            }
            std::process::exit(last["exit"].as_i64().unwrap_or(0) as i32);
        }
        if timeout.is_some_and(|t| started.elapsed() > t) {
            bail!("still running after {}s", started.elapsed().as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn take_timeout(args: &mut Vec<String>) -> Result<Option<std::time::Duration>> {
    take_value(args, "--timeout")?
        .map(|v| {
            v.parse::<u64>()
                .map(std::time::Duration::from_secs)
                .with_context(|| format!("{v:?} is no number of seconds"))
        })
        .transpose()
}

/// `imkr run [--pane N] [--timeout S] -- command…`: types the command into
/// the pane's shell, waits until it ends, prints its output and exits with
/// its code. Needs the shell integration.
pub fn run(args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (Vec::new(), args.to_vec()),
    };
    let mut options = options;
    let pane = take_pane(&mut options)?.context("which pane? use --pane N")?;
    let timeout = take_timeout(&mut options)?;
    if command.is_empty() || !options.is_empty() {
        bail!("usage: imkr run [--pane N] [--timeout S] -- command…");
    }
    let before = last_command(pane)?;
    if before["running"] == serde_json::json!(true) {
        bail!("a command runs in pane {pane} already; `imkr wait --pane {pane}` waits for it");
    }
    client::request(Request::Send {
        pane,
        text: command.join(" "),
        enter: true,
    })?;
    finish(pane, before["finished"].as_u64().unwrap_or(0), timeout)
}

/// `imkr wait [--pane N] [--timeout S]` without `--until`: waits until
/// the command running in the pane's shell ends, prints its output and
/// exits with its code.
fn wait_command(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let timeout = take_timeout(&mut rest)?;
    let now = last_command(pane)?;
    let finished = now["finished"].as_u64().unwrap_or(0);
    if now["running"] != serde_json::json!(true) {
        // Nothing runs: the last one's result.
        return finish(pane, finished.saturating_sub(1), timeout);
    }
    finish(pane, finished, timeout)
}

/// `imkr layout [ls | save NAME | NAME] [--window N]`: the layouts, or
/// saving or giving one to the active window.
pub fn layout(args: &[String]) -> Result<()> {
    match args {
        [] => bail!("usage: imkr layout ls | save NAME | NAME"),
        [ls] if ls == "ls" => {
            for (name, what) in crate::shapes::PRESETS {
                println!("{name:<20} {what}");
            }
            for (name, shape) in crate::shapes::saved() {
                println!("{name:<20} saved, {} panes", shape.panes());
            }
            Ok(())
        }
        [save, name] if save == "save" => {
            client::request(Request::Layout {
                name: name.clone(),
                save: true,
            })?;
            println!("saved the active window's layout as {name}");
            Ok(())
        }
        [name] => {
            let reply = client::request(Request::Layout {
                name: name.clone(),
                save: false,
            })?;
            println!("window {} now has layout {name}", reply["window"]);
            Ok(())
        }
        _ => bail!("usage: imkr layout ls | save NAME | NAME"),
    }
}

/// `imkr explain [--pane N] [--json]`: why a pane counts as the agent it
/// is, in its state.
pub fn explain(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let json = take_flag(&mut rest, "--json");
    let pane = take_pane(&mut rest)?.context("which pane? use --pane N")?;
    let e = client::request(Request::Explain { pane })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&e)?);
        return Ok(());
    }
    let text = |key: &str| match &e[key] {
        serde_json::Value::Null => "-".to_owned(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let argv: Vec<&str> = e["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .collect();
    println!("pane {pane}");
    println!("  runs         {}", argv.join(" "));
    match e["agent"].as_str() {
        Some(agent) => println!("  agent        {agent} ({} name)", text("known_by")),
        None => println!("  agent        none"),
    }
    println!("  hooks said   {}", text("reported"));
    println!("  quiet for    {} ms", text("quiet_ms"));
    println!("  question     {}", text("question"));
    println!("  state        {}", text("state"));
    println!("  because      {}", text("why"));
    Ok(())
}

/// `imkr fleet NAME [-n N] [--agent A] PROMPT…` starts N agents on the
/// same prompt, each in a worktree window of its own (`NAME-1` …);
/// `imkr fleet NAME --diff` shows what each changed since.
pub fn fleet(args: &[String]) -> Result<()> {
    const USAGE: &str =
        "usage: imkr fleet NAME [-n N] [--agent claude] PROMPT… | imkr fleet NAME --diff";
    let mut rest = args.to_vec();
    let diff = take_flag(&mut rest, "--diff");
    let count: usize = take_value(&mut rest, "-n")?
        .map(|n| n.parse())
        .transpose()
        .context("-n wants a number")?
        .unwrap_or(3);
    let agent = take_value(&mut rest, "--agent")?.unwrap_or_else(|| "claude".into());
    let Some((name, prompt)) = rest.split_first() else {
        bail!(USAGE);
    };
    if !crate::worktree::valid_name(name) {
        bail!("{name:?} is no branch name: letters, digits, - _ . and /");
    }
    let here = std::env::current_dir()?;
    let main = crate::worktree::main_root(&here)?;
    let base = format!("refs/imkr/fleets/{name}");
    let git = |dir: &std::path::Path, args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    };
    if diff {
        let start = git(&main, &["rev-parse", "--verify", &base])
            .with_context(|| format!("no fleet {name} in this repository"))?;
        for i in 1.. {
            let member = format!("{name}-{i}");
            let Some(dir) = crate::worktree::dir_for(&main, &member).filter(|d| d.exists()) else {
                break;
            };
            println!("== {member}  ({})", dir.display());
            let commits = git(&dir, &["log", "--oneline", &format!("{start}..HEAD")])?;
            if !commits.is_empty() {
                println!("{commits}");
            }
            let stat = git(&dir, &["diff", "--stat", &start])?;
            let new = git(&dir, &["ls-files", "--others", "--exclude-standard"])?;
            match (stat.is_empty(), new.is_empty()) {
                (true, true) if commits.is_empty() => println!("  no changes"),
                _ => {
                    if !stat.is_empty() {
                        println!("{stat}");
                    }
                    for file in new.lines() {
                        println!(" {file} (new)");
                    }
                }
            }
            println!();
        }
        println!(
            "git -C <dir> diff {} shows one in full",
            &start[..start.len().min(12)]
        );
        return Ok(());
    }
    if prompt.is_empty() {
        bail!(USAGE);
    }
    if !(1..=8).contains(&count) {
        bail!("a fleet has 1 to 8 agents");
    }
    git(&main, &["update-ref", &base, "HEAD"])?;
    let prompt = prompt.join(" ");
    for i in 1..=count {
        let member = format!("{name}-{i}");
        let reply = client::request(Request::Worktree {
            name: member.clone(),
            command: vec![agent.clone(), prompt.clone()],
            cwd: Some(here.clone()),
        })?;
        println!(
            "{member}: window {} in {}",
            reply["window"],
            reply["dir"].as_str().unwrap_or_default()
        );
    }
    println!("compare them with: imkr fleet {name} --diff");
    Ok(())
}

/// `imkr worktree NAME [-- command args...]`
pub fn worktree(args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    };
    let [name] = options.as_slice() else {
        bail!("usage: imkr worktree NAME [-- command args...]");
    };
    let reply = client::request(Request::Worktree {
        name: name.clone(),
        command,
        cwd: std::env::current_dir().ok(),
    })?;
    println!(
        "window {} in {}",
        reply["window"],
        reply["dir"].as_str().unwrap_or_default()
    );
    Ok(())
}

/// `imkr new [--float] [--window N] [--dir D] [-- command args...]`; window 0
/// is a new one.
pub fn new(args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    };
    let mut options = options;
    let float = take_flag(&mut options, "--float");
    let workspace = take_window(&mut options)?;
    let dir = take_value(&mut options, "--dir")?.map(std::path::PathBuf::from);
    // `--start`: the session is started first if it does not run.
    if take_flag(&mut options, "--start") {
        let path = protocol::socket_path()?;
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            let session = std::env::var("IMKR_SESSION").ok();
            client::start_server(&path, session.as_deref())?;
        }
    }
    if let Some(unknown) = options.first() {
        bail!("unknown option {unknown:?}, put the command after --");
    }
    let value = client::request(Request::New {
        command,
        float,
        workspace,
        cwd: dir.or_else(|| std::env::current_dir().ok()),
    })?;
    println!("{}", value["pane"]);
    Ok(())
}

/// `imkr --host H [-s S] new [--window N] [--dir D] [--focus] [-- command…]`: a
/// window whose program runs on H, in a new window of session S there
/// (the default one without), shown here in a new window of `here` (or
/// window N). The program keeps running there when this machine goes.
pub fn new_hosted(here: &std::path::Path, remote: &client::Remote, args: &[String]) -> Result<()> {
    let (options, command) = match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    };
    let mut options = options;
    let workspace = take_window(&mut options)?;
    let dir = take_value(&mut options, "--dir")?;
    let focus = take_flag(&mut options, "--focus");
    if let Some(unknown) = options.first() {
        bail!("unknown option {unknown:?}, put the command after --");
    }
    let host = remote.host.as_str();
    let mut there: Vec<String> = Vec::new();
    if let Some(session) = &remote.session {
        there.extend(["-s".into(), session.clone()]);
    }
    let mut new = there.clone();
    new.extend([
        "new".into(),
        "--start".into(),
        "--window".into(),
        "0".into(),
    ]);
    if let Some(dir) = &dir {
        new.extend(["--dir".into(), dir.clone()]);
    }
    if !command.is_empty() {
        new.push("--".into());
        new.extend(command.iter().cloned());
    }
    let quoted: Vec<String> = new.iter().map(|w| client::sh_quote(w)).collect();
    let words: Vec<&str> = quoted.iter().map(String::as_str).collect();
    let pane = client::remote_output(host, &words, "")?;
    // Its sessions and agents show up here from now on.
    client::remember_host(host);
    let pane = pane.trim();
    if pane.parse::<PaneId>().is_err() {
        bail!("{host} answered {pane:?} instead of a pane");
    }
    let exe = std::env::current_exe().context("cannot find imkr")?;
    let mut hosted = vec![
        exe.to_string_lossy().into_owned(),
        "hosted".into(),
        host.to_owned(),
        pane.to_owned(),
    ];
    hosted.extend(remote.session.iter().cloned());
    let value = client::request_at(
        here,
        Request::New {
            command: hosted,
            float: false,
            workspace: Some(workspace.unwrap_or(0)),
            cwd: None,
        },
    )?;
    let window = value["workspace"].as_u64().unwrap_or_default() as u8;
    if focus && let Some(shown) = value["pane"].as_u64() {
        let _ = client::request_at(
            here,
            Request::Reveal {
                pane: shown as PaneId,
            },
        );
    }
    // A window of its own is named after where it runs.
    if workspace.is_none() {
        let _ = client::request_at(
            here,
            Request::Rename {
                target: RenameTarget::Workspace(window),
                name: Some(host.to_owned()),
            },
        );
    }
    println!("window {window}: pane {pane} on {host}");
    Ok(())
}

/// `imkr hosted HOST PANE [SESSION]`, the program of a window from
/// `new_hosted`: shows pane PANE of HOST here, connecting again when the
/// connection drops, until the pane ends there.
pub fn hosted(args: &[String]) -> Result<()> {
    let (host, pane, session) = match args {
        [host, pane] => (host, pane, None),
        [host, pane, session] => (host, pane, Some(session)),
        _ => bail!("usage: imkr hosted HOST PANE [SESSION]"),
    };
    let mut words: Vec<&str> = Vec::new();
    if let Some(session) = session {
        words.extend(["-s", session]);
    }
    words.extend(["attach-pane", pane]);
    loop {
        let status = std::process::Command::new("ssh")
            .args([
                "-t",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                host,
            ])
            .arg(client::remote_command(&words))
            .status()
            .context("could not run ssh")?;
        // 255: ssh lost or found no connection, LOST: imkr there lost its
        // server; anything else is the pane's or imkr's answer there.
        let again = matches!(status.code(), Some(255 | LOST) | None);
        if !again {
            if !status.success() {
                eprintln!("\r\n[imkr] pane {pane} on {host} is gone. Enter closes this.");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
            }
            return Ok(());
        }
        eprint!("\r\n[imkr] no connection to {host}, trying again in 3 s\r\n");
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// `attach-pane`'s exit code when the connection to its server broke.
const LOST: i32 = 75;

/// `imkr attach-pane PANE`: shows that pane of this machine's session in
/// this terminal, see `Request::Tap`, the other end of `hosted`.
pub fn attach_pane(args: &[String]) -> Result<()> {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    let [pane] = args else {
        bail!("usage: imkr attach-pane PANE");
    };
    let pane: PaneId = pane.parse().context("PANE is a number")?;
    let mut stream = std::os::unix::net::UnixStream::connect(protocol::socket_path()?)
        .context("no imkr server running")?;
    protocol::ClientMsg::Request(Request::Tap {
        pane,
        watch: false,
        size: None,
    })
    .write_to(&mut stream)?;
    match protocol::ServerMsg::read_from(&mut stream)? {
        Some(protocol::ServerMsg::Reply(Ok(_))) => {}
        Some(protocol::ServerMsg::Reply(Err(message))) => bail!(message),
        _ => bail!("the server does not show panes elsewhere, update it"),
    }
    let out = Arc::new(Mutex::new(stream.try_clone()?));
    let send = {
        let out = Arc::clone(&out);
        move |msg: protocol::ClientMsg| msg.write_to(&mut *out.lock().unwrap())
    };
    crossterm::terminal::enable_raw_mode()?;
    let size = || crossterm::terminal::size().ok();
    let mut last = size();
    if let Some((cols, rows)) = last {
        send(protocol::ClientMsg::Event(crossterm::event::Event::Resize(
            cols, rows,
        )))?;
    }
    {
        let send = send.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || send(protocol::ClientMsg::Input(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let now = size();
            if now != last
                && let Some((cols, rows)) = now
            {
                last = now;
                let resize = crossterm::event::Event::Resize(cols, rows);
                if send(protocol::ClientMsg::Event(resize)).is_err() {
                    break;
                }
            }
        }
    });
    let mut stdout = std::io::stdout().lock();
    let ended = loop {
        match protocol::ServerMsg::read_from(&mut stream) {
            Ok(Some(protocol::ServerMsg::Output(bytes))) => {
                if stdout
                    .write_all(&bytes)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break false;
                }
            }
            Ok(Some(protocol::ServerMsg::Exited)) => break true,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break false,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    if !ended {
        // `hosted` connects again: the server was updated, or cut this
        // connection off for falling behind.
        eprintln!("lost the connection to the server");
        std::process::exit(LOST);
    }
    // The pane ended there: the window here closes as well.
    std::process::exit(0);
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
    const USAGE: &str = "usage: imkr hive <ls | save [NAME] | restore [NAME] | switch NAME [--yes] \
                         | rm NAME | rename OLD NEW>";
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
        // Only this hive's sessions: it comes back, the others end.
        [cmd, name, flags @ ..] if cmd == "switch" && flags.iter().all(|f| f == "--yes") => {
            if std::env::var_os("IMKR_SOCKET").is_some() {
                bail!("switching hives needs the sessions' names, unset IMKR_SOCKET");
            }
            let outsiders = client::hive_outsiders(name)?;
            if outsiders.is_empty() {
                println!("nothing runs outside hive {name}");
            } else {
                println!("Switching to hive {name} ends these sessions (they go into `last`):");
                for (host, session) in &outsiders {
                    match host {
                        Some(host) => println!("  {host}: {session}"),
                        None => println!("  {session}"),
                    }
                }
                if flags.is_empty() && !ask("End them and switch?")? {
                    bail!("not switched");
                }
            }
            for line in client::switch_hive(name, &outsiders, None)? {
                println!("{line}");
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

/// `imkr plugins`: the plugins and what is wrong with broken files.
pub fn plugins() -> Result<()> {
    let (plugins, broken) = crate::plugin::load();
    let dir = crate::plugin::dir();
    if let Some(dir) = &dir {
        println!("plugins in {}", dir.display());
    }
    if plugins.is_empty() && broken.is_empty() {
        println!("none yet");
    }
    let sources = dir.as_deref().map(plugin_sources).unwrap_or_default();
    for plugin in &plugins {
        let key = plugin.key.map(|k| format!("^B p {k}")).unwrap_or_default();
        let from = sources
            .get(&plugin.name)
            .map(|s| format!("  (from {s})"))
            .unwrap_or_default();
        println!("  {:<16} {:<10} {}{from}", plugin.name, key, plugin.command);
    }
    for file in &broken {
        println!("  {:<16} broken: {}", file.name, file.problem);
    }
    Ok(())
}

/// Which source (`imkr plugin add`) each linked plugin file comes from.
fn plugin_sources(dir: &Path) -> HashMap<String, String> {
    let mut from = HashMap::new();
    for source in crate::plugin::load_sources(dir) {
        let root = source.root(dir);
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if std::fs::read_link(&path).is_ok_and(|t| t.starts_with(&root))
                && let Some(name) = path.file_stem()
            {
                from.insert(name.to_string_lossy().into_owned(), source.name.clone());
            }
        }
    }
    from
}

/// `imkr plugin add SOURCE [--yes]`, `update [NAME]`, `rm NAME`: plugins
/// from a git repository or a directory, see `plugin::Source`.
pub fn plugin(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: imkr plugin [ls] | search [WORD] | market | add SOURCE|NAME [--yes] \
                         | update [NAME] [--yes] | rm NAME";
    let dir = crate::plugin::dir().context("no config directory")?;
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--yes" && *a != "-y")
        .collect();
    let mut sources = crate::plugin::load_sources(&dir);
    match args.as_slice() {
        ["search"] | ["search", _] => {
            let word = args.get(1).map(|w| w.to_lowercase());
            let offers = catalog_offers(&dir)?;
            let shown: Vec<_> = offers
                .iter()
                .filter(|o| {
                    word.as_ref().is_none_or(|w| {
                        o.entry.name.to_lowercase().contains(w)
                            || o.entry.description.to_lowercase().contains(w)
                    })
                })
                .collect();
            if shown.is_empty() {
                println!("nothing found");
            }
            print_offers(&shown, &sources);
            Ok(())
        }
        ["market"] => market(&dir),
        ["add", given] => {
            let source = resolve_plugin(&dir, given)?;
            let name = source.name.clone();
            if sources.iter().any(|s| s.name == name) {
                bail!("{name} is added already: `imkr plugin update {name}`");
            }
            let git = source.git;
            let root = source.root(&dir);
            let clone_dir = source.clone_dir(&dir);
            if git {
                let clone = Command::new("git")
                    .args(["clone", "--quiet", "--depth", "1", "--", &source.from])
                    .arg(&clone_dir)
                    .status()
                    .context("git is needed to add plugins from a repository")?;
                if !clone.success() {
                    bail!("could not clone {}", source.from);
                }
            }
            let (found, skipped) = crate::plugin::read_source(&root);
            for (name, problem) in &skipped {
                println!("not a plugin: {name}: {problem}");
            }
            if found.is_empty() {
                if git {
                    let _ = std::fs::remove_dir_all(&clone_dir);
                }
                bail!("{given} has no plugin files (*.toml in plugins/ or at its top)");
            }
            println!("{} brings, to run on this machine:", source.name);
            for (plugin, _) in &found {
                for line in crate::plugin::describe(plugin) {
                    println!("  {line}");
                }
            }
            if !yes && !ask("Add these plugins?")? {
                if git {
                    let _ = std::fs::remove_dir_all(&clone_dir);
                }
                bail!("not added");
            }
            let linked = crate::plugin::link_source(&dir, &root)?;
            report_linked(&linked);
            sources.push(source);
            crate::plugin::save_sources(&dir, &sources)?;
            reload_plugins();
            Ok(())
        }
        ["update"] | ["update", _] => {
            let only = args.get(1).copied();
            if let Some(only) = only
                && !sources.iter().any(|s| s.name == only)
            {
                bail!("no plugin source {only}, see `imkr plugin`");
            }
            if sources.is_empty() {
                println!("no plugins added with `imkr plugin add`");
            }
            for source in sources.iter().filter(|s| only.is_none_or(|o| o == s.name)) {
                let root = source.root(&dir);
                if source.git && !update_clone(source, &root, yes)? {
                    continue;
                }
                let linked = crate::plugin::link_source(&dir, &root)?;
                println!("{}:", source.name);
                report_linked(&linked);
            }
            reload_plugins();
            Ok(())
        }
        ["rm", name] => {
            let Some(at) = sources.iter().position(|s| s.name == *name) else {
                bail!("no plugin source {name}, see `imkr plugin`");
            };
            let source = sources.remove(at);
            let root = source.root(&dir);
            let removed = crate::plugin::unlink_source(&dir, &root)?;
            if source.git {
                std::fs::remove_dir_all(source.clone_dir(&dir)).ok();
            }
            crate::plugin::save_sources(&dir, &sources)?;
            println!("removed {}: {}", source.name, removed.join(", "));
            reload_plugins();
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

/// A plugin a catalog offers, with what `imkr plugin add` is given for it.
struct Offer {
    entry: crate::plugin::CatalogEntry,
    catalog: String,
    source: String,
}

/// What the catalogs of the settings offer, each brought up to date.
fn catalog_offers(dir: &Path) -> Result<Vec<Offer>> {
    let catalogs = crate::config::Config::load().0.plugin_catalogs;
    if catalogs.is_empty() {
        bail!(
            "no plugin catalogs: add them to the settings, e.g.\n  \
             plugin_catalogs = [\"owner/imkr-plugins\"]"
        );
    }
    let mut offers = Vec::new();
    for given in &catalogs {
        let fetched = crate::plugin::fetch_catalog(dir, given, true).and_then(|(catalog, root)| {
            Ok((crate::plugin::catalog_entries(&root)?, catalog, root))
        });
        match fetched {
            Ok((entries, catalog, root)) => {
                for entry in entries {
                    let source = crate::plugin::entry_source(&entry, &catalog, &root);
                    offers.push(Offer {
                        entry,
                        catalog: catalog.name.clone(),
                        source,
                    });
                }
            }
            Err(e) => eprintln!("{given}: {e}"),
        }
    }
    Ok(offers)
}

fn print_offers(offers: &[&Offer], sources: &[crate::plugin::Source]) {
    let width = offers.iter().map(|o| o.entry.name.len()).max().unwrap_or(0);
    for (i, offer) in offers.iter().enumerate() {
        let added = if sources.iter().any(|s| s.name == offer.entry.name) {
            " ✓ added"
        } else {
            ""
        };
        println!(
            "{:>3}  {:<width$}  {}{added}  ({})",
            i + 1,
            offer.entry.name,
            offer.entry.description,
            offer.catalog
        );
    }
}

/// What `imkr plugin add` was given: a source, or the name of a plugin in
/// a catalog, which then keeps that name.
fn resolve_plugin(dir: &Path, given: &str) -> Result<crate::plugin::Source> {
    let plain = !given.contains('/') && !given.contains(':') && !Path::new(given).exists();
    if !plain {
        return crate::plugin::parse_source(given).map_err(|e| anyhow!(e));
    }
    let offers = catalog_offers(dir)?;
    let offer = offers
        .into_iter()
        .find(|o| o.entry.name == given)
        .with_context(|| format!("no plugin {given} in the catalogs, see `imkr plugin search`"))?;
    let mut source = crate::plugin::parse_source(&offer.source).map_err(|e| anyhow!(e))?;
    source.name = offer.entry.name;
    Ok(source)
}

/// `imkr plugin market`: the catalogs' plugins to pick from and add, one
/// after the other, for `Ctrl+B p +`.
fn market(dir: &Path) -> Result<()> {
    loop {
        let offers = match catalog_offers(dir) {
            Ok(offers) => offers,
            Err(e) => {
                println!("{e:#}");
                wait_for_enter();
                return Ok(());
            }
        };
        let sources = crate::plugin::load_sources(dir);
        println!("Plugin marketplace\n");
        if offers.is_empty() {
            println!("The catalogs offer no plugins.");
        }
        print_offers(&offers.iter().collect::<Vec<_>>(), &sources);
        print!("\nAdd which (number or name, Enter to close)? ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        let name = match line.parse::<usize>() {
            Ok(n) if (1..=offers.len()).contains(&n) => offers[n - 1].entry.name.clone(),
            _ => line.to_owned(),
        };
        println!();
        if let Err(e) = plugin(&["add".to_owned(), name]) {
            println!("{e:#}");
        }
        println!();
    }
}

fn wait_for_enter() {
    print!("Enter to close ");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// Fetches a source's repository and, if it changed, shows what its
/// plugins run now and takes it on asking. Whether there is something new.
fn update_clone(source: &crate::plugin::Source, root: &Path, yes: bool) -> Result<bool> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .context("git is needed to update plugins")
    };
    if !git(&["fetch", "--quiet", "--depth", "1", "origin"])?
        .status
        .success()
    {
        println!("{}: could not fetch {}", source.name, source.from);
        return Ok(false);
    }
    let head = git(&["rev-parse", "HEAD"])?.stdout;
    let fetched = git(&["rev-parse", "FETCH_HEAD"])?.stdout;
    if head == fetched {
        println!("{}: up to date", source.name);
        return Ok(false);
    }
    // What the new version runs, read without checking it out yet.
    let tree = git(&["ls-tree", "-r", "--name-only", "FETCH_HEAD"])?.stdout;
    let tree = String::from_utf8_lossy(&tree).into_owned();
    let in_plugins = tree
        .lines()
        .any(|f| f.starts_with("plugins/") && f.ends_with(".toml"));
    println!("{} has a new version; its plugins run:", source.name);
    for file in tree.lines().filter(|f| {
        f.ends_with(".toml")
            && if in_plugins {
                f.starts_with("plugins/") && f.matches('/').count() == 1
            } else {
                !f.contains('/')
            }
    }) {
        // `./`: relative to the plugins' directory, not the repository's top.
        let text = git(&["show", &format!("FETCH_HEAD:./{file}")])?.stdout;
        let Ok(plugin) = toml::from_str::<crate::plugin::Plugin>(&String::from_utf8_lossy(&text))
        else {
            continue;
        };
        let name = Path::new(file).file_stem().unwrap_or_default();
        let plugin = crate::plugin::Plugin {
            name: name.to_string_lossy().into_owned(),
            ..plugin
        };
        for line in crate::plugin::describe(&plugin) {
            println!("  {line}");
        }
    }
    if !yes && !ask(&format!("Update {}?", source.name))? {
        return Ok(false);
    }
    if !git(&["reset", "--quiet", "--hard", "FETCH_HEAD"])?
        .status
        .success()
    {
        bail!("could not update {}", source.name);
    }
    Ok(true)
}

fn report_linked(linked: &crate::plugin::Linked) {
    for (plugin, _) in &linked.plugins {
        let key = plugin
            .key
            .map(|k| format!(" (Ctrl+B p {k})"))
            .unwrap_or_default();
        let missing = if plugin.installed() {
            ""
        } else {
            ", but its program is missing here"
        };
        println!("  plugin {}{key}{missing}", plugin.name);
    }
    for name in &linked.taken {
        println!("  {name}: left out, a plugin of that name is there already");
    }
    for name in &linked.gone {
        println!("  {name}: removed, it is no longer there");
    }
}

/// Asks a yes/no question in the terminal; no terminal means no.
fn ask(question: &str) -> Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        println!("{question} Pass --yes to answer without a terminal.");
        return Ok(false);
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "j" | "ja"))
}

/// Tells every session here to read the plugin files again.
fn reload_plugins() {
    for name in protocol::sessions() {
        if let Ok(path) = protocol::session_socket(&name) {
            let _ = client::request_at(&path, Request::ReloadPlugins);
        }
    }
}

/// `imkr revive NAME`: starts a session that stopped with the machine or
/// was closed, from its saved layout, without attaching to it.
pub fn revive(name: &str) -> Result<()> {
    let socket = protocol::session_socket(name)?;
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        println!("{name} runs already");
        return Ok(());
    }
    crate::persist::revive(name)?;
    client::start_server(&socket, Some(name))?;
    println!("{name} is back, attach with `imkr -s {name}` or Ctrl+B S");
    Ok(())
}

/// `imkr forget NAME`: deletes the saved layout of a session that does
/// not run, so it is no longer offered to come back.
pub fn forget(name: &str) -> Result<()> {
    let socket = protocol::session_socket(name)?;
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        bail!("{name} runs, end it with Ctrl+B s x or `imkr -s {name} kill-server`");
    }
    if crate::persist::forget(name)? {
        println!("{name} forgotten");
        Ok(())
    } else {
        bail!("no stopped or closed session {name}, see `imkr ls`")
    }
}

/// `imkr template <ls | save NAME | start NAME [SESSION]>`: session
/// templates, the windows a new session starts with.
pub fn template(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: imkr template <ls | save NAME | start NAME [SESSION]>";
    match args {
        [] => bail!(USAGE),
        [ls] if ls == "ls" => {
            let (templates, broken) = crate::template::load();
            if let Some(dir) = crate::template::dir() {
                println!("templates in {}", dir.display());
            }
            if templates.is_empty() && broken.is_empty() {
                println!("none yet, `imkr template save NAME` makes one of this session");
            }
            for template in &templates {
                println!("  {:<16} {}", template.name, template.summary());
            }
            for file in &broken {
                println!("  {:<16} broken: {}", file.name, file.problem);
            }
            Ok(())
        }
        [save, name] if save == "save" => {
            if !crate::protocol::valid_session_name(name) {
                bail!("template names use letters, digits, - and _");
            }
            let session = crate::protocol::session_name();
            let reply = client::request(Request::Snapshot)?;
            let saved: crate::persist::Saved = serde_json::from_value(reply)?;
            let template = crate::template::Template::from_saved(name, &session, &saved);
            let path = crate::template::write(&template)?;
            println!("{}: {}", path.display(), template.summary());
            Ok(())
        }
        [start, name, rest @ ..] if start == "start" && rest.len() <= 1 => {
            let session = rest.first().unwrap_or(name);
            if !crate::protocol::valid_session_name(session) {
                bail!("session names use letters, digits, - and _");
            }
            let template = crate::template::load()
                .0
                .into_iter()
                .find(|t| t.name == *name)
                .with_context(|| format!("no template {name}, see `imkr template ls`"))?;
            let cwd = std::env::current_dir().ok();
            let saved = template.to_saved(session, cwd.as_deref());
            let started = client::start_from(session, saved)?;
            if started {
                println!("started {session} from {name}, attach with `imkr -s {session}`");
            } else {
                println!("{session} runs already, attach with `imkr -s {session}`");
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
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
        [cmd] if cmd == "sync" => hosts_sync(),
        [cmd] if cmd == "tailnet" => {
            let peers = client::tailnet_peers();
            if peers.is_empty() {
                bail!("no machines found: is Tailscale installed and up (`tailscale status`)?");
            }
            let known = client::known_hosts();
            for peer in peers {
                let what = match (known.contains(&peer.name), peer.online) {
                    (true, _) => "used with imkr",
                    (false, true) => "online · `imkr hosts add` or `imkr --host` uses it",
                    (false, false) => "offline",
                };
                println!("{:<24} {:<8} {what}", peer.name, peer.os);
            }
            Ok(())
        }
        _ => bail!("usage: imkr hosts [add NAME | rm NAME | sync | tailnet]"),
    }
}

/// `imkr hosts sync`: every known machine gets the latest release, all at
/// once: installed where imkr is missing, upgraded with its sessions
/// elsewhere.
fn hosts_sync() -> Result<()> {
    let mut hosts = crate::config::Config::load().0.hosts;
    for host in client::remembered_hosts() {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    if hosts.is_empty() {
        bail!("no other machines known: `imkr hosts add NAME` or `imkr --host NAME`");
    }
    let threads: Vec<_> = hosts
        .into_iter()
        .map(|host| {
            std::thread::spawn(move || {
                let result = match client::installed_on(&host) {
                    Err(e) => Err(e),
                    Ok(false) => client::install_on(&host, false)
                        .map(|()| "installed the latest release".to_owned()),
                    Ok(true) => client::remote_output(&host, &["upgrade"], "").map(|out| {
                        out.lines()
                            .filter(|line| {
                                line.contains("->")
                                    || line.contains("already")
                                    || line.contains("updated to")
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    }),
                };
                (host, result)
            })
        })
        .collect();
    let mut failed = false;
    for thread in threads {
        let Ok((host, result)) = thread.join() else {
            continue;
        };
        match result {
            Ok(done) => println!("{host:<20} {done}"),
            Err(e) => {
                failed = true;
                println!("{host:<20} {e:#}");
            }
        }
    }
    if failed {
        bail!("not every machine is up to date");
    }
    Ok(())
}

/// `imkr update`: the running servers become this binary, keeping every
/// pane and the attached clients, see `upgrade`. Every session on this
/// machine, or only `only` (`-s NAME`).
pub fn update(only: Option<String>) -> Result<()> {
    update_to(std::env::current_exe()?, only)
}

/// `imkr upgrade` has to name the path itself: once the binary has been
/// replaced, `current_exe()` reads back as `… (deleted)`, because the running
/// process still holds the old, now unlinked inode.
pub fn update_to(exe: std::path::PathBuf, only: Option<String>) -> Result<()> {
    // A socket of its own outside the runtime directory (tests) is the only
    // one meant; a pane's own `IMKR_SOCKET` lies inside it.
    let own = std::env::var_os("IMKR_SOCKET")
        .map(std::path::PathBuf::from)
        .filter(|path| path.parent() != protocol::runtime_dir().ok().as_deref());
    let sockets: Vec<(String, std::path::PathBuf)> = match (only, own) {
        (None, Some(path)) => vec![(path.display().to_string(), path)],
        (Some(name), _) => vec![(name.clone(), protocol::session_socket(&name)?)],
        (None, None) => protocol::sessions()
            .into_iter()
            .filter_map(|name| Some((name.clone(), protocol::session_socket(&name).ok()?)))
            .collect(),
    };
    if sockets.is_empty() {
        anyhow::bail!("no imkr server running");
    }
    let mut failed = Vec::new();
    for (name, socket) in sockets {
        let result = update_at(&socket, &exe);
        match result {
            Ok(panes) => println!("{name}: updated to {} ({panes} panes kept)", exe.display()),
            Err(e) => {
                println!("{name}: {e:#}");
                failed.push(name);
            }
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("not updated: {}", failed.join(", "));
    }
    Ok(())
}

/// Hands the server at `socket` over to `exe` and waits until it answers
/// again. The number of panes it kept.
fn update_at(socket: &std::path::Path, exe: &std::path::Path) -> Result<u64> {
    let reply = client::request_at(
        socket,
        Request::Upgrade {
            exe: exe.to_path_buf(),
        },
    )?;
    let panes = reply["panes"].as_u64().unwrap_or(0);
    // The new server takes the socket over; wait until it answers.
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if client::request_at(socket, Request::List).is_ok() {
            return Ok(panes);
        }
    }
    anyhow::bail!("the server did not come back, see its log next to the socket")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_reports_what_changed() {
        let pane = |state: &str, window| (state.to_owned(), "claude".to_owned(), window);
        let old: Snapshot = [
            (("a".into(), 1), pane("working", 1)),
            (("a".into(), 2), pane("", 1)),
        ]
        .into();
        let new: Snapshot = [
            (("a".into(), 1), pane("blocked", 2)),
            (("a".into(), 3), pane("", 1)),
        ]
        .into();
        let events: Vec<String> = changes(&old, &new)
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            events,
            ["state", "pane-moved", "pane-opened", "pane-closed"]
        );
        let state = &changes(&old, &new)[0];
        assert_eq!(state["from"], "working");
        assert_eq!(state["state"], "blocked");
        assert!(changes(&new, &new).is_empty());
    }

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

    #[test]
    fn addresses_name_machine_session_and_pane() {
        let here = protocol::machine_name();
        let parse = |to: &str| parse_address(to).unwrap();
        assert_eq!(parse("claude"), (None, None, "claude".into()));
        assert_eq!(parse("api/3"), (None, Some("api".into()), "3".into()));
        assert_eq!(
            parse("claude@server:api/3"),
            (Some("server".into()), Some("api".into()), "3".into())
        );
        // This machine's own name is no reason for ssh.
        assert_eq!(
            parse(&format!("{here}:api/3")),
            (None, Some("api".into()), "3".into())
        );
        assert!(parse_address("server:api").is_err());
        assert!(parse_address("api/").is_err());
    }
}
