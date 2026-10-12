//! The wire protocol between client and server, spoken over a Unix socket.
//!
//! Every message is one frame: a tag byte, the payload length as a 4 byte
//! big-endian integer, then the payload.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result};
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
    /// The machine the client runs on, when that is not the server's: where
    /// it can switch back to, with the sessions running there.
    #[serde(default)]
    pub origin: Option<Origin>,
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
}

/// An agent of a session, as another session's sidebar lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane: PaneId,
    pub program: String,
    pub workspace: u8,
    /// The name of its directory.
    pub dir: String,
    pub state: String,
    /// Seconds in this state.
    #[serde(default)]
    pub since: u64,
}

impl SessionInfo {
    /// The state that needs attention most among its agents, and how many
    /// agents there are.
    pub fn urgent(&self) -> Option<(AgentState, usize)> {
        let total = self.agents.values().sum();
        [
            AgentState::Blocked,
            AgentState::Done,
            AgentState::Working,
            AgentState::Idle,
        ]
        .into_iter()
        .find(|state| self.agents.get(state.name()).is_some_and(|&n| n > 0))
        .map(|state| (state, total))
    }
}

/// This machine's name, as `hostname` prints it.
pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "here".into())
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
    },
    /// All panes.
    List,
    /// Type `text` into a pane, followed by Enter if `enter`.
    Send {
        pane: PaneId,
        text: String,
        enter: bool,
    },
    /// The text on a pane's screen, or its last `lines` lines including the
    /// history.
    Read { pane: PaneId, lines: Option<usize> },
    /// Name a pane or a workspace, or clear its name with `None`.
    Rename {
        target: RenameTarget,
        name: Option<String>,
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
    /// Show pane `pane` and focus it, e.g. after switching to its session.
    Reveal { pane: PaneId },
    /// The other machines this server knows, with their sessions, as
    /// `Vec<HostInfo>`.
    Places,
    /// Open the session list, e.g. when `imkr` starts and the settings of
    /// the machine it runs on say so.
    OpenSessions,
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
    /// Start `command` (the shell if empty) in a new pane.
    New {
        command: Vec<String>,
        float: bool,
        workspace: Option<u8>,
        cwd: Option<PathBuf>,
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

/// The socket of session `name`.
pub fn session_socket(name: &str) -> Result<PathBuf> {
    Ok(runtime_dir()?.join(format!("{name}.sock")))
}

/// The server socket: `$IMKR_SOCKET` if set, otherwise the socket of this
/// process's session in the runtime directory.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_SOCKET") {
        return Ok(PathBuf::from(path));
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
    use super::*;
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
                origin: Some(Origin {
                    host: "omarchy".into(),
                    sessions: vec!["default".into()],
                    infos: Vec::new(),
                    hosts: Vec::new(),
                    via: Some("codingmachine".into()),
                }),
            }),
            ClientMsg::Request(Request::Status {
                pane: 3,
                state: Some(AgentState::Blocked),
                session: Some("abc".into()),
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
