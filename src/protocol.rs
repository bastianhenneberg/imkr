//! The wire protocol between client and server, spoken over a Unix socket.
//!
//! Every message is one frame: a tag byte, the payload length as a 4 byte
//! big-endian integer, then the payload.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use crossterm::event::Event;
use serde::{Deserialize, Serialize};

use crate::agent::AgentState;
use crate::layout::PaneId;

/// Frames larger than this are treated as a broken stream.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Client to server.
#[derive(Debug, PartialEq)]
pub enum ClientMsg {
    /// A terminal event: key, paste, resize, ...
    Event(Event),
    /// Shut the server down, killing all panes.
    KillServer,
    /// Become an attached client, showing the session. A connection that
    /// does not send this only makes requests. With `alone`, the other
    /// attached clients are detached, as `tmux attach -d` does.
    Attach { alone: bool },
    /// A command from the CLI or an agent, answered with `ServerMsg::Reply`.
    Request(Request),
    /// What the client's terminal can do, sent after attaching and after
    /// every resize.
    Caps(Caps),
    /// A window from another session, `upgrade::MovedWindow` as JSON. The
    /// ptys follow right after as descriptors, see `fdpass`; the answer is
    /// a `ServerMsg::Reply`.
    Move(Vec<u8>),
    /// Typed into the pane this connection taps, see `Request::Tap`.
    Input(Vec<u8>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// Shows images sent with the kitty graphics protocol.
    pub graphics: bool,
    /// The size of a cell in pixels, for sizing images.
    pub cell_width: u16,
    pub cell_height: u16,
    /// Follows `ServerMsg::Switch` to `host:session`, i.e. to other
    /// machines. Clients from before do not, they are not sent there.
    #[serde(default)]
    pub hosts: bool,
    /// Follows `ServerMsg::Switch` to `…#pane`, showing that pane there.
    #[serde(default)]
    pub panes: bool,
    /// Follows `ServerMsg::Switch` to `…#new`, opening a window there.
    #[serde(default)]
    pub windows: bool,
    /// Opens URLs it is sent with `ServerMsg::Open`.
    #[serde(default)]
    pub open: bool,
    /// The machine the client runs on, when that is not the server's: where
    /// it can switch back to, with the sessions running there.
    #[serde(default)]
    pub origin: Option<Origin>,
    /// Shown in the middle of the screen for a moment: the session's name,
    /// in the first caps after a switch. A field rather than a request, so
    /// older servers skip it instead of dropping the client.
    #[serde(default)]
    pub banner: Option<String>,
    /// Where `banner` shows, as the client's machine is set up. A field of
    /// its own: servers of v0.21.8 read `banner` as a string.
    #[serde(default)]
    pub banner_at: crate::config::BannerAt,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub host: String,
    pub sessions: Vec<String>,
    /// The same sessions with their agents, sent again every few seconds,
    /// so the sidebar there shows this machine as this one shows it.
    #[serde(default)]
    pub infos: Vec<SessionInfo>,
    /// The other machines the client's own imkr knows, with their
    /// sessions, so every machine shows them all and the client can be
    /// sent there (it connects from its own machine, with its own keys).
    #[serde(default)]
    pub hosts: Vec<HostInfo>,
    /// The name the client reached this server by, e.g. its ssh alias: how
    /// the user calls this machine, which may not be its hostname.
    #[serde(default)]
    pub via: Option<String>,
    /// The plugins of the client's machine, so they are at hand wherever
    /// the user works; they run where they are opened.
    #[serde(default)]
    pub plugins: Vec<crate::plugin::Plugin>,
    /// The session templates of the client's machine, for new sessions
    /// started from wherever the user works.
    #[serde(default)]
    pub templates: Vec<crate::template::Template>,
}

/// A machine with its sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub host: String,
    pub sessions: Vec<SessionInfo>,
}

