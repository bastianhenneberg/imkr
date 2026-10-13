//! Saving the session's layout, so a server that dies with the machine can
//! bring it back: workspaces, splits, floating panes, each pane's directory,
//! and agents that can be resumed.
//!
//! Processes and screen contents cannot survive a restart. The shells start
//! fresh in their old directories, agents with their resume command.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::layout::PaneId;
use crate::workspace::Workspace;

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub version: u32,
    /// The workspace that was shown.
    pub active: u8,
    pub workspaces: BTreeMap<u8, Workspace>,
    pub panes: BTreeMap<PaneId, SavedPane>,
    /// The template it was made from, when it is a new session's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The accounts new shells start with, by agent, see `account`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedPane {
    pub cwd: Option<PathBuf>,
    /// Since when its agent is idle, seconds since 1970, so it keeps its
    /// age across updates and restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<u64>,
    /// The agent running in the pane, if any.
    pub agent: Option<String>,
    /// The agent's session id, from its hooks.
    pub session: Option<String>,
    /// The name the user gave the pane.
    #[serde(default)]
    pub name: Option<String>,
    /// Typed into the fresh shell, from a template. Running sessions never
    /// save one: what ran before a crash is not started again on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The accounts its agents use, by agent, see `Pane::accounts`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, String>,
}

impl Saved {
    pub fn new(
        active: u8,
        workspaces: BTreeMap<u8, Workspace>,
        panes: BTreeMap<PaneId, SavedPane>,
    ) -> Self {
        Self {
            version: VERSION,
            active,
            workspaces,
            panes,
            template: None,
            accounts: BTreeMap::new(),
        }
    }
}

impl SavedPane {
    /// The command that picks the agent's conversation up again, typed into
    /// the fresh shell: the agent's one of `commands`, with `{session}`
    /// replaced by the session id its hooks reported. One that needs an id
    /// is left out without one.
    pub fn resume_command(&self, commands: &BTreeMap<String, String>) -> Option<String> {
        let template = commands.get(self.agent.as_deref()?)?;
        if !template.contains("{session}") {
            return Some(template.clone());
        }
        let session = self.session.as_deref()?;
        // Session ids are uuids. Anything else is not typed into a shell.
        if session.is_empty()
            || !session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        Some(template.replace("{session}", session))
    }
}

/// Where the session is saved: `$IMKR_STATE`, else next to a socket set
/// with `$IMKR_SOCKET` (so test servers keep their own), else
/// `$XDG_STATE_HOME/imkr/session.json` for the default session and
/// `<name>.json` for named ones, falling back to `~/.local/state`.
/// Not the runtime directory of the socket, that one is gone after a reboot.
pub fn path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_STATE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(socket) = std::env::var_os("IMKR_SOCKET") {
        return Ok(PathBuf::from(socket).with_extension("state.json"));
    }
    path_for(&crate::protocol::session_name())
}

/// Where session `name` is saved, without the overrides of `path`.
pub fn path_for(name: &str) -> Result<PathBuf> {
    let file = if name == crate::protocol::DEFAULT_SESSION {
        "session.json".to_owned()
    } else {
        format!("{name}.json")
    };
    Ok(state_dir()?.join(file))
}

/// `$XDG_STATE_HOME/imkr`, falling back to `~/.local/state/imkr`.
/// The sessions of this machine that ran when it was last looked, see
/// `note_running`.
fn running_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("running"))
}

/// Notes the sessions running now. Every server keeps it current, so after
/// the machine went down with all of them, it still lists them all: those
/// `were_running`. One quit on purpose is left out by the others.
pub fn note_running(names: &[String]) -> Result<()> {
    let path = running_file()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, names.join("\n") + "\n")?;
    Ok(())
}

/// The sessions that ran last time, by `note_running`.
pub fn were_running() -> Vec<String> {
    running_file()
        .and_then(|path| Ok(fs::read_to_string(path)?))
        .map(|text| {
            text.lines()
                .filter(|n| crate::protocol::valid_session_name(n))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn state_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state")
        }
    };
    Ok(base.join("imkr"))
}

