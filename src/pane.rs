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
use std::time::Instant;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::agent::{self, AgentState};
use crate::app::AppEvent;
use crate::graphics::{Advance, Chunk, PaneGraphics, Scanner};
use crate::layout::PaneId;

const SCROLLBACK_LINES: usize = 10_000;

pub struct Pane {
    id: PaneId,
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    last_output: Arc<Mutex<Instant>>,
    /// Images the program shows with the kitty graphics protocol.
    pub graphics: Arc<Mutex<PaneGraphics>>,
    /// The agent state the program reported with `imkr status`.
    pub reported: Option<AgentState>,
    /// The agent's session id from its hooks, e.g. for `claude --resume`.
    pub session: Option<String>,
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
}

impl Pane {
    /// Starts `spawn.command` in a new pty. Output is fed into the VT parser
    /// on a background thread, which sends `AppEvent::PtyOutput` after every
    /// chunk and `AppEvent::PtyExited(id)` at EOF.
    pub fn spawn(spawn: Spawn, events: Sender<AppEvent>) -> Result<Self> {
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

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn shell")?;
        // Drop our copy of the slave side, so the reader sees EOF once the
        // child exits.
        drop(pair.slave);
        Self::start(id, (rows, cols), pair.master, child, &[], events)
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
        let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_LINES);
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
            reported: None,
            session: None,
            name: None,
            answered: Cell::new(false),
            events,
            reader: None,
            released: false,
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
        let (wake, stop) = stop_pipe()?;
        let reader_parser = Arc::clone(&self.parser);
        let reader_last_output = Arc::clone(&self.last_output);
        let reader_graphics = Arc::clone(&self.graphics);
        let events = self.events.clone();
        let thread = thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut scanner = Scanner::default();
                let mut copies = crate::clipboard::Osc52Scanner::default();
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
                                        parser.process(&text);
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
                            drop(parser);
                            *reader_last_output.lock().unwrap() = Instant::now();
                            if events.send(AppEvent::PtyOutput).is_err() {
                                return;
                            }
                        }
                    }
                }
                let _ = events.send(AppEvent::PtyExited(id));
            })?;
        self.reader = Some(Reader { stop, thread });
        Ok(())
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
        let argv = self.foreground_argv();
        let since = self.last_output.lock().unwrap().elapsed();
        let asking = agent::asks_question(self.screen().screen());
        if !asking {
            self.answered.set(false);
        }
        agent::state(&argv, self.reported, since, asking && !self.answered.get())
    }

    /// Notes input from the user or another agent: a question on the screen
    /// counts as answered.
    pub fn note_input(&self) {
        if agent::asks_question(self.screen().screen()) {
            self.answered.set(true);
        }
    }

    /// The name to show for this pane: the agent, else the foreground program.
    pub fn program(&self) -> Option<String> {
        let argv = self.foreground_argv();
        if let Some(agent) = agent::agent_name(&argv) {
            return Some(agent.to_owned());
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

    /// Every row of history and screen as text, by absolute row, for search.
    pub fn rows_text(&self) -> Vec<String> {
        let mut parser = self.parser.lock().unwrap();
        let history = history_len(&mut parser);
        let screen = parser.screen_mut();
        let saved = screen.scrollback();
        let (rows, cols) = screen.size();
        let total = history + usize::from(rows);
        let mut out = vec![String::new(); total];
        let mut top = 0;
        // Page through the history a screen at a time.
        while top < total {
            screen.set_scrollback(history.saturating_sub(top));
            let view_top = history - screen.scrollback();
            for (i, row) in screen.rows(0, cols).enumerate() {
                if let Some(slot) = out.get_mut(view_top + i) {
                    *slot = row;
                }
            }
            top = view_top + usize::from(rows);
        }
        screen.set_scrollback(saved);
        out
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

    pub fn screen(&self) -> MutexGuard<'_, vt100::Parser> {
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
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
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
fn text_between(parser: &mut vt100::Parser, start: (usize, u16), end: (usize, u16)) -> String {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let history = history_len(parser);
    let screen = parser.screen_mut();
    let saved = screen.scrollback();
    let (rows, cols) = screen.size();

    let mut out = String::new();
    for abs in start.0..=end.0 {
        // Scroll so that this row is in view, then read it there.
        let offset = history.saturating_sub(abs);
        screen.set_scrollback(offset);
        let offset = screen.scrollback();
        let Some(view_row) = (abs + offset).checked_sub(history) else {
            continue;
        };
        let Ok(view_row) = u16::try_from(view_row) else {
            continue;
        };
        if view_row >= rows {
            continue;
        }
        let from = if abs == start.0 { start.1 } else { 0 };
        let to = if abs == end.0 {
            end.1.saturating_add(1).min(cols)
        } else {
            cols
        };
        let line = screen.contents_between(view_row, from, view_row, to);
        out.push_str(line.trim_end());
        if abs != end.0 && !screen.row_wrapped(view_row) {
            out.push('\n');
        }
    }
    screen.set_scrollback(saved);
    out
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The history length: vt100 clamps a too large offset to it.
fn history_len(parser: &mut vt100::Parser) -> usize {
    let screen = parser.screen_mut();
    let saved = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(saved);
    len
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
    fn parser_with_history() -> vt100::Parser {
        let mut parser = vt100::Parser::new(3, 20, 100);
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