/// A session as `imkr ls --json` lists it, for the session list and the
/// sidebar, here and from other machines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub panes: usize,
    pub windows: usize,
    /// How many agents are in each state, by state name.
    #[serde(default)]
    pub agents: BTreeMap<String, usize>,
    /// Every agent, for the sidebar of another session.
    #[serde(default)]
    pub agent_panes: Vec<AgentInfo>,
    /// Every pane, agents and shells, for the lists of every window and
    /// pane (`Ctrl+B W`, `Ctrl+B P`); `state` is empty without an agent.
    /// Empty from versions before.
    #[serde(default)]
    pub all_panes: Vec<AgentInfo>,
    /// Not running: `stopped` with the machine or a crash, `closed` on
    /// purpose. Going there brings it back. Versions before list neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dormant: Option<String>,
    /// When it stopped or was closed, seconds since 1970.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dormant_since: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl SessionInfo {
    /// The default session `client::remote_sessions` names for a machine
    /// that runs none: a place to go, started when switched to, but no
    /// session yet. A running server always has a pane.
    pub fn is_stand_in(&self) -> bool {
        self.panes == 0 && self.dormant.is_none()
    }

    /// What a dormant session shows instead of its agents: "stopped 21:57"
    /// or "closed 12.10.".
    pub fn dormant_label(&self) -> Option<String> {
        let kind = self.dormant.as_deref()?;
        let when = chrono_like(self.dormant_since);
        Some(match when {
            Some(when) => format!("{kind} {when}"),
            None => kind.to_owned(),
        })
    }
}

/// `since` (seconds since 1970) as local time: `21:57` today, `12.10.`
/// before. `None` when unknown.
pub fn chrono_like(since: u64) -> Option<String> {
    if since == 0 {
        return None;
    }
    let local = |secs: i64| {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the `tm` it is given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let ok = !unsafe { libc::localtime_r(&t, &mut tm) }.is_null();
        ok.then_some(tm)
    };
    let then = local(since as i64)?;
    let now = local(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64,
    )?;
    if (then.tm_year, then.tm_yday) == (now.tm_year, now.tm_yday) {
        Some(format!("{:02}:{:02}", then.tm_hour, then.tm_min))
    } else {
        Some(format!("{:02}.{:02}.", then.tm_mday, then.tm_mon + 1))
    }
}

/// An agent of a session, as another session's sidebar lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane: PaneId,
    pub program: String,
    pub workspace: u8,
    /// Its window's name, else the name of its directory: what tells
    /// agents apart that are all called `claude`.
    pub dir: String,
    pub state: String,
    /// Seconds in this state.
    #[serde(default)]
    pub since: u64,
    /// Its model and context, e.g. `opus 5.5 · 300k`, if its transcript
    /// is known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meta: String,
    /// For a waiting or finished agent, the bottom of its screen: its
    /// question, or what it said last (`agent::tail`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
}

impl SessionInfo {
    /// How long its agents have been quiet, in seconds: 0 while one works
    /// or waits, `None` without agents.
    pub fn quiet_for(&self) -> Option<u64> {
        if self
            .agent_panes
            .iter()
            .any(|a| a.state == "working" || a.state == "blocked")
        {
            return Some(0);
        }
        self.agent_panes.iter().map(|a| a.since).min()
    }

    /// The state the session shows, and how many agents there are: one
    /// waiting for you first, then one still working, then a finished one,
    /// see `SESSION_ORDER`.
    pub fn urgent(&self) -> Option<(AgentState, usize)> {
        let total = self.agents.values().sum();
        SESSION_ORDER
            .into_iter()
            .find(|state| self.agents.get(state.name()).is_some_and(|&n| n > 0))
            .map(|state| (state, total))
    }

    /// A dot per agent: its state and its window, in `SESSION_ORDER`, then
    /// by window. From a server too old to list its agents, the states only.
    pub fn dots(&self) -> Vec<(AgentState, Option<u8>)> {
        if self.agent_panes.is_empty() {
            return self
                .counts()
                .into_iter()
                .flat_map(|(state, n)| std::iter::repeat_n((state, None), n))
                .collect();
        }
        let mut dots: Vec<(AgentState, Option<u8>)> = self
            .agent_panes
            .iter()
            .filter_map(|a| Some((AgentState::from_name(&a.state)?, Some(a.workspace))))
            .collect();
        sort_dots(&mut dots);
        dots
    }

