//! The client: puts the terminal into raw mode, forwards every terminal
//! event to the server and writes whatever the server renders to stdout.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write, stdout};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use crossterm::execute;
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};

use crate::protocol::{
    Caps, ClientMsg, Origin, Request, ServerMsg, SessionInfo, session_socket, socket_path,
};

/// How a client session ended.
enum Outcome {
    Detached,
    Exited,
    ConnectionLost,
    /// The server sent the client on to another session.
    Switch(String),
}

/// Where the input thread sends terminal events: the connection of the
/// session shown now. Swapped when switching sessions.
type Writer = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// The way to a server: its socket here, or `imkr proxy` on another
/// machine, reached through ssh.
struct Conn {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// The ssh process, ended with the connection.
    ssh: Option<Child>,
}

impl Conn {
    fn local(stream: UnixStream) -> Result<Self> {
        Ok(Conn {
            writer: Box::new(stream.try_clone()?),
            reader: Box::new(stream),
            ssh: None,
        })
    }

    /// Runs `imkr proxy` on `host` through ssh, for session `session` or
    /// the default one there, started if needed with `start`. Asks for a
    /// password or host key on the terminal if ssh must.
    fn remote(host: &str, session: Option<&str>, start: bool) -> Result<Self> {
        let mut args = Vec::new();
        if let Some(name) = session {
            args.extend(["-s", name]);
        }
        args.push("proxy");
        if start {
            args.push("--start");
        }
        let mut ssh = Command::new("ssh")
            .args(["-C", "-T", "-o", "ServerAliveInterval=15", "--", host])
            .arg(remote_command(&args))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not run ssh")?;
        let mut reader = ssh.stdout.take().expect("piped");
        let writer = ssh.stdin.take().expect("piped");
        let mut errors = ssh.stderr.take().expect("piped");
        // The proxy says it reached the server before the protocol starts,
        // so a failure comes with ssh's or imkr's own message.
        let mut ready = [0u8; 1];
        if reader.read_exact(&mut ready).is_err() || ready[0] != PROXY_READY {
            // The login shell's "command not found".
            if ssh.wait().is_ok_and(|status| status.code() == Some(127)) {
                return Err(NotInstalled(host.to_owned()).into());
            }
            let mut message = String::new();
            let _ = errors.read_to_string(&mut message);
            let message = message.trim();
            if message.is_empty() {
                bail!("could not reach imkr on {host}");
            }
            bail!("could not reach imkr on {host}: {message}");
        }
        remember_host(host);
        // Read on, or ssh blocks once the pipe is full. Nothing of it is
        // shown: the terminal belongs to imkr now.
        thread::spawn(move || io::copy(&mut errors, &mut io::sink()));
        Ok(Conn {
            reader: Box::new(reader),
            writer: Box::new(writer),
            ssh: Some(ssh),
        })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(ssh) = &mut self.ssh {
            let _ = ssh.kill();
            let _ = ssh.wait();
        }
    }
}

/// A machine in the user's Tailscale network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailnetPeer {
    /// Its MagicDNS name, which ssh reaches it by.
    pub name: String,
    pub online: bool,
    pub os: String,
}

/// The other machines of the Tailscale network this one is in, from
/// `tailscale status --json`; empty without Tailscale.
pub fn tailnet_peers() -> Vec<TailnetPeer> {
    let Ok(output) = Command::new("tailscale")
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    parse_tailnet(&status)
}

fn parse_tailnet(status: &serde_json::Value) -> Vec<TailnetPeer> {
    let mut peers: Vec<TailnetPeer> = status["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|peers| peers.values())
        .filter_map(|peer| {
            let name = peer["DNSName"]
                .as_str()
                .and_then(|dns| dns.split('.').next())
                .filter(|n| !n.is_empty())
                .or(peer["HostName"].as_str())?
                .to_lowercase();
            Some(TailnetPeer {
                name,
                online: peer["Online"].as_bool().unwrap_or(false),
                os: peer["OS"].as_str().unwrap_or_default().to_owned(),
            })
        })
        // Phones and the like run no imkr.
        .filter(|peer| matches!(peer.os.as_str(), "linux" | "macOS" | "freebsd" | ""))
        .collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

/// imkr is not installed on this host.
#[derive(Debug)]
pub struct NotInstalled(pub String);

impl std::fmt::Display for NotInstalled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "imkr is not installed on {}, `imkr hosts sync` installs it",
            self.0
        )
    }
}

impl std::error::Error for NotInstalled {}

/// For `error` from reaching `host`: if imkr is missing there, asks on the
/// terminal whether to install it, and does. Any other error stays.
fn offer_install(host: &str, error: anyhow::Error) -> Result<()> {
    use std::io::IsTerminal;
    if error.downcast_ref::<NotInstalled>().is_none() || !io::stdin().is_terminal() {
        return Err(error);
    }
    eprint!("imkr is not installed on {host}. Install the latest release there? [Y/n] ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "" | "y" | "Y" | "yes" | "j" | "ja") {
        return Err(error);
    }
    install_on(host, true)
}

/// The install script of the releases, run by a shell on another machine.
const INSTALL: &str = "u=https://raw.githubusercontent.com/bastianhenneberg/imkr/main/install.sh; \
     (curl -fsSL $u 2>/dev/null || wget -qO- $u) | sh";

