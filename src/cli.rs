//! Commands for scripts and agents: `imkr status`, `list`, `send`, `new`,
//! `notify` and `hooks`. They talk to the running server over its socket.

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::agent::AgentState;
use crate::client;
use crate::clipboard;
use crate::layout::PaneId;
use crate::protocol::{self, RenameTarget, Request};

/// `imkr status <working|blocked|idle|failed|clear> [--pane N]`
///
/// Meant for agent hooks, so it stays quiet and succeeds when it runs
/// outside imkr or the server is gone: a hook must never break the agent.
pub fn status(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?;
    let [state] = rest.as_slice() else {
        bail!("usage: imkr status <working|blocked|idle|failed|clear> [--pane N]");
    };
    let state = match state.as_str() {
        "working" => Some(AgentState::Working),
        "blocked" => Some(AgentState::Blocked),
        "idle" => Some(AgentState::Idle),
        "failed" => Some(AgentState::Failed),
        "clear" => None,
        other => bail!("unknown state {other:?}, use working, blocked, idle, failed or clear"),
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
        // `StopFailure` names the error, e.g. `rate_limit`.
        reason: (state == Some(AgentState::Failed))
            .then(|| crate::agent::failure_reason(&field("error").unwrap_or_default()).to_owned()),
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
    subagent && matches!(state, Some(AgentState::Idle | AgentState::Failed) | None)
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
            AgentState::Failed,
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

/// `imkr connect`: other people's imkrs, paired once, that may send
/// messages here and get ours.
pub fn connect(args: &[String]) -> Result<()> {
    use crate::contacts;
    const USAGE: &str = "usage: imkr connect [invite | join URL CODE [--as YOU] [--name THEM] \
                         | brain PERSON [--name THEM] | fetch | inbox | accept ID | decline ID \
                         | trust NAME | untrust NAME | rm NAME]";
    let mut rest = args.to_vec();
    let own = take_value(&mut rest, "--as")?;
    let alias = take_value(&mut rest, "--name")?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match rest.as_slice() {
        [] | ["ls" | "list"] => {
            let contacts = contacts::load();
            if contacts.is_empty() {
                println!(
                    "no contacts yet: `imkr connect invite` here, then `imkr connect join` there"
                );
            }
            for c in &contacts {
                let trust = if c.trusted {
                    "trusted, typed right away"
                } else {
                    "asks first"
                };
                let via = match &c.brain {
                    Some(person) => format!("AI Brain ({person})"),
                    None => c.url.clone(),
                };
                println!("{:<16} {:<28} {trust}", c.name, via);
            }
            let waiting = contacts::waiting().len();
            if waiting > 0 {
                println!("{waiting} message(s) wait: `imkr connect inbox`");
            }
        }
        ["invite"] => {
            let (code, url) = contacts::invite()?;
            println!("Code {code}, good for 10 minutes. On the other machine:");
            println!();
            println!("  imkr connect join {url} {code}");
            println!();
            println!("Both of you are contacts of each other then; messages ask first.");
        }
        ["join", url, code] => {
            let own = own.unwrap_or_else(contacts::own_name);
            let contact = contacts::join(url, code, &own, alias.as_deref())?;
            println!(
                "{} is a contact now, they know you as {own}: `imkr msg {}:SESSION/PANE TEXT`",
                contact.name, contact.name
            );
        }
        ["brain", person] => {
            let contact = contacts::add_brain(person, alias.as_deref())?;
            println!(
                "{} is a contact through AI Brain: `imkr msg {}:SESSION/PANE TEXT`; \
                 their messages come here once they add you too",
                contact.name, contact.name
            );
        }
        ["fetch"] => {
            let n = contacts::fetch_brain()?;
            println!("{n} message(s) from AI Brain");
        }
        ["inbox"] => {
            let waiting = contacts::waiting();
            if waiting.is_empty() {
                println!("no messages wait");
            }
            for m in waiting {
                println!(
                    "#{} from {} to {} ({}):",
                    m.id,
                    m.from,
                    m.to,
                    crate::sidebar::age(std::time::Duration::from_secs(
                        crate::web::now().saturating_sub(m.at)
                    ))
                );
                for line in m.text.lines() {
                    println!("    {line}");
                }
            }
        }
        ["accept", id] => println!("{}", contacts::accept(message_id(id)?)?),
        ["decline", id] => {
            contacts::decline(message_id(id)?)?;
            println!("thrown away");
        }
        ["trust", name] => {
            contacts::set_trusted(name, true)?;
            println!("messages from {name} are typed right away now");
        }
        ["untrust", name] => {
            contacts::set_trusted(name, false)?;
            println!("messages from {name} wait for you again");
        }
        ["rm" | "forget", name] => {
            contacts::remove(name)?;
            println!("{name} can no longer send here");
        }
        _ => bail!(USAGE),
    }
    Ok(())
}

fn message_id(id: &str) -> Result<u64> {
    id.trim_start_matches('#')
        .parse()
        .with_context(|| format!("{id}: say the number `imkr connect inbox` shows"))
}

/// `machine:session/who`, `session/who` or `who`, the machine left out
/// when it is this one.
pub(crate) fn parse_address(to: &str) -> Result<(Option<String>, Option<String>, String)> {
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
    const USAGE: &str = "usage: imkr msg [--wait] <to> <text>... (or the text on stdin)";
    let mut rest = args.to_vec();
    let wait = take_flag(&mut rest, "--wait");
    let stdin = take_flag(&mut rest, "--stdin");
    let given = take_value(&mut rest, "--from")?;
    // Sent from a pane here (not passed on for one elsewhere): its session
    // notes it for the orchestration view.
    let mine = given.is_none();
    let from = given.unwrap_or_else(own_address);
    let Some((to, words)) = rest.split_first() else {
        bail!(USAGE);
    };
    // Without words a piped text is the message, e.g. a long one from a file.
    let text = if stdin || (words.is_empty() && !std::io::stdin().is_terminal()) {
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
    // Another person's imkr: through its web server, never ssh.
    if let Some(contact) = machine.as_deref().and_then(crate::contacts::find) {
        let to = match &session {
            Some(session) => format!("{session}/{who}"),
            None => who.clone(),
        };
        println!("{}", crate::contacts::send(&contact, &to, &text, &from)?);
        if mine {
            note_sent(&rest[0], &text);
        }
        if wait {
            println!("not waiting for an answer: it comes as a message of its own");
        }
        return Ok(());
    }
    if let Some(machine) = machine {
        let session = session.unwrap_or_else(|| protocol::DEFAULT_SESSION.to_owned());
        let mut args = vec!["-s", &session, "msg", "--stdin", "--from", &from];
        if wait {
            args.push("--wait");
        }
        args.push(&who);
        if mine {
            note_sent(to, &text);
        }
        match client::remote_output(&machine, &args, &text) {
            Ok(out) => print!("{out}"),
            // Out of reach from here, e.g. a laptop seen from a server: it
            // picks the message up when it next asks this machine.
            Err(e) => {
                client::queue_outbox(&client::OutboxMessage {
                    machine: machine.clone(),
                    session: Some(session.clone()),
                    who: who.clone(),
                    text,
                    from,
                    at: crate::persist::now(),
                })?;
                // Said as the success it is: agents read "failed" in an ssh
                // error first and gave up on a message that did arrive.
                println!(
                    "queued for {to}: {machine} picks it up from here within a minute while its \
                     imkr runs, nothing more to do (no direct way there: {})",
                    e.to_string().lines().last().unwrap_or("").trim()
                );
                if wait {
                    println!("not waiting for an answer: it comes as a message of its own");
                }
            }
        }
        return Ok(());
    }
    let socket = match &session {
        Some(session) => protocol::session_socket(session)?,
        None => protocol::socket_path()?,
    };
    if mine {
        note_sent(to, &text);
    }
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

/// Tells the session of the pane this runs in that its agent sent `to` a
/// message, for the orchestration view. Older servers do not know the
/// request, and outside imkr there is no pane: never an error.
fn note_sent(to: &str, text: &str) {
    let Some(pane) = std::env::var("IMKR_PANE")
        .ok()
        .and_then(|p| p.parse::<PaneId>().ok())
    else {
        return;
    };
    let _ = client::request(Request::Sent {
        pane,
        to: to.to_owned(),
        text: text.to_owned(),
    });
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
            Some("failed") if started => {
                bail!("pane {pane} stopped with an error, see it with `imkr read --pane {pane}`")
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
                        "title": agent.title,
                    }));
                } else {
                    let at = format!("{} {} › {}", agent.workspace, agent.dir, agent.program);
                    let meta = if agent.meta.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", agent.meta)
                    };
                    let title = if agent.title.is_empty() {
                        String::new()
                    } else {
                        format!("  · {}", agent.title)
                    };
                    println!("{address:<24} {:<8} {at}{meta}{title}", agent.state);
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
    let lines = take_value(&mut rest, "--lines")?
        .map(|n| n.parse::<usize>().context("--lines takes a number"))
        .transpose()?;
    // An address as for `msg`: another session here, or another machine.
    if let [address] = rest.as_slice()
        && !address.starts_with('-')
    {
        let (machine, session, who) = parse_address(address)?;
        if let Some(machine) = machine {
            let session = session.unwrap_or_else(|| protocol::DEFAULT_SESSION.to_owned());
            let lines = lines.map(|n| n.to_string());
            let mut args = vec!["-s", session.as_str(), "read", who.as_str()];
            if let Some(lines) = &lines {
                args.extend(["--lines", lines]);
            }
            print!("{}", client::remote_output(&machine, &args, "")?);
            return Ok(());
        }
        let socket = match &session {
            Some(name) if protocol::sessions().contains(name) => protocol::session_socket(name)?,
            Some(name) => bail!("no session {name} here"),
            None => protocol::socket_path()?,
        };
        let panes = client::request_at(&socket, Request::List)?;
        let pane = find_pane(&panes, &who).with_context(|| format!("no pane {address}"))?;
        let value = client::request_at(&socket, Request::Read { pane, lines })?;
        println!("{}", value["text"].as_str().unwrap_or_default());
        return Ok(());
    }
    let pane =
        take_pane(&mut rest)?.context("which pane? `--pane N` or an address like api/claude")?;
    let value = client::request(Request::Read { pane, lines })?;
    println!("{}", value["text"].as_str().unwrap_or_default());
    Ok(())
}

/// The pane `who` means in a session's pane list (`Request::List`): its
/// number, name or program.
fn find_pane(panes: &serde_json::Value, who: &str) -> Option<PaneId> {
    let wanted = who.to_lowercase();
    panes
        .as_array()?
        .iter()
        .find(|p| {
            p["pane"].as_u64().map(|n| n.to_string()).as_deref() == Some(who)
                || p["name"].as_str().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
                || p["program"].as_str().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
        })
        .and_then(|p| p["pane"].as_u64())
        .map(|n| n as PaneId)
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
        "usage: imkr wait [--pane N] --until <working|blocked|idle|done|failed|ready> [--timeout S]",
    )?;
    let wanted: &[&str] = match until.as_str() {
        "ready" => &["idle", "done", "failed"],
        "working" => &["working"],
        "blocked" => &["blocked"],
        "idle" => &["idle"],
        "done" => &["done"],
        "failed" => &["failed"],
        other => {
            bail!("unknown state {other:?}, use working, blocked, idle, done, failed or ready")
        }
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

/// `imkr note [--pane N] [TEXT…]`: puts a thought on a pane's notepad
/// (`Ctrl+B t`) without telling its agent; without text it lists the pad.
pub fn note(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let pane = take_pane(&mut rest)?;
    let list = rest.first().is_some_and(|w| w == "ls") && rest.len() == 1;
    let text = (!rest.is_empty() && !list).then(|| rest.join(" "));
    let adding = text.is_some();
    let reply = client::request(Request::Note { pane, text })?;
    let notes = reply["notes"].as_array().cloned().unwrap_or_default();
    if adding {
        let open = notes
            .iter()
            .filter(|n| !n["sent"].as_bool().unwrap_or(false))
            .count();
        println!("on the notepad of pane {}, {open} open", reply["pane"]);
        return Ok(());
    }
    for note in &notes {
        let sent = note["sent"].as_bool().unwrap_or(false);
        println!(
            "{} {}",
            if sent { "✓" } else { "•" },
            note["text"].as_str().unwrap_or("")
        );
    }
    if notes.is_empty() {
        println!("the notepad of pane {} is empty (Ctrl+B t)", reply["pane"]);
    }
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

/// The same for bash, with a DEBUG trap for the start of a command. The
/// trap also fires for every other prompt command (Arch's bash.bashrc adds
/// one for the window title), so a command only starts once the prompt is
/// drawn: `_imkr_prompt` runs last and lets the next one through.
const BASH_INTEGRATION: &str = r#"# imkr shell integration: `eval "$(imkr shell-integration bash)"` in ~/.bashrc
if [[ -n "$IMKR" ]]; then
  _imkr_ran= _imkr_ready=
  _imkr_preexec() {
    [[ -z "$_imkr_ready" || "$BASH_COMMAND" == _imkr_status* ]] && return
    _imkr_ready= _imkr_ran=1; printf '\e]133;C\a'
  }
  _imkr_status() { local code=$?; _imkr_ready=; [[ -n "$_imkr_ran" ]] && printf '\e]133;D;%s\a' "$code"; _imkr_ran=; }
  _imkr_prompt() { printf '\e]133;A\a'; _imkr_ready=1; }
  trap _imkr_preexec DEBUG
  if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    PROMPT_COMMAND=(_imkr_status "${PROMPT_COMMAND[@]}" _imkr_prompt)
  else
    _imkr_pc=${PROMPT_COMMAND%;}
    PROMPT_COMMAND="_imkr_status${_imkr_pc:+;$_imkr_pc};_imkr_prompt"
  fi
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
        text: command_line(&command),
        enter: true,
    })?;
    finish(pane, before["finished"].as_u64().unwrap_or(0), timeout)
}