    /// How many agents are in each state, in `SESSION_ORDER`, leaving out
    /// the states nobody is in.
    pub fn counts(&self) -> Vec<(AgentState, usize)> {
        SESSION_ORDER
            .into_iter()
            .filter_map(|state| {
                let n = *self.agents.get(state.name())?;
                (n > 0).then_some((state, n))
            })
            .collect()
    }
}

/// Agent dots in `SESSION_ORDER`, then by window.
pub fn sort_dots(dots: &mut [(AgentState, Option<u8>)]) {
    let rank = |state: &AgentState| SESSION_ORDER.iter().position(|s| s == state);
    dots.sort_by_key(|(state, window)| (rank(state), *window));
}

/// The order a session's agents count in: waiting for you, then still
/// working, then finished. A session in which one agent still works is not
/// finished because another is (it showed green while the other worked).
pub const SESSION_ORDER: [AgentState; 4] = [
    AgentState::Blocked,
    AgentState::Working,
    AgentState::Done,
    AgentState::Idle,
];

/// This machine's name, as `hostname` prints it, or `$IMKR_HOSTNAME`,
/// which tests use to play several machines on one.
pub fn hostname() -> String {
    if let Ok(name) = std::env::var("IMKR_HOSTNAME")
        && !name.is_empty()
    {
        return name;
    }
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "here".into())
}

/// This machine's name for the user and the other machines: `name` from
/// the settings, e.g. its ssh alias, else its hostname. Two machines can
/// share a hostname (`omarchy` on both), so it only names, `machine_id`
/// tells them apart.
pub fn machine_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        if std::env::var_os("IMKR_HOSTNAME").is_some() {
            return hostname();
        }
        crate::config::Config::load()
            .0
            .name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(hostname)
    })
    .clone()
}

/// What tells this machine apart from every other: `/etc/machine-id`, or
/// `$IMKR_HOSTNAME` where tests play several machines on one.
pub fn machine_id() -> String {
    if let Ok(name) = std::env::var("IMKR_HOSTNAME")
        && !name.is_empty()
    {
        return name;
    }
    fs::read_to_string("/etc/machine-id")
        .map(|id| id.trim().to_owned())
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(hostname)
}

/// `host:session` for a session on another machine, as `ServerMsg::Switch`
/// carries it; a plain name is a session on the same machine. Either may
/// end in `#pane`, the pane to show there.
pub fn split_target(target: &str) -> (Option<&str>, &str) {
    let target = target.split_once('#').map_or(target, |(place, _)| place);
    match target.split_once(':') {
        Some((host, name)) => (Some(host), name),
        None => (None, target),
    }
}

/// The pane a switch target names after `#`, if any.
pub fn target_pane(target: &str) -> Option<PaneId> {
    target.split_once('#')?.1.parse().ok()
}

/// Whether a switch target ends in `#new`: a new window there.
pub fn target_wants_window(target: &str) -> bool {
    target
        .split_once('#')
        .is_some_and(|(_, what)| what == "new")
}