/// Installs the latest release on `host` through ssh (into `~/.local/bin`),
/// its output shown if `show`, else only in the error.
pub fn install_on(host: &str, show: bool) -> Result<()> {
    let mut ssh = Command::new("ssh");
    ssh.args(["-T", "-o", "ConnectTimeout=10", "--", host])
        .arg(format!("exec \"${{SHELL:-sh}}\" -lc '{INSTALL}'"))
        .stdin(Stdio::null());
    if show {
        if !ssh.status().context("could not run ssh")?.success() {
            bail!("installing imkr on {host} failed");
        }
        return Ok(());
    }
    let output = ssh.output().context("could not run ssh")?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("installing failed: {}", why.trim());
    }
    Ok(())
}

/// Whether `host` has imkr, without prompting; an error if it cannot be
/// reached.
pub fn installed_on(host: &str) -> Result<bool> {
    let status = Command::new("ssh")
        .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "--", host])
        .arg("exec \"${SHELL:-sh}\" -lc 'PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH; command -v imkr'")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run ssh")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(255) | None => bail!("not reachable"),
        Some(_) => Ok(false),
    }
}

/// Written by `imkr proxy` once it is connected to the server.
const PROXY_READY: u8 = 0x06;

/// The shell command that runs `imkr args...` on another machine through
/// ssh: in a login shell for the profile's PATH, with ~/.cargo/bin and
/// ~/.local/bin added, which are often only on the PATH of interactive
/// shells. `args` are session names and fixed words, nothing to quote.
pub fn remote_command(args: &[&str]) -> String {
    let command = format!(
        "PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr {}",
        args.join(" ")
    );
    format!("exec \"${{SHELL:-sh}}\" -lc {}", sh_quote(&command))
}

/// `word` as one word for sh, quoted when needed.
pub fn sh_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The machine the client shows a session of, `None` for this one. Read by
/// `caps`, which tells a server on another machine where the client is.
static SHOWN_HOST: Mutex<Option<String>> = Mutex::new(None);
/// The session shown, for coming back to it after `reexec_if_replaced`.
static SHOWN_SESSION: Mutex<String> = Mutex::new(String::new());

/// Set for a client that started itself again after its binary was
/// replaced: the place it showed, `host:session` or `session`.
const REEXEC: &str = "IMKR_REEXEC";

/// If `imkr` was replaced on disk since this client started, e.g. by
/// `cargo install` or `imkr upgrade`, becomes the new one, which comes
/// back to the same place. Clients that kept running with old code sent
/// less than servers expect, so the sidebar on another machine came out
/// poorer until the user attached again by hand.
fn reexec_if_replaced(exe: &std::path::Path) {
    let replaced = std::fs::read_link("/proc/self/exe")
        .is_ok_and(|now| now.to_string_lossy().ends_with(" (deleted)"));
    if !replaced || !exe.is_file() {
        return;
    }
    let target = match SHOWN_HOST.lock().unwrap().clone() {
        Some(host) => format!("{host}:{}", SHOWN_SESSION.lock().unwrap()),
        None => SHOWN_SESSION.lock().unwrap().clone(),
    };
    // The new client turns raw mode on again from the terminal as it was.
    let _ = disable_raw_mode();
    let error = Command::new(exe).env(REEXEC, target).exec();
    // Still here: the new binary would not start. Go on with this one.
    let _ = enable_raw_mode();
    eprintln!("imkr: could not restart as the updated binary: {error}");
}

/// For a client started by `reexec_if_replaced`: where it is to go back to.
pub fn reexec_target() -> Option<Remote> {
    let target = std::env::var(REEXEC).ok()?;
    // SAFETY: called first in main, before any thread starts.
    unsafe { std::env::remove_var(REEXEC) };
    let (host, name) = crate::protocol::split_target(&target);
    match host {
        Some(host) => Some(Remote {
            host: host.to_owned(),
            session: Some(name.to_owned()),
        }),
        None => {
            // SAFETY: as above.
            unsafe { std::env::set_var("IMKR_SESSION", name) };
            None
        }
    }
}

/// The file of hosts `imkr --host` reached, one per line.
fn hosts_file() -> Result<std::path::PathBuf> {
    Ok(crate::persist::state_dir()?.join("hosts"))
}

/// The file naming the session used last, as `host:name` or `name`.
fn last_file() -> Result<std::path::PathBuf> {
    Ok(crate::persist::state_dir()?.join("last-session"))
}

/// Notes where the client is, for the next plain `imkr`.
fn remember_place(host: Option<&str>, name: &str) {
    let target = match host {
        Some(host) => format!("{host}:{name}"),
        None => name.to_owned(),
    };
    if let Ok(path) = last_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, target);
    }
}

/// Where the client was last: the host (`None`: this machine) and session.
fn last_place() -> Option<(Option<String>, String)> {
    let text = std::fs::read_to_string(last_file().ok()?).ok()?;
    let (host, name) = crate::protocol::split_target(text.trim());
    if !crate::protocol::valid_session_name(name) {
        return None;
    }
    let host = host.filter(|h| *h != crate::protocol::machine_name());
    Some((host.map(str::to_owned), name.to_owned()))
}

