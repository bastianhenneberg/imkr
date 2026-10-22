//! A pane: one child process running in a pseudo terminal, plus the VT state
//! its output has produced.

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::agent::{self, AgentState};
use crate::app::AppEvent;
use crate::graphics::{Advance, Chunk, PaneGraphics, Scanner};
use crate::layout::PaneId;

const SCROLLBACK_LINES: usize = 10_000;

pub struct Pane {
    id: PaneId,
    parser: Arc<Mutex<crate::term::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    last_output: Arc<Mutex<Instant>>,
    /// Images the program shows with the kitty graphics protocol.
    pub graphics: Arc<Mutex<PaneGraphics>>,
    /// Hyperlinks the program marked with OSC 8, the newest last.
    links: Arc<Mutex<Vec<crate::links::Link>>>,
    /// What the shell's integration (OSC 133) told about its commands.
    shell: Arc<Mutex<Shell>>,
    /// The agent state the program reported with `imkr status`.
    pub reported: Option<AgentState>,
    /// Why its hook said the agent failed, e.g. "limit reached".
    pub failure: Option<String>,
    /// The agent's session id from its hooks, e.g. for `claude --resume`.
    pub session: Option<String>,
    /// The agent's transcript, from its hooks, and what it tells (read
    /// again whenever the file changed, at that time).
    pub transcript: Option<PathBuf>,
    pub meta: Option<(crate::agent::Meta, std::time::SystemTime)>,
    /// A name the user gave the pane, shown instead of the program.
    pub name: Option<String>,
    /// Whether the question on the screen was answered: input came while it
    /// was shown. Cleared once no question is shown anymore.
    answered: Cell<bool>,
    events: Sender<AppEvent>,
    /// The thread reading the pty, while it runs.
    reader: Option<Reader>,
    /// The process went to another session with the pty: it is not ended
    /// with the pane.
    released: bool,
    /// Connections showing this pane elsewhere (`imkr attach-pane`): they
    /// get its output as read, unchanged.
    taps: Taps,
    /// For a pane showing one of another machine: the agent running there
    /// and its state, as imkr there tells (`links::Piece::Remote`).
    remote: Arc<Mutex<Option<(String, AgentState)>>>,
    /// The accounts its agents use, by agent (`claude`, `codex`), when not
    /// `standard`: what its shell started with, or switched to since.
    pub accounts: std::collections::BTreeMap<String, String>,
    /// Which subscription or provider its agent runs on, for its frame:
    /// `henneberg`, `Ollama`; set by `App::name_accounts`.
    pub shown_account: Option<String>,
    /// Its notepad (`Ctrl+B t`), saved with the pane.
    pub notes: Vec<crate::persist::Note>,
}

/// How much of its history a tapped pane sends first.
const TAPPED_HISTORY: usize = 2000;

/// See `Pane::taps`. Bounded: a tap that falls this far behind is cut off,
/// rather than holding up the pane or filling memory.
type Taps = Arc<Mutex<Vec<Tap>>>;

/// See `Pane::tap`.
struct Tap {
    /// The connection, see `Pane::untap`.
    id: u64,
    out: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Only watches: the pane keeps its size and answers its program.
    watch: bool,
}

impl Tap {
    fn send(&self, bytes: &[u8]) -> bool {
        self.out.try_send(bytes.to_vec()).is_ok()
    }
}
const TAP_BACKLOG: usize = 1024;

/// A finished command: where its output starts and ends, its exit code.
type Finished = ((usize, u16), (usize, u16), Option<i32>);

/// The commands of a shell with integration (OSC 133): whether one runs,
/// and where the output of the last one is, as (absolute row, column).
#[derive(Default)]
struct Shell {
    /// The shell sends marks at all.
    seen: bool,
    /// Commands finished so far.
    finished: u64,
    /// Where the output of the command running now starts.
    running: Option<(usize, u16)>,
    /// The last finished: its output and exit code.
    last: Option<Finished>,
}

impl Shell {
    fn mark(&mut self, mark: char, exit: Option<i32>, at: (usize, u16)) {
        self.seen = true;
        match mark {
            'C' => self.running = Some(at),
            'D' => {
                // A `D` without `C` is the first prompt's: no command ran.
                if let Some(from) = self.running.take() {
                    self.last = Some((from, at, exit));
                    self.finished += 1;
                }
            }
            _ => {}
        }
    }
}

/// The thread reading a pane's pty, and the way to stop it.
struct Reader {
    /// Written to, or closed, to stop the thread.
    stop: File,
    thread: JoinHandle<()>,
}