/// Commands for scripts and agents, sent by `imkr status`, `list`,
/// `send` and `new`.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// Set a pane's agent state, or clear it with `None`.
    Status {
        pane: PaneId,
        state: Option<AgentState>,
        /// The agent's own session id, to resume it after a restart.
        #[serde(default)]
        session: Option<String>,
        /// Its transcript, for the model and context shown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<PathBuf>,
    },
    /// All panes.
    List,
    /// Type `text` into a pane, followed by Enter if `enter`.
    Send {
        pane: PaneId,
        text: String,
        enter: bool,
    },
    /// A message for the agent in pane `to` (its number, name or program),
    /// typed in once it is idle, never into a question. `from` is the
    /// sender's address, for the answer.
    Message {
        to: String,
        text: String,
        from: String,
    },
    /// The text on a pane's screen, or its last `lines` lines including the
    /// history.
    Read { pane: PaneId, lines: Option<usize> },
    /// Name a pane or a workspace, or clear its name with `None`.
    Rename {
        target: RenameTarget,
        name: Option<String>,
    },
    /// Puts `text` on a pane's notepad (the focused one without `pane`),
    /// or with `None` only lists it.
    Note {
        pane: Option<PaneId>,
        text: Option<String>,
    },
    /// Rename the session.
    RenameSession { name: String },
    /// Show `text` as a notification, the way the settings say, titled
    /// `title` or else after `pane`.
    Notify {
        text: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Show `text` in the control bar, e.g. why a switch failed.
    Flash { text: String },
    /// Read the plugin files again, after `imkr plugin add` and the like.
    ReloadPlugins,
    /// Read the config and plugins again, after `imkr settings sync`.
    ReloadConfig,
    /// Use account `name` of agent `tool` (see `account`): for the shells
    /// this session starts from now on, or, with `pane`, for the agent in
    /// that pane right away, picking its conversation up again there.
    UseAccount {
        tool: String,
        name: String,
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Send the active client to `target` (`host:session`), e.g. once a
    /// session started from a template on another machine runs.
    Go { target: String },
    /// Show pane `pane` and focus it, e.g. after switching to its session.
    Reveal { pane: PaneId },
    /// Open a new window and ask for its name, as `Ctrl+B n` does, e.g.
    /// after switching to the session it is for.
    NewWindow,
    /// The other machines this server knows, with their sessions, as
    /// `Vec<HostInfo>`.
    Places,
    /// Open the session list, e.g. when `imkr` starts and the settings of
    /// the machine it runs on say so.
    OpenSessions,
    /// Where the user sits who is attached to this session: `machine`,
    /// `via` (the name that machine reaches this one by, if it is another)
    /// and `here`. An error while nobody is attached.
    Operator,
    /// The session's layout as `persist` saves it, for save files.
    Snapshot,
    /// Replace the server with the binary at `exe`, keeping the session,
    /// see `upgrade`.
    Upgrade { exe: PathBuf },
    /// Show or hide the popup `name`, starting `command` (the shell if
    /// empty) in it the first time, titled `title`.
    Popup {
        name: String,
        title: Option<String>,
        command: Vec<String>,
        cwd: Option<PathBuf>,
    },
    /// Move a workspace, the active one without a number, with its running
    /// panes to session `to`, started if needed.
    MoveWindow { workspace: Option<u8>, to: String },
    /// A new window named `name` in git worktree `name` of the repository
    /// `cwd` is in, running `command` (the shell if empty).
    Worktree {
        name: String,
        command: Vec<String>,
        cwd: Option<PathBuf>,
    },
    /// Gives the active window the layout `name` (a preset or a saved
    /// one), or saves its layout as `name` with `save`.
    Layout { name: String, save: bool },
    /// What the shell in pane `pane` told of its commands (OSC 133): how
    /// many finished, whether one runs, the last one's output and exit.
    LastCommand { pane: PaneId },
    /// Why pane `pane` counts as the agent it is, in its state, or as none.
    Explain { pane: PaneId },
    /// Close pane `pane`, ending its program, as `Ctrl+B x` does. Not the
    /// last one: that would end the session.
    Kill { pane: PaneId },
    /// Start `command` (the shell if empty) in a new pane.
    New {
        command: Vec<String>,
        float: bool,
        workspace: Option<u8>,
        cwd: Option<PathBuf>,
    },
    /// Shows pane `pane` on this connection from now on: after the reply,
    /// its history and screen and then its output come as
    /// `ServerMsg::Output`, `ServerMsg::Exited` when it ends. Input goes
    /// back as `ClientMsg::Input`, the size as a resize event.
    /// With `watch` the pane keeps its size and answers its program
    /// itself: it is still shown here (the web view); the connection is
    /// cut off when its size changes, to connect again for a fresh screen.
    Tap {
        pane: PaneId,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        watch: bool,
        /// Rows and columns to give the pane first, so its screen comes
        /// at the size it is shown at (not when watching).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<(u16, u16)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenameTarget {
    Pane(PaneId),
    Workspace(u8),
}

/// The answer to a request: JSON on success, a message on failure.
pub type Reply = std::result::Result<serde_json::Value, String>;

/// Server to client.
#[derive(Debug, PartialEq)]
pub enum ServerMsg {
    /// Bytes to write to the client's terminal as they are.
    Output(Vec<u8>),
    /// The client was detached, the server keeps running.
    Detached,
    /// The server is shutting down.
    Exited,
    /// The answer to a `ClientMsg::Request`.
    Reply(Reply),
    /// Leave this server and attach to the session with this name instead,
    /// starting it if needed.
    Switch(String),
    /// Open this URL on the client's machine, e.g. after a `Ctrl`+click on
    /// a link. Only to clients with `Caps::open`.
    Open(String),
}

impl ClientMsg {
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            ClientMsg::Event(event) => write_frame(w, 1, &serde_json::to_vec(event)?),
            ClientMsg::KillServer => write_frame(w, 2, &[]),
            // The flag rides in the payload, which servers from before it
            // ignore.
            ClientMsg::Attach { alone } => write_frame(w, 3, if *alone { &[1] } else { &[] }),
            ClientMsg::Request(request) => write_frame(w, 4, &serde_json::to_vec(request)?),
            ClientMsg::Caps(caps) => write_frame(w, 6, &serde_json::to_vec(caps)?),
            ClientMsg::Move(json) => write_frame(w, 7, json),
            ClientMsg::Input(bytes) => write_frame(w, 8, bytes),
        }
    }

    /// Reads the next message, `None` at the end of the stream.
    pub fn read_from(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((tag, payload)) = read_frame(r)? else {
            return Ok(None);
        };
        match tag {
            1 => Ok(Some(ClientMsg::Event(serde_json::from_slice(&payload)?))),
            2 => Ok(Some(ClientMsg::KillServer)),
            3 => Ok(Some(ClientMsg::Attach {
                alone: payload.first() == Some(&1),
            })),
            4 => Ok(Some(ClientMsg::Request(serde_json::from_slice(&payload)?))),
            6 => Ok(Some(ClientMsg::Caps(serde_json::from_slice(&payload)?))),
            7 => Ok(Some(ClientMsg::Move(payload))),
            8 => Ok(Some(ClientMsg::Input(payload))),
            _ => Err(invalid(format!("unknown client message {tag}"))),
        }
    }
}

