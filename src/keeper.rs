//! Keepers and their bees (#9133): a session whose agent only steers
//! (`imkr keeper`), and the work sessions it starts, one per task, each
//! with a fresh context and a briefing (`imkr spawn`).
//!
//! imkr brings the craft only: what a keeper does with it is the `keeper`
//! skill, which comes from AI Brain.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::client;
use crate::layout::PaneId;
use crate::protocol::{self, Request};

/// What marks a keeper's session in every list, in the theme's accent.
pub const MARK: &str = "⚑";

/// The skill a keeper's agent is started with.
pub const SKILL: &str = "keeper";

/// How long an agent may take to start before `keeper` and `spawn` give up.
const READY_WITHIN: Duration = Duration::from_secs(120);

/// A bee as its keeper remembers it, in `bees.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bee {
    /// Its session's name.
    pub name: String,
    /// Where `imkr msg` reaches it: `machine:session/pane`.
    pub address: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub worktree: bool,
    /// The AI Brain task it works on.
    #[serde(default)]
    pub task: Option<u64>,
    /// The briefing, as a path on the bee's machine.
    pub brief: String,
    /// When it was started, seconds since 1970.
    pub at: u64,
}

/// The folder of keeper `name`: `<state>/keeper/<name>/`.
pub fn dir(name: &str) -> Result<PathBuf> {
    Ok(crate::persist::state_dir()?.join("keeper").join(name))
}

/// Creates a keeper's folder with `briefings/` and `keeper-stand.md`,
/// leaving what is there.
pub fn prepare(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir.join("briefings"))
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let notes = dir.join("keeper-stand.md");
    if !notes.exists() {
        std::fs::write(
            &notes,
            "# Keeper\n\nPer bee: address, task, briefing, last message, open approvals.\n",
        )?;
    }
    Ok(())
}

/// `base`, or `base-2`, `base-3` … the first that is not `taken`.
pub fn free_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .expect("an unbounded range ends only with a free name")
}

/// Whether session `name` runs here or comes back by itself (stopped by a
/// reboot or crash). One ended on purpose frees its name: a keeper started
/// again under it finds its folder and notes where it left them.
pub fn session_taken(name: &str) -> bool {
    protocol::sessions().iter().any(|s| s == name)
        || crate::persist::dormant_sessions(&[])
            .iter()
            .any(|d| d.name == name && !d.closed)
}

/// Where `tool` looks for skills: its own folder, a config folder set in
/// the environment, and those of the accounts imkr keeps. Empty for an
/// agent whose skills imkr does not know.
pub fn skill_dirs(tool: &str, home: &Path, state: &Path, env: &[Option<PathBuf>]) -> Vec<PathBuf> {
    let own: &[&str] = match tool {
        "claude" => &[".claude/skills"],
        "codex" => &[".codex/skills", ".agents/skills"],
        _ => return Vec::new(),
    };
    let mut dirs: Vec<PathBuf> = own.iter().map(|d| home.join(d)).collect();
    dirs.extend(env.iter().flatten().map(|d| d.join("skills")));
    if let Ok(accounts) = std::fs::read_dir(state.join("accounts").join(tool)) {
        let mut found: Vec<PathBuf> = accounts
            .flatten()
            .map(|a| a.path().join("skills"))
            .collect();
        found.sort();
        dirs.extend(found);
    }
    dirs
}

/// Whether one of `dirs` has the skill `name`.
pub fn has_skill(dirs: &[PathBuf], name: &str) -> bool {
    dirs.iter().any(|d| d.join(name).join("SKILL.md").is_file())
}

/// Says how to get the keeper skill when `tool` has none here, or nothing
/// when it has (or its skills are unknown to imkr).
fn check_skill(tool: &str) -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let env = match tool {
        "claude" => vec![std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)],
        "codex" => vec![std::env::var_os("CODEX_HOME").map(PathBuf::from)],
        _ => Vec::new(),
    };
    let dirs = skill_dirs(tool, &home, &crate::persist::state_dir()?, &env);
    if dirs.is_empty() || has_skill(&dirs, SKILL) {
        return Ok(());
    }
    bail!("{}", missing_skill(tool, &dirs))
}