/// A session that does not run but can come back: stopped with the machine
/// or a crash, its state file left behind, or ended on purpose and kept in
/// the hive `last`.
#[derive(Debug, Clone)]
pub struct Dormant {
    pub name: String,
    pub layout: Saved,
    /// When it stopped or was closed, seconds since 1970.
    pub since: u64,
    /// Ended on purpose, rather than stopped.
    pub closed: bool,
}

/// The sessions on this machine that do not run, of those not in
/// `running`: stopped ones first, then closed ones, newest first in each.
pub fn dormant_sessions(running: &[String]) -> Vec<Dormant> {
    let mut stopped = Vec::new();
    if let Ok(dir) = state_dir()
        && let Ok(entries) = fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = if stem == "session" {
                crate::protocol::DEFAULT_SESSION.to_owned()
            } else {
                stem.to_owned()
            };
            if running.contains(&name) || !crate::protocol::valid_session_name(&name) {
                continue;
            }
            // Other files live here too, e.g. the marks: only layouts count.
            let Ok(Some(layout)) = load(&path) else {
                continue;
            };
            let since = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            stopped.push(Dormant {
                name,
                layout,
                since,
                closed: false,
            });
        }
    }
    stopped.sort_by_key(|d| std::cmp::Reverse(d.since));
    let mut closed: Vec<Dormant> = load_hive(LAST)
        .ok()
        .flatten()
        .map(|hive| hive.sessions)
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !running.contains(name) && !stopped.iter().any(|d| d.name == *name))
        .map(|(name, session)| Dormant {
            name,
            layout: session.layout,
            since: session.saved_at,
            closed: true,
        })
        .collect();
    closed.sort_by_key(|d| std::cmp::Reverse(d.since));
    stopped.extend(closed);
    stopped
}

/// Gets session `name` ready to come back when it was closed on purpose:
/// its layout from the hive `last` becomes its state file again, which the
/// server starting for it restores. A stopped session has one already.
pub fn revive(name: &str) -> Result<()> {
    let path = path_for(name)?;
    if path.exists() {
        return Ok(());
    }
    let hive = load_hive(LAST)?.context("no hive `last`")?;
    let session = hive
        .sessions
        .get(name)
        .with_context(|| format!("{name} is not among the closed sessions"))?;
    write(&path, &to_json(&session.layout)?)
}

/// Forgets session `name` that does not run: its state file and its place
/// in the hive `last` go. False when there was nothing to forget.
pub fn forget(name: &str) -> Result<bool> {
    let path = path_for(name)?;
    let mut found = false;
    prune_scrollback(&path, &[]);
    if path.exists() {
        fs::remove_file(&path)?;
        found = true;
    }
    if let Some(mut hive) = load_hive(LAST)?
        && hive.sessions.remove(name).is_some()
    {
        write_hive(LAST, &hive)?;
        found = true;
    }
    Ok(found)
}

/// A save file: the layouts of several sessions at once, like a
/// tmux-resurrect save. `last` is kept up to date by every session that
/// ends on purpose, others are made with `imkr save NAME`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hive {
    /// The sessions of the machine it was saved on.
    pub sessions: BTreeMap<String, HiveSession>,
    /// That machine's name; empty in hives from before.
    #[serde(default)]
    pub machine: String,
    /// Its machine id, which tells it apart where names are alike.
    #[serde(default)]
    pub machine_id: String,
    /// Every other machine known then, with its sessions: a map of where
    /// everything runs, to bring back and to show machines that are off.
    #[serde(default)]
    pub machines: Vec<HiveMachine>,
}

/// Another machine in a hive.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HiveMachine {
    /// How it was reached, the name for ssh.
    pub name: String,
    /// Its own name, as it calls itself.
    pub hostname: String,
    /// Its machine id, to know it again from wherever the hive is used;
    /// empty in hives from before, which go by `hostname`.
    #[serde(default)]
    pub id: String,
    pub sessions: BTreeMap<String, HiveSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HiveSession {
    /// When it was saved, in seconds since 1970.
    pub saved_at: u64,
    pub layout: Saved,
}