/// What `imkr run` types: one word as the command line it is (`"make &&
/// make test"`), several as the words of one command, each quoted, as a
/// program would get them. Joined as they came, `-- echo "a b;echo x"`
/// ran two commands.
fn command_line(words: &[String]) -> String {
    match words {
        [line] => line.clone(),
        words => words
            .iter()
            .map(|w| client::sh_quote(w))
            .collect::<Vec<_>>()
            .join(" "),
    }
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
/// `imkr operator [--json]`: the machine the user sits at, as the client
/// attached to this session tells, else to another session here. For
/// agents that want to show something on the user's screen, e.g. a browser.
pub fn operator(args: &[String]) -> Result<()> {
    let mut rest = args.to_vec();
    let json = take_flag(&mut rest, "--json");
    if let Some(extra) = rest.first() {
        bail!("unknown argument {extra}, see `imkr operator --json`");
    }
    let own = client::request(Request::Operator);
    let found = own.or_else(|first| {
        crate::protocol::sessions()
            .iter()
            .filter_map(|name| crate::protocol::session_socket(name).ok())
            .find_map(|path| client::request_at(&path, Request::Operator).ok())
            .ok_or(first)
    });
    let found =
        found.map_err(|_| anyhow!("nobody is attached to an imkr session on this machine"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
    } else {
        println!("{}", found["machine"].as_str().unwrap_or_default());
    }
    Ok(())
}

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
            // A plain agent name as a placeholder: started with this
            // machine's `agent_commands`.
            command: vec![
                if agent
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.@".contains(c))
                {
                    format!("<{agent}>")
                } else {
                    agent.clone()
                },
                prompt.clone(),
            ],
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

/// `imkr worktree NAME [-- command args...] | ls [--json] | merge NAME |
/// rm NAME [--force]`
pub fn worktree(args: &[String]) -> Result<()> {
    let here = std::env::current_dir()?;
    let setup = || -> Result<crate::worktree::Setup> {
        let (config, _) = crate::config::Config::load();
        crate::worktree::setup_for(&crate::worktree::main_root(&here)?, &config.worktree)
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["ls"] | ["ls", "--json"] => {
            let list = crate::worktree::list(&here)?;
            if args.len() == 2 {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("no worktrees besides the main checkout");
            }
            for w in list {
                let state = match (w.changed, w.merged) {
                    (0, true) => "merged".to_owned(),
                    (0, false) => format!("{} commit(s) to merge", w.ahead),
                    (n, _) => format!("{n} uncommitted"),
                };
                println!("{:<24} {:<22} {}", w.name, state, w.dir.display());
            }
            return Ok(());
        }
        ["merge", name] => {
            let info = crate::worktree::merge(&here, name)?;
            println!("merged {name}");
            let panes = panes_in(&info.dir);
            crate::worktree::remove(&here, name, &setup()?, false)?;
            println!("removed {name}: worktree, window and branch");
            end_panes(panes);
            return Ok(());
        }
        ["rm", name] | ["rm", name, "--force"] | ["rm", "--force", name] => {
            let force = args.iter().any(|a| a == "--force");
            let panes = crate::worktree::find(&here, name)
                .map(|info| panes_in(&info.dir))
                .unwrap_or_default();
            crate::worktree::remove(&here, name, &setup()?, force)?;
            println!("removed {name}: worktree, window and branch");
            end_panes(panes);
            return Ok(());
        }
        ["merge" | "rm", ..] => {
            bail!("usage: imkr worktree merge NAME | imkr worktree rm NAME [--force]")
        }
        _ => {}
    }
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
    // `--focus` shows the new pane right away, `--name` names a new window:
    // for plugins that open a program to work in, like a conversation.
    let focus = take_flag(&mut options, "--focus");
    let name = take_value(&mut options, "--name")?;
    // `--start`: the session is started first if it does not run.
    let mut fresh = false;
    if take_flag(&mut options, "--start") {
        let path = protocol::socket_path()?;
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            let session = std::env::var("IMKR_SESSION").ok();
            client::start_server(&path, session.as_deref())?;
            // Started for this command: it goes where the empty first
            // shell is instead of into a window beside it (#9068).
            fresh = true;
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
        fresh,
    })?;
    if let (Some(name), Some(window)) = (name, value["workspace"].as_u64()) {
        client::request(Request::Rename {
            target: RenameTarget::Workspace(window as u8),
            name: Some(name),
        })?;
    }
    if focus && let Some(pane) = value["pane"].as_u64() {
        client::request(Request::Reveal {
            pane: pane as PaneId,
        })?;
    }
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
    let words: Vec<&str> = new.iter().map(String::as_str).collect();
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
            fresh: false,
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

/// How long `imkr tap` gathers output before passing it on: a few lines
/// per message instead of every read of the pty.
const TAP_EVERY: u64 = 150;

/// The most output `imkr tap` passes on at once, before base64. AI Brain's
/// Reverb drops messages over 100 KB without a word, and base64 adds a
/// third.
const TAP_MAX: usize = 64 * 1024;

/// `imkr tap ADDRESS [--every MS] [--max BYTES]`: shows a pane to a program
/// rather than a terminal, for ai-brain-harness. Prints JSON lines:
/// `{"screen", "rows", "cols"}` first (history and screen, base64, more
/// than `--max` follows as `out`), then `{"out"}` with what came within
/// `--every` ms, `{"gap"}` with the bytes left out when more came than
/// `--max` (a fresh `screen` follows, as when the pane's size changes), and
/// `{"exit": true}` when the pane ends. Reads JSON lines on stdin:
/// `{"input": base64}` or `{"text": "…"}` are typed into the pane. Watches
/// only: the pane keeps its size and answers terminal queries itself.
pub fn tap(args: &[String]) -> Result<()> {
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::{Arc, Mutex};
    let mut rest = args.to_vec();
    let every = take_value(&mut rest, "--every")?
        .map(|ms| ms.parse::<u64>().context("--every takes milliseconds"))
        .transpose()?
        .unwrap_or(TAP_EVERY)
        .max(10);
    let max = take_value(&mut rest, "--max")?
        .map(|n| n.parse::<usize>().context("--max takes bytes"))
        .transpose()?
        .unwrap_or(TAP_MAX)
        .max(1024);
    let [address] = rest.as_slice() else {
        bail!("usage: imkr tap ADDRESS [--every MS] [--max BYTES]   (ADDRESS as for `imkr read`)");
    };
    let target = TapTarget::find(address)?;
    // One channel for every connection; each says which one it comes
    // from, so a connection that was cut cannot speak for the next.
    let (tx, rx) = std::sync::mpsc::channel::<TapEvent>();
    // Input goes to whichever connection is open, and waits for the next
    // one between two (or before the first).
    let writer = Arc::new(Mutex::new(TapInput::default()));
    {
        let writer = Arc::clone(&writer);
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines() {
                let Ok(line) = line else { break };
                match tap_line(&line) {
                    Some(TapLine::Input(bytes)) => writer.lock().unwrap().send(bytes),
                    Some(TapLine::Screen) => {
                        let _ = tx.send(TapEvent::Screen);
                    }
                    None if line.trim().is_empty() => {}
                    None => eprintln!("imkr tap: not input: {line}"),
                }
            }
        });
    }
    let mut stdout = std::io::stdout().lock();
    let mut emit =
        |value: serde_json::Value| writeln!(stdout, "{value}").and_then(|()| stdout.flush());
    let mut current = 0u64;
    loop {
        current += 1;
        let opened = match target.open() {
            Ok(opened) => opened,
            Err(e) if current == 1 => return Err(e),
            Err(e) => {
                // Gone between two connections, or the server with it.
                let _ = emit(serde_json::json!({ "exit": true, "error": format!("{e:#}") }));
                return Ok(());
            }
        };
        let OpenTap {
            mut conn,
            socket,
            rows,
            cols,
            screen,
        } = opened;
        // Only a screen without history can still be larger; it goes on
        // as `out`, as there is nothing left to leave out.
        let mut chunks = fit_screen(&screen, max).chunks(max);
        let head = chunks.next().unwrap_or_default();
        let mut sent = emit(serde_json::json!({
            "screen": clipboard::base64(head), "rows": rows, "cols": cols,
        }))
        .is_ok();
        for chunk in chunks {
            sent = sent && emit(serde_json::json!({ "out": clipboard::base64(chunk) })).is_ok();
        }
        if !sent {
            return Ok(());
        }
        let mut reader = std::mem::replace(&mut conn.reader, Box::new(std::io::empty()));
        {
            let tx = tx.clone();
            std::thread::spawn(move || {
                loop {
                    let event = match protocol::ServerMsg::read_from(&mut reader) {
                        Ok(Some(protocol::ServerMsg::Output(bytes))) => {
                            TapEvent::Out(current, bytes)
                        }
                        Ok(Some(protocol::ServerMsg::Exited)) => TapEvent::Exited(current),
                        Ok(Some(_)) => continue,
                        // Cut off: the pane's size changed, the server was
                        // updated or this fell behind.
                        Ok(None) | Err(_) => TapEvent::Lost(current),
                    };
                    let ended = !matches!(event, TapEvent::Out(..));
                    if tx.send(event).is_err() || ended {
                        break;
                    }
                }
            });
        }
        writer.lock().unwrap().connect(std::mem::replace(
            &mut conn.writer,
            Box::new(std::io::sink()),
        ));
        // What a connection ends with: the pane, or only this connection
        // (a fresh one follows).
        let mut exited = false;
        // Waits for output, then gathers what comes within `every`.
        'connection: loop {
            let mut pending = Vec::new();
            let mut until: Option<std::time::Instant> = None;
            let mut done = false;
            loop {
                let event = match until {
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    Some(until) => {
                        rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now()))
                    }
                };
                match event {
                    Ok(TapEvent::Out(from, bytes)) if from == current => {
                        pending.extend_from_slice(&bytes);
                        until.get_or_insert_with(|| {
                            std::time::Instant::now() + std::time::Duration::from_millis(every)
                        });
                    }
                    Ok(TapEvent::Exited(from)) if from == current => {
                        exited = true;
                        done = true;
                    }
                    Ok(TapEvent::Lost(from)) if from == current => done = true,
                    Ok(TapEvent::Screen) => done = true,
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => done = true,
                }
                if done || until.is_some_and(|until| std::time::Instant::now() >= until) {
                    break;
                }
            }
            if pending.len() > max {
                // Rather a gap that shows than a message Reverb drops; the
                // fresh screen of the next connection covers it.
                if emit(serde_json::json!({ "gap": pending.len() })).is_err() {
                    return Ok(());
                }
                break 'connection;
            }
            if !pending.is_empty()
                && emit(serde_json::json!({ "out": clipboard::base64(&pending) })).is_err()
            {
                return Ok(());
            }
            if done {
                break 'connection;
            }
        }
        writer.lock().unwrap().out = None;
        if let Some(socket) = socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        drop(conn);
        if exited {
            let _ = emit(serde_json::json!({ "exit": true }));
            return Ok(());
        }
        // Not at once: a pane that keeps cutting off would spin.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// What draws a tapped pane, in at most `max` bytes if it can be: the