/// The message for a missing keeper skill.
pub fn missing_skill(tool: &str, looked: &[PathBuf]) -> String {
    let looked: Vec<String> = looked.iter().map(|d| d.display().to_string()).collect();
    format!(
        "no `{SKILL}` skill for {tool} on this machine (looked in {}).\n\
         imkr brings the commands, the skill comes from AI Brain:\n  \
         ai-brain-harness select --dazu skill/{SKILL}\n  \
         ai-brain-harness sync --apply\n\
         then run this again",
        looked.join(", ")
    )
}

/// What a keeper's agent gets typed in first: its skill, and where it
/// keeps its briefings and notes.
pub fn first_input(tool: &str, dir: &Path) -> String {
    let folder = format!(
        "Keeper folder: {} (briefings/ for the briefings, keeper-stand.md for your notes)",
        dir.display()
    );
    match tool {
        "claude" => format!("/{SKILL} {folder}"),
        "codex" => format!("${SKILL} {folder}"),
        _ => format!("Use the skill {SKILL}. {folder}"),
    }
}

/// The message that hands a bee its briefing.
pub fn brief_message(brief: &str, task: Option<u64>) -> String {
    match task {
        Some(task) => format!("Read your briefing and work through it (task #{task}): {brief}"),
        None => format!("Read your briefing and work through it: {brief}"),
    }
}

fn lock(dir: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    std::fs::create_dir_all(dir)?;
    let path = dir.join("bees.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    // SAFETY: flock on a descriptor we own; released when `file` closes.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("cannot lock {}", path.display());
    }
    Ok(file)
}

