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
    /// Its notepad (`Ctrl+B t`): thoughts kept for its agent, also when the
    /// session ends and comes back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

/// One line on a pane's notepad.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    /// Handed to the pane's agent already; kept, struck through.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sent: bool,
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
    if let Some(socket) = crate::protocol::test_socket() {
        return Ok(socket.with_extension("state.json"));
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
    if let Some(session) = load_hive(LAST)?.and_then(|mut hive| hive.sessions.remove(name)) {
        return write(&path, &to_json(&session.layout)?);
    }
    // Older ones are in the archive; back out of it, it is a session again.
    let entry = unarchive(name)?
        .with_context(|| format!("{name} is not among the closed or archived sessions"))?;
    write(&path, &to_json(&entry.layout)?)
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
        hive.hive_of.remove(name);
        write_hive(LAST, &hive)?;
        found = true;
    }
    let mut list = archived();
    let before = list.len();
    list.retain(|a| a.name != name);
    if list.len() < before {
        write_archive(&list)?;
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
    /// In `last`: the hive each session ran in when it ended, to file it
    /// under that hive in the archive later (#506).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hive_of: BTreeMap<String, String>,
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
    // Not a pane's own IMKR_SOCKET: `imkr hive ls` in a pane looked in
    // `<socket>.saves` and found no hives.
    if let Some(socket) = crate::protocol::test_socket() {
        return Ok(socket.with_extension("saves"));
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
    match hive_for(session) {
        Some(owner) => hive.hive_of.insert(session.to_owned(), owner),
        None => hive.hive_of.remove(session),
    };
    let dropped = keep_newest(&mut hive, LAST_KEEPS);
    // What falls out of `last` goes into the archive, never away (#506).
    let dropped: Vec<Archived> = dropped
        .into_iter()
        .map(|(name, session)| Archived {
            hive: hive.hive_of.remove(&name),
            name,
            saved_at: session.saved_at,
            layout: session.layout,
        })
        .collect();
    if !dropped.is_empty() {
        archive(dropped)?;
    }
    write_hive(LAST, &hive)?;
    Ok(())
}

/// How many ended sessions `last` keeps: the newest. Older ones go into
/// the archive (`archived`); sessions stopped by a crash or reboot are not
/// in `last` and always stay.
pub const LAST_KEEPS: usize = 10;

/// Takes out the sessions saved longest ago until `keep` are left, and
/// returns them.
fn keep_newest(hive: &mut Hive, keep: usize) -> Vec<(String, HiveSession)> {
    let mut dropped = Vec::new();
    while hive.sessions.len() > keep {
        let Some(oldest) = hive
            .sessions
            .iter()
            .min_by_key(|(_, s)| s.saved_at)
            .map(|(name, _)| name.clone())
        else {
            break;
        };
        if let Some(session) = hive.sessions.remove(&oldest) {
            dropped.push((oldest, session));
        }
    }
    dropped
}

/// An ended session older than the ten in `last`, kept with the hive it
/// ran in (#506).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Archived {
    pub name: String,
    /// The hive active on this machine when it ended; none without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive: Option<String>,
    /// When it ended, in seconds since 1970.
    pub saved_at: u64,
    pub layout: Saved,
}

/// The archive file, next to the hives; the dot keeps it out of their list.
fn archive_path() -> Result<PathBuf> {
    Ok(saves_dir()?.join(".archive.json"))
}

/// The archived sessions of this machine, newest first.
pub fn archived() -> Vec<Archived> {
    let mut list: Vec<Archived> = archive_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    list.sort_by_key(|a| std::cmp::Reverse(a.saved_at));
    list
}

fn write_archive(list: &[Archived]) -> Result<()> {
    write(&archive_path()?, &serde_json::to_string_pretty(list)?)
}

/// Files `entries` in the archive; one of the same name and hive is
/// replaced, the newer counts.
fn archive(entries: Vec<Archived>) -> Result<()> {
    let mut list = archived();
    for entry in entries {
        list.retain(|a| !(a.name == entry.name && a.hive == entry.hive));
        list.push(entry);
    }
    list.sort_by_key(|a| std::cmp::Reverse(a.saved_at));
    write_archive(&list)
}

/// Another machine's archive as last fetched from it, kept here so it
/// shows, and can be started here, while that machine is off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Replica {
    /// The machine, as ssh reaches it.
    pub host: String,
    pub sessions: Vec<Archived>,
}

/// Where the archive of `host` is kept here: a dot file next to the hives.
fn replica_path(host: &str) -> Result<PathBuf> {
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(saves_dir()?.join(format!(".archive.{safe}.json")))
}

/// Keeps `sessions`, the archive of `host`, until the next fetch.
pub fn store_replica(host: &str, sessions: Vec<Archived>) -> Result<()> {
    let replica = Replica {
        host: host.to_owned(),
        sessions,
    };
    write(&replica_path(host)?, &serde_json::to_string(&replica)?)
}