impl ServerMsg {
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            ServerMsg::Output(bytes) => write_frame(w, 1, bytes),
            ServerMsg::Detached => write_frame(w, 2, &[]),
            ServerMsg::Exited => write_frame(w, 3, &[]),
            ServerMsg::Reply(reply) => write_frame(w, 4, &serde_json::to_vec(reply)?),
            ServerMsg::Switch(name) => write_frame(w, 5, name.as_bytes()),
            ServerMsg::Open(url) => write_frame(w, 6, url.as_bytes()),
        }
    }

    /// Reads the next message, `None` at the end of the stream.
    pub fn read_from(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((tag, payload)) = read_frame(r)? else {
            return Ok(None);
        };
        match tag {
            1 => Ok(Some(ServerMsg::Output(payload))),
            2 => Ok(Some(ServerMsg::Detached)),
            3 => Ok(Some(ServerMsg::Exited)),
            4 => Ok(Some(ServerMsg::Reply(serde_json::from_slice(&payload)?))),
            5 => Ok(Some(ServerMsg::Switch(
                String::from_utf8(payload).map_err(|e| invalid(e.to_string()))?,
            ))),
            6 => Ok(Some(ServerMsg::Open(
                String::from_utf8(payload).map_err(|e| invalid(e.to_string()))?,
            ))),
            _ => Err(invalid(format!("unknown server message {tag}"))),
        }
    }
}

fn write_frame(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| invalid("frame too large".into()))?;
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(tag);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(payload);
    w.write_all(&frame)?;
    w.flush()
}