/// Notes `host` for the session list, once it was reached.
pub fn remember_host(host: &str) {
    let mut hosts = remembered_hosts();
    if hosts.iter().any(|h| h == host) {
        return;
    }
    hosts.push(host.to_owned());
    if let Ok(path) = hosts_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, hosts.join("\n") + "\n");
    }
}

/// Forgets `host` again. False if it was not remembered.
pub fn forget_host(host: &str) -> bool {
    let mut hosts = remembered_hosts();
    let before = hosts.len();
    hosts.retain(|h| h != host);
    if hosts.len() == before {
        return false;
    }
    if let Ok(path) = hosts_file() {
        let text = if hosts.is_empty() {
            String::new()
        } else {
            hosts.join("\n") + "\n"
        };
        let _ = std::fs::write(path, text);
    }
    true
}

/// The hosts `imkr --host` reached before.
pub fn remembered_hosts() -> Vec<String> {
    hosts_file()
        .and_then(|path| Ok(std::fs::read_to_string(path)?))
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The sessions on this machine: those running, then those stopped or
/// closed that can come back (`SessionInfo::dormant`).
pub fn local_sessions() -> Vec<SessionInfo> {
    let mut sessions: Vec<SessionInfo> = crate::protocol::sessions()
        .into_iter()
        .map(|name| {
            let mut info = SessionInfo {
                name: name.clone(),
                ..SessionInfo::default()
            };
            let reply = session_socket(&name).and_then(|path| request_at(&path, Request::List));
            if let Ok(panes) = reply {
                let panes = panes.as_array().cloned().unwrap_or_default();
                let mut windows: Vec<u64> = panes
                    .iter()
                    .filter_map(|p| p["workspace"].as_u64())
                    .collect();
                windows.sort_unstable();
                windows.dedup();
                info.panes = panes.len();
                info.windows = windows.len();
                for pane in &panes {
                    let state = pane["state"].as_str();
                    if let Some(state) = state {
                        *info.agents.entry(state.to_owned()).or_default() += 1;
                    }
                    let dir = pane["workspace_name"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            let cwd = pane["cwd"].as_str()?;
                            let name = std::path::Path::new(cwd).file_name()?;
                            Some(name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default();
                    let entry = crate::protocol::AgentInfo {
                        pane: pane["pane"].as_u64().unwrap_or(0) as crate::layout::PaneId,
                        program: pane["name"]
                            .as_str()
                            .or(pane["program"].as_str())
                            .unwrap_or(if state.is_some() { "agent" } else { "shell" })
                            .to_owned(),
                        workspace: pane["workspace"].as_u64().unwrap_or(0) as u8,
                        dir,
                        state: state.unwrap_or_default().to_owned(),
                        since: pane["since"].as_u64().unwrap_or(0),
                        tail: pane["tail"].as_str().unwrap_or_default().to_owned(),
                        meta: pane["meta"].as_str().unwrap_or_default().to_owned(),
                    };
                    if state.is_some() {
                        info.agent_panes.push(entry.clone());
                    }
                    info.all_panes.push(entry);
                }
            }
            info
        })
        .collect();
    // Those that do not run but can come back, after them.
    let running: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
    for dormant in crate::persist::dormant_sessions(&running) {
        sessions.push(SessionInfo {
            name: dormant.name,
            panes: dormant.layout.panes.len(),
            windows: dormant.layout.workspaces.len(),
            dormant: Some(if dormant.closed { "closed" } else { "stopped" }.to_owned()),
            dormant_since: dormant.since,
            ..SessionInfo::default()
        });
    }
    sessions
}

/// `names`, or the default session if none runs: there is always one to
/// go to, started when switched to.
fn or_default(names: Vec<String>) -> Vec<String> {
    if names.is_empty() {
        vec![crate::protocol::DEFAULT_SESSION.to_owned()]
    } else {
        names
    }
}

/// The sessions running on `host`, asked through ssh without prompting.
pub fn remote_sessions(host: &str) -> Result<Vec<SessionInfo>> {
    let output = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(&["ls", "--json"]))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not run ssh")?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.lines().last().unwrap_or("").trim();
        bail!("{host}: {message}");
    }
    let sessions: Vec<SessionInfo> = serde_json::from_slice(&output.stdout)?;
    if sessions.is_empty() {
        return Ok(vec![SessionInfo {
            name: crate::protocol::DEFAULT_SESSION.to_owned(),
            ..SessionInfo::default()
        }]);
    }
    Ok(sessions)
}

/// A session on another machine: its host for ssh, and the session's name
/// there if not the default one.
pub struct Remote {
    pub host: String,
    pub session: Option<String>,
}

/// Attaches to the running server, here or on the `remote` machine. With
/// `start`, starts one first if there is none. With `alone`, other
/// terminals attached to it are detached. With `last`, i.e. plain `imkr`,
/// it goes to the session used last, wherever it is, and opens the session
/// list there if this machine's settings say so.
pub fn run(start: bool, alone: bool, remote: Option<Remote>, last: bool) -> Result<()> {
    let mut remote = remote;
    let mut flash = None;
    if start && crate::config::Config::load().0.revive_sessions {
        revive_stopped();
    }
    if last && let Some((last_host, last_name)) = last_place() {
        match last_host {
            Some(host) => {
                remote = Some(Remote {
                    host,
                    session: Some(last_name),
                })
            }
            // SAFETY: nothing else runs yet, no thread reads the environment.
            None => unsafe { std::env::set_var("IMKR_SESSION", last_name) },
        }
    }
    // Where the client is now: the host (`None` here) and the session.
    let mut host = remote.as_ref().map(|r| r.host.clone());
    let mut name = remote
        .as_ref()
        .and_then(|r| r.session.clone())
        .unwrap_or_else(crate::protocol::session_name);
    *SHOWN_HOST.lock().unwrap() = host.clone();
    *SHOWN_SESSION.lock().unwrap() = name.clone();
    let exe = std::env::current_exe().ok();
    let conn = match &remote {
        Some(r) => match Conn::remote(&r.host, r.session.as_deref(), start).or_else(|e| {
            offer_install(&r.host, e)?;
            Conn::remote(&r.host, r.session.as_deref(), start)
        }) {
            Ok(conn) => conn,
            // The last session was elsewhere and is out of reach: here then.
            Err(e) if last => {
                flash = Some(format!("{e:#}"));
                (host, name) = (None, crate::protocol::DEFAULT_SESSION.to_owned());
                *SHOWN_HOST.lock().unwrap() = None;
                connect(None, &name)?
            }
            Err(e) => return Err(e),
        },
        None => {
            let path = socket_path()?;
            let stream = match UnixStream::connect(&path) {
                Ok(stream) => stream,
                Err(_) if start => start_server(&path, None)?,
                Err(_) => bail!("no imkr server running"),
            };
            Conn::local(stream)?
        }
    };
    remember_place(host.as_deref(), &name);

    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        Clear(ClearType::All)
    )?;

    // One input thread for the whole run, writing to whichever session is
    // shown. It stays blocked in `event::read` at the end, which is fine
    // since the process exits right after.
    let writer: Writer = Arc::new(Mutex::new(None));
    let input = Arc::clone(&writer);
    thread::Builder::new().name("input".into()).spawn(move || {
        while let Ok(event) = event::read() {
            if let Some(stream) = input.lock().unwrap().as_mut() {
                let resized = matches!(event, Event::Resize(..));
                let _ = ClientMsg::Event(event).write_to(stream);
                // A new size can mean a new cell size, e.g. after zooming.
                if resized {
                    let _ = ClientMsg::Caps(caps()).write_to(stream);
                }
            }
        }
    })?;

    // On another machine, its server learns every few seconds what runs
    // here, for its sidebar.
    let origin = Arc::clone(&writer);
    thread::Builder::new()
        .name("origin".into())
        .spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(3));
                if let Some(exe) = &exe {
                    reexec_if_replaced(exe);
                }
                if SHOWN_HOST.lock().unwrap().is_none() {
                    continue;
                }
                let caps = caps();
                if let Some(stream) = origin.lock().unwrap().as_mut() {
                    let _ = ClientMsg::Caps(caps).write_to(stream);
                }
            }
        })?;

    let mut conn = conn;
    // The setting of this machine, wherever the session is.
    let pick = last && crate::config::Config::load().0.sessions_on_start;
    let mut hello = last.then_some((pick, flash));
    let mut reveal = None;
    let mut window = false;
    let outcome = loop {
        match session(
            conn,
            &writer,
            alone,
            hello.take(),
            reveal.take(),
            std::mem::take(&mut window),
        ) {
            // Another session, on this machine, the one shown or another.
            Ok(Outcome::Switch(target)) => {
                reveal = crate::protocol::target_pane(&target);
                window = crate::protocol::target_wants_window(&target);
                let (to_host, to_name) = crate::protocol::split_target(&target);
                let to_host = match to_host {
                    Some(h) if h == crate::protocol::machine_name() => None,
                    Some(h) => Some(h.to_owned()),
                    None => host.clone(),
                };
                let to_name = to_name.to_owned();
                *SHOWN_HOST.lock().unwrap() = to_host.clone();
                conn = match connect(to_host.as_deref(), &to_name) {
                    Ok(next) => {
                        (host, name) = (to_host, to_name);
                        remember_place(host.as_deref(), &name);
                        *SHOWN_SESSION.lock().unwrap() = name.clone();
                        next
                    }
                    // Back to where it was, which then tells why.
                    Err(e) => {
                        reveal = None;
                        window = false;
                        *SHOWN_HOST.lock().unwrap() = host.clone();
                        match connect(host.as_deref(), &name) {
                            Ok(back) => {
                                let mut back = back;
                                let why = format!("not switched: {e:#}");
                                let _ = ClientMsg::Request(Request::Flash { text: why })
                                    .write_to(&mut back.writer);
                                back
                            }
                            Err(_) => break Err(e),
                        }
                    }
                };
                execute!(stdout(), Clear(ClearType::All))?;
            }
            other => break other,
        }
    };

    let _ = execute!(
        stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();

    match outcome? {
        Outcome::Detached => println!("[detached]"),
        Outcome::Exited => println!("[exited]"),
        Outcome::ConnectionLost => println!("[lost connection to server]"),
        Outcome::Switch(_) => {}
    }
    Ok(())
}