/// The archives of the other machines, as last fetched.
pub fn replicas() -> Vec<Replica> {
    let Ok(entries) = saves_dir().and_then(|dir| Ok(fs::read_dir(dir)?)) else {
        return Vec::new();
    };
    let mut list: Vec<Replica> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".archive.") && name.ends_with(".json")
        })
        .filter_map(|e| serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    list.sort_by(|a, b| a.host.cmp(&b.host));
    list
}

/// Takes `name` out of the archive kept of `host`, once it came back or
/// was forgotten there, so it does not show until the next fetch.
pub fn drop_from_replica(host: &str, name: &str) -> Result<()> {
    let Some(mut replica) = replicas().into_iter().find(|r| r.host == host) else {
        return Ok(());
    };
    replica.sessions.retain(|a| a.name != name);
    store_replica(host, replica.sessions)
}

/// Takes out of this machine's archive the entry `name` ended at
/// `saved_at`, and only that one: another machine started it meanwhile.
/// False when it is not there (any more).
pub fn drop_archived(name: &str, saved_at: u64) -> Result<bool> {
    let mut list = archived();
    let before = list.len();
    list.retain(|a| !(a.name == name && a.saved_at == saved_at));
    if list.len() == before {
        return Ok(false);
    }
    write_archive(&list)?;
    Ok(true)
}

/// An archived session of another machine started here while that machine
/// was off; it is to leave that machine's archive once it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Taken {
    pub host: String,
    pub name: String,
    pub saved_at: u64,
}

fn taken_path() -> Result<PathBuf> {
    Ok(saves_dir()?.join(".archive-taken.json"))
}