/// history (plain text lines, before the first escape code) shortened
/// from the start, the screen itself kept whole.
fn fit_screen(screen: &[u8], max: usize) -> &[u8] {
    let Some(over) = screen.len().checked_sub(max).filter(|over| *over > 0) else {
        return screen;
    };
    let history = screen
        .iter()
        .position(|b| *b == 0x1b)
        .unwrap_or(screen.len());
    let cut = screen[over.min(history)..history]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(history, |at| over.min(history) + at + 1);
    &screen[cut..]
}

/// What reaches `imkr tap`'s loop: output, the end of a connection (by its
/// number) or a request for a fresh screen.
enum TapEvent {
    Out(u64, Vec<u8>),
    Exited(u64),
    Lost(u64),
    Screen,
}

/// A line on `imkr tap`'s stdin.
#[derive(Debug, PartialEq)]
enum TapLine {
    /// Typed into the pane.
    Input(Vec<u8>),
    /// A fresh screen, after the other side lost track.
    Screen,
}

/// Where `imkr tap` types: the open connection, or what waits for one.
#[derive(Default)]
struct TapInput {
    out: Option<Box<dyn Write + Send>>,
    waiting: Vec<u8>,
}

impl TapInput {
    fn send(&mut self, bytes: Vec<u8>) {
        self.waiting.extend_from_slice(&bytes);
        self.flush();
    }

    fn connect(&mut self, out: Box<dyn Write + Send>) {
        self.out = Some(out);
        self.flush();
    }

    fn flush(&mut self) {
        let Some(out) = self.out.as_mut() else { return };
        if self.waiting.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.waiting);
        if protocol::ClientMsg::Input(bytes.clone())
            .write_to(out)
            .and_then(|()| out.flush())
            .is_err()
        {
            // Cut off meanwhile: the next connection types it.
            self.out = None;
            self.waiting = bytes;
        }
    }
}

/// A line on `imkr tap`'s stdin: `{"input": base64}` or `{"text": "…"}`
/// to type, `{"screen": true}` for a fresh screen.
fn tap_line(line: &str) -> Option<TapLine> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value["screen"] == true {
        return Some(TapLine::Screen);
    }
    if let Some(text) = value["text"].as_str() {
        return Some(TapLine::Input(text.as_bytes().to_vec()));
    }
    clipboard::unbase64(value["input"].as_str()?).map(TapLine::Input)
}

/// The pane `imkr tap` shows, found once: a fresh connection after a cut
/// goes to the same pane, not to another that took its name.
struct TapTarget {
    machine: Option<String>,
    session: Option<String>,
    pane: PaneId,
}

/// A connection showing a pane, with its size and what draws it now.
struct OpenTap {
    conn: client::Conn,
    /// The local socket, to end the connection with while a thread reads.
    socket: Option<std::os::unix::net::UnixStream>,
    rows: u64,
    cols: u64,
    screen: Vec<u8>,
}

impl TapTarget {
    fn find(address: &str) -> Result<Self> {
        let (machine, session, who) = parse_address(address)?;
        let panes = match &machine {
            Some(host) => {
                let session = session.as_deref().unwrap_or(protocol::DEFAULT_SESSION);
                let text = client::remote_output(host, &["-s", session, "list", "--json"], "")?;
                serde_json::from_str(&text).context("no pane list from there")?
            }
            None => client::request_at(&tap_socket(session.as_deref())?, Request::List)?,
        };
        let pane = find_pane(&panes, &who).with_context(|| format!("no pane {address}"))?;
        Ok(TapTarget {
            machine,
            session,
            pane,
        })
    }