/// What this terminal can do. Images only where the kitty graphics protocol
/// is known to work: kitty, Ghostty and WezTerm, and never inside tmux,
/// which does not pass it on. `IMKR_GRAPHICS=1` or `0` decides instead.
fn caps() -> Caps {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let graphics = match env("IMKR_GRAPHICS").as_str() {
        "1" | "on" | "true" => true,
        "0" | "off" | "false" => false,
        _ => {
            std::env::var_os("TMUX").is_none()
                && (env("TERM").contains("kitty")
                    || env("TERM").contains("ghostty")
                    || ["WezTerm", "ghostty"].contains(&env("TERM_PROGRAM").as_str()))
        }
    };
    // Terminals that do not say their pixel size get a common one.
    let (cell_width, cell_height) = match terminal::window_size() {
        Ok(size) if size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0 => {
            (size.width / size.columns, size.height / size.rows)
        }
        _ => (10, 20),
    };
    // On another machine's server, the client says where it is, so it can
    // be sent back.
    let via = SHOWN_HOST.lock().unwrap().clone();
    let origin = via.is_some().then(|| {
        let infos = local_sessions();
        // What this machine's imkr knows of the others: any of its
        // sessions knows the same.
        let hosts = infos
            .iter()
            .find_map(|s| {
                let reply = session_socket(&s.name)
                    .and_then(|path| request_at(&path, Request::Places))
                    .ok()?;
                serde_json::from_value(reply).ok()
            })
            .unwrap_or_default();
        Origin {
            host: crate::protocol::machine_name(),
            sessions: or_default(infos.iter().map(|s| s.name.clone()).collect()),
            infos,
            hosts,
            via,
            plugins: crate::plugin::load().0,
            templates: crate::template::load().0,
        }
    });
    Caps {
        graphics,
        cell_width,
        cell_height,
        hosts: true,
        panes: true,
        windows: true,
        open: true,
        origin,
    }
}