/// The save every session ending on purpose puts itself into.
pub const LAST: &str = "last";

/// Where save files go: `saves` next to the session files, or next to the
/// socket or state file a test set.
pub fn saves_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_STATE") {
        return Ok(PathBuf::from(path).with_extension("saves"));
    }
    if let Some(socket) = std::env::var_os("IMKR_SOCKET") {
        return Ok(PathBuf::from(socket).with_extension("saves"));
    }
    Ok(state_dir()?.join("saves"))
}

/// The save file called `name`.
pub fn save_path(name: &str) -> Result<PathBuf> {
    if !crate::protocol::valid_session_name(name) {
        anyhow::bail!("{name:?} is no save name: letters, digits, - and _ only");
    }
    Ok(saves_dir()?.join(format!("{name}.json")))
}

pub fn load_hive(name: &str) -> Result<Option<Hive>> {
    let path = save_path(name)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let hive: Hive =
        serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(Some(hive))
}

pub fn write_hive(name: &str, hive: &Hive) -> Result<PathBuf> {
    let path = save_path(name)?;
    write(&path, &serde_json::to_string_pretty(hive)?)?;
    Ok(path)
}

/// Puts `session`'s layout into the save `last`, next to the sessions that
/// are there already.
pub fn keep_in_last(session: &str, layout: Saved) -> Result<()> {
    let mut hive = load_hive(LAST).ok().flatten().unwrap_or_default();
    hive.sessions.insert(
        session.to_owned(),
        HiveSession {
            saved_at: now(),
            layout,
        },
    );
    write_hive(LAST, &hive)?;
    Ok(())
}

/// A hive as its menu lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiveInfo {
    pub name: String,
    pub sessions: Vec<String>,
    /// The other machines' sessions, as `machine:session`.
    pub elsewhere: Vec<String>,
    /// When its newest session was saved, in seconds since 1970.
    pub saved_at: u64,
}

/// Every hive, `last` first, then by name. Files that cannot be read are
/// left out.
pub fn hive_infos() -> Vec<HiveInfo> {
    let mut infos: Vec<HiveInfo> = saves()
        .into_iter()
        .filter_map(|name| {
            let hive = load_hive(&name).ok()??;
            Some(HiveInfo {
                saved_at: hive
                    .sessions
                    .values()
                    .map(|s| s.saved_at)
                    .max()
                    .unwrap_or(0),
                elsewhere: hive
                    .machines
                    .iter()
                    .flat_map(|m| m.sessions.keys().map(move |s| format!("{}:{s}", m.name)))
                    .collect(),
                sessions: hive.sessions.into_keys().collect(),
                name,
            })
        })
        .collect();
    infos.sort_by_key(|info| (info.name != LAST, info.name.clone()));
    infos
}

/// When a hive was saved: its newest session, on any machine.
pub fn hive_time(hive: &Hive) -> u64 {
    hive.sessions
        .values()
        .chain(hive.machines.iter().flat_map(|m| m.sessions.values()))
        .map(|s| s.saved_at)
        .max()
        .unwrap_or(0)
}

/// The hives to share with other machines, by name: all but `last`, which
/// every machine keeps of its own ended sessions.
pub fn shared_hives() -> BTreeMap<String, Hive> {
    saves()
        .into_iter()
        .filter(|name| name != LAST)
        .filter_map(|name| Some((name.clone(), load_hive(&name).ok()??)))
        .collect()
}

/// Takes in hives from another machine, each only if it is newer than the
/// one here of that name. Returns the names taken.
pub fn import_hives(hives: BTreeMap<String, Hive>) -> Result<Vec<String>> {
    let mut taken = Vec::new();
    for (name, hive) in hives {
        if name == LAST {
            continue;
        }
        let newer = match load_hive(&name)? {
            Some(here) => hive_time(&hive) > hive_time(&here),
            None => true,
        };
        if newer {
            write_hive(&name, &hive)?;
            taken.push(name);
        }
    }
    Ok(taken)
}