fn read_frame(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 5];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME {
        return Err(invalid(format!("frame of {len} bytes is too large")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((header[0], payload)))
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The session this process belongs to: `$IMKR_SESSION`, set by
/// `imkr -s NAME`, or `default`.
/// `IMKR_SOCKET` when it names a socket of its own outside the runtime
/// directory: a test world, with its state and saves next to it. Inside an
/// imkr pane it names the pane's own session in the runtime directory, and
/// that one keeps its files where every session does.
pub fn test_socket() -> Option<PathBuf> {
    std::env::var_os("IMKR_SOCKET")
        .map(PathBuf::from)
        .filter(|path| path.parent() != runtime_dir().ok().as_deref())
}

pub fn session_name() -> String {
    if let Some(name) = RENAMED.lock().unwrap().clone() {
        return name;
    }
    std::env::var("IMKR_SESSION")
        .ok()
        .filter(|name| valid_session_name(name))
        .unwrap_or_else(|| DEFAULT_SESSION.to_owned())
}

pub const DEFAULT_SESSION: &str = "default";

/// The session's name after the server was renamed, which the environment
/// it started with does not know.
static RENAMED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Renames this process's session, see `App::rename_session`.
pub fn set_session_name(name: &str) {
    *RENAMED.lock().unwrap() = Some(name.to_owned());
}

/// Session names become file names: letters, digits, `-` and `_`.
pub fn valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Where the sockets live: `$XDG_RUNTIME_DIR/imkr`, falling back to
/// `/tmp/imkr-<uid>`. Created readable only by the current user.
pub fn runtime_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("imkr"),
        // SAFETY: getuid has no preconditions and cannot fail.
        None => PathBuf::from(format!("/tmp/imkr-{}", unsafe { libc::getuid() })),
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// The longest socket path, in bytes: `sun_path` holds 108 on Linux and
/// 104 on macOS, the closing null byte included.
#[cfg(target_os = "macos")]
pub const MAX_SOCKET_PATH: usize = 103;
#[cfg(not(target_os = "macos"))]
pub const MAX_SOCKET_PATH: usize = 107;

/// `path` if a socket can live there, else an error that says why and how
/// to get round it, instead of the system's "shorter than SUN_LEN".
pub fn fits_socket(path: PathBuf) -> Result<PathBuf> {
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        bail!(
            "socket path is {len} bytes, the limit is {MAX_SOCKET_PATH}: {}\n\
             set IMKR_SOCKET (or XDG_RUNTIME_DIR) to something shorter, e.g. /tmp/imkr.sock",
            path.display()
        );
    }
    Ok(path)
}

/// The socket of session `name`.
pub fn session_socket(name: &str) -> Result<PathBuf> {
    fits_socket(runtime_dir()?.join(format!("{name}.sock")))
}

/// The server socket: `$IMKR_SOCKET` if set, otherwise the socket of this
/// process's session in the runtime directory.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_SOCKET") {
        return fits_socket(PathBuf::from(path));
    }
    session_socket(&session_name())
}