/// The archived sessions taken from other machines and not yet dropped there.
pub fn taken() -> Vec<Taken> {
    taken_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Remembers that `entry` of `host` was started here.
pub fn note_taken(host: &str, entry: &Archived) -> Result<()> {
    let mut list = taken();
    list.push(Taken {
        host: host.to_owned(),
        name: entry.name.clone(),
        saved_at: entry.saved_at,
    });
    write(&taken_path()?, &serde_json::to_string(&list)?)
}

/// Forgets `done`, dropped on its machine now.
pub fn clear_taken(done: &Taken) -> Result<()> {
    let mut list = taken();
    list.retain(|t| t != done);
    write(&taken_path()?, &serde_json::to_string(&list)?)
}

/// Takes session `name` out of the archive: the one of the active hive,
/// else the newest of that name.
fn unarchive(name: &str) -> Result<Option<Archived>> {
    let mut list = archived();
    let active = active_hive();
    let at = list
        .iter()
        .position(|a| a.name == name && a.hive == active)
        .or_else(|| list.iter().position(|a| a.name == name));
    let Some(at) = at else {
        return Ok(None);
    };
    let entry = list.remove(at);
    write_archive(&list)?;
    Ok(Some(entry))
}

/// The hive session `session` of this machine belongs to as it ends: the
/// active hive if it holds it, else the newest saved hive that does (a
/// switch saves the hive it leaves before ending its sessions, and has made
/// the next one active by then), else the active hive, as the one worked in.
fn hive_for(session: &str) -> Option<String> {
    let active = active_hive();
    let id = crate::protocol::machine_id();
    let holding: Vec<(String, u64)> = saves()
        .into_iter()
        .filter(|name| name != LAST)
        .filter_map(|name| {
            let hive = load_hive(&name).ok()??;
            let here = if hive.machine_id.is_empty() || hive.machine_id == id {
                hive.sessions.get(session)
            } else {
                hive.machines
                    .iter()
                    .find(|m| m.id == id)
                    .and_then(|m| m.sessions.get(session))
            };
            here.map(|s| (name, s.saved_at))
        })
        .collect();
    if let Some(active) = &active
        && holding.iter().any(|(name, _)| name == active)
    {
        return Some(active.clone());
    }
    holding
        .into_iter()
        .max_by_key(|(_, saved_at)| *saved_at)
        .map(|(name, _)| name)
        .or(active)
}

/// The hive the user works in on this machine: set by switching to a hive
/// and by saving one, saved again before the next switch (#507).
pub fn active_hive() -> Option<String> {
    let text = fs::read_to_string(saves_dir().ok()?.join(".active")).ok()?;
    let name = text.trim();
    (crate::protocol::valid_session_name(name) && name != LAST).then(|| name.to_owned())
}

/// Remembers `name` as the active hive, or none.
pub fn set_active_hive(name: Option<&str>) -> Result<()> {
    let path = saves_dir()?.join(".active");
    match name.filter(|&n| n != LAST) {
        Some(name) => write(&path, name),
        None => match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
    }
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
    fs::remove_file(&path).with_context(|| format!("there is no hive called {name}"))?;
    if active_hive().as_deref() == Some(name) {
        set_active_hive(None)?;
    }
    Ok(())
}

/// Renames hive `old` to `name`, never over another hive.
pub fn rename_hive(old: &str, name: &str) -> Result<()> {
    let from = save_path(old)?;
    let to = save_path(name)?;
    if to.exists() {
        anyhow::bail!("there is a hive called {name} already");
    }
    fs::rename(&from, &to).with_context(|| format!("there is no hive called {old}"))?;
    if active_hive().as_deref() == Some(old) {
        set_active_hive(Some(name))?;
    }
    Ok(())
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
            // The archive lives here too, as a dot file.
            (!name.starts_with('.')).then_some(name)
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
                    notes: vec![Note {
                        text: "ask about the login tests".into(),
                        sent: false,
                    }],
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
    fn last_keeps_the_newest_ten() {
        let mut hive = Hive::default();
        for i in 1..=12u64 {
            hive.sessions.insert(
                format!("s{i:02}"),
                HiveSession {
                    saved_at: i,
                    layout: sample(),
                },
            );
        }
        keep_newest(&mut hive, LAST_KEEPS);
        assert_eq!(hive.sessions.len(), 10);
        assert!(!hive.sessions.contains_key("s01") && !hive.sessions.contains_key("s02"));
        assert!(hive.sessions.contains_key("s03") && hive.sessions.contains_key("s12"));
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

        // The active hive follows renames and goes with its hive (#507).
        write_hive("alpha", &Hive::default()).unwrap();
        set_active_hive(Some("alpha")).unwrap();
        assert_eq!(active_hive().as_deref(), Some("alpha"));
        rename_hive("alpha", "kunde").unwrap();
        assert_eq!(active_hive().as_deref(), Some("kunde"));
        set_active_hive(Some(LAST)).unwrap();
        assert_eq!(active_hive(), None, "`last` is never the active hive");
        set_active_hive(Some("kunde")).unwrap();

        // What falls out of `last` goes into the archive with its hive,
        // never away, and comes back from there (#506).
        for i in 0..LAST_KEEPS + 2 {
            keep_in_last(&format!("s{i:02}"), sample()).unwrap();
        }
        let last = load_hive(LAST).unwrap().unwrap();
        assert_eq!(last.sessions.len(), LAST_KEEPS);
        let archived = archived();
        assert_eq!(archived.len(), 4);
        // Nothing is lost: every session is in `last` or in the archive.
        let mut all: Vec<String> = last.sessions.keys().cloned().collect();
        all.extend(archived.iter().map(|a| a.name.clone()));
        all.sort();
        let mut expected: Vec<String> = (0..LAST_KEEPS + 2).map(|i| format!("s{i:02}")).collect();
        expected.extend(["play".to_owned(), "work".to_owned()]);
        expected.sort();
        assert_eq!(all, expected);
        // Each carries the hive it ended in: none before one was active.
        for entry in &archived {
            let hive = entry.name.starts_with('s').then(|| "kunde".to_owned());
            assert_eq!(entry.hive, hive, "{}", entry.name);
        }
        assert_eq!(saves(), ["kunde", "last"], "the archive is no hive");
        let back = archived[0].name.clone();
        let path = path_for(&back).unwrap();
        revive(&back).unwrap();
        assert!(path.exists());
        assert!(!self::archived().iter().any(|a| a.name == back));
        fs::remove_file(&path).unwrap();
        let gone = archived[1].name.clone();
        assert!(forget(&gone).unwrap());
        assert!(!self::archived().iter().any(|a| a.name == gone));
        // Another machine's archive is kept here, apart from this one's.
        let theirs = Archived {
            name: "api".into(),
            hive: Some("kunde".into()),
            saved_at: 5,
            layout: sample(),
        };
        store_replica("coding.machine", vec![theirs.clone()]).unwrap();
        let replicas = replicas();
        assert_eq!(replicas.len(), 1);
        assert_eq!(replicas[0].host, "coding.machine");
        assert_eq!(replicas[0].sessions, std::slice::from_ref(&theirs));
        assert!(!self::archived().iter().any(|a| a.name == "api"));
        assert!(!saves().iter().any(|s| s.starts_with('.')), "{:?}", saves());
        drop_from_replica("coding.machine", "api").unwrap();
        assert!(self::replicas()[0].sessions.is_empty());
        // Started here while that machine was off: noted until it dropped it.
        note_taken("coding.machine", &theirs).unwrap();
        assert_eq!(taken().len(), 1);
        clear_taken(&taken()[0]).unwrap();
        assert!(taken().is_empty());
        // Only the very entry leaves an archive, not one ended later.
        let first = self::archived()[0].clone();
        assert!(!drop_archived(&first.name, first.saved_at + 1).unwrap());
        assert!(drop_archived(&first.name, first.saved_at).unwrap());
        assert!(!self::archived().contains(&first));
        delete_hive("kunde").unwrap();
        assert_eq!(active_hive(), None);
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
