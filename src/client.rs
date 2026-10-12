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

use crate::protocol::{Caps, ClientMsg, Request, ServerMsg, session_socket, socket_path};

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
        // ~/.cargo/bin is often only on the PATH of interactive shells.
        let mut command = String::from("PATH=$HOME/.cargo/bin:$HOME/.local/bin:$PATH exec imkr");
        if let Some(name) = session {
            command.push_str(" -s ");
            command.push_str(name);
        }
        command.push_str(" proxy");
        if start {
            command.push_str(" --start");
        }
        // A login shell, for what else the profile puts on the PATH.
        let remote = format!("exec \"${{SHELL:-sh}}\" -lc '{command}'");
        let mut ssh = Command::new("ssh")
            .args([
                "-C",
                "-T",
                "-o",
                "ServerAliveInterval=15",
                "--",
                host,
                &remote,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("could not run ssh")?;
        let mut reader = ssh.stdout.take().expect("piped");
        let writer = ssh.stdin.take().expect("piped");
        // The proxy says it reached the server before the protocol starts,
        // so a failure shows ssh's or imkr's own message.
        let mut ready = [0u8; 1];
        if reader.read_exact(&mut ready).is_err() || ready[0] != PROXY_READY {
            let _ = ssh.wait();
            bail!("could not reach imkr on {host}");
        }
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

/// Written by `imkr proxy` once it is connected to the server.
const PROXY_READY: u8 = 0x06;

/// A session on another machine: its host for ssh, and the session's name
/// there if not the default one.
pub struct Remote {
    pub host: String,
    pub session: Option<String>,
}

/// Attaches to the running server, here or on the `remote` machine. With
/// `start`, starts one first if there is none. With `alone`, other
/// terminals attached to it are detached.
pub fn run(start: bool, alone: bool, remote: Option<Remote>) -> Result<()> {
    let host = remote.as_ref().map(|r| r.host.as_str());
    let conn = match &remote {
        Some(r) => Conn::remote(&r.host, r.session.as_deref(), start)?,
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

    let mut conn = conn;
    let outcome = loop {
        match session(conn, &writer, alone) {
            // Another session on the same machine.
            Ok(Outcome::Switch(name)) => {
                let next = match host {
                    Some(host) => Conn::remote(host, Some(&name), true),
                    None => session_socket(&name).and_then(|path| {
                        let stream = match UnixStream::connect(&path) {
                            Ok(stream) => stream,
                            Err(_) => start_server(&path, Some(&name))?,
                        };
                        Conn::local(stream)
                    }),
                };
                conn = match next {
                    Ok(conn) => conn,
                    Err(e) => break Err(e),
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
    Caps {
        graphics,
        cell_width,
        cell_height,
    }
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

/// Shows the session behind `conn` until it ends. Dropping `conn` at the
/// end also ends its ssh, if any.
fn session(mut conn: Conn, writer: &Writer, alone: bool) -> Result<Outcome> {
    let mut to_server = std::mem::replace(&mut conn.writer, Box::new(io::sink()));
    let stream = &mut conn.reader;
    let (cols, rows) = terminal::size()?;
    ClientMsg::Attach { alone }.write_to(&mut to_server)?;
    ClientMsg::Event(Event::Resize(cols, rows)).write_to(&mut to_server)?;
    ClientMsg::Caps(caps()).write_to(&mut to_server)?;
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
    let mut hive = crate::persist::Hive::default();
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

/// Starts every session of save file `name` that does not run, each
/// brought back from its layout there. Returns the sessions started.
pub fn restore_hive(name: &str) -> Result<Vec<String>> {
    if std::env::var_os("IMKR_SOCKET").is_some() {
        bail!("restoring sessions needs their names, unset IMKR_SOCKET");
    }
    let Some(hive) = crate::persist::load_hive(name)? else {
        bail!("there is no hive called {name}");
    };
    let mut started = Vec::new();
    for (session, saved) in hive.sessions {
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