    fn open(&self) -> Result<OpenTap> {
        let (mut conn, socket) = match &self.machine {
            Some(host) => (
                client::Conn::remote(
                    host,
                    Some(self.session.as_deref().unwrap_or(protocol::DEFAULT_SESSION)),
                    false,
                )?,
                None,
            ),
            None => {
                let unix =
                    std::os::unix::net::UnixStream::connect(tap_socket(self.session.as_deref())?)
                        .context("the session does not run")?;
                let socket = unix.try_clone()?;
                (client::Conn::local(unix)?, Some(socket))
            }
        };
        protocol::ClientMsg::Request(Request::Tap {
            pane: self.pane,
            watch: true,
            size: None,
        })
        .write_to(&mut conn.writer)?;
        conn.writer.flush()?;
        let reply = match protocol::ServerMsg::read_from(&mut conn.reader)? {
            Some(protocol::ServerMsg::Reply(Ok(reply))) => reply,
            Some(protocol::ServerMsg::Reply(Err(message))) => bail!(message),
            _ => bail!("the server does not show panes elsewhere, update it"),
        };
        // The tap sends what draws the pane now before anything else.
        let screen = loop {
            match protocol::ServerMsg::read_from(&mut conn.reader)? {
                Some(protocol::ServerMsg::Output(bytes)) => break bytes,
                Some(protocol::ServerMsg::Exited) | None => bail!("pane {} ended", self.pane),
                Some(_) => {}
            }
        };
        Ok(OpenTap {
            conn,
            socket,
            rows: reply["rows"].as_u64().unwrap_or_default(),
            cols: reply["cols"].as_u64().unwrap_or_default(),
            screen,
        })
    }
}

/// The socket of session `session` here, or of this one.
fn tap_socket(session: Option<&str>) -> Result<PathBuf> {
    match session {
        Some(name) if protocol::sessions().iter().any(|s| s == name) => {
            protocol::session_socket(name)
        }
        Some(name) => bail!("no session {name} here"),
        None => protocol::socket_path(),
    }
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
    ("StopFailure", "*", "failed"),
    ("SessionEnd", "*", "clear"),
];

/// The first Claude Code that knows `StopFailure`; older ones reject the
/// whole settings.json over the unknown key (#8751).
const STOP_FAILURE_SINCE: (u32, u32, u32) = (2, 1, 78);