pub fn delete_hive(name: &str) -> Result<()> {
    let path = save_path(name)?;
    fs::remove_file(&path).with_context(|| format!("there is no hive called {name}"))
}

/// Renames hive `old` to `name`, never over another hive.
pub fn rename_hive(old: &str, name: &str) -> Result<()> {
    let from = save_path(old)?;
    let to = save_path(name)?;
    if to.exists() {
        anyhow::bail!("there is a hive called {name} already");
    }
    fs::rename(&from, &to).with_context(|| format!("there is no hive called {old}"))
}

/// The save files by name.
pub fn saves() -> Vec<String> {
    let Ok(entries) = saves_dir().and_then(|dir| Ok(fs::read_dir(dir)?)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.strip_suffix(".json")?.to_owned();
            Some(name)
        })
        .collect();
    names.sort();
    names
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn to_json(saved: &Saved) -> Result<String> {
    Ok(serde_json::to_string_pretty(saved)?)
}

/// Writes `json` to `path` through a temporary file, so a crash while
/// writing never leaves half a file.
pub fn write(path: &Path, json: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// The saved session, `None` if there is none. A file that cannot be read
/// is an error, the caller starts fresh.
pub fn load(path: &Path) -> Result<Option<Saved>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let saved: Saved =
        serde_json::from_str(&text).with_context(|| format!("cannot read {}", path.display()))?;
    if saved.version != VERSION {
        anyhow::bail!("{} is from another imkr version", path.display());
    }
    Ok(Some(saved))
}

pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Where the panes' history of the session saved at `state` is kept, one
/// text file per pane. It outlives the state file: a session closed on
/// purpose can come back from the hive `last` with its history.
pub fn scrollback_dir(state: &Path) -> PathBuf {
    state.with_extension("scrollback")
}