/// The bees of the keeper in `dir`, oldest first.
pub fn bees(dir: &Path) -> Vec<Bee> {
    std::fs::read_to_string(dir.join("bees.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Adds `bee` to the keeper in `dir`, in place of one of the same name.
pub fn record(dir: &Path, bee: Bee) -> Result<()> {
    let _lock = lock(dir)?;
    let mut all = bees(dir);
    all.retain(|b| b.name != bee.name);
    all.push(bee);
    let path = dir.join("bees.json");
    let new = dir.join("bees.json.new");
    std::fs::write(&new, serde_json::to_string_pretty(&all)?)?;
    std::fs::rename(&new, &path)?;
    Ok(())
}

/// The bee of the keeper in `dir` that `address` (as `imkr msg` takes
/// it, or as a bee signs its messages) is: the session tells.
pub fn bee_at<'a>(bees: &'a [Bee], address: &str) -> Option<&'a Bee> {
    let (_, session, _) = crate::cli::parse_address(address).ok()?;
    let session = session?;
    bees.iter().rev().find(|b| b.name == session)
}

/// Where a briefing given on stdin is kept on the bee's machine, readable
/// by its owner only: it may tell internals.
fn keep_brief(state: &Path, name: &str, text: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = state.join("bees");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.md"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

/// How a started agent came up.
#[derive(Debug, PartialEq)]
enum Ready {
    Idle,
    /// It asks something first, e.g. whether to trust a new folder: that
    /// is the user's to answer.
    Asking,
}

/// Starts session `name` here with the default agent in `dir`, in a new
/// git worktree `name` of the repository there with `worktree`, and waits
/// until the agent is ready. Returns its pane.
fn start(
    name: &str,
    dir: &Path,
    worktree: bool,
    by: Option<String>,
) -> Result<(PathBuf, PaneId, PathBuf, Ready)> {
    let socket = protocol::session_socket(name)?;
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        bail!("session {name} runs already");
    }
    client::start_server(&socket, Some(name))?;
    let agent = vec!["<agent>".to_owned()];
    let reply = if worktree {
        client::request_at(
            &socket,
            Request::Worktree {
                name: name.to_owned(),
                command: agent,
                cwd: Some(dir.to_owned()),
                fresh: true,
                by: by.clone(),
            },
        )
    } else {
        client::request_at(
            &socket,
            Request::New {
                command: agent,
                float: false,
                workspace: Some(0),
                cwd: Some(dir.to_owned()),
                fresh: true,
                by: by.clone(),
            },
        )
    }
    .with_context(|| format!("session {name} started, but no agent in it"))?;
    let pane = reply["pane"].as_u64().context("no pane for the agent")? as PaneId;
    let dir = reply["dir"]
        .as_str()
        .map_or_else(|| dir.to_owned(), PathBuf::from);
    let ready = wait_ready(&socket, pane, name)?;
    Ok((socket, pane, dir, ready))
}

/// Waits until the agent in `pane` is idle twice in a row (a program that
/// is still drawing its first screen can look quiet for a moment), or
/// asks something.
fn wait_ready(socket: &Path, pane: PaneId, name: &str) -> Result<Ready> {
    let start = Instant::now();
    let mut quiet = 0;
    loop {
        let panes = client::request_at(socket, Request::List)?;
        let entry = panes
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["pane"].as_u64() == Some(pane as u64))
            .with_context(|| format!("the agent in {name} ended right away"))?;
        let state = entry["state"].as_str().unwrap_or("no agent yet");
        match state {
            "idle" | "done" => {
                quiet += 1;
                if quiet >= 2 {
                    return Ok(Ready::Idle);
                }
            }
            "blocked" => return Ok(Ready::Asking),
            "failed" => bail!(
                "the agent in {name} failed while starting, see `imkr -s {name} read --pane {pane}`"
            ),
            _ => quiet = 0,
        }
        if start.elapsed() > READY_WITHIN {
            bail!(
                "the agent in {name} was not ready in {} s ({state}), see `imkr -s {name} read --pane {pane}`",
                READY_WITHIN.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

/// The address of `pane` in session `session` on this machine.
fn address(session: &str, pane: PaneId) -> String {
    format!("{}:{session}/{pane}", protocol::machine_name())
}

/// An agent asking something before it starts (e.g. whether to trust a
/// new folder) is the user's to answer: said with where it is and what
/// to send afterwards.
fn waits_for_answer(address: &str, then: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{address} is waiting for an answer before it starts (e.g. whether to trust the \
         folder), nothing was answered for you.\n\
         see it:  imkr read {address}\n\
         then:    {then}"
    )
}

/// `imkr keeper [NAME] [--host H] [--dir DIR]`: a session whose agent
/// starts with the keeper skill and its folder.
pub fn keeper(args: &[String], host: Option<String>) -> Result<()> {
    use crate::cli::{take_flag, take_value};
    const USAGE: &str =
        "usage: imkr keeper [NAME] [--host H] [--dir DIR] | --mark [SESSION] | --unmark [SESSION]";
    let mut rest = args.to_vec();
    let mark = take_flag(&mut rest, "--mark");
    let unmark = take_flag(&mut rest, "--unmark");
    if mark || unmark {
        if mark && unmark {
            bail!(USAGE);
        }
        let session = match rest.as_slice() {
            [] => None,
            [name] => Some(name.clone()),
            _ => bail!(USAGE),
        };
        return set_mark(session, mark, host.as_deref());
    }
    let dir = take_value(&mut rest, "--dir")?;
    // Passed on from another machine: answer in JSON, attach to nothing.
    let json = take_flag(&mut rest, "--json");
    let given = match rest.as_slice() {
        [] => None,
        [name] if !name.starts_with('-') => Some(name.clone()),
        _ => bail!(USAGE),
    };
    if let Some(name) = &given
        && !protocol::valid_session_name(name)
    {
        bail!("{name:?} is no session name: letters, digits, - and _");
    }
    if let Some(host) = host {
        let mut there = vec!["keeper".to_owned(), "--json".to_owned()];
        there.extend(given);
        if let Some(dir) = dir {
            there.extend(["--dir".to_owned(), dir]);
        }
        let words: Vec<&str> = there.iter().map(String::as_str).collect();
        let out = client::remote_output(&host, &words, "")?;
        let value: serde_json::Value = serde_json::from_str(out.trim())
            .with_context(|| format!("{host} answered {:?}", out.trim()))?;
        let name = value["name"].as_str().unwrap_or_default().to_owned();
        let pane = value["pane"].as_u64().unwrap_or_default();
        client::remember_host(&host);
        println!(
            "keeper {host}:{name}/{pane} in {}",
            value["dir"].as_str().unwrap_or_default()
        );
        return show(Some(&host), &name);
    }
    let (config, _) = crate::config::Config::load();
    let tool = config.default_agent.clone();
    check_skill(&tool)?;
    let name = match given {
        Some(name) if session_taken(&name) => bail!("there is a session {name} already"),
        Some(name) => name,
        None => free_name("keeper", session_taken),
    };
    let folder = self::dir(&name)?;
    prepare(&folder)?;
    let cwd = match dir {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()?,
    };
    let (socket, pane, _, ready) = start(&name, &cwd, false, None)?;
    client::request_at(&socket, Request::Keeper { mark: true })?;
    let at = address(&name, pane);
    if ready == Ready::Asking {
        let then = format!(
            "imkr send --pane {pane} -s {name} {}",
            crate::client::sh_quote(&first_input(&tool, &folder))
        );
        return Err(waits_for_answer(&at, &then));
    }
    client::request_at(
        &socket,
        Request::Send {
            pane,
            text: first_input(&tool, &folder),
            enter: true,
        },
    )?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "name": name, "pane": pane, "address": at, "dir": folder })
        );
        return Ok(());
    }
    println!("keeper {at} in {}", folder.display());
    show(None, &name)
}