/// Opens `url` with the desktop's handler, on this machine, unseen.
fn open_url(url: &str) {
    if !crate::links::openable(url) {
        return;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|mut child| thread::spawn(move || child.wait()));
}

/// Tells a running server to shut down.
pub fn kill_server() -> Result<()> {
    kill_server_at(&socket_path()?)
}

/// Ends the server at `path` and waits until it is gone.
pub fn kill_server_at(path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(path).context("no imkr server running")?;
    ClientMsg::KillServer.write_to(&mut stream)?;
    // Wait until the server has closed every shell and let go of the
    // socket, so `imkr kill-server && imkr` starts a new one instead
    // of reaching the old one on its way out.
    for _ in 0..150 {
        if UnixStream::connect(path).is_err() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    bail!("the server did not shut down within 3 seconds")
}

/// Sends `request` to the running server and returns its answer.
pub fn request(request: Request) -> Result<serde_json::Value> {
    request_at(&socket_path()?, request)
}

/// Sends `request` to the server at `path`.
pub fn request_at(path: &Path, request: Request) -> Result<serde_json::Value> {
    let mut stream = UnixStream::connect(path).context("no imkr server running")?;
    ClientMsg::Request(request).write_to(&mut stream)?;
    loop {
        match ServerMsg::read_from(&mut stream)? {
            Some(ServerMsg::Reply(Ok(value))) => return Ok(value),
            Some(ServerMsg::Reply(Err(message))) => bail!(message),
            Some(_) => {}
            None => bail!("the server closed the connection"),
        }
    }
}

/// The session `name` on `host` (`None`: this machine), started if needed.
fn connect(host: Option<&str>, name: &str) -> Result<Conn> {
    match host {
        Some(host) => Conn::remote(host, Some(name), true),
        None => {
            let path = session_socket(name)?;
            let stream = match UnixStream::connect(&path) {
                Ok(stream) => stream,
                Err(_) => start_server(&path, Some(name))?,
            };
            Conn::local(stream)
        }
    }
}

/// Shows the session behind `conn` until it ends. Dropping `conn` at the
/// end also ends its ssh, if any.
/// With `hello`, imkr just started: whether the session list should open,
/// and why it is not where it was last, if that is the case.
/// With `reveal`, that pane is shown and focused there.
fn session(
    mut conn: Conn,
    writer: &Writer,
    alone: bool,
    hello: Option<(bool, Option<String>)>,
    reveal: Option<crate::layout::PaneId>,
    window: bool,
) -> Result<Outcome> {
    let mut to_server = std::mem::replace(&mut conn.writer, Box::new(io::sink()));
    let stream = &mut conn.reader;
    let (cols, rows) = terminal::size()?;
    ClientMsg::Attach { alone }.write_to(&mut to_server)?;
    ClientMsg::Event(Event::Resize(cols, rows)).write_to(&mut to_server)?;
    ClientMsg::Caps(caps()).write_to(&mut to_server)?;
    if let Some(pane) = reveal {
        ClientMsg::Request(Request::Reveal { pane }).write_to(&mut to_server)?;
    }
    if window {
        ClientMsg::Request(Request::NewWindow).write_to(&mut to_server)?;
    }
    if let Some((pick, flash)) = hello {
        if pick {
            ClientMsg::Request(Request::OpenSessions).write_to(&mut to_server)?;
        }
        if let Some(text) = flash {
            ClientMsg::Request(Request::Flash { text }).write_to(&mut to_server)?;
        }
    }
    *writer.lock().unwrap() = Some(to_server);

    let mut out = stdout().lock();
    loop {
        match ServerMsg::read_from(stream) {
            Ok(Some(ServerMsg::Output(bytes))) => {
                out.write_all(&bytes)?;
                out.flush()?;
            }
            Ok(Some(ServerMsg::Detached)) => return Ok(Outcome::Detached),
            Ok(Some(ServerMsg::Exited)) => return Ok(Outcome::Exited),
            Ok(Some(ServerMsg::Reply(_))) => {}
            Ok(Some(ServerMsg::Switch(name))) => return Ok(Outcome::Switch(name)),
            Ok(Some(ServerMsg::Open(url))) => open_url(&url),
            Ok(None) | Err(_) => return Ok(Outcome::ConnectionLost),
        }
    }
}

/// The layouts of every running session, for a hive.
pub fn snapshot_all() -> Result<crate::persist::Hive> {
    snapshot_all_but(None)
}

/// Like `snapshot_all`, with `own` standing in for the session of that name:
/// a server cannot ask itself over its socket while it handles a key.
pub fn snapshot_all_but(
    own: Option<(&str, crate::persist::Saved)>,
) -> Result<crate::persist::Hive> {
    let mut hive = crate::persist::Hive {
        machine: crate::protocol::machine_name(),
        machine_id: crate::protocol::machine_id(),
        ..Default::default()
    };
    for name in crate::protocol::sessions() {
        let layout = match &own {
            Some((own_name, saved)) if *own_name == name => saved.clone(),
            _ => {
                let reply = request_at(&session_socket(&name)?, Request::Snapshot)
                    .with_context(|| format!("session {name}"))?;
                serde_json::from_value(reply)?
            }
        };
        hive.sessions.insert(
            name,
            crate::persist::HiveSession {
                saved_at: crate::persist::now(),
                layout,
            },
        );
    }
    if let Some((name, saved)) = own
        && !hive.sessions.contains_key(name)
    {
        hive.sessions.insert(
            name.to_owned(),
            crate::persist::HiveSession {
                saved_at: crate::persist::now(),
                layout: saved,
            },
        );
    }
    Ok(hive)
}

/// The hosts this machine knows: from the settings and from `--host`.
pub fn known_hosts() -> Vec<String> {
    let mut hosts = crate::config::Config::load().0.hosts;
    hosts.extend(remembered_hosts());
    let here = crate::protocol::machine_name();
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| *h != here && seen.insert(h.clone()));
    hosts
}