/// Keeps `text`, pane `id`'s history, readable by the user only: it holds
/// whatever ran in the terminal.
pub fn write_scrollback(state: &Path, id: PaneId, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = scrollback_dir(state);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let tmp = dir.join(format!("{id}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    fs::rename(&tmp, dir.join(format!("{id}.txt")))?;
    Ok(())
}

/// Pane `id`'s history kept by `write_scrollback`, if any.
pub fn read_scrollback(state: &Path, id: PaneId) -> Option<String> {
    fs::read_to_string(scrollback_dir(state).join(format!("{id}.txt"))).ok()
}

/// Drops the history of every pane but `keep`, all of it when `keep` is
/// empty.
pub fn prune_scrollback(state: &Path, keep: &[PaneId]) {
    let dir = scrollback_dir(state);
    if keep.is_empty() {
        let _ = fs::remove_dir_all(dir);
        return;
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<PaneId>().ok());
        if !id.is_some_and(|id| keep.contains(&id)) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Axis;
    use ratatui::layout::Rect;

    fn sample() -> Saved {
        let mut one = Workspace::new(0);
        one.split(Axis::Row, 1);
        one.add_float(2, Rect::new(0, 0, 100, 40));
        let workspaces = BTreeMap::from([(1, one), (3, Workspace::new(5))]);
        let panes = BTreeMap::from([
            (
                0,
                SavedPane {
                    cwd: Some("/tmp".into()),
                    ..Default::default()
                },
            ),
            (
                1,
                SavedPane {
                    cwd: Some("/home".into()),
                    agent: Some("claude".into()),
                    session: Some("0b8a-42".into()),
                    name: Some("backend".into()),
                    command: None,
                    idle_since: Some(1_790_000_000),
                    accounts: BTreeMap::from([("claude".into(), "privat".into())]),
                },
            ),
            (2, SavedPane::default()),
            (5, SavedPane::default()),
        ]);
        Saved::new(3, workspaces, panes)
    }

    #[test]
    fn survives_a_round_trip_through_a_file() {
        let dir = std::env::temp_dir().join(format!("imkr-test-{}", std::process::id()));
        let path = dir.join("session.json");
        let saved = sample();
        write(&path, &to_json(&saved).unwrap()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(saved));
        remove(&path);
        assert_eq!(load(&path).unwrap(), None);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn restored_layout_keeps_its_panes() {
        let saved = sample();
        let text = to_json(&saved).unwrap();
        let back: Saved = serde_json::from_str(&text).unwrap();
        assert_eq!(back.workspaces[&1].panes(), vec![0, 1, 2]);
        assert!(back.workspaces[&1].is_floating(2));
        assert_eq!(back.active, 3);
    }

    #[test]
    fn only_known_agents_with_clean_session_ids_resume() {
        let claude = |session: &str| SavedPane {
            agent: Some("claude".into()),
            session: Some(session.into()),
            ..Default::default()
        };
        let commands = crate::config::Config::default().resume_commands();
        assert_eq!(
            claude("9f1c-77ab").resume_command(&commands).as_deref(),
            Some("claude --resume 9f1c-77ab")
        );
        assert_eq!(claude("x; rm -rf ~").resume_command(&commands), None);
        assert_eq!(claude("").resume_command(&commands), None);
        // Agents without session ids continue their last conversation.
        let aider = SavedPane {
            agent: Some("aider".into()),
            ..Default::default()
        };
        assert_eq!(
            aider.resume_command(&commands).as_deref(),
            Some("aider --restore-chat-history")
        );
        let kilo = SavedPane {
            agent: Some("kilo".into()),
            ..Default::default()
        };
        assert_eq!(kilo.resume_command(&commands), None); // pi with the session its extension reported.
        let pi = SavedPane {
            agent: Some("pi".into()),
            session: Some("0b5c9f2e-1111".into()),
            ..Default::default()
        };
        assert_eq!(
            pi.resume_command(&commands).as_deref(),
            Some("pi --session 0b5c9f2e-1111")
        );
    }

    #[test]
    fn hives_collect_sessions_and_are_managed() {
        let dir = std::env::temp_dir().join(format!("imkr-saves-{}", std::process::id()));
        // SAFETY: the only test that sets IMKR_STATE.
        unsafe { std::env::set_var("IMKR_STATE", dir.join("state.json")) };
        keep_in_last("work", sample()).unwrap();
        keep_in_last("play", Saved::new(1, BTreeMap::new(), BTreeMap::new())).unwrap();
        keep_in_last("work", sample()).unwrap();
        let hive = load_hive(LAST).unwrap().unwrap();
        assert_eq!(hive.sessions.keys().collect::<Vec<_>>(), ["play", "work"]);
        assert_eq!(hive.sessions["work"].layout, sample());
        assert_eq!(saves(), ["last"]);
        assert!(save_path("../evil").is_err());

        // Hives are listed with `last` first, renamed and deleted.
        let mut hive = Hive::default();
        hive.sessions.insert(
            "api".into(),
            HiveSession {
                saved_at: 7,
                layout: sample(),
            },
        );
        write_hive("alpha", &hive).unwrap();
        let infos = hive_infos();
        let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["last", "alpha"]);
        assert_eq!(infos[1].sessions, ["api"]);
        assert_eq!(infos[1].saved_at, 7);
        assert!(rename_hive("alpha", "last").is_err());
        rename_hive("alpha", "beta").unwrap();
        delete_hive("beta").unwrap();
        assert!(delete_hive("beta").is_err());
        assert_eq!(saves(), ["last"]);
        unsafe { std::env::remove_var("IMKR_STATE") };
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn scrollback_is_kept_private_and_pruned() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("imkr-scroll-{}", std::process::id()));
        let state = dir.join("work.json");
        write_scrollback(&state, 1, "one\ntwo").unwrap();
        write_scrollback(&state, 2, "other").unwrap();
        assert_eq!(read_scrollback(&state, 1).as_deref(), Some("one\ntwo"));
        let file = scrollback_dir(&state).join("1.txt");
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        prune_scrollback(&state, &[2]);
        assert_eq!(read_scrollback(&state, 1), None);
        assert_eq!(read_scrollback(&state, 2).as_deref(), Some("other"));
        prune_scrollback(&state, &[]);
        assert!(!scrollback_dir(&state).exists());
        let _ = fs::remove_dir_all(dir);
    }
}