/// What to run in a new pane and where.
pub struct Spawn<'a> {
    pub id: PaneId,
    pub rows: u16,
    pub cols: u16,
    /// Start here, or in the server's directory without it.
    pub cwd: Option<&'a Path>,
    /// Run this, or the user's shell when empty.
    pub command: &'a [String],
    /// The server socket, handed to the program so `imkr` finds it.
    pub socket: &'a Path,
    /// The accounts its agents use, see `account`: their environment is
    /// the shell's.
    pub accounts: &'a std::collections::BTreeMap<String, String>,
}

/// Where `tmux` for the panes lives while the tmux shim is on, see `tmux`.
static TMUX_SHIM: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_tmux_shim(dir: Option<PathBuf>) {
    *TMUX_SHIM.lock().unwrap() = dir;
}

impl Pane {
    /// Starts `spawn.command` in a new pty. Output is fed into the VT parser
    /// on a background thread, which sends `AppEvent::PtyOutput` after every
    /// chunk and `AppEvent::PtyExited(id)` at EOF.
    pub fn spawn(spawn: Spawn, events: Sender<AppEvent>) -> Result<Self> {
        Self::spawn_after(spawn, &[], events)
    }

    /// `spawn` with `before` fed into the terminal first, e.g. the history
    /// from before a restart.
    pub fn spawn_after(spawn: Spawn, before: &[u8], events: Sender<AppEvent>) -> Result<Self> {
        let id = spawn.id;
        let rows = spawn.rows.max(1);
        let cols = spawn.cols.max(1);

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open pty")?;

        let mut cmd = if spawn.command.is_empty() {
            CommandBuilder::new_default_prog()
        } else {
            CommandBuilder::from_argv(spawn.command.iter().map(Into::into).collect())
        };
        match spawn.cwd {
            Some(cwd) if cwd.is_dir() => cmd.cwd(cwd),
            _ => {
                if let Ok(cwd) = std::env::current_dir() {
                    cmd.cwd(cwd);
                }
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("IMKR", "1");
        cmd.env("IMKR_PANE", id.to_string());
        cmd.env("IMKR_SOCKET", spawn.socket);
        // Each agent's account: the one chosen for this session, else the
        // default (`imkr account default`). Noted in the pane as it started,
        // so a default set later does not rename what already runs. One
        // logged out since gives way to the default, so that an agent
        // resumed here does not start at a login screen.
        let mut accounts = spawn.accounts.clone();
        if let Ok(store) = crate::account::Store::here() {
            for tool in crate::account::TOOLS {
                let name = accounts
                    .get(*tool)
                    .filter(|name| !store.find(tool, name).is_some_and(|a| store.login_dead(&a)))
                    .cloned()
                    .unwrap_or_else(|| store.default_of(tool));
                if let Some(account) = store.find(tool, &name) {
                    for (key, value) in store.env(&account) {
                        cmd.env(key, value);
                    }
                }
                if name == crate::account::STANDARD {
                    accounts.remove(*tool);
                } else {
                    accounts.insert((*tool).to_owned(), name);
                }
            }
        }
        if let Some(dir) = TMUX_SHIM.lock().unwrap().clone() {
            // What tmux sets: its socket, server pid and session.
            let tmux = format!("{},{},0", spawn.socket.display(), std::process::id());
            cmd.env("TMUX", tmux);
            cmd.env("TMUX_PANE", format!("%{id}"));
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.display()));
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        // Drop our copy of the slave side, so the reader sees EOF once the
        // child exits.
        drop(pair.slave);
        let mut pane = Self::start(id, (rows, cols), pair.master, child, before, events)?;
        pane.accounts = accounts;
        Ok(pane)
    }

    /// Takes over the pane the server before `imkr update` ran, see
    /// `upgrade`.
    pub fn adopt(
        id: PaneId,
        from: &crate::upgrade::HandoverPane,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        let child = crate::upgrade::AdoptedChild::new(from.pid);
        Self::from_handover(id, from, Box::new(child), events)
    }

    /// Takes over a pane another session moved here, whose process stays
    /// that session's child.
    pub fn take_over(
        id: PaneId,
        from: &crate::upgrade::HandoverPane,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        let child = crate::upgrade::ForeignChild::new(from.pid);
        Self::from_handover(id, from, Box::new(child), events)
    }

    fn from_handover(
        id: PaneId,
        from: &crate::upgrade::HandoverPane,
        child: Box<dyn Child + Send + Sync>,
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        use crate::upgrade::{AdoptedMaster, replay};
        // SAFETY: the descriptor was handed over for this pane only.
        let master = unsafe { AdoptedMaster::from_raw_fd(from.fd) };
        let size = (from.rows.max(1), from.cols.max(1));
        let before = replay(&from.history, &from.screen, size.0);
        let mut pane = Self::start(id, size, Box::new(master), child, &before, events)?;
        pane.reported = from.reported;
        pane.failure = from.failure.clone();
        pane.transcript = from.transcript.clone();
        pane.parser.lock().unwrap().set_title(from.title.clone());
        *pane.remote.lock().unwrap() = from.remote.clone();
        // What it wrote was read before the handover: the pane is not busy
        // just because it was taken over.
        if let Some(long_ago) = Instant::now().checked_sub(Duration::from_secs(60)) {
            *pane.last_output.lock().unwrap() = long_ago;
        }
        Ok(pane)
    }

    /// What `imkr update` hands over for this pane: the pty, the process
    /// and what the screen shows. `None` if the pty has no descriptor.
    pub fn handover(&self) -> Option<crate::upgrade::HandoverPane> {
        let fd = self.master.as_raw_fd()?;
        let pid = self.child.process_id()?;
        let history_len = self.history_len();
        let (rows, cols) = self.size;
        let history = if history_len == 0 {
            String::new()
        } else {
            self.text((0, 0), (history_len - 1, cols))
        };
        let mut parser = self.parser.lock().unwrap();
        let offset = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(0);
        let screen = parser.screen().state_formatted();
        parser.screen_mut().set_scrollback(offset);
        Some(crate::upgrade::HandoverPane {
            fd,
            pid,
            rows,
            cols,
            history,
            screen,
            reported: self.reported,
            failure: self.failure.clone(),
            transcript: self.transcript.clone(),
            title: parser.screen().title(),
            remote: self.remote_agent(),
        })
    }

    /// Reads the pty in the background into the terminal state, which starts
    /// with `before` fed in.
    fn start(
        id: PaneId,
        (rows, cols): (u16, u16),
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn Child + Send + Sync>,
        before: &[u8],
        events: Sender<AppEvent>,
    ) -> Result<Self> {
        // A copy of the master rather than portable-pty's writer: that one
        // types Ctrl+D when dropped, which ends a shell whose pane moved to
        // another session. Closing a pane ends its process with SIGHUP anyway.
        let writer: Box<dyn Write + Send> = match master.as_raw_fd() {
            // SAFETY: the master stays open while it is borrowed here.
            Some(fd) => Box::new(File::from(
                unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?,
            )),
            None => master.take_writer()?,
        };
        let mut parser = crate::term::Parser::new(rows, cols, SCROLLBACK_LINES);
        parser.process(before);
        let mut pane = Self {
            id,
            parser: Arc::new(Mutex::new(parser)),
            master,
            writer,
            child,
            size: (rows, cols),
            last_output: Arc::new(Mutex::new(Instant::now())),
            graphics: Arc::new(Mutex::new(PaneGraphics::default())),
            links: Arc::new(Mutex::new(Vec::new())),
            shell: Arc::new(Mutex::new(Shell::default())),
            reported: None,
            failure: None,
            session: None,
            transcript: None,
            meta: None,
            name: None,
            answered: Cell::new(false),
            events,
            reader: None,
            released: false,
            taps: Arc::default(),
            remote: Arc::default(),
            accounts: Default::default(),
            shown_account: None,
            notes: Vec::new(),
        };
        pane.read_on()?;
        Ok(pane)
    }

    /// Starts the thread that reads the pty into the terminal state. It
    /// sends `AppEvent::PtyOutput` after every chunk and
    /// `AppEvent::PtyExited` at the end, unless `stop_reading` stopped it.
    pub fn read_on(&mut self) -> Result<()> {
        if self.reader.is_some() {
            return Ok(());
        }
        let id = self.id;
        let mut reader = self.master.try_clone_reader()?;
        let pty = self.master.as_raw_fd();
        // Where the terminal's answers to the program go: the cursor
        // position, colours or size it asked for.
        let mut answer_to: Option<File> = pty.and_then(|fd| {
            // SAFETY: the master stays open while the reader runs.
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            fd.try_clone_to_owned().ok().map(File::from)
        });
        let (wake, stop) = stop_pipe()?;
        let reader_parser = Arc::clone(&self.parser);
        let reader_last_output = Arc::clone(&self.last_output);
        let reader_graphics = Arc::clone(&self.graphics);
        let reader_links = Arc::clone(&self.links);
        let reader_shell = Arc::clone(&self.shell);
        let reader_taps = Arc::clone(&self.taps);
        let reader_remote = Arc::clone(&self.remote);
        let events = self.events.clone();
        let thread = thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut scanner = Scanner::default();
                let mut copies = crate::clipboard::Osc52Scanner::default();
                let mut hyperlinks = crate::links::LinkScanner::default();
                let mut open: Option<(String, (usize, u16))> = None;
                loop {
                    // Stopped: the pty is someone else's from here on, not a
                    // byte more is read.
                    if let Some(pty) = pty
                        && !wait_readable(pty, wake.as_raw_fd())
                    {
                        return;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for copy in copies.feed(&buf[..n]) {
                                let _ = events.send(AppEvent::Clipboard(copy));
                            }
                            let mut parser = reader_parser.lock().unwrap();
                            for chunk in scanner.feed(&buf[..n]) {
                                match chunk {
                                    Chunk::Text(text) => {
                                        // Clearing the screen clears its images.
                                        let all = find_bytes(&text, b"\x1b[3J");
                                        if all || find_bytes(&text, b"\x1b[2J") {
                                            let history = history_len(&mut parser);
                                            reader_graphics
                                                .lock()
                                                .unwrap()
                                                .clear_screen(history, all);
                                        }
                                        for piece in hyperlinks.feed(&text) {
                                            let cursor = |parser: &mut crate::term::Parser| {
                                                let history = history_len(parser);
                                                let (row, col) = parser.screen().cursor_position();
                                                (history + usize::from(row), col)
                                            };
                                            match piece {
                                                crate::links::Piece::Text(text) => {
                                                    parser.process(&text);
                                                }
                                                crate::links::Piece::Start(url) => {
                                                    open = Some((url, cursor(&mut parser)));
                                                }
                                                crate::links::Piece::End => {
                                                    let Some((url, from)) = open.take() else {
                                                        continue;
                                                    };
                                                    let to = cursor(&mut parser);
                                                    let mut links = reader_links.lock().unwrap();
                                                    links.push(crate::links::Link {
                                                        url,
                                                        from,
                                                        to,
                                                    });
                                                    let over = links
                                                        .len()
                                                        .saturating_sub(crate::links::KEPT);
                                                    links.drain(..over);
                                                }
                                                crate::links::Piece::Remote { agent, state } => {
                                                    *reader_remote.lock().unwrap() =
                                                        AgentState::from_name(&state)
                                                            .filter(|_| !agent.is_empty())
                                                            .map(|state| (agent, state));
                                                }
                                                crate::links::Piece::Mark(mark, exit) => {
                                                    let at = cursor(&mut parser);
                                                    reader_shell
                                                        .lock()
                                                        .unwrap()
                                                        .mark(mark, exit, at);
                                                }
                                            }
                                        }
                                    }
                                    Chunk::Graphics(body) => {
                                        let history = history_len(&mut parser);
                                        let (row, col) = parser.screen().cursor_position();
                                        let cursor = (history + usize::from(row), col);
                                        let advance =
                                            reader_graphics.lock().unwrap().handle(&body, cursor);
                                        // The cursor moves past a shown image, as it
                                        // does in the terminals that draw it.
                                        if let Some(Advance { cols, rows }) = advance {
                                            let down =
                                                "\n".repeat(usize::from(rows.saturating_sub(1)));
                                            parser
                                                .process(format!("{down}\x1b[{cols}C").as_bytes());
                                        }
                                    }
                                }
                            }
                            let answers = parser.answers();
                            // Under the parser's lock, so a new tap gets
                            // everything after the screen it was sent.
                            let tapped = {
                                let mut taps = reader_taps.lock().unwrap();
                                taps.retain(|tap| tap.send(&buf[..n]));
                                taps.iter().any(|tap| !tap.watch)
                            };
                            drop(parser);
                            // A tapped pane's terminal is the one showing it,
                            // which answers itself.
                            if !answers.is_empty()
                                && !tapped
                                && let Some(out) = &mut answer_to
                            {
                                let _ = out.write_all(&answers);
                            }
                            *reader_last_output.lock().unwrap() = Instant::now();
                            if events.send(AppEvent::PtyOutput).is_err() {
                                return;
                            }
                        }
                    }
                }
                // An empty chunk tells taps the pane ended, rather than
                // that they were cut off.
                for tap in reader_taps.lock().unwrap().drain(..) {
                    let _ = tap.send(&[]);
                }
                let _ = events.send(AppEvent::PtyExited(id));
            })?;
        self.reader = Some(Reader { stop, thread });
        Ok(())
    }

    /// Shows the pane elsewhere: the bytes that draw its history and
    /// screen as they are now, and a receiver for everything it outputs
    /// from then on.
    /// See `Tap::watch`.
    pub fn tap(&self, id: u64, watch: bool) -> (Vec<u8>, std::sync::mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(TAP_BACKLOG);
        let parser = self.parser.lock().unwrap();
        let mut out = Vec::new();
        let history = parser.history_len();
        let (_, cols) = parser.size();
        for abs in history.saturating_sub(TAPPED_HISTORY)..history {
            if let Some((text, wrapped)) = parser.abs_row(abs, 0, cols) {
                out.extend_from_slice(text.trim_end().as_bytes());
                if !wrapped {
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
        out.extend_from_slice(&parser.state_formatted());
        self.taps.lock().unwrap().push(Tap { id, out: tx, watch });
        drop(parser);
        (out, rx)
    }

    /// Whether the pane is shown elsewhere, see `tap`; its size is then
    /// that of the terminal showing it.
    pub fn tapped(&self) -> bool {
        self.taps.lock().unwrap().iter().any(|tap| !tap.watch)
    }

    /// Drops the tap of connection `id` once it is gone, rather than when
    /// the next output finds no one listening: until then the pane would
    /// keep the size of a terminal that no longer shows it.
    pub fn untap(&self, id: u64) {
        self.taps.lock().unwrap().retain(|tap| tap.id != id);
    }

    /// Rows and columns.
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Stops reading the pty and waits until the reader is done, so the
    /// terminal state holds everything read and nothing more comes in.
    pub fn stop_reading(&mut self) {
        if let Some(Reader { mut stop, thread }) = self.reader.take() {
            let _ = stop.write_all(&[1]);
            let _ = thread.join();
        }
    }

    /// Lets the process go on without this pane: it is not ended when the
    /// pane is dropped, but still waited for, so it does not linger as a
    /// zombie once it exits.
    pub fn release(&mut self) {
        self.stop_reading();
        self.released = true;
        let mut child = std::mem::replace(
            &mut self.child,
            Box::new(crate::upgrade::AdoptedChild::new(0)),
        );
        let _ = thread::Builder::new()
            .name("released-child".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }

    /// The process in the foreground of this pane, e.g. the shell, or nvim
    /// started from it.
    fn foreground_pid(&self) -> Option<u32> {
        self.master
            .process_group_leader()
            .map(|pid| pid as u32)
            .or_else(|| self.child.process_id())
    }

    /// Ends what runs in front of the shell (SIGTERM to its process
    /// group), e.g. an agent to start again in another account. False when
    /// the shell itself is in front.
    pub fn stop_foreground(&self) -> bool {
        let (Some(group), Some(shell)) = (self.foreground_pid(), self.child.process_id()) else {
            return false;
        };
        if group == shell {
            return false;
        }
        // SAFETY: kill only sends a signal.
        unsafe { libc::kill(-(group as libc::pid_t), libc::SIGTERM) == 0 }
    }

    /// The command line of the foreground process.
    pub fn foreground_argv(&self) -> Vec<String> {
        let Some(pid) = self.foreground_pid() else {
            return Vec::new();
        };
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|raw| {
                raw.split(|b| *b == 0)
                    .filter(|arg| !arg.is_empty())
                    .map(|arg| String::from_utf8_lossy(arg).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the agent in this pane is doing, `None` without an agent.
    pub fn agent_state(&self) -> Option<AgentState> {
        if let Some((_, state)) = self.remote_agent() {
            return Some(state);
        }
        let argv = self.foreground_argv();
        let since = self.last_output.lock().unwrap().elapsed();
        let asking = agent::asks_question(self.screen().screen());
        if !asking {
            self.answered.set(false);
        }
        let failed =
            since >= agent::QUIET_ERROR && agent::failure(self.screen().screen()).is_some();
        agent::state(
            &argv,
            self.reported,
            since,
            asking && !self.answered.get(),
            failed,
        )
    }

    /// The shell's commands as its integration tells them: whether it has
    /// one, how many finished, whether one runs, and the output and exit
    /// code of the last.
    pub fn last_command(&self) -> serde_json::Value {
        let (seen, finished, running, last) = {
            let shell = self.shell.lock().unwrap();
            (
                shell.seen,
                shell.finished,
                shell.running.is_some(),
                shell.last,
            )
        };
        let (output, exit) = match last {
            // Up to the cell before the mark: `text` takes its end with it.
            Some((from, (row, col), exit)) if (row, col) > from => {
                let end = match col {
                    0 => (row - 1, self.size.1),
                    col => (row, col - 1),
                };
                (self.text(from, end).trim_end().to_owned(), exit)
            }
            Some((_, _, exit)) => (String::new(), exit),
            None => (String::new(), None),
        };
        serde_json::json!({
            "integration": seen,
            "finished": finished,
            "running": running,
            "exit": exit,
            "output": output,
        })
    }

    /// Why the pane counts as an agent in its state, or as none, step by
    /// step, for `imkr explain`.
    pub fn explain(&self) -> serde_json::Value {
        let argv = self.foreground_argv();
        let found = agent::agent_match(&argv);
        let since = self.last_output.lock().unwrap().elapsed();
        let phrase = agent::question_phrase(self.screen().screen());
        let state = self.agent_state();
        let why = if argv.is_empty() {
            "no foreground process found".to_owned()
        } else if found.is_none() && agent::is_shell(&argv) {
            match argv.get(1).filter(|arg| !arg.starts_with('-')) {
                Some(script) => {
                    let name = script.rsplit('/').next().unwrap_or(script);
                    format!(
                        "a shell runs the script {name}, which is no agent imkr knows; \
                         add \"{name}\" to `extra_agents` in the config if it is one"
                    )
                }
                None => "a shell runs in the foreground: no agent".to_owned(),
            }
        } else if let Some(reported) = self.reported {
            format!(
                "its hooks reported {} (`imkr status`), that wins",
                reported.name()
            )
        } else if found.is_none() {
            let name = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
            format!(
                "{name} is no agent imkr knows; add \"{name}\" to `extra_agents` in the config if it is one"
            )
        } else if since < agent::ACTIVE {
            format!("it wrote output {} ms ago: working", since.as_millis())
        } else if phrase.is_some() && !self.answered.get() {
            "quiet, and a question is on the screen: blocked".to_owned()
        } else if phrase.is_some() {
            "quiet; the question on the screen was answered already: idle".to_owned()
        } else {
            "quiet, no question on the screen: idle".to_owned()
        };
        serde_json::json!({
            "argv": argv,
            "agent": found.as_ref().map(|(name, _)| name),
            "known_by": found.map(|(_, by)| by),
            "reported": self.reported.map(AgentState::name),
            "quiet_ms": since.as_millis() as u64,
            "question": phrase,
            "answered": self.answered.get(),
            "session": self.session,
            "transcript": self.transcript,
            "state": state.map(AgentState::name),
            "why": why,
        })
    }

    /// Notes input from the user or another agent: a question on the screen
    /// counts as answered.
    pub fn note_input(&self) {
        if agent::asks_question(self.screen().screen()) {
            self.answered.set(true);
        }
    }

    /// The name to show for this pane: the agent, else the foreground program.
    /// The agent running on another machine that this pane shows, see
    /// The account its agent runs in, when not `standard`.
    pub fn account(&self) -> Option<String> {
        self.accounts.get(&self.program()?).cloned()
    }

    /// What the sidebar tells about its agent: model and context, and the
    /// account when not `standard`.
    /// What the program says it does, from its terminal title: Claude
    /// Code puts a summary of its task there. Spinners and the like in
    /// front are cut off; a title that only names the program is none.
    pub fn title(&self) -> String {
        let title = self.parser.lock().unwrap().title().unwrap_or_default();
        clean_title(&title, self.program().as_deref())
    }

    pub fn meta_label(&self) -> String {
        let parts: Vec<String> = self
            .meta()
            .map(crate::agent::Meta::label)
            .into_iter()
            .chain(self.account())
            .collect();
        parts.join(" · ")
    }

    /// `remote`.
    pub fn remote_agent(&self) -> Option<(String, AgentState)> {
        self.remote.lock().unwrap().clone()
    }

    /// Tells the terminals showing this pane elsewhere what runs in it.
    pub fn tell_taps(&self, bytes: &[u8]) {
        let _parser = self.parser.lock().unwrap();
        self.taps.lock().unwrap().retain(|tap| tap.send(bytes));
    }

    pub fn program(&self) -> Option<String> {
        if let Some((agent, _)) = self.remote_agent() {
            return Some(agent);
        }
        let argv = self.foreground_argv();
        if let Some(agent) = agent::agent_name(&argv) {
            return Some(agent);
        }
        let first = argv.first()?;
        Some(
            first
                .rsplit('/')
                .next()
                .unwrap_or(first)
                .trim_start_matches('-')
                .to_owned(),
        )
    }

    /// Why the agent stopped: what its hook said, or the error at the
    /// bottom of its screen.
    pub fn failure_reason(&self) -> &str {
        self.failure
            .as_deref()
            .or_else(|| agent::failure(self.screen().screen()))
            .unwrap_or("error")
    }

    /// The working directory of the program in the foreground of this pane,
    /// e.g. the shell, or nvim started from it. Read from `/proc`, so Linux
    /// only.
    pub fn cwd(&self) -> Option<PathBuf> {
        let pid = self.foreground_pid()?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// How far the view is scrolled back into the history, 0 when the live
    /// screen is shown.
    pub fn scroll_offset(&self) -> usize {
        self.parser.lock().unwrap().screen().scrollback()
    }

    /// Scrolls the view by `lines`, positive into the history. The offset is
    /// clamped to the history there is.
    pub fn scroll(&mut self, lines: isize) {
        let mut parser = self.parser.lock().unwrap();
        let screen = parser.screen_mut();
        let offset = screen.scrollback().saturating_add_signed(lines);
        screen.set_scrollback(offset);
    }

    pub fn scroll_to(&mut self, offset: usize) {
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_scrollback(offset);
    }

    /// The number of lines in the history above the live screen.
    pub fn history_len(&self) -> usize {
        history_len(&mut self.parser.lock().unwrap())
    }

    /// The text from `start` to `end` inclusive, both as (absolute row,
    /// column), where absolute row 0 is the oldest line of the history.
    /// Lines wrapped by the terminal are joined, trailing spaces dropped.
    pub fn text(&self, start: (usize, u16), end: (usize, u16)) -> String {
        text_between(&mut self.parser.lock().unwrap(), start, end)
    }

    /// The text of `count` rows from absolute row `first` on, trailing
    /// blanks cut: what a view of the pane shows.
    pub fn rows_from(&self, first: usize, count: usize) -> Vec<String> {
        let parser = self.parser.lock().unwrap();
        let (_, cols) = parser.size();
        (first..first + count)
            .map(|abs| {
                parser
                    .abs_row(abs, 0, cols)
                    .map(|(text, _)| text.trim_end().to_owned())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Every row of history and screen as text, by absolute row, for search.
    pub fn rows_text(&self) -> Vec<String> {
        let parser = self.parser.lock().unwrap();
        let (rows, cols) = parser.size();
        let total = parser.history_len() + usize::from(rows);
        (0..total)
            .map(|abs| {
                parser
                    .abs_row(abs, 0, cols)
                    .map(|(text, _)| text.trim_end().to_owned())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// The visible screen as text, or the last `lines` lines of history and
    /// screen together.
    pub fn read(&self, lines: Option<usize>) -> String {
        let Some(lines) = lines else {
            return self.screen().screen().contents();
        };
        let history = self.history_len();
        let (rows, cols) = self.screen().screen().size();
        let total = history + usize::from(rows);
        // The last lines with something on them: the empty rows under the
        // prompt do not count.
        let all = self.text((0, 0), (total - 1, cols));
        let all: Vec<&str> = all.trim_end().lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }

    /// The link at absolute cell `at`: a hyperlink the program marked, else
    /// a URL written there as text, also one that wraps onto the next row.
    pub fn link_at(&self, at: (usize, u16)) -> Option<String> {
        if let Some(url) = crate::links::link_at(&self.links.lock().unwrap(), at) {
            return crate::links::openable(url).then(|| url.to_owned());
        }
        let cols = self.size.1;
        let (row, col) = at;
        let mut line = self.text((row, 0), (row, cols));
        // A URL to the end of the row may go on in the next.
        if line.chars().count() >= usize::from(cols) && !line.ends_with(' ') {
            let next = self.text((row + 1, 0), (row + 1, cols));
            line.extend(next.chars().take_while(|c| !c.is_whitespace()));
        }
        crate::links::url_in(&line, usize::from(col))
    }

    /// Reads the transcript again if it changed since, and returns what it
    /// tells.
    pub fn refresh_meta(&mut self) -> Option<&crate::agent::Meta> {
        let path = self.transcript.as_ref()?;
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        if self.meta.as_ref().is_none_or(|(_, at)| *at != modified)
            && let Some(meta) = crate::agent::transcript_meta(path)
        {
            self.meta = Some((meta, modified));
        }
        self.meta.as_ref().map(|(meta, _)| meta)
    }

    /// What the agent's transcript told last.
    pub fn meta(&self) -> Option<&crate::agent::Meta> {
        self.meta.as_ref().map(|(meta, _)| meta)
    }

    /// When the program last wrote something.
    pub fn last_output(&self) -> Instant {
        *self.last_output.lock().unwrap()
    }

    pub fn screen(&self) -> MutexGuard<'_, crate::term::Parser> {
        self.parser.lock().unwrap()
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resizes the pty and the VT screen. A no-op when the size is unchanged.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if self.size == (rows, cols) {
            return Ok(());
        }
        self.size = (rows, cols);
        let mut parser = self.parser.lock().unwrap();
        parser.screen_mut().set_size(rows, cols);
        // What watches sees the old size: it connects again for the new.
        self.taps.lock().unwrap().retain(|tap| !tap.watch);
        drop(parser);
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }
}

/// See `Pane::text`.
fn text_between(
    parser: &mut crate::term::Parser,
    start: (usize, u16),
    end: (usize, u16),
) -> String {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let (_, cols) = parser.size();
    let mut out = String::new();
    for abs in start.0..=end.0 {
        let from = if abs == start.0 { start.1 } else { 0 };
        let to = if abs == end.0 {
            end.1.saturating_add(1).min(cols)
        } else {
            cols
        };
        let Some((line, wrapped)) = parser.abs_row(abs, from, to) else {
            continue;
        };
        if abs != end.0 && wrapped {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end());
            if abs != end.0 {
                out.push('\n');
            }
        }
    }
    out
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The number of lines in the history above the screen.
fn history_len(parser: &mut crate::term::Parser) -> usize {
    parser.history_len()
}

impl Drop for Pane {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A pipe whose write end stops a reader thread waiting on the read end.
/// Both ends are closed on exec.
fn stop_pipe() -> Result<(OwnedFd, File)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills both descriptors, which are then ours.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: as above.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

/// Waits until the pty has something to read, or has ended. False when
/// `wake` became readable instead, i.e. the reader is to stop.
fn wait_readable(pty: RawFd, wake: RawFd) -> bool {
    let mut fds = [
        libc::pollfd {
            fd: pty,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: fds is a valid array of two pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            // Reading tells what is wrong.
            return true;
        }
        if fds[1].revents != 0 {
            return false;
        }
        if fds[0].revents != 0 {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3 rows of screen, lines "line 0" .. "line 9" written, so 7 lines
    /// went into the history.
    fn parser_with_history() -> crate::term::Parser {
        let mut parser = crate::term::Parser::new(3, 20, 100);
        for i in 0..10 {
            parser.process(format!("line {i}").as_bytes());
            if i < 9 {
                parser.process(b"\r\n");
            }
        }
        parser
    }

    #[test]
    fn history_length_counts_scrolled_off_lines() {
        let mut parser = parser_with_history();
        assert_eq!(history_len(&mut parser), 7);
        assert_eq!(parser.screen().scrollback(), 0);
    }

    #[test]
    fn text_reaches_into_the_history() {
        let mut parser = parser_with_history();
        assert_eq!(text_between(&mut parser, (0, 0), (1, 19)), "line 0\nline 1");
        assert_eq!(
            text_between(&mut parser, (6, 5), (9, 5)),
            "6\nline 7\nline 8\nline 9"
        );
        // Reversed order works too, and the view stays where it was.
        assert_eq!(text_between(&mut parser, (2, 3), (2, 0)), "line");
        assert_eq!(parser.screen().scrollback(), 0);
    }
}

#[cfg(test)]
mod bench {
    #[test]
    #[ignore]
    fn reading_costs() {
        use std::time::Instant;
        let mut data = Vec::new();
        for i in 0..400_000 {
            data.extend_from_slice(
                format!("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx {i}\r\n")
                    .as_bytes(),
            );
        }
        let chunks: Vec<&[u8]> = data.chunks(8192).collect();
        let t = Instant::now();
        let mut parser = crate::term::Parser::new(50, 200, super::SCROLLBACK_LINES);
        for c in &chunks {
            parser.process(c);
        }
        eprintln!(
            "terminal {:?} for {} MB",
            t.elapsed(),
            data.len() / 1_000_000
        );
        let t = Instant::now();
        let mut s = crate::clipboard::Osc52Scanner::default();
        for c in &chunks {
            let _ = s.feed(c);
        }
        eprintln!("osc52 {:?}", t.elapsed());
        let t = Instant::now();
        let mut s = crate::graphics::Scanner::default();
        for c in &chunks {
            let _ = s.feed(c);
        }
        eprintln!("graphics {:?}", t.elapsed());
        let t = Instant::now();
        let mut s = crate::links::LinkScanner::default();
        for c in &chunks {
            let _ = s.feed(c);
        }
        eprintln!("links {:?}", t.elapsed());
        let mut short = Vec::new();
        for i in 0..2_000_000 {
            short.extend_from_slice(format!("{i}\r\n").as_bytes());
        }
        for scrollback in [super::SCROLLBACK_LINES, 1000] {
            let t = Instant::now();
            let mut parser = crate::term::Parser::new(48, 170, scrollback);
            for c in short.chunks(4096) {
                parser.process(c);
            }
            eprintln!(
                "terminal 2M short lines, scrollback {scrollback}: {:?}",
                t.elapsed()
            );
        }
    }
}

/// `title` without what is not words in front (spinners, ✳), empty when
/// all that is left names the program (`Claude Code`, `claude`).
pub fn clean_title(title: &str, program: Option<&str>) -> String {
    let text = title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_owned();
    let plain = text.to_lowercase();
    let names_program = program.is_some_and(|p| {
        let p = p.to_lowercase();
        plain == p
            || plain
                .strip_prefix(&p)
                .is_some_and(|rest| rest.trim() == "code")
    });
    if names_program || plain == "claude code" {
        String::new()
    } else {
        text
    }
}

#[cfg(test)]
mod title_tests {
    use super::clean_title;

    #[test]
    fn cleans_agent_titles() {
        assert_eq!(
            clean_title("⠐ Fix the archive bug", Some("claude")),
            "Fix the archive bug"
        );
        assert_eq!(clean_title("✳ Claude Code", Some("claude")), "");
        assert_eq!(clean_title("codex", Some("codex")), "");
        assert_eq!(clean_title("  ", Some("claude")), "");
        assert_eq!(clean_title("✶ 3 tests failing", None), "3 tests failing");
    }
}