/// Everything for a hive: this machine's sessions as `snapshot_all_but`
/// takes them, and those of every known host, asked through ssh. A host
/// that does not answer keeps what `previous` had of it, so a machine
/// that is off stays on the map with its last known sessions.
pub fn snapshot_everywhere(
    own: Option<(&str, crate::persist::Saved)>,
    previous: Option<&crate::persist::Hive>,
) -> Result<crate::persist::Hive> {
    let mut hive = snapshot_all_but(own)?;
    for host in known_hosts() {
        match remote_snapshot(&host) {
            Ok(there) => hive.machines.push(crate::persist::HiveMachine {
                name: host,
                hostname: there.machine,
                id: there.machine_id,
                sessions: there.sessions,
            }),
            Err(_) => {
                let before = previous.and_then(|p| p.machines.iter().find(|m| m.name == host));
                if let Some(before) = before {
                    hive.machines.push(before.clone());
                }
            }
        }
    }
    Ok(hive)
}

/// The layouts of the sessions on `host`, asked through ssh without
/// prompting.
fn remote_snapshot(host: &str) -> Result<crate::persist::Hive> {
    let output = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(&["hive", "snapshot"]))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not run ssh")?;
    if !output.status.success() {
        bail!("{host} did not answer");
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Gives hive `name` to every known host that answers, which keeps it if
/// it is newer than its own. Returns the hosts that took it.
pub fn push_hive(name: &str, hive: &crate::persist::Hive) -> Vec<String> {
    let json = match serde_json::to_vec(&std::collections::BTreeMap::from([(name, hive)])) {
        Ok(json) => json,
        Err(_) => return Vec::new(),
    };
    known_hosts()
        .into_iter()
        .filter(|host| {
            let child = Command::new("ssh")
                .args([
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    "--",
                    host,
                ])
                .arg(remote_command(&["hive", "import"]))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else {
                return false;
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&json);
            }
            child.wait().is_ok_and(|status| status.success())
        })
        .collect()
}

/// Takes the hives of `host` that are newer than those here. Returns the
/// names taken.
pub fn pull_hives(host: &str) -> Result<Vec<String>> {
    let output = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(&["hive", "export"]))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("could not run ssh")?;
    if !output.status.success() {
        bail!("{host} did not answer");
    }
    crate::persist::import_hives(serde_json::from_slice(&output.stdout)?)
}