/// The version in `claude --version` output, e.g. "2.1.20 (Claude Code)".
fn claude_version(output: &str) -> Option<(u32, u32, u32)> {
    let word = output.split_whitespace().next()?;
    let mut parts = word.split('.').map(|part| part.parse().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The versions of every `claude` on the PATH, oldest first: they all read
/// the same settings.json. Those that do not answer are left out.
fn claude_versions() -> Vec<((u32, u32, u32), PathBuf)> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut found: Vec<_> = std::env::split_paths(&path)
        .map(|dir| dir.join("claude"))
        .filter(|program| program.is_file())
        .filter_map(|program| {
            let out = Command::new(&program).arg("--version").output().ok()?;
            Some((
                claude_version(&String::from_utf8_lossy(&out.stdout))?,
                program,
            ))
        })
        .collect();
    found.sort();
    found
}

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

/// The extension that makes pi report its state, as the hooks do for
/// Claude Code; `imkr hooks --install` puts it into pi's extensions.
const PI_EXTENSION: &str = include_str!("../integrations/pi/imkr-agent-state.ts");

/// Puts `PI_EXTENSION` into `~/.pi/agent/extensions/` when pi is set up
/// here. Where it went, `None` without pi.
fn install_pi_extension(home: &Path) -> Result<Option<std::path::PathBuf>> {
    let agent = home.join(".pi/agent");
    if !agent.is_dir() {
        return Ok(None);
    }
    let path = agent.join("extensions/imkr-agent-state.ts");
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    std::fs::write(&path, PI_EXTENSION)?;
    Ok(Some(path))
}

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
    let old = claude_versions()
        .into_iter()
        .find(|(version, _)| *version < STOP_FAILURE_SINCE);
    for &(event, matcher, state) in HOOKS {
        if event == "StopFailure" && old.is_some() {
            continue;
        }
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
    if let Some(((major, minor, patch), program)) = old {
        println!(
            "Left out StopFailure: {} is Claude Code {major}.{minor}.{patch}, which rejects it \
             (it came in 2.1.78). Errors show as failed from the screen instead; update that \
             claude and run `imkr hooks --install` again.",
            program.display()
        );
    }
    if let Some(extension) = install_pi_extension(&home)? {
        println!(
            "pi extension installed: {}; pi started from now on reports its state too.",
            extension.display()
        );
    }
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
        // What is saved under a name is the hive worked in from now on.
        crate::persist::set_active_hive(Some(name))?;
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
                let marked = if crate::persist::active_hive().as_deref() == Some(name.as_str()) {
                    format!("{name} ●")
                } else {
                    name.clone()
                };
                println!("{marked:<12} {}", sessions.join(", "));
            }
            let archived = crate::persist::archived();
            if !archived.is_empty() {
                let names: Vec<String> = archived
                    .iter()
                    .map(|a| match &a.hive {
                        Some(hive) => format!("{} ({hive})", a.name),
                        None => a.name.clone(),
                    })
                    .collect();
                println!(
                    "{:<12} {} · `imkr revive NAME`",
                    "archive",
                    names.join(", ")
                );
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

/// `imkr archive [export]`: the sessions ended before the ten in `last`,
/// by hive, here and as last fetched from the other machines; `export`
/// gives this machine's own as JSON, for the others to fetch.
pub fn archive(args: &[String]) -> Result<()> {
    match args {
        [cmd] if cmd == "export" => {
            println!("{}", serde_json::to_string(&crate::persist::archived())?);
            Ok(())
        }
        // Another machine started this entry while this one was off.
        [cmd, name, saved_at] if cmd == "drop" => {
            let saved_at: u64 = saved_at.parse().context("SAVED_AT is seconds since 1970")?;
            if crate::persist::drop_archived(name, saved_at)? {
                println!("{name} left the archive");
            }
            Ok(())
        }
        [] | [_] if args.first().is_none_or(|a| a == "ls") => {
            let line = |machine: &str, entry: &crate::persist::Archived| {
                let when = crate::protocol::chrono_like(entry.saved_at).unwrap_or_default();
                let hive = entry.hive.as_deref().unwrap_or("no hive");
                println!("{:<14} {:<20} {hive:<14} {when}", machine, entry.name);
            };
            let here = crate::protocol::machine_name();
            for entry in crate::persist::archived() {
                line(&here, &entry);
            }
            for replica in crate::persist::replicas() {
                for entry in &replica.sessions {
                    line(&replica.host, entry);
                }
            }
            Ok(())
        }
        _ => bail!("usage: imkr archive [ls | export | drop NAME SAVED_AT]"),
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
            if crate::protocol::test_socket().is_some() {
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
            for line in client::switch_hive(name, &outsiders, None, None)? {
                println!("{line}");
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

/// `imkr account …`: several accounts of Claude Code and Codex here, see
/// `account`. A name may say its agent, `claude:privat`, where both have it.
pub fn account(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: imkr account [ls [--json]] | add claude|codex NAME [--url URL] [--model M] \
                         [--token-stdin | --token KEY] | login NAME | rm NAME | sync | env NAME \
                         | run NAME -- COMMAND… | use NAME | switch NAME [--pane N] \
                         | default [NAME] | usage [--json]";
    // How much each account used of its plan; `--wait` keeps it on screen
    // in the popup of Ctrl+B s u.
    if args.first().is_some_and(|a| a == "usage") {
        let mut rest = args[1..].to_vec();
        let json = take_flag(&mut rest, "--json");
        let wait = take_flag(&mut rest, "--wait");
        if wait {
            println!("asking the accounts …");
        }
        let report = crate::usage::report()?;
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            if wait {
                print!("\x1b[1A\x1b[2K");
            }
            let colour = std::io::IsTerminal::is_terminal(&std::io::stdout());
            print!("{}", crate::usage::print(&report, colour));
        }
        if wait {
            print!("\nEnter closes. ");
            std::io::Write::flush(&mut std::io::stdout())?;
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        return Ok(());
    }
    use crate::account::{Account, Store, TOOLS};
    let store = Store::here()?;
    // `NAME` or `tool:NAME`, the only one of that name if no tool is said.
    let pick = |given: &str| -> Result<Account> {
        let (tool, name) = match given.split_once(':') {
            Some((tool, name)) => (Some(tool), name),
            None => (None, given),
        };
        let found: Vec<Account> = TOOLS
            .iter()
            .filter(|t| tool.is_none_or(|tool| tool == **t))
            .filter_map(|t| store.named(t, name))
            .collect();
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] => bail!("no account {given}, see `imkr account`"),
            _ => bail!("both agents have an account {name}: say claude:{name} or codex:{name}"),
        }
    };
    let mut rest: Vec<String> = args.to_vec();
    let pane = take_pane(&mut rest)?;
    let token_stdin = take_flag(&mut rest, "--token-stdin");
    let url = take_value(&mut rest, "--url")?;
    let model = take_value(&mut rest, "--model")?;
    let token = take_value(&mut rest, "--token")?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match rest.as_slice() {
        // For programs (the AI Brain harness reads back where its MCP
        // entry landed): every account with its directory and, for
        // Claude, the `.claude.json` that `sync` fills.
        ["ls", "--json"] => {
            let mut out = Vec::new();
            for tool in TOOLS {
                let default = store.default_of(tool);
                for account in store.listed(tool) {
                    let dir = store
                        .dir_of(&account)
                        .unwrap_or_else(|| store.usual_dir(tool));
                    let claude_json = (*tool == "claude").then(|| match store.dir_of(&account) {
                        Some(dir) => dir.join(".claude.json"),
                        None => store.home.join(".claude.json"),
                    });
                    out.push(serde_json::json!({
                        "tool": tool,
                        "name": account.name,
                        "standard": account.name == default,
                        "dir": dir,
                        "claude_json": claude_json,
                        "logged_in": store.logged_in(&account),
                        "login": store.identity(&account).map(|who| who.shown),
                        "provider": account.url.as_ref().map(|_| account.kind()),
                    }));
                }
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        [] | ["ls"] => {
            for tool in TOOLS {
                println!("{tool}");
                let default = store.default_of(tool);
                for account in store.listed(tool) {
                    // By the subscription it is logged in to, `(standard)` for
                    // the one new shells start in.
                    let mut name = crate::account::shown(&store, &account);
                    if account.url.is_some() {
                        name = account.name.clone();
                    }
                    if account.name == default {
                        name.push_str(" (standard)");
                    }
                    let mut note = String::new();
                    if account.is_standard() {
                        note.push_str(&format!("  ~/.{tool}"));
                    } else if account.url.is_some() {
                        note.push_str(&format!("  {}", account.kind()));
                    }
                    if let Some(who) = store.identity(&account) {
                        note.push_str(&format!("  {}", who.shown));
                    }
                    if store.login_dead(&account) {
                        note.push_str(&format!(
                            "  logged out (the login was refused): `imkr account login {}:{}`",
                            account.tool, account.name
                        ));
                    } else if !store.logged_in(&account) {
                        note.push_str(&format!(
                            "  not logged in: `imkr account login {}:{}`",
                            account.tool, account.name
                        ));
                    }
                    if let Some(ends) = crate::login::login_ends_soon(&store, &account) {
                        note.push_str(&format!(
                            "  login ends {}: `imkr account login {}:{}`",
                            crate::usage::reset_label(ends, crate::persist::now()),
                            account.tool,
                            account.name
                        ));
                    }
                    if let Some(until) = store.used_up_until(&account) {
                        note.push_str(&format!("  used up until {until}"));
                    }
                    if let Some(other) = store.same_login(&account) {
                        note.push_str(&format!(
                            "  same subscription as {}",
                            crate::account::shown(&store, &other)
                        ));
                    }
                    println!("  {name:<24}{note}");
                }
            }
            println!("in this order a used up account falls back to the next");
            Ok(())
        }
        // Again for an account there is, or one this person had: logged in
        // anew, never a second account of the same person (#8577).
        ["add", tool, name] if url.is_none() && token.is_none() && !token_stdin => {
            if let Some(there) = store.find(tool, name) {
                println!("{tool} account {name} is there already: logging it in again");
                return login(&store, &there);
            }
            if let Some(was) = store.logged_out_as(tool, name) {
                println!(
                    "{tool} account {} was {name} and is logged out: logging it in again \
                     instead of adding a second one",
                    was.name
                );
                return login(&store, &was);
            }
            let account = store.add(Account {
                name: (*name).to_owned(),
                tool: (*tool).to_owned(),
                url: None,
                token: None,
                model: None,
            })?;
            println!("{} account {} added", account.tool, account.name);
            login(&store, &account)?;
            let Some(other) = store.same_person(&account) else {
                return Ok(());
            };
            let who = store
                .identity(&account)
                .map(|i| i.shown)
                .unwrap_or_default();
            if account_in_use(&account.tool, &account.name) {
                println!(
                    "careful: {} is logged in as {who}, like account {}; agents use {} \
                     already, so it stays: `imkr account rm {}:{}` once they are done",
                    account.name, other.name, account.name, account.tool, account.name
                );
                return Ok(());
            }
            store.move_login(&account, &other)?;
            store.remove(&account.tool, &account.name)?;
            println!(
                "{who} is account {} already: logged in there, no second account",
                other.name
            );
            Ok(())
        }
        ["add", tool, name] => {
            let token = match (token, token_stdin) {
                (Some(token), _) => Some(token),
                (None, true) => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    Some(text.trim().to_owned()).filter(|t| !t.is_empty())
                }
                (None, false) => None,
            };
            let provider = url.is_some();
            let account = store.add(Account {
                name: (*name).to_owned(),
                tool: (*tool).to_owned(),
                url,
                token,
                model,
            })?;
            println!("{} account {} added", account.tool, account.name);
            if !provider {
                login(&store, &account)?;
                // Logged in to a subscription that has an account already:
                // a second one shares its limit and gains nothing. Only
                // said: removing it would throw away a login agents may
                // already use.
                if let Some(other) = store.same_login(&account) {
                    println!(
                        "careful: {} is logged in as {}, the same as account {}: its limit \
                         is shared; `imkr account rm {}:{}` if you do not need both",
                        account.name,
                        store.identity(&other).map(|i| i.shown).unwrap_or_default(),
                        other.name,
                        account.tool,
                        account.name
                    );
                }
            }
            Ok(())
        }
        ["default"] => {
            for tool in TOOLS {
                let name = store.default_of(tool);
                let shown = store
                    .find(tool, &name)
                    .map(|a| crate::account::shown(&store, &a))
                    .unwrap_or(name);
                println!("{tool}: {shown}");
            }
            Ok(())
        }
        // New shells start in it, in every session here; running agents
        // keep theirs (`switch` moves one).
        ["default", given] => {
            let account = pick(given)?;
            if !store.logged_in(&account) {
                bail!(
                    "{} is not logged in: `imkr account login {given}`",
                    account.name
                );
            }
            store.set_default(&account)?;
            println!(
                "new shells start in {} account {} (standard); running agents keep theirs, \
                 `imkr account switch {} --pane N` moves one",
                account.tool,
                crate::account::shown(&store, &account),
                given
            );
            Ok(())
        }
        ["login", given] => {
            let account = pick(given)?;
            login(&store, &account)?;
            if let Some(other) = store.same_login(&account) {
                println!(
                    "careful: {} is the same subscription as account {}, its limit is shared; \
                     `imkr account rm {}:{}` if you do not need both",
                    account.name, other.name, account.tool, account.name
                );
            }
            Ok(())
        }
        // In the session this runs in: for new shells, or the agent of a
        // pane right away.
        ["use", given] | ["switch", given] => {
            let account = pick(given)?;
            let now = rest[0] == "switch";
            if now && pane.is_none() {
                bail!("which pane? `--pane N`, or run it in the pane");
            }
            let reply = client::request(Request::UseAccount {
                tool: account.tool.clone(),
                name: account.name.clone(),
                pane: if now { pane } else { None },
            })?;
            if now {
                println!(
                    "pane {}: {} goes on in account {}",
                    reply["pane"], account.tool, account.name
                );
            } else {
                println!(
                    "new shells of this session use {} account {}; running agents keep theirs \
                     (`imkr account switch {}:{} --pane N`)",
                    account.tool, account.name, account.tool, account.name
                );
            }
            Ok(())
        }
        ["rm" | "remove", given] => {
            let account = pick(given)?;
            if account.is_standard() {
                bail!("`standard` is the agent's own directory, it stays");
            }
            store.remove(&account.tool, &account.name)?;
            println!("{} account {} removed", account.tool, account.name);
            Ok(())
        }
        ["sync"] => {
            for account in store.load() {
                let linked = store.link_shared(&account)?;
                if !linked.is_empty() {
                    println!(
                        "{}:{}: linked {}",
                        account.tool,
                        account.name,
                        linked.join(", ")
                    );
                }
            }
            println!("accounts share what is new, and Claude's MCP servers again");
            Ok(())
        }
        ["env", given] => {
            for (name, value) in store.env(&pick(given)?) {
                println!("export {name}={}", client::sh_quote(&value));
            }
            Ok(())
        }
        ["run", given, "--", command @ ..] | ["run", given, command @ ..]
            if !command.is_empty() =>
        {
            use std::os::unix::process::CommandExt;
            let account = pick(given)?;
            store.link_shared(&account)?;
            let mut run = Command::new(command[0]);
            run.args(&command[1..]);
            // Nothing of the account the shell started with stays.
            for name in crate::account::variables(&account.tool) {
                run.env_remove(name);
            }
            let error = run.envs(store.env(&account)).exec();
            Err(error).with_context(|| format!("could not run {}", command[0]))
        }
        _ => bail!(USAGE),
    }
}

/// Logs a subscription in: its agent in this terminal, with its directory.
fn login(store: &crate::account::Store, account: &crate::account::Account) -> Result<()> {
    if account.url.is_some() || (account.is_standard() && account.tool != "claude") {
        println!("{} needs no login", account.name);
        return Ok(());
    }
    use std::io::IsTerminal;
    let command = crate::account::login_command(account);
    if !std::io::stdin().is_terminal() {
        println!(
            "log it in from a terminal: `imkr account login {}:{}`",
            account.tool, account.name
        );
        return Ok(());
    }
    if account.tool == "claude" {
        println!("Claude Code starts in this account: log in, then leave it with /exit.");
    }
    let mut run = Command::new(&command[0]);
    run.args(&command[1..]);
    // Into this account's directory, not the one the shell started with.
    for name in crate::account::variables(&account.tool) {
        run.env_remove(name);
    }
    let status = run
        .envs(store.env(account))
        .status()
        .with_context(|| format!("could not run {}", command[0]))?;
    if store.logged_in(account) {
        println!("{} account {} is logged in", account.tool, account.name);
    } else if !status.success() {
        println!(
            "not logged in; again with `imkr account login {}:{}`",
            account.tool, account.name
        );
    }
    Ok(())
}

/// `imkr outbox`: the messages waiting here for machines out of reach;
/// `imkr outbox take MACHINE` hands that machine its own, as JSON.
pub fn outbox(args: &[String]) -> Result<()> {
    match args {
        [cmd, machine] if cmd == "take" => {
            println!("{}", serde_json::to_string(&client::take_outbox(machine))?);
            Ok(())
        }
        [] => {
            let dir = crate::persist::state_dir()?.join("outbox");
            let mut count = 0;
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let Some(message) = std::fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<client::OutboxMessage>(&t).ok())
                else {
                    continue;
                };
                count += 1;
                println!(
                    "for {}:{}/{} from {}: {}",
                    message.machine,
                    message.session.as_deref().unwrap_or("default"),
                    message.who,
                    message.from,
                    message.text.lines().next().unwrap_or("")
                );
            }
            if count == 0 {
                println!("the outbox is empty");
            }
            Ok(())
        }
        _ => bail!("usage: imkr outbox [take MACHINE]"),
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
    let hosts = known_hosts()?;
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
    let mut current = Vec::new();
    for thread in threads {
        let Ok((host, result)) = thread.join() else {
            continue;
        };
        match result {
            Ok(done) => {
                println!("{host:<20} {done}");
                current.push(host);
            }
            Err(e) => {
                failed = true;
                println!("{host:<20} {e:#}");
            }
        }
    }
    // Then the settings, one machine after the other: two at once would
    // write the same files here.
    for host in current {
        match sync_settings_with(&host, true) {
            Ok(done) => println!("{host:<20} {done}"),
            Err(e) => {
                failed = true;
                println!("{host:<20} settings: {e:#}");
            }
        }
    }
    if failed {
        bail!("not every machine is up to date");
    }
    Ok(())
}

/// The other machines: from the config, then the remembered ones.
fn known_hosts() -> Result<Vec<String>> {
    let mut hosts = crate::config::Config::load().0.hosts;
    for host in client::remembered_hosts() {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    if hosts.is_empty() {
        bail!("no other machines known: `imkr hosts add NAME` or `imkr --host NAME`");
    }
    Ok(hosts)
}

/// `imkr settings <sync [--yes] [HOST…] | export | import>`: the settings
/// shared with the other machines, see `settings`. `export` and `import`
/// are what `sync` runs there.
pub fn settings(args: &[String]) -> Result<()> {
    const USAGE: &str = "usage: imkr settings sync [--yes] [HOST…]";
    match args {
        [cmd] if cmd == "export" => {
            println!("{}", serde_json::to_string(&crate::settings::collect()?)?);
            Ok(())
        }
        [cmd] if cmd == "import" => {
            let items: Vec<crate::settings::Item> = serde_json::from_reader(std::io::stdin())?;
            let written = crate::settings::apply(&items)?;
            reload_settings();
            println!("{}", written.join(", "));
            Ok(())
        }
        [cmd, rest @ ..] if cmd == "sync" => {
            let yes = rest.iter().any(|a| a == "--yes" || a == "-y");
            let named: Vec<String> = rest
                .iter()
                .filter(|a| !a.starts_with('-'))
                .cloned()
                .collect();
            let hosts = if named.is_empty() {
                known_hosts()?
            } else {
                named
            };
            let mut failed = false;
            for host in hosts {
                match sync_settings_with(&host, yes) {
                    Ok(done) => println!("{host:<20} {done}"),
                    Err(e) => {
                        failed = true;
                        println!("{host:<20} {e:#}");
                    }
                }
            }
            if failed {
                bail!("not every machine was synced");
            }
            Ok(())
        }
        _ => bail!(USAGE),
    }
}

/// The settings with `host`: what goes which way, listed, then copied,
/// after asking unless `yes`. Says in one line what happened.
fn sync_settings_with(host: &str, yes: bool) -> Result<String> {
    use crate::settings::{Way, picked, plan};
    let out = client::remote_output(host, &["settings", "export"], "")
        .context("could not read its settings (is imkr there up to date? `imkr hosts sync`)")?;
    let remote: Vec<crate::settings::Item> =
        serde_json::from_str(&out).context("its settings did not come as expected")?;
    let local = crate::settings::collect()?;
    let base = crate::settings::base(host);
    let (steps, undecided) = plan(&local, &remote, base.as_deref());
    for path in &undecided {
        println!("{host:<20} {path}: differs, but both are as old, left alone");
    }
    if steps.is_empty() {
        crate::settings::remember_base(host)?;
        return Ok("settings: the same".into());
    }
    println!("{host:<20} settings:");
    for step in &steps {
        let arrow = match step.way {
            Way::Pull => "← here ",
            Way::Push => "→ there",
        };
        println!("  {arrow}  {} ({})", step.path, step.why);
    }
    if !yes && !ask(&format!("Copy these between here and {host}?"))? {
        return Ok("settings: left as they were".into());
    }
    let pulls = picked(&remote, &steps, Way::Pull);
    let pushes = picked(&local, &steps, Way::Push);
    if !pulls.is_empty() {
        crate::settings::apply(&pulls)?;
        reload_settings();
    }
    if !pushes.is_empty() {
        client::remote_output(
            host,
            &["settings", "import"],
            &serde_json::to_string(&pushes)?,
        )?;
    }
    // What both have now is the base for next time; files left alone
    // stay undecided until one side changes them.
    if undecided.is_empty() {
        crate::settings::remember_base(host)?;
    }
    Ok(format!(
        "settings: {} came here, {} went there (replaced ones in .sync-backup next to each config)",
        pulls.len(),
        pushes.len()
    ))
}

/// Tells every session here to read its config and plugins again.
fn reload_settings() {
    for name in protocol::sessions() {
        if let Ok(path) = protocol::session_socket(&name) {
            let _ = client::request_at(&path, Request::ReloadConfig);
        }
    }
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
    let own = protocol::test_socket();
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

/// `imkr push test`: sends a test push the way `[push]` in the config says,
/// and waits to say whether it arrived.
pub fn push(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) != Some("test") || args.len() > 1 {
        bail!("usage: imkr push test");
    }
    let (config, problem) = crate::config::Config::load();
    if let Some(problem) = problem {
        eprintln!("config: {problem}");
    }
    let push = &config.push;
    if !push.enabled() {
        bail!("nothing to push to: set ntfy or webhook under [push] in the config");
    }
    let event = crate::push::Event {
        kind: "test".into(),
        machine: protocol::machine_name(),
        session: protocol::session_name(),
        agent: "imkr".into(),
        text: "can reach you".into(),
        task: "A test from imkr push test".into(),
        link: push.link.clone().unwrap_or_default(),
        ..crate::push::Event::default()
    };
    let mut failed = false;
    if let Some(address) = &push.ntfy {
        let Some((server, topic)) = crate::push::ntfy_parts(address) else {
            bail!("not an ntfy topic address: {address} (e.g. https://ntfy.sh/a-topic)");
        };
        match crate::push::post(&server, &crate::push::ntfy_body(&topic, &event)) {
            Ok(()) => println!("ntfy: sent to {topic} on {server}"),
            Err(e) => {
                failed = true;
                println!("ntfy: {e:#}");
            }
        }
    }
    if let Some(address) = &push.webhook {
        match crate::push::post(address, &serde_json::to_value(&event)?) {
            Ok(()) => println!("webhook: sent to {address}"),
            Err(e) => {
                failed = true;
                println!("webhook: {e:#}");
            }
        }
    }
    if failed {
        bail!("not every push went out");
    }
    Ok(())
}

/// The panes of the sessions here that work in `dir` or below, but not the
/// one this runs in (its shell is where the user typed): to end once their
/// worktree is gone. Taken before, when their directory still exists.
fn panes_in(dir: &std::path::Path) -> Vec<(PathBuf, crate::layout::PaneId)> {
    let own = std::env::var("IMKR_PANE")
        .ok()
        .and_then(|p| p.parse::<u64>().ok())
        .map(|p| (protocol::session_name(), p));
    let mut found = Vec::new();
    for name in protocol::sessions() {
        let Ok(socket) = protocol::session_socket(&name) else {
            continue;
        };
        let Ok(panes) = client::request_at(&socket, Request::List) else {
            continue;
        };
        for pane in panes.as_array().cloned().unwrap_or_default() {
            let id = pane["pane"].as_u64();
            if own
                .as_ref()
                .is_some_and(|(s, p)| *s == name && id == Some(*p))
            {
                continue;
            }
            let inside = pane["cwd"]
                .as_str()
                .is_some_and(|cwd| std::path::Path::new(cwd).starts_with(dir));
            if let (true, Some(id)) = (inside, id) {
                found.push((socket.clone(), id as crate::layout::PaneId));
            }
        }
    }
    found
}

fn end_panes(panes: Vec<(PathBuf, crate::layout::PaneId)>) {
    for (socket, pane) in panes {
        let _ = client::request_at(&socket, Request::Kill { pane });
    }
}

/// Whether an agent in a session here runs in account `tool:name`.
fn account_in_use(tool: &str, name: &str) -> bool {
    protocol::sessions().into_iter().any(|session| {
        protocol::session_socket(&session)
            .and_then(|socket| client::request_at(&socket, Request::List))
            .ok()
            .and_then(|panes| panes.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .any(|p| p["program"].as_str() == Some(tool) && p["account"].as_str() == Some(name))
    })
}

/// `imkr screen …` (#9074): seeing and driving the machine's screen.
///
/// Every action asks `screen::desktop()` for the backend of this system,
/// `probe()` whether it may happen at all, and the permission on this
/// machine whether this agent may. With `--host H` the whole command runs
/// over there instead, and a screenshot comes back as bytes.
pub fn screen(args: &[String], host: Option<&str>) -> Result<()> {
    use crate::screen;
    const USAGE: &str = "usage: imkr screen status \
                         | shot [--monitor M] [--region X,Y,WxH] [--width W] [--out PATH] \
                         | move X Y | click [X Y] [--button left|right|middle] \
                         | type TEXT | key KEYS… | log [--lines N] | allow | refuse \
                         [--host HOST] [--json]";
    let mut rest = args.to_vec();
    let json = take_flag(&mut rest, "--json");
    // Carries the PNG itself in the JSON, for `--host`: the picture is
    // taken there and has to get here somehow.
    let want_bytes = take_flag(&mut rest, "--base64");
    let stdin_text = take_flag(&mut rest, "--stdin");
    let monitor = take_value(&mut rest, "--monitor")?;
    let width = match take_value(&mut rest, "--width")? {
        Some(w) => w
            .parse::<u32>()
            .with_context(|| format!("--width takes a number, not {w:?}"))?,
        None => screen::SHOT_WIDTH,
    };
    let out = take_value(&mut rest, "--out")?.map(PathBuf::from);
    let save = take_flag(&mut rest, "--save");
    let region_spec = take_value(&mut rest, "--region")?;
    let region = match &region_spec {
        Some(spec) => Some(screen::Region::parse(spec)?),
        None => None,
    };
    let button = match take_value(&mut rest, "--button")? {
        Some(name) => screen::Button::parse(&name)?,
        None => screen::Button::Left,
    };
    let lines = match take_value(&mut rest, "--lines")? {
        Some(n) => n
            .parse::<usize>()
            .with_context(|| format!("--lines takes a number, not {n:?}"))?,
        None => 30,
    };
    let Some(what) = rest.first().cloned() else {
        bail!("{USAGE}");
    };
    rest.remove(0);

    // Another machine: everything but the screenshot is just the same
    // command over there. A screenshot comes back as base64 in its JSON
    // and is written here, so the path an agent gets is a local file.
    if let Some(host) = host {
        return screen_on(
            host,
            &what,
            &rest,
            &monitor,
            width,
            out,
            button,
            region_spec,
            save,
            json,
            lines,
        );
    }

    // Reading the log and answering a question are the user's own, and
    // need neither a backend nor a permission.
    match what.as_str() {
        "log" => {
            let path = screen::log_path()?;
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let last: Vec<&str> = text.lines().rev().take(lines).collect();
            for line in last.into_iter().rev() {
                println!("{line}");
            }
            return Ok(());
        }
        "allow" => {
            // The answer belongs to whoever sits here. In a session that
            // is the `y` on imkr's own question; in a plain terminal this
            // command. An agent's shell has neither.
            if !std::io::stdin().is_terminal() {
                bail!(
                    "`imkr screen allow` wants a terminal: answer imkr's question with y, \
                     or set computer_use = \"on\" in the config of the machine whose screen \
                     it is"
                );
            }
            let grant = screen::allow("the user here")?;
            println!(
                "agents may see and drive this screen for {} minutes",
                grant.left().unwrap_or_default().as_secs() / 60
            );
            return Ok(());
        }
        "refuse" => {
            screen::refuse("the user here")?;
            println!("agents may not see or drive this screen");
            return Ok(());
        }
        _ => {}
    }

    let desk = screen::desktop();
    // `status` answers even where there is no backend at all: that is the
    // answer somebody is asking for.
    if what == "status" {
        let config = crate::config::Config::load().0;
        let grant = screen::grant();
        let probe = match &desk {
            Ok(desk) => desk.probe(),
            Err(e) => screen::Probe {
                blocked: vec![format!("{e}")],
                ..screen::Probe::default()
            },
        };
        let allowed = match config.computer_use {
            screen::ComputerUse::On => true,
            screen::ComputerUse::Ask => grant.allowed(),
            screen::ComputerUse::Off => false,
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "machine": protocol::machine_name(),
                    "setting": config.computer_use.word(),
                    "allowed": allowed,
                    "allowed_for": grant.left().map(|l| l.as_secs()),
                    "allowed_to": (!grant.who.is_empty()).then_some(grant.who.clone()),
                    "ready": probe.ok() && allowed,
                    "backend": probe.backend,
                    "monitor": screen::pick_monitor(&probe.monitors, monitor.as_deref(), config.computer_use_monitor.as_deref()).ok().flatten(),
                    "monitor_setting": config.computer_use_monitor.clone(),
                    "blocked": probe.blocked,
                    "gaps": probe.backend.is_empty().then(|| screen::gaps()
                        .into_iter()
                        .map(|(system, why)| serde_json::json!({ "system": system, "why": why }))
                        .collect::<Vec<_>>()),
                    "notes": probe.notes,
                    "tools": probe.tools.iter().map(|(t, there)| serde_json::json!({ "tool": t, "there": there })).collect::<Vec<_>>(),
                    "monitors": probe.monitors,
                }))?
            );
            return Ok(());
        }
        println!("machine   {}", protocol::machine_name());
        println!(
            "backend   {}",
            if probe.backend.is_empty() {
                "none"
            } else {
                &probe.backend
            }
        );
        println!(
            "setting   computer_use = {}{}",
            config.computer_use.word(),
            match (config.computer_use, grant.left()) {
                (screen::ComputerUse::Ask, Some(left)) => format!(
                    " · allowed for {} more minutes{}",
                    left.as_secs() / 60,
                    if grant.who.is_empty() {
                        String::new()
                    } else {
                        format!(" to {}", grant.who)
                    }
                ),
                (screen::ComputerUse::Ask, None) => " · imkr asks here first".to_owned(),
                _ => String::new(),
            }
        );
        let picked = screen::pick_monitor(
            &probe.monitors,
            monitor.as_deref(),
            config.computer_use_monitor.as_deref(),
        );
        for m in &probe.monitors {
            println!(
                "screen    {} {}x{} logical at {},{} · scale {} · {}{}",
                m.name,
                m.width,
                m.height,
                m.x,
                m.y,
                m.scale,
                if m.on { "on" } else { "OFF (dpms)" },
                if picked.as_ref().ok() == Some(&Some(m.name.clone())) {
                    " ← used without --monitor"
                } else {
                    ""
                }
            );
        }
        if let Err(e) = &picked {
            println!("screen    none to use: {e}");
        }
        for (tool, there) in &probe.tools {
            println!("tool      {tool} {}", if *there { "✓" } else { "MISSING" });
        }
        for note in &probe.notes {
            println!("note      {note}");
        }
        for why in &probe.blocked {
            println!("blocked   {why}");
        }
        // Where there is no backend at all, say which systems are not
        // supported yet and what each would take — a gap named is an
        // answer, an empty list is a riddle.
        if probe.backend.is_empty() {
            for (system, why) in screen::gaps() {
                println!("gap       {system}: {why}");
            }
        }
        println!(
            "ready     {}",
            match (probe.ok(), allowed) {
                (true, true) => "yes".to_owned(),
                (false, _) => "no — see blocked above".to_owned(),
                (true, false) => format!(
                    "no — computer_use is {} on {}",
                    config.computer_use.word(),
                    protocol::machine_name()
                ),
            }
        );
        return Ok(());
    }

    let desk = desk?;
    // Nothing happens on a locked or dark screen, whatever the setting
    // says: a lock screen is the one thing imkr must never drive.
    let probe = desk.probe();
    if !probe.ok() {
        bail!(
            "cannot use this screen ({}): {}. `imkr screen status` says more.",
            desk.name(),
            probe.blocked.join("; ")
        );
    }
    let driving = what != "shot";
    let who = own_address();
    permit(&who, if driving { "drive" } else { "see" })?;
    let monitors = desk.monitors()?;
    let settled = crate::config::Config::load().0.computer_use_monitor;

    match what.as_str() {
        "shot" => {
            // A region is an absolute box and picks its own screens, so
            // only a whole-screen shot needs one chosen.
            let on = match region {
                Some(_) => None,
                None => screen::pick_monitor(&monitors, monitor.as_deref(), settled.as_deref())?,
            };
            let shot = screen::shot(desk.as_ref(), on.as_deref(), region, width, out)?;
            screen::acted(
                &who,
                "shot",
                &format!(
                    "{} {}x{}",
                    match (&on, region) {
                        (_, Some(r)) => r.geometry(),
                        (Some(name), None) => name.clone(),
                        (None, None) => "all".to_owned(),
                    },
                    shot.width,
                    shot.height
                ),
            );
            let mut answer = shot.json();
            if want_bytes {
                let bytes = std::fs::read(&shot.path)?;
                answer["png"] = clipboard::base64(&bytes).into();
            }
            // For a person to look at: into the shared folder, named the
            // way their own machine sees it (#9082).
            let placed = match save {
                true => Some(crate::share::put(&shot.path, crate::share::SHOTS)?),
                false => None,
            };
            if let Some(placed) = &placed {
                answer["path"] = placed.path.display().to_string().into();
                answer["shared"] = placed.json();
            }
            if json || want_bytes {
                println!("{}", serde_json::to_string_pretty(&answer)?);
            } else {
                match &placed {
                    Some(placed) => println!("{}", placed.line()),
                    None => println!("{}", shot.path.display()),
                }
                println!(
                    "{} · {}x{} · {}",
                    on.as_deref().unwrap_or("every screen"),
                    shot.width,
                    shot.height,
                    shot.hint()
                );
            }
        }
        "move" => {
            let (x, y) = coordinates(&rest)?;
            // With a screen named, X and Y are its own — which is what an
            // agent reads off a screenshot of that one screen.
            let (x, y) = screen::place(&monitors, monitor.as_deref(), x, y)?;
            let (x, y) = screen::move_to(desk.as_ref(), x, y)?;
            screen::acted(&who, "move", &format!("{x},{y}"));
            if json {
                println!("{}", serde_json::json!({ "x": x, "y": y }));
            } else {
                println!("pointer at {x},{y}");
            }
        }
        "click" => {
            // Without coordinates it clicks where the pointer is; with
            // them it moves first and makes sure it got there, so a click
            // never lands somewhere else than it was asked to.
            let at = match rest.is_empty() {
                true => desk.cursor()?,
                false => {
                    let (x, y) = coordinates(&rest)?;
                    let (x, y) = screen::place(&monitors, monitor.as_deref(), x, y)?;
                    screen::move_to(desk.as_ref(), x, y)?
                }
            };
            desk.click(button)?;
            let which = match button {
                screen::Button::Left => "left",
                screen::Button::Right => "right",
                screen::Button::Middle => "middle",
            };
            screen::acted(&who, "click", &format!("{which} at {},{}", at.0, at.1));
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "x": at.0, "y": at.1, "button": which })
                );
            } else {
                println!("{which} click at {},{}", at.0, at.1);
            }
        }
        "type" => {
            let text = match stdin_text {
                true => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
                false => rest.join(" "),
            };
            if text.is_empty() {
                bail!("nothing to type: `imkr screen type TEXT` or --stdin");
            }
            desk.text(&text)?;
            // The log says how much was typed, never what: it goes into
            // whatever has the focus, and that may be a password field.
            screen::acted(
                &who,
                "type",
                &format!("{} characters", text.chars().count()),
            );
            if !json {
                println!("typed {} characters", text.chars().count());
            }
        }
        "key" => {
            if rest.is_empty() {
                bail!("which key? e.g. `imkr screen key ctrl+c` or `imkr screen key Return`");
            }
            let mut pressed = Vec::new();
            for spec in &rest {
                let key = screen::Key::parse(spec)?;
                desk.key(&key)?;
                pressed.push(spec.clone());
            }
            screen::acted(&who, "key", &pressed.join(" "));
            if !json {
                println!("pressed {}", pressed.join(" "));
            }
        }
        other => bail!("no `imkr screen {other}`\n\n{USAGE}"),
    }
    Ok(())
}