/// `imkr keeper --mark|--unmark [SESSION]`: marks a session as a keeper's
/// afterwards, or takes the mark off; without a name the one this runs in.
fn set_mark(session: Option<String>, mark: bool, host: Option<&str>) -> Result<()> {
    let flag = if mark { "--mark" } else { "--unmark" };
    if let Some(host) = host {
        let mut there = vec!["keeper", flag];
        there.extend(session.as_deref());
        print!("{}", client::remote_output(host, &there, "")?);
        return Ok(());
    }
    let (socket, name) = match session {
        Some(name) => (protocol::session_socket(&name)?, name),
        None => (protocol::socket_path()?, protocol::session_name()),
    };
    client::request_at(&socket, Request::Keeper { mark })
        .with_context(|| format!("no session {name} running here"))?;
    if mark {
        println!("{name} is marked as a keeper");
    } else {
        println!("{name} is no keeper any more");
    }
    Ok(())
}

/// Goes to the new session when a person typed the command: the client of
/// the pane it ran in switches, a plain terminal attaches.
fn show(host: Option<&str>, name: &str) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }
    let target = match host {
        Some(host) => format!("{host}:{name}"),
        None => name.to_owned(),
    };
    if std::env::var_os("IMKR_PANE").is_some() {
        client::request(Request::Go { target })?;
        return Ok(());
    }
    let remote = host.map(|host| client::Remote {
        host: host.to_owned(),
        session: Some(name.to_owned()),
    });
    if remote.is_none() {
        // SAFETY: no other thread runs; the client reads them next.
        unsafe {
            std::env::set_var("IMKR_SESSION", name);
            std::env::remove_var("IMKR_SOCKET");
        }
    }
    client::run(false, false, remote, false)
}