/// Brings hive `name` back, as a map of every machine in it: this
/// machine's sessions that do not run are started from their layouts; every
/// other machine is made known (the sidebar shows it, or that it is off)
/// and, if it answers, starts its missing sessions too. Returns what
/// happened, a line per machine.
///
/// `from` is a machine to fetch the hive from first, for a machine that
/// has none yet, e.g. one that replaces a broken one. `take` names a
/// machine of the hive whose sessions come back here instead of there.
pub fn restore_hive(name: &str, from: Option<&str>, take: Option<&str>) -> Result<Vec<String>> {
    if std::env::var_os("IMKR_SOCKET").is_some() {
        bail!("restoring sessions needs their names, unset IMKR_SOCKET");
    }
    if let Some(from) = from {
        pull_hives(from).with_context(|| format!("no hives from {from}"))?;
        remember_host(from);
    }
    let Some(hive) = crate::persist::load_hive(name)? else {
        bail!("there is no hive called {name}");
    };
    let here = crate::protocol::machine_name();
    // The machine it was saved on, then the others; hives from before
    // know only the one.
    let saver = if hive.machine.is_empty() {
        here.clone()
    } else {
        hive.machine.clone()
    };
    let mut machines = vec![crate::persist::HiveMachine {
        name: saver.clone(),
        hostname: saver,
        id: hive.machine_id,
        sessions: hive.sessions,
    }];
    // This machine by its id; hives from before only know names.
    let id = crate::protocol::machine_id();
    let is_here = |m: &crate::persist::HiveMachine| {
        if m.id.is_empty() {
            m.hostname == here
        } else {
            m.id == id
        }
    };
    machines.extend(hive.machines);

    if let Some(take) = take
        && !machines
            .iter()
            .any(|m| m.name == take || m.hostname == take)
    {
        bail!("hive {name} has no machine {take}");
    }
    let mut report = Vec::new();
    for machine in machines {
        let taken = take.is_some_and(|t| machine.name == t || machine.hostname == t);
        if taken {
            let started = restore_sessions(machine.sessions)?;
            report.push(format!(
                "⌂ {here} takes over {}: started {}",
                machine.name,
                if started.is_empty() {
                    "nothing new".to_owned()
                } else {
                    started.join(", ")
                }
            ));
            continue;
        }
        if is_here(&machine) {
            let started = restore_sessions(machine.sessions)?;
            report.push(match started.as_slice() {
                [] => format!("⌂ {here}: all running"),
                started => format!("⌂ {here}: started {}", started.join(", ")),
            });
            continue;
        }
        remember_host(&machine.name);
        let restored = remote_restore(&machine.name, &machine.sessions);
        // It knows this machine from now on, a new one included.
        if restored.is_ok() {
            let _ = remote_run(&machine.name, &["hosts", "add", &here]);
        }
        report.push(match restored {
            Ok(started) if started.is_empty() => format!("⇄ {}: all running", machine.name),
            Ok(started) => format!("⇄ {}: started {}", machine.name, started.join(", ")),
            Err(_) => format!("⇄ {}: offline, shown as such", machine.name),
        });
    }
    Ok(report)
}

/// Starts again the sessions of this machine that ran when it went down
/// (`persist::were_running`) and stopped with it, each from its saved
/// layout and history, as going there would.
fn revive_stopped() {
    if std::env::var_os("IMKR_SOCKET").is_some() {
        return;
    }
    let names = crate::persist::were_running();
    if names.is_empty() {
        return;
    }
    let stopped = crate::persist::dormant_sessions(&[]);
    for name in names {
        let Some(dormant) = stopped.iter().find(|d| d.name == name && !d.closed) else {
            continue;
        };
        let Ok(socket) = session_socket(&dormant.name) else {
            continue;
        };
        if UnixStream::connect(&socket).is_err() {
            let _ = start_server(&socket, Some(&dormant.name));
        }
    }
}

/// Starts each of `sessions` that does not run here, from its layout.
/// Returns those started.
pub fn restore_sessions(
    sessions: std::collections::BTreeMap<String, crate::persist::HiveSession>,
) -> Result<Vec<String>> {
    let mut started = Vec::new();
    for (session, saved) in sessions {
        let socket = session_socket(&session)?;
        if UnixStream::connect(&socket).is_ok() {
            continue;
        }
        // The server brings back what it finds in its state file.
        let path = crate::persist::path_for(&session)?;
        crate::persist::write(&path, &crate::persist::to_json(&saved.layout)?)?;
        start_server(&socket, Some(&session))?;
        started.push(session);
    }
    Ok(started)
}

/// Starts session `name` here with `layout`, unless it runs. True when
/// it was started.
pub fn start_from(name: &str, layout: crate::persist::Saved) -> Result<bool> {
    let session = crate::persist::HiveSession {
        saved_at: crate::persist::now(),
        layout,
    };
    let started = restore_sessions([(name.to_owned(), session)].into())?;
    Ok(!started.is_empty())
}

/// Has `host` start session `name` with `layout`, unless it runs there.
pub fn start_from_on(host: &str, name: &str, layout: crate::persist::Saved) -> Result<bool> {
    let session = crate::persist::HiveSession {
        saved_at: crate::persist::now(),
        layout,
    };
    let started = remote_restore(host, &[(name.to_owned(), session)].into())?;
    Ok(!started.is_empty())
}