/// Two numbers as a logical coordinate.
fn coordinates(rest: &[String]) -> Result<(i32, i32)> {
    let [x, y] = rest else {
        bail!("X and Y, as `imkr screen status` lists the screens (logical pixels)");
    };
    Ok((
        x.parse()
            .with_context(|| format!("{x:?} is not an x coordinate"))?,
        y.parse()
            .with_context(|| format!("{y:?} is not a y coordinate"))?,
    ))
}

/// Whether `who` may do `what` (`see` or `drive`) to this screen now.
///
/// `on` always, `off` never, `ask` once somebody here has said yes: the
/// question goes to every session on this machine, and the answer lands in
/// the permission file this waits on.
fn permit(who: &str, what: &str) -> Result<()> {
    use crate::screen;
    let config = crate::config::Config::load().0;
    match config.computer_use {
        screen::ComputerUse::On => return Ok(()),
        screen::ComputerUse::Off => bail!(
            "{} does not let agents see or drive its screen: computer_use is off in its \
             config (settings menu: \"Agents may use this screen\"). It is each machine's \
             own setting and never travels with `imkr settings sync`.",
            protocol::machine_name()
        ),
        screen::ComputerUse::Ask => {}
    }
    if screen::grant().allowed() {
        return Ok(());
    }
    screen::ask(who, what)?;
    let asked = ask_sessions(who, what);
    let until = std::time::Instant::now() + screen::ASK_FOR;
    while std::time::Instant::now() < until {
        let grant = screen::grant();
        if grant.allowed() {
            return Ok(());
        }
        if grant.asking.as_ref().and_then(|a| a.answer) == Some(false) {
            bail!("the user at {} said no", protocol::machine_name());
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    bail!(
        "nobody at {} answered within {} seconds{}. Ask the user to allow it \
         (imkr shows the question in its session, or `imkr screen allow` in a \
         terminal there).",
        protocol::machine_name(),
        screen::ASK_FOR.as_secs(),
        if asked == 0 {
            " — no imkr session runs there to ask in"
        } else {
            ""
        }
    )
}

/// Puts the question in front of the sessions on this machine; how many
/// took it.
///
/// A socket of its own outside the runtime directory (a test world) means
/// that one session and no other — asking the machine's real sessions
/// from a test would put a dialog in front of somebody's actual work.
/// Inside a pane `IMKR_SOCKET` points into the runtime directory and is
/// not such a socket, so a question from an agent reaches every session,
/// which is the point: whichever one the user is looking at can answer.
fn ask_sessions(who: &str, what: &str) -> usize {
    let request = || Request::ScreenAsk {
        who: who.to_owned(),
        what: what.to_owned(),
    };
    let sockets: Vec<PathBuf> = match protocol::test_socket() {
        Some(path) => vec![path],
        None => protocol::sessions()
            .iter()
            .filter_map(|name| protocol::session_socket(name).ok())
            .collect(),
    };
    sockets
        .iter()
        .filter(|path| client::request_at(path, request()).is_ok())
        .count()
}

/// `imkr screen … --host H`: the same command over there. A screenshot
/// comes back in its JSON and is written here, so the path in the answer
/// is a file on this machine.
#[allow(clippy::too_many_arguments)]
fn screen_on(
    host: &str,
    what: &str,
    rest: &[String],
    monitor: &Option<String>,
    width: u32,
    out: Option<PathBuf>,
    button: crate::screen::Button,
    region: Option<String>,
    save: bool,
    json: bool,
    lines: usize,
) -> Result<()> {
    use crate::screen;
    let mut line: Vec<String> = vec!["screen".into(), what.into()];
    line.extend(rest.iter().cloned());
    let mut input = String::new();
    if let Some(name) = monitor {
        line.extend(["--monitor".to_owned(), name.clone()]);
    }
    if let Some(region) = region {
        line.extend(["--region".to_owned(), region]);
    }
    match what {
        "shot" => {
            line.push("--base64".into());
            line.extend(["--width".to_owned(), width.to_string()]);
        }
        "type" => {
            // The text goes over on stdin, not in the command line of an
            // ssh call: a password typed into a prompt would otherwise sit
            // in the process list of two machines.
            input = match rest.is_empty() {
                true => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
                false => rest.join(" "),
            };
            line = vec!["screen".into(), "type".into(), "--stdin".into()];
        }
        "click" => {
            line.extend([
                "--button".to_owned(),
                match button {
                    screen::Button::Left => "left",
                    screen::Button::Right => "right",
                    screen::Button::Middle => "middle",
                }
                .to_owned(),
            ]);
        }
        "status" | "log" => {
            line.extend(["--lines".to_owned(), lines.to_string()]);
            if what == "status" {
                line.pop();
                line.pop();
            }
            if json {
                line.push("--json".into());
            }
        }
        _ => {}
    }
    if json && !matches!(what, "status" | "log" | "shot") {
        line.push("--json".into());
    }
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let answer = client::remote_output(host, &words, &input)?;
    if what != "shot" {
        print!("{answer}");
        return Ok(());
    }
    let mut shot: serde_json::Value =
        serde_json::from_str(&answer).with_context(|| format!("{host} answered {answer:?}"))?;
    let png = shot["png"]
        .as_str()
        .context("the screenshot came back without its picture")?;
    let bytes = clipboard::unbase64(png).context("the picture is not base64")?;
    let path = match out {
        Some(path) => path,
        None => {
            let dir = crate::persist::state_dir()?.join("screenshots");
            std::fs::create_dir_all(&dir)?;
            dir.join(format!(
                "shot-{host}-{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            ))
        }
    };
    std::fs::write(&path, &bytes)?;
    shot["png"] = serde_json::Value::Null;
    shot["path"] = path.display().to_string().into();
    shot["machine"] = host.into();
    // The picture came over as bytes, so it is a file on THIS machine —
    // and the shared folder that matters is the one of the machine the
    // user sits at, not the one whose screen was photographed (#9082).
    let placed = match save {
        true => Some(crate::share::put(&path, crate::share::SHOTS)?),
        false => None,
    };
    if let Some(placed) = &placed {
        shot["path"] = placed.path.display().to_string().into();
        shot["shared"] = placed.json();
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&shot)?);
    } else {
        match &placed {
            Some(placed) => println!("{}", placed.line()),
            None => println!("{}", path.display()),
        }
        println!(
            "{}x{} · {}",
            shot["image"]["width"],
            shot["image"]["height"],
            // As a string, not as JSON: `shot["hint"]` prints its quotes.
            shot["hint"].as_str().unwrap_or_default()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_claude_gets_no_stop_failure() {
        let old = claude_version("2.1.20 (Claude Code)").unwrap();
        assert!(old < STOP_FAILURE_SINCE);
        assert!(claude_version("2.1.304 (Claude Code)\n").unwrap() >= STOP_FAILURE_SINCE);
        assert!(claude_version("2.1.78").unwrap() >= STOP_FAILURE_SINCE);
        assert_eq!(claude_version("mise ERROR claude not found"), None);
        assert_eq!(claude_version(""), None);
    }

    #[test]
    fn run_quotes_several_words_but_not_one_line() {
        let words = |w: &[&str]| w.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(command_line(&words(&["cargo", "test"])), "cargo test");
        assert_eq!(
            command_line(&words(&["echo", "a b;echo ZWEI"])),
            "echo 'a b;echo ZWEI'"
        );
        assert_eq!(command_line(&words(&["echo", "it's"])), r"echo 'it'\''s'");
        assert_eq!(
            command_line(&words(&["make && make test"])),
            "make && make test"
        );
    }

    #[test]
    fn tap_leaves_out_the_oldest_history_first() {
        let screen = b"one\r\ntwo\r\nthree\r\n\x1b[H\x1b[2Jscreen";
        assert_eq!(fit_screen(screen, 100), screen);
        // Whole lines go, the newest stay.
        assert_eq!(fit_screen(screen, 25), b"three\r\n\x1b[H\x1b[2Jscreen");
        assert_eq!(fit_screen(screen, 18), b"\x1b[H\x1b[2Jscreen");
        // The screen itself is never cut.
        assert_eq!(fit_screen(screen, 5), b"\x1b[H\x1b[2Jscreen");
    }

    #[test]
    fn tap_takes_input_as_base64_or_text() {
        let input = clipboard::base64(b"\x1b[A\r");
        assert_eq!(
            tap_line(&format!(r#"{{"input":"{input}"}}"#)),
            Some(TapLine::Input(b"\x1b[A\r".to_vec()))
        );
        assert_eq!(
            tap_line(r#"{"text":"ls\r"}"#),
            Some(TapLine::Input(b"ls\r".to_vec()))
        );
        assert_eq!(tap_line(r#"{"screen":true}"#), Some(TapLine::Screen));
        assert_eq!(tap_line(r#"{"input":"not base64!"}"#), None);
        assert_eq!(tap_line("ls"), None);
    }

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