/// The sessions with a server running, by name, sorted.
pub fn sessions() -> Vec<String> {
    let Ok(dir) = runtime_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        // Links are old names of renamed sessions.
        .filter(|entry| entry.file_type().is_ok_and(|t| !t.is_symlink()))
        .filter_map(|entry| {
            let path = entry.path();
            let name = path
                .file_name()?
                .to_str()?
                .strip_suffix(".sock")?
                .to_owned();
            // A socket without a server behind it is left over from a crash.
            std::os::unix::net::UnixStream::connect(&path).ok()?;
            Some(name)
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_session_with_one_agent_working_is_not_finished() {
        let mut info = SessionInfo::default();
        info.agents.insert("done".into(), 1);
        info.agents.insert("working".into(), 1);
        assert_eq!(info.urgent(), Some((AgentState::Working, 2)));
        assert_eq!(
            info.counts(),
            [(AgentState::Working, 1), (AgentState::Done, 1)]
        );
        info.agents.insert("blocked".into(), 1);
        assert_eq!(info.urgent(), Some((AgentState::Blocked, 3)));
    }

    use super::*;

    #[test]
    fn long_socket_paths_say_how_long() {
        let short = PathBuf::from("/tmp/imkr.sock");
        assert_eq!(fits_socket(short.clone()).unwrap(), short);
        let edge = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH - 1)));
        assert!(fits_socket(edge).is_ok());
        let long = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH)));
        let error = fits_socket(long).unwrap_err().to_string();
        assert!(
            error.contains(&format!("{} bytes", MAX_SOCKET_PATH + 1)),
            "{error}"
        );
        assert!(error.contains("IMKR_SOCKET"), "{error}");
    }
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn client_messages_round_trip() {
        let msgs = [
            ClientMsg::Event(Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
            ))),
            ClientMsg::Event(Event::Resize(120, 40)),
            ClientMsg::Event(Event::Paste("hallo\nwelt".into())),
            ClientMsg::KillServer,
            ClientMsg::Attach { alone: false },
            ClientMsg::Attach { alone: true },
            ClientMsg::Caps(Caps {
                graphics: true,
                cell_width: 9,
                cell_height: 19,
                hosts: true,
                panes: true,
                windows: true,
                open: true,
                origin: Some(Origin {
                    host: "omarchy".into(),
                    sessions: vec!["default".into()],
                    infos: Vec::new(),
                    hosts: Vec::new(),
                    via: Some("codingmachine".into()),
                    plugins: Vec::new(),
                    templates: Vec::new(),
                }),
                banner: Some("work".into()),
                banner_at: crate::config::BannerAt::Top,
            }),
            ClientMsg::Request(Request::Status {
                pane: 3,
                state: Some(AgentState::Blocked),
                session: Some("abc".into()),
                transcript: Some("/tmp/t.jsonl".into()),
            }),
            ClientMsg::Request(Request::Notify {
                text: "build finished".into(),
                title: None,
                pane: Some(2),
            }),
            ClientMsg::Request(Request::New {
                command: vec!["claude".into(), "--resume".into()],
                float: true,
                workspace: Some(2),
                cwd: None,
            }),
            ClientMsg::Request(Request::Tap {
                pane: 4,
                watch: false,
                size: None,
            }),
            ClientMsg::Request(Request::Tap {
                pane: 4,
                watch: false,
                size: Some((40, 50)),
            }),
            ClientMsg::Request(Request::Tap {
                pane: 4,
                watch: true,
                size: None,
            }),
            ClientMsg::Input(b"\x1b[A\r".to_vec()),
        ];
        let mut wire = Vec::new();
        for msg in &msgs {
            msg.write_to(&mut wire).unwrap();
        }
        let mut r = wire.as_slice();
        for msg in msgs {
            assert_eq!(ClientMsg::read_from(&mut r).unwrap(), Some(msg));
        }
        assert_eq!(ClientMsg::read_from(&mut r).unwrap(), None);
    }

    #[test]
    fn server_messages_round_trip() {
        let msgs = [
            ServerMsg::Output(b"\x1b[2J\x1b[Hhi".to_vec()),
            ServerMsg::Output(Vec::new()),
            ServerMsg::Detached,
            ServerMsg::Exited,
            ServerMsg::Reply(Ok(serde_json::json!({"pane": 4}))),
            ServerMsg::Reply(Err("no pane 9".into())),
            ServerMsg::Switch("work".into()),
            ServerMsg::Open("https://example.com".into()),
        ];
        let mut wire = Vec::new();
        for msg in &msgs {
            msg.write_to(&mut wire).unwrap();
        }
        let mut r = wire.as_slice();
        for msg in msgs {
            assert_eq!(ServerMsg::read_from(&mut r).unwrap(), Some(msg));
        }
        assert_eq!(ServerMsg::read_from(&mut r).unwrap(), None);
    }

    #[test]
    fn session_names_are_safe_file_names() {
        assert!(valid_session_name("work"));
        assert!(valid_session_name("side-project_2"));
        assert!(!valid_session_name(""));
        assert!(!valid_session_name("../x"));
        assert!(!valid_session_name("a b"));
    }

    #[test]
    fn truncated_frame_is_an_error_not_a_clean_end() {
        let mut wire = Vec::new();
        ServerMsg::Output(b"hello".to_vec())
            .write_to(&mut wire)
            .unwrap();
        wire.truncate(7);
        assert!(ServerMsg::read_from(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let wire = [1u8, 0xff, 0xff, 0xff, 0xff];
        assert!(ServerMsg::read_from(&mut wire.as_slice()).is_err());
    }
}