/// Runs `imkr args...` on `host` through ssh without prompting, with
/// `input` on its stdin, and returns what it printed.
pub fn remote_output(host: &str, args: &[&str], input: &str) -> Result<String> {
    let mut child = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(args))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("{host}: {}", why.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs `imkr args...` on `host` through ssh without prompting.
fn remote_run(host: &str, args: &[&str]) -> Result<()> {
    let status = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(args))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run ssh")?;
    if !status.success() {
        bail!("{host} did not do it");
    }
    Ok(())
}

/// Has `host` start those of `sessions` that do not run there.
fn remote_restore(
    host: &str,
    sessions: &std::collections::BTreeMap<String, crate::persist::HiveSession>,
) -> Result<Vec<String>> {
    let mut child = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            host,
        ])
        .arg(remote_command(&["hive", "start"]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not run ssh")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&serde_json::to_vec(sessions)?)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("{host} did not answer");
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Whether `name` belongs to the agent process that runs imkr rather than
/// to the user: Claude Code's markers of its own session.
fn is_agent_session_var(name: &str) -> bool {
    name == "CLAUDECODE"
        || name.starts_with("CLAUDE_CODE_")
        || name == "CLAUDE_PID"
        || name == "CLAUDE_EFFORT"
}

/// Starts a server in the background and connects to it. With `session`,
/// the server is that named session, whatever this process was started for.
pub fn start_server(path: &Path, session: Option<&str>) -> Result<UnixStream> {
    // A socket file without a server behind it is left over from a crash.
    let _ = std::fs::remove_file(path);

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.with_extension("log"))?;
    let mut cmd = Command::new(std::env::current_exe()?);
    if let Some(name) = session {
        cmd.env("IMKR_SESSION", name).env_remove("IMKR_SOCKET");
    }
    // Started by an agent, e.g. a session from a template, the server must
    // not pass the agent's own session on to every pane: a Claude Code there
    // would take itself for that agent's child and stop saving transcripts.
    // The panes' shells set the user's own variables again.
    for (name, _) in std::env::vars_os() {
        if is_agent_session_var(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd.arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // SAFETY: setsid is async-signal-safe. A new session detaches the server
    // from this terminal, so closing the terminal does not kill it.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("failed to start the imkr server")?;

    for _ in 0..100 {
        if let Ok(stream) = UnixStream::connect(path) {
            return Ok(stream);
        }
        thread::sleep(Duration::from_millis(20));
    }
    bail!(
        "the imkr server did not start, see {}",
        path.with_extension("log").display()
    )
}

/// `imkr proxy`: connects stdin and stdout to this machine's server, for a
/// client elsewhere that came through ssh. With `start`, starts the server
/// first if there is none.
pub fn proxy(start: bool) -> Result<()> {
    let path = socket_path()?;
    let stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(_) if start => start_server(&path, None)?,
        Err(_) => bail!("no imkr server running"),
    };
    // Raw fds and plain reads and writes: frames must reach the other side
    // right away, not when a buffer fills up.
    // SAFETY: fds 0 and 1 are stdin and stdout, used by nothing else here.
    let (mut input, mut out) = unsafe { (File::from_raw_fd(0), File::from_raw_fd(1)) };
    out.write_all(&[PROXY_READY])?;
    let mut to_server = stream.try_clone()?;
    thread::spawn(move || {
        let _ = pass_on(&mut input, &mut to_server);
        // The client is gone: the server drops the connection, which ends
        // the copy below.
        let _ = to_server.shutdown(std::net::Shutdown::Write);
    });
    let mut from_server = stream;
    pass_on(&mut from_server, &mut out)?;
    Ok(())
}

/// Copies `from` to `to` until `from` ends, each read as soon as it comes.
fn pass_on(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => to.write_all(&buf[..n])?,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_quoted_for_sh() {
        assert_eq!(sh_quote("claude"), "claude");
        assert_eq!(sh_quote("~/a b"), "'~/a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
        // A quoted word survives the login shell's `-lc` around it.
        let line = remote_command(&[&sh_quote("echo 'x y'")]);
        assert!(line.starts_with("exec \"${SHELL:-sh}\" -lc '"), "{line}");
    }

    #[test]
    fn tailnet_peers_by_their_magicdns_name() {
        let status = serde_json::json!({
            "Self": { "HostName": "laptop", "DNSName": "laptop.tail1234.ts.net.", "OS": "linux" },
            "Peer": {
                "a": { "HostName": "CodingMachine", "DNSName": "codingmachine.tail1234.ts.net.", "OS": "linux", "Online": true },
                "b": { "HostName": "pixel", "DNSName": "pixel.tail1234.ts.net.", "OS": "android", "Online": true },
                "c": { "HostName": "Desk", "DNSName": "", "OS": "macOS", "Online": false }
            }
        });
        let peers = parse_tailnet(&status);
        let names: Vec<&str> = peers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["codingmachine", "desk"]);
        assert!(peers[0].online && !peers[1].online);
    }

    #[test]
    fn the_agents_own_session_stays_out_of_new_servers() {
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_PID",
        ] {
            assert!(is_agent_session_var(name), "{name}");
        }
        for name in ["CLAUDE_CONFIG_DIR", "HOME", "IMKR_SESSION", "PATH"] {
            assert!(!is_agent_session_var(name), "{name}");
        }
    }
}