/// `imkr spawn NAME --brief FILE [--host H] [--dir REPO] [--worktree]
/// [--task N] [--keeper K] [--json]`: a bee, its own session with the
/// default agent, that gets its briefing as the first message.
pub fn spawn(args: &[String], host: Option<String>) -> Result<()> {
    use crate::cli::{take_flag, take_value};
    const USAGE: &str = "usage: imkr spawn NAME --brief FILE|- [--host H] [--dir REPO] \
                         [--worktree] [--task N] [--keeper K] [--json]";
    let mut rest = args.to_vec();
    let brief = take_value(&mut rest, "--brief")?.context(USAGE)?;
    let dir = take_value(&mut rest, "--dir")?;
    let worktree = take_flag(&mut rest, "--worktree");
    let task = take_value(&mut rest, "--task")?
        .map(|n| n.trim_start_matches('#').parse::<u64>())
        .transpose()
        .context("--task wants a number")?;
    let keeper = take_value(&mut rest, "--keeper")?;
    let json = take_flag(&mut rest, "--json");
    // Passed on from the keeper's machine: it keeps the record.
    let given = take_value(&mut rest, "--from")?;
    // The keeper's pane, so lists show the bee under it (#9087).
    let by = take_value(&mut rest, "--by")?.or_else(protocol::caller);
    let [name] = rest.as_slice() else {
        bail!(USAGE);
    };
    if !protocol::valid_session_name(name) {
        bail!("{name:?} is no session name: letters, digits, - and _");
    }
    let text = if brief == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        text
    } else {
        std::fs::read_to_string(&brief).with_context(|| format!("cannot read {brief}"))?
    };
    if text.trim().is_empty() {
        bail!("the briefing is empty");
    }
    let mine = given.is_none();
    let from = given.unwrap_or_else(crate::cli::own_address);

    let bee = match &host {
        Some(host) => {
            let mut there = vec![
                "spawn",
                name.as_str(),
                "--brief",
                "-",
                "--from",
                from.as_str(),
                "--json",
            ];
            let task_text = task.map(|t| t.to_string());
            if let Some(dir) = &dir {
                there.extend(["--dir", dir]);
            }
            if worktree {
                there.push("--worktree");
            }
            if let Some(by) = &by {
                there.extend(["--by", by]);
            }
            if let Some(task) = &task_text {
                there.extend(["--task", task]);
            }
            let out = client::remote_output(host, &there, &text)?;
            let value: serde_json::Value = serde_json::from_str(out.trim())
                .with_context(|| format!("{host} answered {:?}", out.trim()))?;
            client::remember_host(host);
            let pane = value["pane"].as_u64().unwrap_or_default();
            let bee = Bee {
                name: name.clone(),
                // The name this machine reaches it by.
                address: format!("{host}:{name}/{pane}"),
                host: Some(host.clone()),
                dir: value["dir"].as_str().map(str::to_owned),
                worktree,
                task,
                brief: value["brief"].as_str().unwrap_or_default().to_owned(),
                at: crate::persist::now(),
            };
            (bee, value["asking"].as_bool() == Some(true))
        }
        None => {
            if session_taken(name) {
                bail!("there is a session {name} already");
            }
            let brief = if brief == "-" {
                keep_brief(&crate::persist::state_dir()?, name, &text)?
            } else {
                std::fs::canonicalize(&brief)?
            };
            let cwd = match &dir {
                Some(dir) => PathBuf::from(dir),
                None => std::env::current_dir()?,
            };
            let (socket, pane, dir, ready) = start(name, &cwd, worktree, by.clone())?;
            let brief = brief.display().to_string();
            let asking = ready == Ready::Asking;
            if !asking {
                client::request_at(
                    &socket,
                    Request::Message {
                        to: pane.to_string(),
                        text: brief_message(&brief, task),
                        from: from.clone(),
                    },
                )?;
            }
            let bee = Bee {
                name: name.clone(),
                address: address(name, pane),
                host: None,
                dir: Some(dir.display().to_string()),
                worktree,
                task,
                brief,
                at: crate::persist::now(),
            };
            (bee, asking)
        }
    };
    let (bee, asking) = bee;
    if mine {
        let keeper = keeper.unwrap_or_else(protocol::session_name);
        record(&self::dir(&keeper)?, bee.clone())?;
        if !asking {
            crate::cli::note_sent(&bee.address, &brief_message(&bee.brief, task));
        }
    }
    if json || !mine {
        let mut value = serde_json::to_value(&bee)?;
        value["pane"] = bee
            .address
            .rsplit('/')
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .into();
        value["asking"] = asking.into();
        println!("{value}");
        // Passed on: the keeper's side says it, with the name it knows.
        if !mine {
            return Ok(());
        }
    }
    if asking {
        let then = format!(
            "imkr msg {} {}",
            bee.address,
            crate::client::sh_quote(&brief_message(&bee.brief, task))
        );
        return Err(waits_for_answer(&bee.address, &then));
    }
    if !json {
        println!("{}", bee.address);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imkr-keeper-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_taken_name_gets_the_next_number() {
        assert_eq!(free_name("keeper", |_| false), "keeper");
        let taken = ["keeper", "keeper-2"];
        assert_eq!(free_name("keeper", |n| taken.contains(&n)), "keeper-3");
        // A gap is used.
        let taken = ["keeper", "keeper-3"];
        assert_eq!(free_name("keeper", |n| taken.contains(&n)), "keeper-2");
    }

    #[test]
    fn bees_are_recorded_and_replaced_by_name() {
        let dir = temp("bees");
        let bee = |name: &str, task| Bee {
            name: name.into(),
            address: format!("laptop:{name}/1"),
            host: None,
            dir: None,
            worktree: false,
            task,
            brief: format!("/b/{name}.md"),
            at: 1,
        };
        record(&dir, bee("api-fix", Some(1))).unwrap();
        record(&dir, bee("docs", None)).unwrap();
        record(&dir, bee("api-fix", Some(2))).unwrap();
        let all = bees(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].name, "api-fix");
        assert_eq!(all[1].task, Some(2));
        // Found by how a bee signs its messages, too.
        assert_eq!(
            bee_at(&all, "claude@laptop:docs/1").map(|b| &b.name[..]),
            Some("docs")
        );
        assert_eq!(bee_at(&all, "server:other/1"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_skill_says_how_to_get_it() {
        let home = temp("skill");
        let state = home.join("state");
        let dirs = skill_dirs("claude", &home, &state, &[None]);
        assert!(!has_skill(&dirs, SKILL));
        let message = missing_skill("claude", &dirs);
        assert!(message.contains("ai-brain-harness select --dazu skill/keeper"));
        assert!(message.contains("sync --apply"));
        // In an account's folder counts as much as in ~/.claude.
        let account = state.join("accounts/claude/work/skills/keeper");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(account.join("SKILL.md"), "---\nname: keeper\n---\n").unwrap();
        let dirs = skill_dirs("claude", &home, &state, &[None]);
        assert!(has_skill(&dirs, SKILL));
        // Agents whose skills imkr does not know are not held up.
        assert!(skill_dirs("pi", &home, &state, &[None]).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_first_input_names_the_skill_the_agent_way() {
        let dir = Path::new("/s/keeper/keeper");
        assert!(first_input("claude", dir).starts_with("/keeper Keeper folder: /s/keeper/keeper"));
        assert!(first_input("codex", dir).starts_with("$keeper "));
        assert!(first_input("pi", dir).contains("skill keeper"));
    }

    #[test]
    fn the_keeper_mark_is_kept_and_old_saves_have_none() {
        let mut saved = crate::persist::Saved::new(1, Default::default(), Default::default());
        assert!(!serde_json::to_string(&saved).unwrap().contains("keeper"));
        saved.keeper = true;
        let back: crate::persist::Saved =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert!(back.keeper);
        // What older versions send has no mark.
        let mut old = serde_json::to_value(protocol::SessionInfo {
            name: "imker".into(),
            keeper: true,
            ..Default::default()
        })
        .unwrap();
        old.as_object_mut().unwrap().remove("keeper");
        let info: protocol::SessionInfo = serde_json::from_value(old).unwrap();
        assert!(!info.keeper);
    }

    #[test]
    fn a_kept_briefing_is_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let state = temp("brief");
        let path = keep_brief(&state, "bee", "secret").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&state);
    }
}
