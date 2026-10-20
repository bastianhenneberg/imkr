//! The server's state and event loop. Pty output, client input and client
//! connections all come in over one channel, and after each batch of events
//! the screen is rendered for the attached client.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use crate::agent::{self, AgentState};
use crate::bindings::{self, Command, Menu};
use crate::config::{
    self, BarEdge, Bars, Config, Notifications, Placement, SETTINGS, Side, Spacing,
};
use crate::filetree::FileTree;
use crate::keys;
use crate::layout::{Axis, MIN_PANE_SIZE, PaneId};
use crate::menu;
use crate::pane::{Pane, Spawn};
use crate::persist::{self, Saved, SavedPane};
use crate::protocol::{self, Caps, ClientMsg, RenameTarget, Reply, Request, ServerMsg};
use crate::render::ScreenView;
use crate::sidebar::{self, state_style};
use crate::theme;
use crate::upgrade::{self, Handover};
use crate::workspace::Workspace;
use crate::{clipboard, mouse};

/// How long after a repeatable command (focus, resize) another arrow key
/// repeats it without pressing the prefix again. Same idea as tmux's
/// `repeat-time`.
const REPEAT_TIME: Duration = Duration::from_millis(600);

/// How often the server looks at agent states without other events.
const TICK: Duration = Duration::from_millis(500);
/// How long the session's name shows after a switch, see `draw_banner`.
const BANNER_FOR: Duration = Duration::from_millis(1200);
/// The shortest time between two frames drawn for program output alone.
const FRAME: Duration = Duration::from_millis(8);
/// How often agents, hooks, git and files are looked at while busy.
const UPKEEP: Duration = Duration::from_millis(100);

/// Seconds since 1970.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether `event` comes from the user or a client, to be drawn at once.
fn is_input(event: &AppEvent) -> bool {
    !matches!(event, AppEvent::PtyOutput)
}

/// The screen size assumed until a client tells us its real one.
const DEFAULT_SCREEN: Rect = Rect::new(0, 0, 80, 24);

pub type ClientId = u64;

pub enum AppEvent {
    PtyOutput,
    PtyExited(PaneId),
    /// A program put something into the clipboard (OSC 52), for the
    /// terminal of the client.
    Clipboard(Vec<u8>),
    ClientConnected(UnixStream),
    ClientMsg(ClientId, ClientMsg),
    /// A window moved here from another session, with its ptys, or why it
    /// could not be read.
    MoveIn(
        ClientId,
        std::result::Result<(upgrade::MovedWindow, Vec<OwnedFd>), String>,
    ),
    ClientGone(ClientId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// The prefix was pressed, the next key is a command in this menu.
    Prefix(Menu),
    /// The which-key menu, its entries picked with the arrows: the menu
    /// and the entry chosen, see `bindings::pick_keys`.
    Pick(Menu, usize),
    /// A repeatable command just ran from this menu, its keys repeat without
    /// the prefix until the deadline.
    Repeat(Instant, Menu),
    /// A destructive action waits for the user to confirm it with `y`.
    Confirm(Action),
    /// The key reference is shown, scrolled down by this many lines. j/k
    /// scroll, any other key closes it.
    Help(u16),
    /// The quit menu is open with the given entry selected.
    Menu(usize),
    /// The settings menu is open with the given setting selected.
    Settings(usize),
    /// Scrolling and selecting with the keyboard, see `App::copy`.
    Copy,
    /// Typing a name, see `App::prompt`.
    Prompt,
    /// The sidebar has the keyboard, the given clickable row is selected.
    Sidebar(usize),
    /// The pane picker is open, see `App::picker`.
    Picker,
    /// The session switcher is open with the given entry selected.
    Sessions(usize),
    /// The hives menu is open with the given hive selected.
    Hives(usize),
    /// The agents' accounts, see `account_items`.
    Accounts(usize),
    /// The account to start an agent in, see `AgentStart`.
    AgentAccount(usize),
    /// A jump like flash.nvim's, see `Jump`.
    Jump,
    /// Choosing the session to move this window to, that entry selected.
    MoveTo(u8, usize),
    /// The list of every agent, here and elsewhere, that one selected.
    Agents(usize),
    /// The agents waiting for the user or done, that one selected, its
    /// question below with the answer typed so far (`App::inbox_answer`).
    Inbox(usize),
    /// Choosing the machine for a new session, that one selected.
    NewSessionWhere(usize),
    /// Choosing the session, on any machine, for a new window.
    NewWindowWhere(usize),
    /// Choosing the template for the new session `App::new_session`.
    NewSessionTemplate(usize),
    /// The list of every window everywhere, that one selected.
    Windows(usize),
    /// The list of every pane everywhere, that one selected.
    Panes(usize),
    /// The marks, that one selected.
    Marks(usize),
    /// Reviewing the changes in `App::review`.
    Review,
    /// The notepad in `App::notepad` is open.
    Notes,
    /// The layouts for this window, that one selected.
    Layouts(usize),
    /// The menu of `App::context`, that item selected.
    Context(usize),
    /// What agents did since then (seconds since 1970), that one selected.
    Recap(u64, usize),
}

/// A pane's notepad while it is open (`Ctrl+B t`). The notes themselves
/// live in `Pane::notes`, so they are saved with the pane.
struct Notepad {
    pane: PaneId,
    /// The note selected with ↑↓; `None` while typing a new one.
    selected: Option<usize>,
    /// The line being typed.
    input: String,
}

/// A menu opened by a right click: where, and what it offers.
struct Context {
    area: Rect,
    title: String,
    items: Vec<(char, &'static str, Command)>,
}

/// A review open with `Ctrl+B v`: the diff, where the cursor is, the
/// notes so far, and the pane they go to.
struct Review {
    target: PaneId,
    root: PathBuf,
    lines: Vec<crate::review::Line>,
    cursor: usize,
    scroll: usize,
    notes: Vec<crate::review::Note>,
}

/// The pane picker: every pane in every workspace, filtered by what is
/// typed.
struct Picker {
    query: String,
    selected: usize,
    /// Everything, not only panes: the search of `Ctrl+B /`.
    global: bool,
    /// The hives, read once when it opens rather than on every frame.
    hives: Vec<persist::HiveInfo>,
}

/// Where an entry of the window or pane list leads.
#[derive(Clone)]
enum Spot {
    Window(u8),
    Pane(PaneId),
    /// A pane in another session, here or on another machine.
    Elsewhere(Place, PaneId),
}

/// A session in the session list before it becomes a line: where it is,
/// the hint, its agents by state, what it says when it does not run, and
/// how long its agents are quiet (`App::quiet_for`).
type SessionEntry = (
    Place,
    String,
    BTreeMap<String, usize>,
    Option<String>,
    Option<u64>,
);

/// An agent in the agent list before it becomes a line: its state, how
/// long, where it leads, its label, its session and its machine.
type AgentEntry = (
    AgentState,
    Duration,
    AgentAt,
    String,
    String,
    Option<String>,
);

/// A pane in the pane list before it becomes a line: where it leads, its
/// label, its agent's state, the hint and its machine.
type PaneEntry = (Spot, String, Option<AgentState>, String, Option<String>);

/// A window in the window list, before it becomes a line.
struct WindowEntry {
    spot: Spot,
    number: u8,
    name: String,
    states: Vec<AgentState>,
    programs: Vec<String>,
    /// Where it is in its machine: `shown`, `this session` or the session.
    at: String,
    /// Its machine, the group it is listed under.
    group: Option<String>,
}

/// The key of entry `i` in a list: 1 to 9, then none.
fn list_key_char(i: usize) -> char {
    if i < 9 {
        char::from_digit(i as u32 + 1, 10).unwrap_or(' ')
    } else {
        ' '
    }
}

/// How many agents are in each state, in `protocol::SESSION_ORDER`: "◆1 ●2".
fn state_counts(states: &[AgentState]) -> String {
    protocol::SESSION_ORDER
        .into_iter()
        .filter_map(|state| {
            let n = states.iter().filter(|s| **s == state).count();
            (n > 0).then(|| format!("{}{n}", state.symbol()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One thing found by the picker, and where Enter goes.
struct Found {
    /// What it is, e.g. `window`; empty in the pane picker.
    kind: &'static str,
    label: String,
    hint: String,
    style: Style,
    go: Go,
}

enum Go {
    Pane(PaneId),
    Window(u8),
    Place(Place),
    Agent(usize),
    Plugin(String),
    Hive(String),
    Mark(usize),
    Command(Command),
}

/// One pane in the picker.
struct PickerEntry {
    pane: PaneId,
    workspace: u8,
    label: String,
    state: Option<AgentState>,
    dir: String,
}

/// A line of text being typed.
struct Prompt {
    purpose: PromptFor,
    text: String,
}

#[derive(Clone, PartialEq, Eq)]
enum PromptFor {
    Rename(RenameTarget),
    /// A new name for the session called this.
    RenameSession(String),
    /// Searching the copy mode's pane, down or up.
    Search {
        forward: bool,
    },
    /// The name of a session to start and switch to, on that machine
    /// (`None`: this one).
    NewSession(Option<String>),
    /// The directory session `Place` starts in, from the template (one
    /// shell without).
    SessionDir(Place, Option<crate::template::Template>),
    /// The name to save the running sessions as a hive under.
    SaveHive,
    /// A new name for the hive called this.
    RenameHive(String),
    /// The session to move this window to.
    MoveWindow(u8),
    /// The branch of a new worktree window.
    Worktree,
    /// A note on the line under the review's cursor.
    ReviewNote,
    /// The name to save this window's layout as.
    SaveLayout,
    /// The name of a window just opened. Esc closes it again and goes back
    /// to the window it was opened from.
    NewWindow {
        window: u8,
        pane: PaneId,
        back: u8,
    },
}

/// A mouse drag in progress.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// Moving a floating pane by its title bar, grabbed `dx` cells from its
    /// left edge.
    Move { id: PaneId, dx: u16 },
    /// Resizing a floating pane by its bottom right corner.
    Resize { id: PaneId },
    /// Selecting text.
    Select,
    /// Moving the divider between tiled panes at this path in the layout.
    Divider,
}

/// Selected text in a pane, from `anchor` to `head`, as (absolute row,
/// column) where row 0 is the oldest line of the pane's history.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Selection {
    pane: PaneId,
    anchor: (usize, u16),
    head: (usize, u16),
}

/// The copy mode cursor, in the same coordinates as a selection.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CopyCursor {
    pane: PaneId,
    pos: (usize, u16),
}

/// A jump like flash.nvim's (`Mode::Jump`): what is typed so far and the
/// matches on screen, each with its label.
#[derive(Clone, Debug, Default)]
struct Jump {
    query: String,
    targets: Vec<JumpTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JumpTarget {
    pane: PaneId,
    /// Absolute row and column, as `CopyCursor::pos`.
    pos: (usize, u16),
    /// Where it shows: the pane's view row.
    row: u16,
    label: Option<char>,
}

/// A move by words in copy mode, see `word_motion`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    Next,
    Back,
    End,
}

/// How many rows around the cursor a move by words looks at.
const WORD_ROWS: usize = 50;

/// Where a move by words lands in `rows` from `at` (row, column in
/// characters): the start of the next word, of this or the one before, or
/// the end of this or the next. Words are runs of anything but blanks.
fn word_motion(rows: &[String], at: (usize, usize), motion: Word) -> Option<(usize, usize)> {
    // Every cell as (row, col, is part of a word), a blank between rows.
    let mut cells: Vec<(usize, usize, bool)> = Vec::new();
    for (r, text) in rows.iter().enumerate() {
        for (c, ch) in text.chars().enumerate() {
            cells.push((r, c, !ch.is_whitespace()));
        }
        cells.push((r, text.chars().count(), false));
    }
    let here = cells
        .iter()
        .position(|&(r, c, _)| (r, c) >= at)
        .unwrap_or(cells.len().saturating_sub(1));
    let word = |i: usize| cells.get(i).is_some_and(|cell| cell.2);
    let found = match motion {
        Word::Next => (here + 1..cells.len()).find(|&i| word(i) && !word(i - 1)),
        Word::End => (here + 1..cells.len()).find(|&i| word(i) && !word(i + 1)),
        Word::Back => (0..here)
            .rev()
            .find(|&i| word(i) && (i == 0 || !word(i - 1))),
    }?;
    Some((cells[found].0, cells[found].1))
}

/// The keys labels are taken from, the easy ones first.
const JUMP_LABELS: &str = "asdfghjklqwertyuiopzxcvbnm";

/// The entries of the quit menu, in order.
const MENU: [(char, MenuEntry); 3] = [
    ('d', MenuEntry::Detach),
    ('x', MenuEntry::ClosePane),
    ('Q', MenuEntry::EndSession),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuEntry {
    Detach,
    ClosePane,
    EndSession,
}

/// Actions that destroy something and are therefore confirmed first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ClosePane(PaneId),
    KillServer,
    /// End the session named in `App::confirm_target`.
    EndSession,
    /// Forget the stopped or closed session named in `App::confirm_target`.
    ForgetSession,
    /// Delete the hive named in `App::confirm_target`.
    DeleteHive,
    /// Save the running sessions over the hive named in `App::confirm_target`.
    OverwriteHive,
    /// Switch to the hive named in `App::confirm_target`, ending the
    /// sessions in `App::confirm_list`.
    SwitchHive,
    /// Start here the archived session `host:name` in `App::confirm_target`
    /// of a machine that does not answer.
    StartArchivedHere,
}

/// The sessions besides this one, here and on other machines, as the
/// watcher thread last found them.
#[derive(Clone, Default)]
struct Places {
    /// The other sessions on this machine.
    local: Vec<protocol::SessionInfo>,
    /// Every known host with its sessions, or why they could not be listed.
    hosts: Vec<(
        String,
        std::result::Result<Vec<protocol::SessionInfo>, String>,
    )>,
    /// Agents on other machines that came to need the user or finished,
    /// as notification title and text, for the server to pass on.
    alerts: Vec<(String, String)>,
    /// The sessions each host had when it last answered, to show them
    /// while it is off.
    last_seen: HashMap<String, Vec<String>>,
    /// Machines of the Tailscale network that are online, as suggestions.
    tailnet: Vec<String>,
    /// The devices that have `imkr web` of this machine open.
    web: Vec<crate::web::Seen>,
}

/// A session to switch to: on this machine (`host` is `None`) or another.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    host: Option<String>,
    name: String,
}

impl Place {
    /// The target `ServerMsg::Switch` carries.
    fn target(&self) -> String {
        match &self.host {
            Some(host) => format!("{host}:{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// A pane outside every window, shown over the active one on demand, e.g.
/// a plugin's TUI. It runs on while hidden.
struct Popup {
    pane: PaneId,
    title: String,
}

/// A pane marked for Alt+1 … Alt+9, like Harpoon: where it runs and how
/// to know it again after a restart gave it another id.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Mark {
    /// The machine, `None` for this one.
    host: Option<String>,
    session: String,
    pane: PaneId,
    window: u8,
    /// Its name or program then, e.g. `claude`.
    label: String,
    /// The agent's own session id, the surest way to find it again.
    #[serde(default)]
    agent: Option<String>,
}

/// Where the marks are kept: next to the session files, per machine.
fn marks_path() -> Option<PathBuf> {
    Some(persist::saves_dir().ok()?.with_file_name("marks.json"))
}

fn load_marks() -> Vec<Mark> {
    marks_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Where an agent in the agent list runs: a pane of this session, or the
/// one at this index of `App::other_agents`.
#[derive(Clone, Copy)]
enum AgentAt {
    Here(PaneId),
    Other(usize),
}

/// What the inbox lists: an agent, or a contact's message waiting to be
/// accepted (`contacts::waiting`).
#[derive(Clone, Copy)]
enum InboxAt {
    Agent(AgentAt),
    Message(u64),
}

/// An inbox entry, kept across the list's changes: an agent by its machine
/// (`None` for this one), session and pane, or a message by its number.
#[derive(Clone, PartialEq)]
enum InboxId {
    Agent(Option<String>, String, PaneId),
    Message(u64),
}

/// What the hooks compare: each pane's state (empty without an agent),
/// program and window.
type HookPanes = BTreeMap<PaneId, (String, String, u8)>;

/// How often the panes' history is kept for after a restart, and how many
/// lines of it.
const SCROLLBACK_EVERY: Duration = Duration::from_secs(20);
const SCROLLBACK_KEPT: usize = 3000;

/// What a pane restored after a restart starts with: its history as text,
/// then a line that says where the new shell begins.
fn restored_history(text: &str) -> Vec<u8> {
    let mut bytes = text.trim_end().replace('\n', "\r\n").into_bytes();
    bytes.extend_from_slice(b"\r\n\x1b[2m\xe2\x94\x80\xe2\x94\x80 restored after a restart \xe2\x94\x80\xe2\x94\x80\x1b[0m\r\n");
    bytes
}

/// How many lines of a waiting agent's screen the inbox shows.
const INBOX_LINES: usize = 12;

/// How often the sessions here and on other machines are listed again.
const LOCAL_EVERY: Duration = Duration::from_secs(2);
const HOSTS_EVERY: Duration = Duration::from_secs(5);
/// How often panes' frames look up which account their agent runs in.
const ACCOUNTS_EVERY: Duration = Duration::from_secs(10);
/// How often hives saved on other machines are fetched.
const HIVES_EVERY: Duration = Duration::from_secs(60);
/// How often messages from contacts through AI Brain are fetched.
const BRAIN_EVERY: Duration = Duration::from_secs(30);
/// How often a server makes sure `imkr web` runs, if switched on.
const WEB_EVERY: Duration = Duration::from_secs(60);

/// Where images are placed, by (image, placement): column, row, columns
/// and rows.
type Placements = HashMap<(u32, u32), (u16, u16, u16, u16)>;

/// An attached client. Rendering goes through a ratatui terminal whose
/// output is sent to the client instead of being written to a tty.
struct Client {
    id: ClientId,
    stream: UnixStream,
    /// Drawn at the shared screen size, `App::screen`, which is never larger
    /// than `size`.
    terminal: Terminal<CrosstermBackend<FrameSink>>,
    /// The size of the client's own terminal.
    size: Rect,
    /// What the client's terminal can do.
    caps: Caps,
    /// The title last sent to the client's terminal window.
    window_title: String,
    /// Images shown on the client now, by (image, placement), with where.
    shown_images: Placements,
    /// Send every image again: the client has none of them.
    resend_images: bool,
    /// The window this client shows, with `own_window_per_client`.
    workspace: u8,
}

impl Client {
    /// A terminal of exactly `screen`'s size. It never asks a tty for its
    /// size, the server has none, the client reports it instead.
    fn terminal_for(
        stream: &UnixStream,
        screen: Rect,
    ) -> io::Result<Terminal<CrosstermBackend<FrameSink>>> {
        let sink = FrameSink {
            stream: stream.try_clone()?,
            buf: Vec::new(),
        };
        Terminal::with_options(
            CrosstermBackend::new(sink),
            TerminalOptions {
                viewport: Viewport::Fixed(screen),
            },
        )
    }

    fn send(&mut self, msg: &ServerMsg) -> io::Result<()> {
        msg.write_to(&mut self.stream)
    }
}

/// Collects what ratatui writes and sends it to the client as one
/// `ServerMsg::Output` frame per flush.
struct FrameSink {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Write for FrameSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        ServerMsg::Output(std::mem::take(&mut self.buf)).write_to(&mut self.stream)
    }
}

pub struct App {
    panes: HashMap<PaneId, Pane>,
    /// The active workspace.
    ws: Workspace,
    /// The number of the active workspace.
    workspace: u8,
    /// The other workspaces. Their panes keep running, they are just not
    /// drawn.
    hidden: BTreeMap<u8, Workspace>,
    next_id: PaneId,
    /// The screen all attached clients share: as large as the smallest of
    /// them, like in tmux.
    screen: Rect,
    /// Where the panes are drawn, i.e. the screen minus the status bar.
    body: Rect,
    /// The attached clients. They all show the same and type into it.
    clients: Vec<Client>,
    /// How many clients are attached, for drawing: `render` takes them out
    /// of `clients` while it draws.
    attached: usize,
    /// The machine the active client sits at, if not this one, with its
    /// sessions: kept for drawing like `attached`.
    origin: Option<protocol::Origin>,
    /// The client whose input is handled now: it is the one detached by
    /// `Ctrl+B d`, sent on by a session switch and given the clipboard.
    active: Option<ClientId>,
    /// Every open connection, attached or not, to answer requests on.
    conns: HashMap<ClientId, UnixStream>,
    /// Connections showing a pane elsewhere, see `Request::Tap`.
    taps: HashMap<ClientId, PaneId>,
    /// Those of `taps` that only watch: their size is not the pane's.
    watchers: HashSet<ClientId>,
    /// The machine list (`Mode::NewSessionWhere`) picks where a window
    /// runs, not a session.
    hosting: bool,
    /// Since when agents were idle before an update or restart, seconds
    /// since 1970, by pane, until they are idle again here, for at most
    /// `CARRIED_FOR` (a resumed agent works a little first).
    carried: HashMap<PaneId, u64>,
    carried_until: Option<Instant>,
    /// When `name_accounts` last looked.
    accounts_named: Option<Instant>,
    /// What the terminals showing a pane elsewhere were last told runs in
    /// it, see `tell_taps`.
    told: HashMap<PaneId, Option<(String, AgentState)>>,
    /// `Ctrl+B S` leaves out old sessions, see `is_old_session`; `o` shows them.
    hide_old: bool,
    /// `Ctrl+B S` also lists the archive, `a` toggles it (#506).
    show_archive: bool,
    next_client_id: ClientId,
    /// The server socket, handed to every pane as `IMKR_SOCKET`.
    socket: PathBuf,
    /// What is typed after `/` in a list, which then shows only matches.
    list_query: Option<String>,
    /// The answer typed in the inbox for the agent selected there.
    inbox_answer: String,
    /// Agents' state changes as (seconds since 1970, pane, new state), the
    /// newest last, for the recap. `Done` stands for finishing a turn.
    activity: std::collections::VecDeque<(u64, PaneId, AgentState)>,
    /// Since when no terminal is attached, and since when the last recap
    /// looked.
    away_since: Option<u64>,
    recap_since: Option<u64>,
    /// The menu a right click opened, while `Mode::Context` shows it.
    context: Option<Context>,
    /// The review, kept with its notes while closed until they are sent.
    review: Option<Review>,
    /// The open notepad: whose, and what is selected or being typed.
    notepad: Option<Notepad>,
    /// The agent selected in the inbox. The list changes while it is open,
    /// answers must not go to whichever agent moved into its place.
    inbox_on: Option<InboxId>,
    /// The popups by name, and the one shown, if any, or shown last.
    popups: BTreeMap<String, Popup>,
    popup: Option<String>,
    popup_shown: bool,
    /// Popups fill everything but the bars, toggled with `Ctrl+B z`.
    popup_full: bool,
    /// The plugins, see `plugin`, shared with the thread that runs their
    /// status commands, and the files that are broken.
    plugins: Arc<Mutex<Vec<crate::plugin::Plugin>>>,
    /// This machine's own plugins; `plugins` adds the user's from their
    /// machine when they work here from another.
    own_plugins: Vec<crate::plugin::Plugin>,
    broken_plugins: Vec<crate::plugin::Broken>,
    plugin_status: crate::plugin::Statuses,
    /// The panes marked for Alt+1 … Alt+9, in that order.
    marks: Vec<Mark>,
    /// The other sessions, here and elsewhere, kept up to date by a thread.
    places: Arc<Mutex<Places>>,
    /// The hosts from the settings, for that thread.
    watched_hosts: Arc<Mutex<Vec<String>>>,
    /// Panes moved to another session: its socket and their id there, to
    /// pass on what their agents report to this socket.
    moved: HashMap<PaneId, (PathBuf, PaneId)>,
    /// The shell the session started with when nothing was there to
    /// restore, while it is the only pane. A window moved into a session
    /// started for it takes its place.
    first_shell: Option<PaneId>,
    /// The last agent state seen per pane, and since when it holds.
    agents: HashMap<PaneId, (AgentState, Instant)>,
    /// Agents that finished while the user was not looking, shown as done
    /// until their pane is focused.
    unseen: HashSet<PaneId>,
    /// Where the sidebar is drawn, if it is shown.
    sidebar: Option<Rect>,
    /// Focus mode: no bars, no sidebar, no borders, only the focused pane.
    focus_mode: bool,
    /// The server socket's listener, handed on by `imkr update`.
    listener: RawFd,
    /// The binary `imkr update` asked to become, once the reply is out.
    upgrade_to: Option<PathBuf>,
    /// Links from the session's old socket names to its socket, so panes
    /// started before a rename still reach it. Removed at the end.
    old_sockets: Vec<PathBuf>,
    /// The focused pane's repository, for the sidebar.
    git: Option<GitState>,
    /// The focused pane's project, for the sidebar.
    files: FileTree,
    /// How many rows the sidebar is scrolled down.
    sidebar_scroll: usize,
    /// The focused pane as last seen, and the one before it, for `Ctrl+B ;`.
    current_focus: Option<PaneId>,
    last_focus: Option<PaneId>,
    /// Where the layout is saved to survive a restart, see `persist`.
    state_path: Option<PathBuf>,
    /// The Omarchy theme in use when the theme was last applied.
    followed: Option<theme::Stamp>,
    /// What was saved last, to write only when something changed.
    last_saved: String,
    last_save_at: Instant,
    /// What the hooks last saw: each pane's state, program and window, and
    /// the window shown. `None` until the first look, which runs none.
    hook_seen: Option<(HookPanes, u8)>,
    /// When each pane's history was last kept for after a restart.
    scrollback_at: HashMap<PaneId, Instant>,
    last_scrollback_at: Instant,
    events: Sender<AppEvent>,
    title: String,
    config: Config,
    /// Shown in the settings menu: where the config was saved, or why it
    /// could not be loaded or saved.
    config_note: Option<String>,
    mode: Mode,
    drag: Option<Drag>,
    /// The divider being dragged, see `Layout::divider_at`.
    divider: Vec<bool>,
    selection: Option<Selection>,
    copy: Option<CopyCursor>,
    /// The jump being typed, see `Jump`.
    jump: Option<Jump>,
    /// A short message for the control bar, e.g. what was copied. Cleared by
    /// the next key.
    flash: Option<String>,
    /// What `draw_banner` shows, since when.
    banner: Option<(String, crate::config::BannerAt, Instant)>,
    /// The session or hive a confirmed `Action` is about, by name: a list
    /// index could point elsewhere by the time `y` is pressed.
    confirm_target: Option<String>,
    /// What a confirmation lists, e.g. the sessions switching hives ends.
    confirm_list: Vec<String>,
    /// The session named last, while its template is chosen.
    new_session: Option<Place>,
    /// Messages for agents that are busy or asking something: pane and
    /// text, typed in once the agent is idle.
    mailbox: Vec<(PaneId, String)>,
    /// When each pane last got a message, so the next one waits a little.
    mail_sent: HashMap<PaneId, Instant>,
    prompt: Option<Prompt>,
    /// The accounts new shells start with, by agent, see `account`.
    accounts: BTreeMap<String, String>,
    /// Agents ending to start again in another account: the command to
    /// type once the shell is back in front, and since when it waits.
    switching: HashMap<PaneId, (String, Instant)>,
    /// Agents waiting for the account they start in (`Mode::AgentAccount`).
    agent_start: Option<AgentStart>,
    /// The template's agents of another kind, asked for after `agent_start`.
    waiting_agents: Option<AgentStart>,
    /// When an agent's used up account was last noticed, so that the
    /// message still on its screen does not count again.
    used_up_seen: HashMap<PaneId, Instant>,
    /// The directories listed on other machines for the directory prompt,
    /// by machine and directory; `None` while ssh is still asking.
    dir_lists: DirLists,
    /// The directory chosen with the arrows under the directory prompt.
    dir_pick: Option<usize>,
    picker: Option<Picker>,
    /// The last copy mode search and its direction, for n and N.
    search: Option<(String, bool)>,
    /// When and where the left button went down last, to spot double clicks.
    last_click: Option<(Instant, Position)>,
    quit: bool,
}

impl App {
    /// Starts with one pane and runs until the last pane is closed or the
    /// server is told to shut down.
    pub fn run(
        events: Sender<AppEvent>,
        rx: &Receiver<AppEvent>,
        socket: PathBuf,
        listener: RawFd,
        handover: Option<Handover>,
    ) -> Result<()> {
        let (config, config_error) = Config::load();
        let config_note = config_error
            .as_ref()
            .map(|e| format!("config ignored: {e}"))
            .or_else(|| config.hook_problems());
        if let Some(e) = &config_error {
            eprintln!("config: {e}");
        }
        let body = screen_layout(DEFAULT_SCREEN, &config.bars, &config.spacing).body;

        let mut app = App {
            panes: HashMap::new(),
            ws: Workspace::empty(),
            workspace: 1,
            hidden: BTreeMap::new(),
            next_id: 0,
            screen: DEFAULT_SCREEN,
            body,
            clients: Vec::new(),
            attached: 0,
            origin: None,
            active: None,
            taps: HashMap::new(),
            watchers: HashSet::new(),
            hosting: false,
            carried: HashMap::new(),
            carried_until: None,
            accounts_named: None,
            told: HashMap::new(),
            hide_old: true,
            show_archive: false,
            conns: HashMap::new(),
            next_client_id: 0,
            socket,
            moved: HashMap::new(),
            places: Arc::default(),
            marks: load_marks(),
            list_query: None,
            inbox_answer: String::new(),
            review: None,
            notepad: None,
            context: None,
            activity: Default::default(),
            away_since: None,
            recap_since: None,
            inbox_on: None,
            popups: BTreeMap::new(),
            popup: None,
            popup_shown: false,
            popup_full: false,
            plugins: Arc::default(),
            own_plugins: Vec::new(),
            broken_plugins: Vec::new(),
            plugin_status: Arc::default(),
            watched_hosts: Arc::default(),
            first_shell: None,
            agents: HashMap::new(),
            unseen: HashSet::new(),
            sidebar: None,
            focus_mode: false,
            listener,
            upgrade_to: None,
            old_sockets: Vec::new(),
            git: None,
            files: FileTree::default(),
            sidebar_scroll: 0,
            current_focus: None,
            last_focus: None,
            state_path: persist::path().ok(),
            followed: None,
            last_saved: String::new(),
            last_save_at: Instant::now(),
            scrollback_at: HashMap::new(),
            hook_seen: None,
            last_scrollback_at: Instant::now(),
            events,
            title: shell_name(),
            config,
            config_note,
            mode: Mode::Normal,
            drag: None,
            divider: Vec::new(),
            selection: None,
            copy: None,
            jump: None,
            flash: None,
            banner: None,
            confirm_target: None,
            confirm_list: Vec::new(),
            new_session: None,
            mailbox: Vec::new(),
            mail_sent: HashMap::new(),
            prompt: None,
            dir_lists: Arc::default(),
            accounts: BTreeMap::new(),
            switching: HashMap::new(),
            agent_start: None,
            waiting_agents: None,
            used_up_seen: HashMap::new(),
            dir_pick: None,
            picker: None,
            search: None,
            last_click: None,
            quit: false,
        };
        app.apply_theme();
        app.load_plugins();
        crate::plugin::watch_statuses(Arc::clone(&app.plugins), Arc::clone(&app.plugin_status));
        *app.watched_hosts.lock().unwrap() = app.config.hosts.clone();
        watch_places(Arc::clone(&app.places), Arc::clone(&app.watched_hosts));
        match handover {
            Some(handover) => app.adopt(handover),
            None => app.restore(),
        }
        if app.panes.is_empty() {
            app.start_fresh()?;
        }
        app.event_loop(rx);
        for mut client in app.clients.drain(..) {
            let _ = client.send(&ServerMsg::Exited);
        }
        for link in &app.old_sockets {
            let _ = std::fs::remove_file(link);
        }
        // Ended on purpose: kept in the save file `last` instead, from where
        // `imkr restore` brings it back. A session whose last shell
        // exited has nothing to keep.
        // Stopped with the machine: the saved layout stays as it was before
        // the shells died, and the next server brings it back.
        if crate::server::stopped() {
            return Ok(());
        }
        app.save_scrollback(true);
        if !app.panes.is_empty()
            && let Err(e) = persist::keep_in_last(&protocol::session_name(), app.saved())
        {
            eprintln!("could not keep the session in the last save: {e:#}");
        }
        // The session was ended on purpose, there is nothing to bring back.
        if let Some(path) = &app.state_path {
            persist::remove(path);
        }
        Ok(())
    }

    /// One shell in workspace 1.
    fn start_fresh(&mut self) -> Result<()> {
        // History of a session by this name before is no one's now.
        if let Some(path) = &self.state_path {
            persist::prune_scrollback(path, &[]);
        }
        let inner = pane_inner(self.body);
        let first = Pane::spawn(
            Spawn {
                id: 0,
                rows: inner.height,
                cols: inner.width,
                cwd: None,
                command: &[],
                socket: &self.socket,
                accounts: &self.accounts,
            },
            self.events.clone(),
        )?;
        self.panes.insert(0, first);
        self.ws = Workspace::new(0);
        self.workspace = 1;
        self.hidden.clear();
        self.next_id = 1;
        self.first_shell = Some(0);
        Ok(())
    }

    /// Brings back the layout saved before the server last died: every
    /// workspace, split and floating pane, each shell in its old directory,
    /// agents with their resume command typed in.
    fn restore(&mut self) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        let saved = match persist::load(&path) {
            Ok(Some(saved)) => saved,
            Ok(None) => return,
            Err(e) => {
                eprintln!("not restoring the session: {e:#}");
                return;
            }
        };
        self.carry_idle(&saved);
        self.accounts = saved.accounts.clone();

        let template = saved.template.clone();
        let resume_commands = self.config.resume_commands();
        // A template's agents ask for their account first, by agent, when
        // more than one is logged in (`ask_agent_account`).
        let ask_for = |command: &str| -> Option<String> {
            template.as_ref()?;
            let tool = command.split_whitespace().next()?;
            if !crate::account::TOOLS.contains(&tool) {
                return None;
            }
            let store = crate::account::Store::here().ok()?;
            let logged_in = store
                .of_tool(tool)
                .iter()
                .filter(|a| store.logged_in(a))
                .count();
            (logged_in > 1).then(|| tool.to_owned())
        };
        let mut asking: BTreeMap<String, Vec<(PaneId, String)>> = BTreeMap::new();
        let mut workspaces = saved.workspaces;
        for ws in workspaces.values_mut() {
            for (id, rect) in ws.rects(self.body) {
                let spec = saved.panes.get(&id).cloned().unwrap_or_default();
                let cwd = spec.cwd.as_deref().map(crate::template::home_dir);
                let inner = pane_inner(rect);
                let before = persist::read_scrollback(&path, id)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| restored_history(&text))
                    .unwrap_or_default();
                let spawned = Pane::spawn_after(
                    Spawn {
                        id,
                        rows: inner.height,
                        cols: inner.width,
                        cwd: cwd.as_deref(),
                        command: &[],
                        socket: &self.socket,
                        accounts: &spec.accounts,
                    },
                    &before,
                    self.events.clone(),
                );
                match spawned {
                    Ok(mut pane) => {
                        pane.notes = spec.notes.clone();
                        let resume = match self.config.resume {
                            config::Resume::Off => None,
                            _ => spec.resume_command(&resume_commands),
                        };
                        // A template's command always starts; a resume only
                        // with the user's Enter when they asked for that.
                        if resume.is_none()
                            && let Some(command) = &spec.command
                            && let Some(tool) = ask_for(command)
                        {
                            asking.entry(tool).or_default().push((id, command.clone()));
                        } else if let Some(command) = resume.clone().or(spec.command.clone()) {
                            let enter =
                                resume.is_none() || self.config.resume == config::Resume::Auto;
                            let line = if enter {
                                format!("{command}\r")
                            } else {
                                command
                            };
                            let _ = pane.write(line.as_bytes());
                        }
                        pane.session = spec.session.clone();
                        pane.name = spec.name.clone();
                        self.panes.insert(id, pane);
                    }
                    Err(e) => {
                        eprintln!("could not restore pane {id}: {e:#}");
                        ws.remove(id);
                    }
                }
            }
        }
        let kept: Vec<PaneId> = self.panes.keys().copied().collect();
        persist::prune_scrollback(&path, &kept);
        if !self.install(workspaces, saved.active) {
            return;
        }
        let mut asking = asking.into_iter().map(|(tool, panes)| AgentStart {
            tools: vec![tool],
            panes,
        });
        if let Some(first) = asking.next() {
            self.waiting_agents = asking.next();
            self.ask_agent_account(first);
        }
        let count = self.panes.len();
        let panes = if count == 1 { "pane" } else { "panes" };
        self.flash = Some(match template {
            Some(template) => format!("started {count} {panes} from template {template}"),
            None => format!("restored {count} {panes} from before the restart"),
        });
    }

    /// Shows `active` of `workspaces`, or the first if it is gone, and
    /// keeps the others hidden. False when no workspace has panes.
    fn install(&mut self, mut workspaces: BTreeMap<u8, Workspace>, active: u8) -> bool {
        workspaces.retain(|_, ws| !ws.is_empty());
        self.next_id = self.panes.keys().max().map_or(0, |id| id + 1);
        let active = if workspaces.contains_key(&active) {
            Some(active)
        } else {
            workspaces.keys().next().copied()
        };
        let Some(active) = active else {
            return false;
        };
        self.ws = workspaces.remove(&active).expect("checked above");
        self.workspace = active;
        self.hidden = workspaces;
        true
    }

    /// How long this session's agents have been quiet, in seconds: 0 while
    /// one works or waits, `None` without agents.
    fn quiet_for(&self) -> Option<u64> {
        let mut quiet: Option<u64> = None;
        for (state, since) in self.agents.values() {
            let secs = match state {
                AgentState::Working | AgentState::Blocked => 0,
                _ => since.elapsed().as_secs(),
            };
            quiet = Some(quiet.map_or(secs, |q| q.min(secs)));
        }
        quiet
    }

    /// Keeps since when the agents of `saved` were idle, see `carried`.
    fn carry_idle(&mut self, saved: &Saved) {
        self.carried = saved
            .panes
            .iter()
            .filter_map(|(&id, pane)| Some((id, pane.idle_since?)))
            .collect();
        self.carried_until = Some(Instant::now() + CARRIED_FOR);
    }

    /// Takes over from the server before `imkr update`: its panes with
    /// their processes and screens, its layout and its attached client.
    fn adopt(&mut self, handover: Handover) {
        self.screen = Rect::new(0, 0, handover.screen.0, handover.screen.1);
        self.focus_mode = handover.focus_mode;
        self.unseen.extend(handover.unseen.iter().copied());
        let saved = handover.saved;
        self.carry_idle(&saved);
        self.accounts = saved.accounts.clone();
        let mut handed = handover.panes;
        let mut workspaces = saved.workspaces;
        for ws in workspaces.values_mut() {
            for id in ws.panes() {
                let spec = saved.panes.get(&id).cloned().unwrap_or_default();
                match handed
                    .remove(&id)
                    .map(|p| Pane::adopt(id, &p, self.events.clone()))
                {
                    Some(Ok(mut pane)) => {
                        pane.session = spec.session;
                        pane.name = spec.name;
                        pane.accounts = spec.accounts;
                        pane.notes = spec.notes;
                        self.panes.insert(id, pane);
                    }
                    Some(Err(e)) => {
                        eprintln!("update: could not take over pane {id}: {e:#}");
                        ws.remove(id);
                    }
                    None => ws.remove(id),
                }
            }
        }
        for (name, id, title) in handover.popups {
            let Some(rest) = handed.remove(&id) else {
                continue;
            };
            match Pane::adopt(id, &rest, self.events.clone()) {
                Ok(pane) => {
                    self.panes.insert(id, pane);
                    self.popups.insert(name, Popup { pane: id, title });
                }
                Err(e) => eprintln!("update: could not take over popup {name}: {e:#}"),
            }
        }
        self.popup_full = handover.popup_full;
        if let Some((name, shown)) = handover.popup
            && self.popups.contains_key(&name)
        {
            self.popup = Some(name);
            self.popup_shown = shown;
        }
        // A pane without a place in the layout ends like a closed one.
        for (id, rest) in handed {
            if let Ok(pane) = Pane::adopt(id, &rest, self.events.clone()) {
                drop(pane);
            }
        }
        if !self.install(workspaces, saved.active) {
            return;
        }
        (_, self.body, self.sidebar) = self.areas(self.screen);
        let mut clients = handover.clients;
        // A server from before several clients hands over just one.
        if clients.is_empty()
            && let Some(fd) = handover.client
        {
            clients.push(upgrade::HandoverClient {
                fd,
                size: handover.screen,
                caps: handover.caps,
                workspace: 0,
            });
        }
        for handed in clients {
            let _ = upgrade::set_cloexec(handed.fd, true);
            // SAFETY: the server before handed this connection over.
            let stream = unsafe { UnixStream::from_raw_fd(handed.fd) };
            let id = self.next_client_id;
            if let Err(e) = self.connect(stream).and_then(|()| self.attach(id, false)) {
                eprintln!("update: could not keep a client: {e:#}");
                continue;
            }
            if let Some(client) = self.clients.iter_mut().find(|c| c.id == id) {
                client.caps = handed.caps;
                client.size = Rect::new(0, 0, handed.size.0, handed.size.1);
                if handed.workspace != 0 {
                    client.workspace = handed.workspace;
                }
            }
        }
        // Drawn afresh: the new terminal state knows nothing of what the
        // clients show.
        self.fit_screen(true);
        self.flash = Some(format!(
            "updated to imkr {}, {} panes kept",
            env!("CARGO_PKG_VERSION"),
            self.panes.len()
        ));
    }

    /// Becomes the binary at `exe` with the session kept, see `upgrade`.
    /// Returns only if that fails.
    fn upgrade(&mut self, exe: &std::path::Path) {
        let mut panes = BTreeMap::new();
        for (&id, pane) in &self.panes {
            let Some(handed) = pane.handover() else {
                self.flash = Some(format!("update: pane {id} cannot be handed over"));
                return;
            };
            panes.insert(id, handed);
        }
        let clients: Vec<_> = self
            .clients
            .iter()
            .map(|c| upgrade::HandoverClient {
                fd: c.stream.as_raw_fd(),
                size: (c.size.width, c.size.height),
                caps: c.caps.clone(),
                workspace: c.workspace,
            })
            .collect();
        let handover = Handover {
            listener: self.listener,
            client: clients.first().map(|c| c.fd),
            screen: (self.screen.width, self.screen.height),
            caps: clients.first().map(|c| c.caps.clone()).unwrap_or_default(),
            clients,
            popups: self
                .popups
                .iter()
                .map(|(name, p)| (name.clone(), p.pane, p.title.clone()))
                .collect(),
            popup: self.popup.clone().map(|name| (name, self.popup_shown)),
            popup_full: self.popup_full,
            focus_mode: self.focus_mode,
            unseen: self.unseen.iter().copied().collect(),
            saved: self.saved(),
            panes,
        };
        let e = upgrade::exec(exe, &self.socket, &handover);
        eprintln!("update failed: {e:#}");
        self.flash = Some(format!("update failed: {e:#}"));
    }

    /// The layout as `persist` saves it.
    fn saved(&self) -> Saved {
        let mut workspaces = self.hidden.clone();
        workspaces.insert(self.workspace, self.ws.clone());
        let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
        let panes = self
            .panes
            .iter()
            .filter(|(id, _)| !popups.contains(id))
            .map(|(&id, pane)| {
                let argv = pane.foreground_argv();
                let saved = SavedPane {
                    cwd: pane.cwd(),
                    agent: agent::agent_name(&argv),
                    session: pane.session.clone(),
                    name: pane.name.clone(),
                    command: hosted_command(&argv),
                    idle_since: self
                        .agents
                        .get(&id)
                        .filter(|(state, _)| *state == AgentState::Idle)
                        .map(|(_, since)| unix_now().saturating_sub(since.elapsed().as_secs())),
                    accounts: pane.accounts.clone(),
                    notes: pane.notes.clone(),
                };
                (id, saved)
            })
            .collect();
        let mut saved = Saved::new(self.workspace, workspaces, panes);
        saved.accounts = self.accounts.clone();
        saved
    }

    /// Saves the layout when it changed, at most once a second.
    fn save_state(&mut self) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        // Its panes are dying with it, a layout without them is not worth keeping.
        if crate::server::stopped() {
            return;
        }
        if self.last_save_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_save_at = Instant::now();

        let Ok(json) = persist::to_json(&self.saved()) else {
            return;
        };
        if json == self.last_saved {
            return;
        }
        match persist::write(&path, &json) {
            Ok(()) => self.last_saved = json,
            Err(e) => eprintln!("could not save the session: {e:#}"),
        }
    }

    /// Keeps the history of every pane that wrote something since the last
    /// time, every `SCROLLBACK_EVERY` or `now`, for `restore`. Files of
    /// panes gone are left: at a shutdown the shells may die before the
    /// server, `restore` drops what it does not need.
    fn save_scrollback(&mut self, now: bool) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        if !now && self.last_scrollback_at.elapsed() < SCROLLBACK_EVERY {
            return;
        }
        self.last_scrollback_at = Instant::now();
        let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
        for (&id, pane) in &self.panes {
            let written = self.scrollback_at.get(&id);
            if popups.contains(&id) || written.is_some_and(|&at| at >= pane.last_output()) {
                continue;
            }
            match persist::write_scrollback(&path, id, &pane.read(Some(SCROLLBACK_KEPT))) {
                Ok(()) => {
                    self.scrollback_at.insert(id, Instant::now());
                }
                Err(e) => eprintln!("could not keep the history of pane {id}: {e:#}"),
            }
        }
    }

    fn event_loop(&mut self, rx: &Receiver<AppEvent>) {
        let mut last_upkeep: Option<Instant> = None;
        while !self.quit && !crate::server::stopped() {
            (_, self.body, self.sidebar) = self.areas(self.screen);
            self.sync_sizes();
            // What looks at agents, git and files, at most every UPKEEP:
            // a burst of output would run it a thousand times a second.
            if last_upkeep.is_none_or(|at| at.elapsed() >= UPKEEP) {
                last_upkeep = Some(Instant::now());
                self.update_agents();
                self.check_hooks();
                self.refresh_git();
                self.refresh_files();
                self.follow_omarchy();
                self.pass_on_alerts();
                if *self.watched_hosts.lock().unwrap() != self.config.hosts {
                    *self.watched_hosts.lock().unwrap() = self.config.hosts.clone();
                }
                self.save_state();
                self.save_scrollback(false);
            }
            self.track_focus();
            self.sync_graphics();
            self.scroll_sidebar_to_selection();
            if self.config.renumber_windows {
                self.renumber_windows();
            }
            self.render();
            let rendered = Instant::now();

            // Block for the next event, then take everything else that is
            // already queued, so a burst of output costs one redraw.
            let event = match self.mode {
                Mode::Repeat(until, _) => {
                    match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                        Ok(event) => event,
                        Err(RecvTimeoutError::Timeout) => {
                            self.mode = Mode::Normal;
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Wake up now and then to notice agents going quiet.
                _ => match rx.recv_timeout(TICK) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => {
                        last_upkeep = None;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
            };
            let mut input = is_input(&event);
            self.handle(event);
            // Output alone waits for the rest of the frame, so programs that
            // print fast are drawn at FRAME, not at every read; what the
            // user does is drawn right away.
            loop {
                let next = if input {
                    rx.try_recv().map_err(|_| ())
                } else {
                    let left = FRAME.saturating_sub(rendered.elapsed());
                    rx.recv_timeout(left).map_err(|_| ())
                };
                let Ok(event) = next else { break };
                input |= is_input(&event);
                self.handle(event);
            }
            if input {
                last_upkeep = None;
            }
            if let Some(exe) = self.upgrade_to.take() {
                self.upgrade(&exe);
            }
        }
    }

    /// Gives every pty the size of the area inside its pane's border.
    fn sync_sizes(&mut self) {
        if let Some(id) = self.shown_popup() {
            let inner = pane_inner(self.popup_rect());
            if let Some(pane) = self.panes.get_mut(&id) {
                let _ = pane.resize(inner.height, inner.width);
            }
        }
        for (id, rect) in self.rects_of(&self.ws) {
            let inner = self.inner(rect);
            if let Some(pane) = self.panes.get_mut(&id).filter(|p| !p.tapped())
                && let Err(e) = pane.resize(inner.height, inner.width)
            {
                eprintln!("failed to resize pane {id}: {e:#}");
            }
        }
    }

    /// Draws the screen for every attached client. A client that cannot be
    /// written to anymore is dropped.
    fn render(&mut self) {
        if self.clients.is_empty() {
            return;
        }
        // Asked before taking the clients out, which makes them look detached.
        let graphics = self.graphics_enabled();
        self.attached = self.clients.len();
        self.origin = self.newest_origin();
        self.merge_plugins();
        let mut clients = std::mem::take(&mut self.clients);
        let resend = clients.iter().any(|c| c.caps.graphics && c.resend_images);
        let (data, _) = self.image_state(graphics, resend);
        let before = clients.len();
        // The active client last, so its window is the one shown after.
        let back = self.workspace;
        clients.sort_by_key(|c| Some(c.id) == self.active);
        clients.retain_mut(|client| {
            client.workspace = self.view(client.workspace);
            if client.terminal.draw(|frame| self.draw(frame)).is_err() {
                return false;
            }
            if graphics && client.caps.graphics {
                let wanted = self.image_places(graphics);
                let mut images = data.clone();
                placements(&mut images, &client.shown_images, &wanted);
                client.shown_images = wanted;
                client.resend_images = false;
                if !images.is_empty() && client.send(&ServerMsg::Output(images)).is_err() {
                    return false;
                }
            }
            // The terminal window's title names the workspace and the pane.
            let title = self.window_title_text();
            if title != client.window_title {
                let clean: String = title.chars().filter(|c| !c.is_control()).collect();
                let osc = format!("\x1b]2;{clean}\x07").into_bytes();
                if client.send(&ServerMsg::Output(osc)).is_err() {
                    return false;
                }
                client.window_title = title;
            }
            true
        });
        self.view(back);
        let lost = clients.len() < before;
        self.clients = clients;
        if lost {
            self.fit_screen(false);
        }
    }

    /// Shows window `n` for drawing or handling a client's input, when
    /// each client has its own; the window shown then. A window gone
    /// since leaves the current one.
    fn view(&mut self, n: u8) -> u8 {
        if self.config.own_window_per_client && n != self.workspace && self.hidden.contains_key(&n)
        {
            self.show(n);
        }
        self.workspace
    }

    /// After handling anything: the client typing now sees what it made
    /// the current window, others keep theirs.
    fn follow_active(&mut self) {
        let n = self.workspace;
        let all = !self.config.own_window_per_client;
        let active = self.active;
        for client in &mut self.clients {
            if all || Some(client.id) == active {
                client.workspace = n;
            }
        }
    }

    fn handle(&mut self, event: AppEvent) {
        let result = match event {
            AppEvent::PtyOutput => Ok(()),
            AppEvent::PtyExited(id) => {
                self.close(id);
                Ok(())
            }
            AppEvent::Clipboard(sequence) => {
                self.send_active(&sequence);
                self.flash = Some("copied to the clipboard".into());
                Ok(())
            }
            AppEvent::ClientConnected(stream) => self.connect(stream),
            AppEvent::ClientMsg(_, ClientMsg::KillServer) => {
                self.quit = true;
                Ok(())
            }
            AppEvent::ClientMsg(id, ClientMsg::Attach { alone }) => self.attach(id, alone),
            AppEvent::ClientMsg(id, ClientMsg::Caps(caps)) => {
                if let Some(text) = caps.banner.clone() {
                    self.banner = Some((text, caps.banner_at, Instant::now()));
                }
                if let Some(client) = self.client_mut(id) {
                    // Images the client does not know yet go out again.
                    client.resend_images |= caps.graphics && !client.caps.graphics;
                    client.caps = caps;
                }
                Ok(())
            }
            AppEvent::ClientMsg(id, ClientMsg::Request(Request::Tap { pane, watch, size })) => {
                self.tap(id, pane, watch, size)
            }
            AppEvent::ClientMsg(id, ClientMsg::Input(bytes)) => {
                if let Some(pane) = self.taps.get(&id).and_then(|p| self.panes.get_mut(p)) {
                    pane.note_input();
                    pane.write(&bytes)
                } else {
                    Ok(())
                }
            }
            AppEvent::ClientMsg(id, ClientMsg::Event(Event::Resize(cols, rows)))
                if self.taps.contains_key(&id) =>
            {
                let watching = self.watchers.contains(&id);
                match self.taps.get(&id).and_then(|p| self.panes.get_mut(p)) {
                    Some(pane) if !watching => pane.resize(rows, cols),
                    _ => Ok(()),
                }
            }
            AppEvent::ClientMsg(id, ClientMsg::Request(request)) => {
                let reply = self.request(request);
                match self.conns.get_mut(&id) {
                    Some(stream) => ServerMsg::Reply(reply).write_to(stream).map_err(Into::into),
                    None => Ok(()),
                }
            }
            AppEvent::ClientMsg(id, ClientMsg::Event(event)) if self.client_mut(id).is_some() => {
                self.active = Some(id);
                if let Some(n) = self.client_mut(id).map(|c| c.workspace) {
                    self.view(n);
                }
                self.handle_event(event)
            }
            AppEvent::ClientMsg(..) => Ok(()),
            AppEvent::MoveIn(id, window) => {
                let reply = window.and_then(|(window, fds)| self.move_in(window, fds));
                match self.conns.get_mut(&id) {
                    Some(stream) => ServerMsg::Reply(reply).write_to(stream).map_err(Into::into),
                    None => Ok(()),
                }
            }
            AppEvent::ClientGone(id) => {
                self.conns.remove(&id);
                if let Some(pane) = self.taps.remove(&id).and_then(|p| self.panes.get(&p)) {
                    pane.untap(id);
                }
                self.watchers.remove(&id);
                if self.take_client(id).is_some() {
                    self.fit_screen(false);
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            eprintln!("{e:#}");
        }
        self.follow_active();
    }

    /// Tells the terminals showing a pane elsewhere which agent runs in it
    /// and its state, when that changed, so the agent shows there as one.
    fn tell_taps(&mut self, seen: &HashMap<PaneId, (AgentState, Instant)>) {
        self.told
            .retain(|id, _| self.panes.get(id).is_some_and(Pane::tapped));
        for (&id, pane) in &self.panes {
            if !pane.tapped() {
                continue;
            }
            let now = seen
                .get(&id)
                .map(|&(state, _)| (pane.program().unwrap_or_else(|| "agent".into()), state));
            if self.told.get(&id) == Some(&now) {
                continue;
            }
            let agent = now
                .as_ref()
                .map(|(name, state)| (name.as_str(), state.name()));
            pane.tell_taps(&crate::links::remote_state(agent));
            self.told.insert(id, now);
        }
    }

    /// Shows pane `pane` on connection `id`, see `Request::Tap`.
    fn tap(
        &mut self,
        id: ClientId,
        pane: PaneId,
        watch: bool,
        size: Option<(u16, u16)>,
    ) -> Result<()> {
        let Some(stream) = self.conns.get_mut(&id) else {
            return Ok(());
        };
        let Some(shown) = self.panes.get_mut(&pane) else {
            return ServerMsg::Reply(Err(format!("no pane {pane}")))
                .write_to(stream)
                .map_err(Into::into);
        };
        if let Some((rows, cols)) = size.filter(|_| !watch) {
            shown.resize(rows, cols)?;
        }
        let (rows, cols) = shown.size();
        let reply = serde_json::json!({ "pane": pane, "rows": rows, "cols": cols });
        ServerMsg::Reply(Ok(reply)).write_to(stream)?;
        let mut stream = stream.try_clone()?;
        let (screen, output) = shown.tap(id, watch);
        self.taps.insert(id, pane);
        if watch {
            self.watchers.insert(id);
        }
        // The new terminal learns what runs in it with the next look.
        self.told.remove(&pane);
        thread::Builder::new()
            .name(format!("tap-{id}"))
            .spawn(move || {
                let mut send = |bytes| ServerMsg::Output(bytes).write_to(&mut stream);
                if send(screen).is_err() {
                    return;
                }
                for bytes in output {
                    if bytes.is_empty() {
                        // The pane ended: so does the window showing it.
                        let _ = ServerMsg::Exited.write_to(&mut stream);
                        break;
                    }
                    if send(bytes).is_err() {
                        return;
                    }
                }
                // Ended, or cut off for falling behind: the other side
                // connects again for a fresh screen.
                let _ = stream.shutdown(std::net::Shutdown::Both);
            })?;
        Ok(())
    }

    /// Starts reading from a new connection. It only shows the session once
    /// it sends `ClientMsg::Attach`, so scripts can make requests without
    /// taking the screen from the attached client.
    fn connect(&mut self, stream: UnixStream) -> Result<()> {
        let id = self.next_client_id;
        self.next_client_id += 1;

        let mut reader = stream.try_clone()?;
        let events = self.events.clone();
        thread::Builder::new()
            .name(format!("client-{id}"))
            .spawn(move || {
                while let Ok(Some(msg)) = ClientMsg::read_from(&mut reader) {
                    let event = match msg {
                        // The descriptors come right after the frame and
                        // must be taken off the socket before reading on.
                        ClientMsg::Move(json) => {
                            AppEvent::MoveIn(id, receive_window(&reader, &json))
                        }
                        msg => AppEvent::ClientMsg(id, msg),
                    };
                    if events.send(event).is_err() {
                        return;
                    }
                }
                let _ = events.send(AppEvent::ClientGone(id));
            })?;
        self.conns.insert(id, stream);
        Ok(())
    }

    /// Attaches connection `id`, next to the clients attached already, or
    /// in their place with `alone`. Its size comes with its first resize.
    fn attach(&mut self, id: ClientId, alone: bool) -> Result<()> {
        if self.client_mut(id).is_some() {
            return Ok(());
        }
        let Some(stream) = self.conns.get(&id) else {
            return Ok(());
        };
        let stream = stream.try_clone()?;
        if alone {
            let others: Vec<ClientId> = self.clients.iter().map(|c| c.id).collect();
            for other in others {
                self.detach(other);
            }
        }
        // A template's agents waiting for their account still ask.
        if self.clients.is_empty() && self.agent_start.is_none() {
            self.mode = Mode::Normal;
        }
        let terminal = Client::terminal_for(&stream, self.screen)?;
        self.clients.push(Client {
            id,
            stream,
            terminal,
            size: self.screen,
            caps: Caps::default(),
            window_title: String::new(),
            shown_images: HashMap::new(),
            resend_images: true,
            workspace: self.workspace,
        });
        self.run_hooks("attach", Vec::new());
        // Back after a while: what the agents did meanwhile.
        if self.clients.len() == 1
            && let Some(since) = self.away_since.take()
            && unix_now().saturating_sub(since) >= 60
        {
            self.recap_since = Some(since);
            if !self.recap_items(since).0.is_empty() {
                self.mode = Mode::Recap(since, 0);
            }
        }
        Ok(())
    }

    /// Detaches client `id`; the others stay.
    fn detach(&mut self, id: ClientId) {
        if let Some(mut client) = self.take_client(id) {
            let _ = client.send(&ServerMsg::Detached);
            self.fit_screen(false);
            self.run_hooks("detach", Vec::new());
        }
    }

    /// Runs the hooks of what changed since the last look: agent states,
    /// panes opened and closed, the window shown.
    fn check_hooks(&mut self) {
        let plugin_hooks = self
            .plugins
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.from.is_none() && !p.hooks.is_empty());
        if self.config.hooks.is_empty() && !plugin_hooks {
            self.hook_seen = None;
            return;
        }
        let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
        let now: HookPanes = self
            .panes
            .iter()
            .filter(|(id, _)| !popups.contains(id))
            .map(|(&id, pane)| {
                let state = self.shown_state(id).map(AgentState::name).unwrap_or("");
                let program = pane
                    .name
                    .clone()
                    .or_else(|| pane.program())
                    .unwrap_or_default();
                (
                    id,
                    (
                        state.to_owned(),
                        program,
                        self.workspace_of(id).unwrap_or(0),
                    ),
                )
            })
            .collect();
        let Some((seen, window)) = self.hook_seen.replace((now.clone(), self.workspace)) else {
            return;
        };
        let pane_env = |id: PaneId, (state, program, window): &(String, String, u8)| {
            vec![
                ("IMKR_HOOK_PANE", id.to_string()),
                ("IMKR_HOOK_PROGRAM", program.clone()),
                ("IMKR_HOOK_STATE", state.clone()),
                ("IMKR_HOOK_WINDOW", window.to_string()),
            ]
        };
        for (&id, info) in &now {
            match seen.get(&id) {
                None => self.run_hooks("pane-opened", pane_env(id, info)),
                Some(was) if was.0 != info.0 && !info.0.is_empty() => {
                    self.run_hooks(&info.0, pane_env(id, info));
                }
                Some(_) => {}
            }
        }
        for (&id, info) in &seen {
            if !now.contains_key(&id) {
                self.run_hooks("pane-closed", pane_env(id, info));
            }
        }
        if window != self.workspace {
            let env = vec![("IMKR_HOOK_WINDOW", self.workspace.to_string())];
            self.run_hooks("window", env);
        }
    }

    /// Starts the hooks `on` `event` in the background, unseen, with `env`.
    fn run_hooks(&self, event: &str, env: Vec<(&str, String)>) {
        use std::os::unix::process::CommandExt;
        let plugins = self.plugins.lock().unwrap().clone();
        let from_plugins = plugins
            .iter()
            .filter(|p| p.from.is_none())
            .flat_map(|p| p.hooks.iter());
        for hook in self
            .config
            .hooks
            .iter()
            .chain(from_plugins)
            .filter(|hook| hook.on == event)
        {
            let mut command = std::process::Command::new("sh");
            command
                .arg("-c")
                .arg(&hook.run)
                .env("IMKR_EVENT", event)
                .env("IMKR_SESSION", protocol::session_name())
                .env("IMKR_SOCKET", &self.socket)
                .envs(env.iter().map(|(k, v)| (*k, v)))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0);
            match command.spawn() {
                Ok(mut child) => {
                    thread::spawn(move || child.wait());
                }
                Err(e) => eprintln!("hook {:?} on {event}: {e}", hook.run),
            }
        }
    }

    /// Detaches the client whose input is handled now.
    fn detach_active(&mut self) {
        if let Some(id) = self.active {
            self.detach(id);
        }
    }

    fn client_mut(&mut self, id: ClientId) -> Option<&mut Client> {
        self.clients.iter_mut().find(|c| c.id == id)
    }

    fn take_client(&mut self, id: ClientId) -> Option<Client> {
        let i = self.clients.iter().position(|c| c.id == id)?;
        let client = self.clients.remove(i);
        if self.clients.is_empty() {
            self.away_since = Some(unix_now());
        }
        Some(client)
    }

    /// The agents that did something since `since`, waiting ones first:
    /// each with its panes and what it did when, as `13:02 ● · 13:20 ✦`.
    fn recap_items(&self, since: u64) -> (Vec<PaneId>, Vec<menu::MenuItem>) {
        let mut panes: Vec<PaneId> = Vec::new();
        for &(at, id, _) in &self.activity {
            if at >= since && !panes.contains(&id) && self.panes.contains_key(&id) {
                panes.push(id);
            }
        }
        panes.sort_by_key(|id| self.shown_state(*id).map(sidebar::urgency));
        let items = panes
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let state = self.shown_state(id);
                let program = self.pane_who(id).unwrap_or_default();
                let window = self.window_name_of(id).unwrap_or_default();
                let n = self.workspace_of(id).unwrap_or(0);
                let what: Vec<String> = self
                    .activity
                    .iter()
                    .filter(|&&(at, pane, _)| pane == id && at >= since)
                    .map(|&(at, _, state)| {
                        let time = protocol::chrono_like(at).unwrap_or_default();
                        format!("{time} {}", state.symbol())
                    })
                    .collect();
                // The last few, the line has room for no more.
                let what = what[what.len().saturating_sub(6)..].join(" · ");
                menu::MenuItem {
                    key: list_key_char(i),
                    title: format!(
                        "{} {n} {window} › {program}",
                        state.map_or(" ", AgentState::symbol)
                    ),
                    hint: what,
                    danger: state == Some(AgentState::Blocked),
                    apart: false,
                    color: state.and_then(|s| sidebar::state_style(s).fg),
                    group: None,
                    marks: Vec::new(),
                }
            })
            .collect();
        (panes, items)
    }

    /// Enter or a digit goes to that agent.
    fn recap_key(&mut self, key: KeyEvent, since: u64, selected: usize) {
        let (panes, _) = self.recap_items(since);
        let count = panes.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Recap(since, (selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Recap(since, (selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => c as usize - '1' as usize,
            _ => return,
        };
        self.mode = Mode::Normal;
        if let Some(&id) = panes.get(pick) {
            self.reveal(id);
        }
    }

    /// Opens the recap of what happened since the last time away, if
    /// anything did.
    fn open_recap(&mut self) {
        let Some(since) = self.recap_since.or(self.away_since) else {
            self.flash = Some("no time away yet to look back on".into());
            return;
        };
        if self.recap_items(since).0.is_empty() {
            self.flash = Some("no agent did anything while you were away".into());
            return;
        }
        self.mode = Mode::Recap(since, 0);
    }

    /// Sends `bytes` to every attached client, e.g. a bell.
    fn broadcast(&mut self, bytes: &[u8]) {
        for client in &mut self.clients {
            let _ = client.send(&ServerMsg::Output(bytes.to_vec()));
        }
    }

    /// Sends `bytes` to the client whose input is handled now, or to all if
    /// that one is gone.
    /// Has the user's terminal open `url`, on the machine it runs on.
    fn open_link(&mut self, url: String) {
        let active = self.active;
        let Some(client) = self.clients.iter_mut().find(|c| Some(c.id) == active) else {
            return;
        };
        if !client.caps.open {
            self.flash = Some("this terminal's imkr cannot open links yet, reattach".into());
            return;
        }
        let _ = client.send(&ServerMsg::Open(url.clone()));
        self.flash = Some(format!("opening {url}"));
    }

    fn send_active(&mut self, bytes: &[u8]) {
        let active = self.active;
        match self.clients.iter_mut().find(|c| Some(c.id) == active) {
            Some(client) => {
                let _ = client.send(&ServerMsg::Output(bytes.to_vec()));
            }
            None => self.broadcast(bytes),
        }
    }

    /// Makes the shared screen as large as the smallest client and redraws
    /// every client whose screen changed, or all with `all`. Clients larger
    /// than the screen get the rest filled with dots, as in tmux.
    fn fit_screen(&mut self, all: bool) {
        let Some(width) = self.clients.iter().map(|c| c.size.width).min() else {
            return;
        };
        let height = self
            .clients
            .iter()
            .map(|c| c.size.height)
            .min()
            .unwrap_or(0);
        let screen = Rect::new(0, 0, width, height);
        let changed = screen != self.screen;
        self.screen = screen;
        for client in &mut self.clients {
            if !all && !changed && client.terminal.get_frame().area() == screen {
                continue;
            }
            let mut out = b"\x1b[H\x1b[2J".to_vec();
            out.extend_from_slice(&outside(client.size, screen));
            // Rebuilt rather than resized: resizing makes ratatui ask the tty
            // for its size. A new terminal draws everything again.
            let rebuilt = client
                .send(&ServerMsg::Output(out))
                .and_then(|()| Client::terminal_for(&client.stream, screen));
            match rebuilt {
                Ok(terminal) => {
                    client.terminal = terminal;
                    client.shown_images.clear();
                    client.resend_images = true;
                }
                Err(e) => eprintln!("client {}: {e:#}", client.id),
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => self.paste(&text),
            Event::Resize(cols, rows) => self.resize_screen(cols, rows),
            Event::Mouse(event) => self.mouse(event),
            _ => Ok(()),
        }
    }

    /// Answers a request from `imkr status`, `list`, `send` or `new`.
    fn request(&mut self, request: Request) -> Reply {
        match request {
            // Needs the connection, see `handle`.
            Request::Tap { .. } => Err("a tap is asked for on its own connection".into()),
            Request::Status {
                pane,
                state,
                session,
                transcript,
            } => {
                // An agent in a pane that moved on still reports here.
                if let Some((socket, there)) = self.moved.get(&pane).cloned() {
                    let request = Request::Status {
                        pane: there,
                        state,
                        session,
                        transcript,
                    };
                    thread::spawn(move || crate::client::request_at(&socket, request));
                    return Ok(serde_json::json!({ "pane": pane, "moved": true }));
                }
                let p = self.panes.get_mut(&pane).ok_or(format!("no pane {pane}"))?;
                p.reported = state;
                if session.is_some() {
                    p.session = session;
                }
                if transcript.is_some() {
                    p.transcript = transcript;
                }
                if state.is_none() {
                    p.transcript = None;
                    p.meta = None;
                }
                Ok(serde_json::json!({ "pane": pane }))
            }
            Request::Notify { text, title, pane } => {
                let title = match (title, pane) {
                    (Some(title), _) => title,
                    (None, Some(id)) => self.pane_who(id).unwrap_or_else(|| "imkr".into()),
                    (None, None) => "imkr".into(),
                };
                self.flash = Some(format!("{title}: {text}"));
                self.send_notification(&title, &text);
                Ok(serde_json::json!({ "notified": true }))
            }
            Request::List => Ok(self.list()),
            Request::Rename { target, name } => self.rename(target, name),
            Request::Note { pane, text } => {
                let id = pane.unwrap_or(self.ws.focus);
                let pane = self.panes.get_mut(&id).ok_or(format!("no pane {id}"))?;
                if let Some(text) = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
                    pane.notes.push(crate::persist::Note { text, sent: false });
                }
                Ok(serde_json::json!({ "pane": id, "notes": pane.notes }))
            }
            Request::Popup {
                name,
                title,
                command,
                cwd,
            } => {
                self.toggle_popup(&name, title, &command, cwd.as_deref())
                    .map_err(|e| format!("{e:#}"))?;
                Ok(serde_json::json!({ "popup": name, "shown": self.popup_shown }))
            }
            Request::Places => {
                let hosts: Vec<protocol::HostInfo> = self
                    .places
                    .lock()
                    .unwrap()
                    .hosts
                    .iter()
                    .filter_map(|(host, sessions)| {
                        Some(protocol::HostInfo {
                            host: host.clone(),
                            sessions: sessions.as_ref().ok()?.clone(),
                        })
                    })
                    .collect();
                serde_json::to_value(hosts).map_err(|e| e.to_string())
            }
            Request::Operator => {
                if self.clients.is_empty() {
                    return Err("nobody is attached to this session".into());
                }
                // A client on this machine tells no origin: the user sits here.
                let origin = self.newest_origin();
                Ok(serde_json::json!({
                    "machine": origin
                        .as_ref()
                        .map_or_else(protocol::machine_name, |o| o.host.clone()),
                    "via": origin.as_ref().and_then(|o| o.via.clone()),
                    "here": origin.is_none(),
                }))
            }
            Request::OpenSessions => {
                self.open_sessions();
                Ok(serde_json::json!({}))
            }
            Request::NewWindow => {
                self.run_command(Command::NewWorkspace)
                    .map_err(|e| format!("{e:#}"))?;
                Ok(serde_json::json!({}))
            }
            Request::Reveal { pane } => {
                self.reveal(pane);
                Ok(serde_json::json!({ "pane": pane }))
            }
            Request::Flash { text } => {
                self.flash = Some(text);
                Ok(serde_json::json!({}))
            }
            Request::UseAccount { tool, name, pane } => {
                let store = crate::account::Store::here().map_err(|e| format!("{e:#}"))?;
                let account = store
                    .find(&tool, &name)
                    .ok_or(format!("no {tool} account {name}, see `imkr account`"))?;
                if !store.logged_in(&account) {
                    return Err(format!(
                        "{tool} account {name} is not logged in: `imkr account login {tool}:{name}`"
                    ));
                }
                match pane {
                    // For the session no entry means the default, so
                    // `standard` is kept when another one is the default.
                    None if name == store.default_of(&tool) => {
                        self.accounts.remove(&tool);
                        Ok(serde_json::json!({ "tool": tool, "account": name }))
                    }
                    None => {
                        self.accounts.insert(tool.clone(), name.clone());
                        Ok(serde_json::json!({ "tool": tool, "account": name }))
                    }
                    Some(id) => self.switch_account(id, &account),
                }
            }
            Request::ReloadConfig => {
                self.reload_config();
                Ok(serde_json::json!({}))
            }
            Request::ReloadPlugins => {
                self.load_plugins();
                Ok(serde_json::json!({ "plugins": self.own_plugins.len() }))
            }
            Request::Go { target } => {
                self.switch_session(target);
                Ok(serde_json::json!({}))
            }
            Request::MoveWindow { workspace, to } => {
                self.move_window(workspace.unwrap_or(self.workspace), &to)
            }
            Request::Message { to, text, from } => {
                let pane = self.addressee(&to)?;
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                // An address can be answered; the user in a terminal is no pane.
                let header = if from.contains(':') {
                    format!("[imkr message from {from}, answer with: imkr msg {from} <text>]")
                } else {
                    format!("[imkr message from {from}]")
                };
                self.mailbox.push((pane, format!("{header} {text}")));
                self.deliver_mail();
                let queued = self.mailbox.iter().any(|(p, _)| *p == pane);
                Ok(serde_json::json!({ "pane": pane, "queued": queued }))
            }
            Request::Read { pane, lines } => {
                let p = self.panes.get(&pane).ok_or(format!("no pane {pane}"))?;
                Ok(serde_json::json!({ "pane": pane, "text": p.read(lines) }))
            }
            Request::Send { pane, text, enter } => {
                self.type_into(pane, &text, enter)?;
                Ok(serde_json::json!({ "pane": pane }))
            }
            Request::New {
                command,
                float,
                workspace,
                cwd,
            } => self.new_pane(&command, float, workspace, cwd),
            Request::Worktree { name, command, cwd } => self.worktree_window(&name, &command, cwd),
            Request::Layout { name, save } => self
                .layout(&name, save)
                .map(|()| serde_json::json!({ "layout": name, "window": self.workspace })),
            Request::LastCommand { pane } => self
                .panes
                .get(&pane)
                .map(Pane::last_command)
                .ok_or(format!("no pane {pane}")),
            Request::Explain { pane } => self
                .panes
                .get(&pane)
                .map(Pane::explain)
                .ok_or(format!("no pane {pane}")),
            Request::Kill { pane } => {
                if !self.panes.contains_key(&pane) {
                    return Err(format!("no pane {pane}"));
                }
                let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
                if self.panes.keys().filter(|id| !popups.contains(id)).count() < 2 {
                    return Err("the last pane is not closed this way".into());
                }
                self.close(pane);
                Ok(serde_json::json!({ "pane": pane, "closed": true }))
            }
            Request::Snapshot => serde_json::to_value(self.saved()).map_err(|e| e.to_string()),
            Request::RenameSession { name } => {
                self.rename_session(&name)?;
                Ok(serde_json::json!({ "session": name }))
            }
            Request::Upgrade { exe } => {
                if !exe.is_file() {
                    return Err(format!("{} does not exist", exe.display()));
                }
                self.upgrade_to = Some(exe);
                Ok(serde_json::json!({ "panes": self.panes.len() }))
            }
        }
    }

    /// Every pane with its workspace, program, state and directory.
    fn list(&self) -> serde_json::Value {
        let mut panes = Vec::new();
        let mut all: Vec<(u8, &Workspace)> = self.hidden.iter().map(|(n, ws)| (*n, ws)).collect();
        all.push((self.workspace, &self.ws));
        all.sort_by_key(|(n, _)| *n);
        for (n, ws) in all {
            for (id, _) in ws.rects(self.body) {
                let Some(pane) = self.panes.get(&id) else {
                    continue;
                };
                panes.push(serde_json::json!({
                    "pane": id,
                    "workspace": n,
                    "floating": ws.is_floating(id),
                    "focused": n == self.workspace && id == self.ws.focus,
                    "program": pane.program(),
                    "state": self.shown_state(id).map(AgentState::name),
                    "since": self.agents.get(&id).map(|(_, since)| since.elapsed().as_secs()),
                    "cwd": pane.cwd(),
                    "session": pane.session,
                    "name": pane.name,
                    "workspace_name": ws.name,
                    "meta": pane.meta().map(agent::Meta::label),
                    "account": pane.account(),
                    "tail": matches!(
                        self.shown_state(id),
                        Some(AgentState::Blocked | AgentState::Done)
                    )
                    .then(|| agent::tail(pane.screen().screen(), INBOX_LINES)),
                }));
            }
        }
        serde_json::Value::Array(panes)
    }

    /// Types `text` into pane `pane`, then Enter if `enter`, as a paste if
    /// it has several lines and the program takes pastes.
    fn type_into(&mut self, pane: PaneId, text: &str, enter: bool) -> Result<(), String> {
        let p = self.panes.get_mut(&pane).ok_or(format!("no pane {pane}"))?;
        p.note_input();
        let bracketed = p.screen().screen().bracketed_paste();
        let mut bytes = Vec::new();
        if bracketed && text.contains('\n') {
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
        } else {
            bytes.extend_from_slice(text.as_bytes());
        }
        p.write(&bytes).map_err(|e| e.to_string())?;
        if enter {
            // A separate write, so programs see Enter as a key and
            // not as part of a paste.
            thread::sleep(Duration::from_millis(20));
            p.write(b"\r").map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The pane a message `to` means: its number, or the one pane with that
    /// name or program.
    fn addressee(&self, to: &str) -> Result<PaneId, String> {
        if let Ok(id) = to.parse::<PaneId>() {
            return self
                .panes
                .contains_key(&id)
                .then_some(id)
                .ok_or(format!("no pane {id}"));
        }
        let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
        let wanted = to.to_lowercase();
        let mut found: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(id, _)| !popups.contains(id))
            .filter(|(_, pane)| {
                pane.name.as_deref().map(str::to_lowercase).as_deref() == Some(wanted.as_str())
                    || pane.program().map(|p| p.to_lowercase()).as_deref() == Some(wanted.as_str())
            })
            .map(|(&id, _)| id)
            .collect();
        found.sort_unstable();
        match found.as_slice() {
            [id] => Ok(*id),
            [] => Err(format!("no pane named {to}, see `imkr agents`")),
            many => Err(format!(
                "{} panes are {to} ({}), name one with its number",
                many.len(),
                many.iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Types waiting messages into their panes: right away into a pane
    /// without an agent, into an agent once it is idle or done. Never while
    /// it works, and never into a question, which is the user's to answer.
    fn deliver_mail(&mut self) {
        let mail = std::mem::take(&mut self.mailbox);
        for (id, text) in mail {
            if !self.panes.contains_key(&id) {
                continue;
            }
            let ready = matches!(
                self.shown_state(id),
                None | Some(AgentState::Idle | AgentState::Done)
            );
            // One at a time: after a message the agent gets a few seconds to
            // start on it, and older messages for it go first.
            let settling = self
                .mail_sent
                .get(&id)
                .is_some_and(|at| at.elapsed() < Duration::from_secs(3));
            if !ready || settling || self.mailbox.iter().any(|(p, _)| *p == id) {
                self.mailbox.push((id, text));
                continue;
            }
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let mut bytes = Vec::new();
            if pane.screen().screen().bracketed_paste() {
                bytes.extend_from_slice(b"\x1b[200~");
                bytes.extend_from_slice(text.as_bytes());
                bytes.extend_from_slice(b"\x1b[201~");
            } else {
                bytes.extend_from_slice(text.as_bytes());
            }
            if pane.write(&bytes).is_err() {
                continue;
            }
            thread::sleep(Duration::from_millis(20));
            let _ = pane.write(b"\r");
            self.mail_sent.insert(id, Instant::now());
            self.flash = Some(format!("✉ message for pane {id} delivered"));
        }
        self.mail_sent
            .retain(|_, at| at.elapsed() < Duration::from_secs(3));
    }

    /// Starts `command` in a new pane: floating or split off the focused one,
    /// in workspace `n` or the active one. Hidden workspaces stay hidden.
    /// A right click: a window tab's menu, or a pane's, unless the program
    /// in it uses the mouse itself (Shift+right click opens it anyway).
    fn open_context(&mut self, event: MouseEvent) -> Result<()> {
        let pos = Position::new(event.column, event.row);
        let (title, items): (String, Vec<(char, &'static str, Command)>) =
            if let Some(n) = self.tab_at(pos).filter(|&n| n > 0) {
                self.switch_workspace(n);
                (
                    format!(" Window {n} "),
                    vec![
                        ('r', "rename", Command::RenameWorkspace),
                        ('l', "layout…", Command::Layouts),
                        ('m', "move to session…", Command::MoveWorkspace),
                        ('c', "new window", Command::NewWorkspace),
                        ('t', "new window in a worktree…", Command::WorktreeWindow),
                        ('h', "new window on another machine…", Command::HostedWindow),
                    ],
                )
            } else if let Some((id, rect)) = self.pane_at(pos) {
                let inner = self.inner(rect);
                if self.forward_mouse(id, event, inner)? {
                    return Ok(());
                }
                self.ws.focus(id);
                (
                    format!(" Pane {id} "),
                    vec![
                        ('%', "split side by side", Command::SplitRow),
                        ('"', "split stacked", Command::SplitColumn),
                        ('z', "zoom", Command::Zoom),
                        ('f', "float / tile", Command::ToggleFloat),
                        ('r', "rename", Command::RenamePane),
                        ('>', "move to window…", Command::Open(Menu::PaneTo)),
                        ('!', "make it a popup", Command::BreakPopup),
                        ('x', "close", Command::ClosePane),
                    ],
                )
            } else {
                return Ok(());
            };
        let width = items
            .iter()
            .map(|(_, label, _)| label.chars().count())
            .max()
            .unwrap_or(0) as u16
            + 8;
        let height = items.len() as u16 + 2;
        let screen = self.screen;
        let area = Rect {
            x: pos.x.min(screen.width.saturating_sub(width)),
            y: pos.y.min(screen.height.saturating_sub(height)),
            width: width.min(screen.width),
            height: height.min(screen.height),
        };
        self.context = Some(Context { area, title, items });
        self.mode = Mode::Context(0);
        Ok(())
    }

    /// Runs the context menu's item `i` and closes it.
    fn run_context(&mut self, i: usize) -> Result<()> {
        let command = self
            .context
            .take()
            .and_then(|c| c.items.get(i).map(|item| item.2));
        self.mode = Mode::Normal;
        match command {
            Some(Command::Open(menu)) => {
                self.mode = Mode::Prefix(menu);
                Ok(())
            }
            Some(command) => self.run_command(command),
            None => Ok(()),
        }
    }

    fn context_key(&mut self, key: KeyEvent, selected: usize) -> Result<()> {
        let count = self.context.as_ref().map_or(1, |c| c.items.len().max(1));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.context = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Context((selected + count - 1) % count)
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Context((selected + 1) % count)
            }
            KeyCode::Enter => return self.run_context(selected),
            KeyCode::Char(c) => {
                let found = self
                    .context
                    .as_ref()
                    .and_then(|ctx| ctx.items.iter().position(|item| item.0 == c));
                if let Some(i) = found {
                    return self.run_context(i);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A click on an item runs it, anywhere else closes the menu.
    fn context_mouse(&mut self, event: MouseEvent) -> Result<()> {
        let pos = Position::new(event.column, event.row);
        let Some(area) = self.context.as_ref().map(|c| c.area) else {
            self.mode = Mode::Normal;
            return Ok(());
        };
        let inside = area.contains(pos) && pos.y > area.y && pos.y + 1 < area.bottom();
        match event.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) if inside => {
                self.mode = Mode::Context(usize::from(pos.y - area.y - 1));
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                return self.run_context(usize::from(pos.y - area.y - 1));
            }
            MouseEventKind::Down(_) => {
                self.context = None;
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn draw_context(&self, frame: &mut Frame, selected: usize) {
        let Some(context) = &self.context else {
            return;
        };
        let t = theme::current();
        let lines: Vec<Line> = context
            .items
            .iter()
            .enumerate()
            .map(|(i, (key, label, command))| {
                let mut style = if *command == Command::ClosePane {
                    Style::new().fg(t.danger)
                } else {
                    Style::new()
                };
                if i == selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                Line::from(vec![
                    Span::styled(format!(" {key}  "), Style::new().fg(t.accent)),
                    Span::styled(
                        format!(
                            "{label:<w$} ",
                            w = usize::from(context.area.width.saturating_sub(7))
                        ),
                        style,
                    ),
                ])
            })
            .collect();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.accent))
            .title(context.title.clone());
        frame.render_widget(Clear, context.area);
        frame.render_widget(Paragraph::new(lines).block(block), context.area);
    }

    /// The layouts for this window: the presets, then the saved ones.
    fn layout_items(&self) -> (Vec<String>, Vec<menu::MenuItem>) {
        let n = self.ws.layout.panes().len();
        let mut names = Vec::new();
        let mut items = Vec::new();
        for (name, what) in crate::shapes::PRESETS {
            names.push((*name).to_owned());
            items.push((name.to_string(), (*what).to_owned(), None));
        }
        for (name, shape) in crate::shapes::saved() {
            let places = shape.panes();
            let hint = if places == n {
                format!("saved, {places} panes")
            } else {
                format!("saved, {places} panes, this window has {n}")
            };
            names.push(name.clone());
            items.push((name, hint, Some("saved".to_owned())));
        }
        let items = items
            .into_iter()
            .enumerate()
            .map(|(i, (title, hint, group))| menu::MenuItem {
                key: list_key_char(i),
                title,
                hint,
                danger: false,
                apart: false,
                color: None,
                group,
                marks: Vec::new(),
            })
            .collect();
        (names, items)
    }

    fn layouts_key(&mut self, key: KeyEvent, selected: usize) {
        let (names, _) = self.layout_items();
        let count = names.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Layouts((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Layouts((selected + 1) % count);
                return;
            }
            KeyCode::Char('s') => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::SaveLayout,
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
                return;
            }
            KeyCode::Char('x') => {
                if let Some(name) = names.get(selected) {
                    self.flash = Some(match crate::shapes::remove(name) {
                        Ok(true) => format!("layout {name} removed"),
                        Ok(false) => "only saved layouts can be removed".into(),
                        Err(e) => format!("{e:#}"),
                    });
                }
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => c as usize - '1' as usize,
            _ => return,
        };
        self.mode = Mode::Normal;
        if let Some(name) = names.get(pick).cloned()
            && let Err(e) = self.layout(&name, false)
        {
            self.flash = Some(e);
        }
    }

    /// Gives the active window the layout `name`, or saves its own as
    /// `name` with `save`. Its panes take the places in order, places left
    /// get a new shell, panes left over go after the last; floating ones
    /// stay as they are.
    fn layout(&mut self, name: &str, save: bool) -> Result<(), String> {
        if name.is_empty() {
            return Err("a layout needs a name".into());
        }
        if save {
            let shape = crate::shapes::Shape::of(&self.ws.layout)
                .ok_or("this window has no tiled panes")?;
            crate::shapes::save(name, shape).map_err(|e| format!("{e:#}"))?;
            self.flash = Some(format!("layout {name} saved"));
            return Ok(());
        }
        let n = self.ws.layout.panes().len().max(1);
        let (shape, even) = crate::shapes::named(name, n).ok_or(format!("no layout {name}"))?;
        let focus = self.ws.focus;
        for _ in n..shape.panes() {
            self.new_pane(&[], false, Some(self.workspace), None)?;
        }
        let panes = self.ws.layout.panes();
        let layout = shape.build(&panes, even).ok_or("no panes to lay out")?;
        self.ws.layout = layout;
        self.ws.zoomed = None;
        if self.ws.contains(focus) {
            self.ws.focus(focus);
        }
        self.flash = Some(format!("layout {name}"));
        Ok(())
    }

    /// Opens the review of the focused pane's repository: its diff read
    /// anew, the notes not sent yet kept.
    fn open_review(&mut self) {
        let target = self.ws.focus;
        let Some(root) = self.git.as_ref().map(|git| git.root.clone()) else {
            self.flash = Some("the focused pane is in no git repository".into());
            return;
        };
        let lines = match crate::review::diff(&root) {
            Ok(lines) => lines,
            Err(e) => {
                self.flash = Some(format!("no diff: {e:#}"));
                return;
            }
        };
        if lines.is_empty() {
            self.flash = Some("no changes to review".into());
            return;
        }
        let notes = match self.review.take() {
            Some(old) if old.root == root => old.notes,
            _ => Vec::new(),
        };
        // The first changed line.
        let cursor = lines
            .iter()
            .position(|l| {
                matches!(
                    l.kind,
                    crate::review::Kind::Added | crate::review::Kind::Removed
                )
            })
            .unwrap_or(0);
        self.review = Some(Review {
            target,
            root,
            lines,
            cursor,
            scroll: 0,
            notes,
        });
        self.mode = Mode::Review;
    }

    /// Moves through the diff, notes lines, sends the notes.
    fn review_key(&mut self, key: KeyEvent) {
        use crate::review::Kind;
        let page = usize::from(self.body.height.saturating_sub(4)).max(1);
        let Some(review) = &mut self.review else {
            self.mode = Mode::Normal;
            return;
        };
        let last = review.lines.len().saturating_sub(1);
        let file_starts = |review: &Review| -> Vec<usize> {
            review
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.kind == Kind::File)
                .map(|(i, _)| i)
                .collect()
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Down | KeyCode::Char('j') => review.cursor = (review.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => review.cursor = review.cursor.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                review.cursor = (review.cursor + page).min(last)
            }
            KeyCode::PageUp => review.cursor = review.cursor.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => review.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => review.cursor = last,
            KeyCode::Char('n') => {
                if let Some(&next) = file_starts(review).iter().find(|&&i| i > review.cursor) {
                    review.cursor = next;
                }
            }
            KeyCode::Char('p') => {
                if let Some(&prev) = file_starts(review)
                    .iter()
                    .rev()
                    .find(|&&i| i < review.cursor)
                {
                    review.cursor = prev;
                }
            }
            KeyCode::Char('c') | KeyCode::Enter => {
                let text = review
                    .lines
                    .get(review.cursor)
                    .and_then(|line| review.notes.iter().find(|n| n.is_on(line)))
                    .map(|n| n.text.clone())
                    .unwrap_or_default();
                self.prompt = Some(Prompt {
                    purpose: PromptFor::ReviewNote,
                    text,
                });
                self.mode = Mode::Prompt;
            }
            KeyCode::Char('x') => {
                if let Some(line) = review.lines.get(review.cursor) {
                    review.notes.retain(|n| !n.is_on(line));
                }
            }
            KeyCode::Char('s') => {
                if review.notes.is_empty() {
                    self.flash = Some("no notes yet: c notes the line".into());
                    return;
                }
                let (target, count) = (review.target, review.notes.len());
                let text = crate::review::prompt(&review.notes);
                if !self.panes.contains_key(&target) {
                    self.flash = Some(format!("pane {target} is gone, the notes stay"));
                    return;
                }
                self.mailbox.push((target, text));
                self.review = None;
                self.mode = Mode::Normal;
                let notes = if count == 1 { "note" } else { "notes" };
                self.flash = Some(format!(
                    "{count} {notes} for pane {target}, typed in once it is idle"
                ));
            }
            _ => {}
        }
        // Keep the cursor in view.
        if let Some(review) = &mut self.review {
            if review.cursor < review.scroll {
                review.scroll = review.cursor;
            } else if review.cursor >= review.scroll + page {
                review.scroll = review.cursor + 1 - page;
            }
        }
    }

    /// Opens the focused pane's notepad: what to tell its agent later,
    /// written down without disturbing it.
    fn open_notes(&mut self) {
        let pane = self.ws.focus;
        if !self.panes.contains_key(&pane) {
            return;
        }
        self.notepad = Some(Notepad {
            pane,
            selected: None,
            input: String::new(),
        });
        self.mode = Mode::Notes;
    }

    /// Typing adds a note; with one picked (↑↓) Enter hands it to the
    /// pane's agent, `y` copies it, `e` takes it back into the line, `x`
    /// removes it.
    fn notes_key(&mut self, key: KeyEvent) {
        let Some(pad) = &mut self.notepad else {
            self.mode = Mode::Normal;
            return;
        };
        let Some(pane) = self.panes.get_mut(&pad.pane) else {
            self.notepad = None;
            self.mode = Mode::Normal;
            return;
        };
        let count = pane.notes.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (pad.selected, key.code) {
            // Closing keeps what was typed: nothing written down is lost.
            (_, KeyCode::Esc) => {
                let text = pad.input.trim().to_owned();
                if !text.is_empty() {
                    pane.notes.push(crate::persist::Note { text, sent: false });
                    self.flash = Some("the line is kept as a note".into());
                }
                self.notepad = None;
                self.mode = Mode::Normal;
            }
            (None, KeyCode::Up) if count > 0 => pad.selected = Some(count - 1),
            (None, KeyCode::Enter) => {
                let text = pad.input.trim().to_owned();
                if !text.is_empty() {
                    pane.notes.push(crate::persist::Note { text, sent: false });
                    pad.input.clear();
                }
            }
            (None, KeyCode::Backspace) => {
                pad.input.pop();
            }
            (None, KeyCode::Char(c)) if !ctrl => pad.input.push(c),
            (Some(i), KeyCode::Up | KeyCode::Char('k')) => pad.selected = Some(i.saturating_sub(1)),
            (Some(i), KeyCode::Down | KeyCode::Char('j')) => {
                pad.selected = (i + 1 < count).then_some(i + 1);
            }
            (Some(i), KeyCode::Enter) => {
                let (id, text) = (pad.pane, pane.notes[i].text.clone());
                pane.notes[i].sent = true;
                self.mailbox.push((id, text));
                self.flash = Some(format!(
                    "note for pane {id}, typed in once its agent is idle"
                ));
            }
            (Some(i), KeyCode::Char('y')) => {
                let text = pane.notes[i].text.clone();
                self.send_active(&clipboard::osc52(&text));
                self.flash = Some("note copied".into());
            }
            (Some(i), KeyCode::Char('e')) => {
                pad.input = pane.notes.remove(i).text;
                pad.selected = None;
            }
            (Some(i), KeyCode::Char('x') | KeyCode::Delete) => {
                pane.notes.remove(i);
                pad.selected = if pane.notes.is_empty() {
                    None
                } else {
                    Some(i.min(pane.notes.len() - 1))
                };
            }
            // Typing while a note is picked starts a new one.
            (Some(_), KeyCode::Char(c)) if !ctrl => {
                pad.selected = None;
                pad.input.push(c);
            }
            _ => {}
        }
    }

    /// The notepad at the bottom of its pane: the notes, sent ones struck
    /// through, the picked one marked, and the line being typed.
    fn draw_notes(&self, frame: &mut Frame, body: Rect) {
        let Some(pad) = &self.notepad else {
            return;
        };
        let Some(pane) = self.panes.get(&pad.pane) else {
            return;
        };
        let t = theme::current();
        let area = self
            .ws
            .rects(self.body)
            .into_iter()
            .find(|(id, _)| *id == pad.pane)
            .map_or(body, |(_, rect)| rect);
        // Rows the notes take at `width` once long ones wrap, plus the
        // line to type in and the frame.
        let wanted = |width: u16| -> u16 {
            let inner = usize::from(width.saturating_sub(2)).max(1);
            let rows: usize = pane
                .notes
                .iter()
                .map(|n| (n.text.chars().count() + 2).div_ceil(inner).max(1))
                .sum();
            (rows + 3).min(usize::from(u16::MAX)) as u16
        };
        let rect = match self.config.notes_at {
            // Along the right side, full height: the agent's output stays
            // in view on the left.
            config::NotesAt::Right => {
                let width = (area.width * 2 / 5).clamp(30.min(area.width), area.width);
                Rect {
                    x: area.x + area.width - width,
                    y: area.y,
                    width,
                    height: area.height,
                }
            }
            config::NotesAt::Middle => {
                let width = (area.width * 3 / 5).clamp(40.min(area.width), area.width);
                let height = wanted(width)
                    .clamp(5, (area.height * 3 / 5).max(5))
                    .min(area.height);
                Rect {
                    x: area.x + (area.width - width) / 2,
                    y: area.y + (area.height - height) / 2,
                    width,
                    height,
                }
            }
            config::NotesAt::Bottom => {
                let height = wanted(area.width)
                    .clamp(4, (area.height / 2).max(4))
                    .min(area.height);
                Rect {
                    x: area.x,
                    y: area.y + area.height - height,
                    width: area.width,
                    height,
                }
            }
        };
        let open = pane.notes.iter().filter(|n| !n.sent).count();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.warning))
            .title(format!(" ✎ notes · pane {} · {open} open ", pad.pane));
        let inner = block.inner(rect);
        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);
        let rows = usize::from(inner.height.saturating_sub(1));
        // The picked note stays in view; else the newest ones.
        let last = pad.selected.unwrap_or(pane.notes.len().saturating_sub(1));
        let first = (last + 1).saturating_sub(rows);
        let mut lines: Vec<Line> = pane
            .notes
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, note)| {
                let mut style = if note.sent {
                    Style::new()
                        .fg(t.subtle)
                        .add_modifier(Modifier::CROSSED_OUT)
                } else {
                    Style::new()
                };
                if pad.selected == Some(i) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                Line::from(vec![
                    Span::styled("• ", Style::new().fg(t.warning)),
                    Span::styled(note.text.clone(), style),
                ])
            })
            .collect();
        lines.push(Line::from(vec![
            Span::styled("› ", Style::new().fg(t.accent)),
            Span::raw(pad.input.clone()),
            Span::styled(
                if pad.selected.is_none() { "▏" } else { "" },
                Style::new().fg(t.accent),
            ),
        ]));
        frame.render_widget(
            Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
            inner,
        );
    }

    /// The diff over the panes: changed lines in the theme's colours, the
    /// cursor line marked, notes under their lines.
    fn draw_review(&self, frame: &mut Frame, body: Rect) {
        use crate::review::Kind;
        let Some(review) = &self.review else {
            return;
        };
        let t = theme::current();
        let name = review
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let notes = review.notes.len();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.accent))
            .title(format!(
                " ⬢ Review {name} · {notes} note{} for pane {} ",
                if notes == 1 { "" } else { "s" },
                review.target
            ));
        let inner = block.inner(body);
        frame.render_widget(Clear, body);
        frame.render_widget(block, body);
        let height = usize::from(inner.height);
        let mut lines: Vec<Line> = Vec::new();
        for (i, line) in review.lines.iter().enumerate().skip(review.scroll) {
            if lines.len() >= height {
                break;
            }
            let mut style = match line.kind {
                Kind::File => Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                Kind::Hunk => Style::new().fg(t.cyan),
                Kind::Added => Style::new().fg(t.success),
                Kind::Removed => Style::new().fg(t.danger),
                Kind::Meta => Style::new().fg(t.subtle),
                Kind::Context => Style::new(),
            };
            if i == review.cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let number = line
                .number
                .map_or("     ".to_owned(), |n| format!("{n:>5}"));
            lines.push(Line::from(vec![
                Span::styled(format!("{number} "), Style::new().fg(t.subtle)),
                Span::styled(line.text.clone(), style),
            ]));
            for note in review.notes.iter().filter(|n| n.is_on(line)) {
                lines.push(Line::styled(
                    format!("      ✎ {}", note.text),
                    Style::new().fg(t.warning).add_modifier(Modifier::ITALIC),
                ));
            }
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// Opens window `name` in git worktree `name` of the repository `cwd`
    /// (else the focused pane's directory) is in, running `command`, and
    /// shows it.
    fn worktree_window(&mut self, name: &str, command: &[String], cwd: Option<PathBuf>) -> Reply {
        let from = cwd
            .or_else(|| self.focused_cwd())
            .ok_or("no directory to start from")?;
        let dir = crate::worktree::create(&from, name).map_err(|e| format!("{e:#}"))?;
        let n = self.free_workspace().ok_or("all 9 windows are in use")?;
        let reply = self.new_pane(command, false, Some(n), Some(dir.clone()))?;
        if let Some(ws) = self.hidden.get_mut(&n) {
            ws.name = Some(name.to_owned());
        }
        self.switch_workspace(n);
        Ok(serde_json::json!({ "window": n, "dir": dir, "pane": reply["pane"] }))
    }

    fn new_pane(
        &mut self,
        command: &[String],
        float: bool,
        workspace: Option<u8>,
        cwd: Option<PathBuf>,
    ) -> Reply {
        // 0: a window of its own, the first free number.
        let n = match workspace {
            Some(0) => self.free_workspace().ok_or("all 9 windows are in use")?,
            n => n.unwrap_or(self.workspace),
        };
        if !(1..=9).contains(&n) {
            return Err(format!("no window {n}, there are 1 to 9"));
        }
        let cwd = cwd.or_else(|| self.focused_cwd());
        let id = self.next_id;
        self.next_id += 1;

        let body = self.body;
        let ws = if n == self.workspace {
            &mut self.ws
        } else {
            self.hidden.entry(n).or_insert_with(Workspace::empty)
        };
        if float {
            ws.add_float(id, body);
        } else if ws.is_empty() {
            *ws = Workspace::new(id);
        } else if ws.is_floating(ws.focus) || !ws.split(Axis::Row, id) {
            ws.add_float(id, body);
        }
        let rect = ws
            .rects(body)
            .into_iter()
            .find_map(|(pane, rect)| (pane == id).then_some(rect))
            .unwrap_or(body);

        let inner = pane_inner(rect);
        let spawned = Pane::spawn(
            Spawn {
                id,
                rows: inner.height,
                cols: inner.width,
                cwd: cwd.as_deref(),
                command,
                socket: &self.socket,
                accounts: &self.accounts,
            },
            self.events.clone(),
        );
        match spawned {
            Ok(pane) => {
                self.panes.insert(id, pane);
                Ok(serde_json::json!({ "pane": id, "workspace": n }))
            }
            Err(e) => {
                let ws = if n == self.workspace {
                    &mut self.ws
                } else {
                    self.hidden.get_mut(&n).expect("inserted above")
                };
                ws.remove(id);
                if n != self.workspace && ws.is_empty() {
                    self.hidden.remove(&n);
                }
                Err(format!("{e:#}"))
            }
        }
    }

    /// Looks at what every agent is doing. When one starts waiting for the
    /// user where the user is not looking, it rings the bell and says so.
    /// Names, for each pane's frame, the subscription or provider its agent
    /// runs in (`Pane::shown_account`). Read from the accounts' login files,
    /// so at most every `ACCOUNTS_EVERY`, and only for Claude and Codex
    /// running here: a hosted window's agent runs in an account there.
    fn name_accounts(&mut self) {
        if self
            .accounts_named
            .is_some_and(|at| at.elapsed() < ACCOUNTS_EVERY)
        {
            return;
        }
        self.accounts_named = Some(Instant::now());
        let Ok(store) = crate::account::Store::here() else {
            return;
        };
        let mut known: HashMap<(String, String), String> = HashMap::new();
        for pane in self.panes.values_mut() {
            let tool = pane.program().filter(|p| {
                crate::account::TOOLS.contains(&p.as_str()) && pane.remote_agent().is_none()
            });
            pane.shown_account = tool.and_then(|tool| {
                let name = pane
                    .accounts
                    .get(&tool)
                    .cloned()
                    .unwrap_or_else(|| crate::account::STANDARD.to_owned());
                if let Some(shown) = known.get(&(tool.clone(), name.clone())) {
                    return Some(shown.clone());
                }
                let account = store.find(&tool, &name)?;
                let shown = crate::account::shown(&store, &account);
                known.insert((tool, name), shown.clone());
                Some(shown)
            });
        }
    }

    fn update_agents(&mut self) {
        self.finish_switches();
        self.check_used_up();
        for pane in self.panes.values_mut() {
            pane.refresh_meta();
        }
        self.name_accounts();
        let now = Instant::now();
        if self.carried_until.is_some_and(|until| now > until) {
            self.carried.clear();
            self.carried_until = None;
        }
        let mut seen = HashMap::new();
        for (&id, pane) in &self.panes {
            let Some(state) = pane.agent_state() else {
                continue;
            };
            let since = match self.agents.get(&id) {
                Some(&(old, since)) if old == state => since,
                // Idle before an update or restart: since then.
                _ if state == AgentState::Idle
                    && let Some(at) = self.carried.remove(&id) =>
                {
                    now.checked_sub(Duration::from_secs(unix_now().saturating_sub(at)))
                        .unwrap_or(now)
                }
                _ => now,
            };
            seen.insert(id, (state, since));
        }
        for (&id, &(state, _)) in &seen {
            let was = self.agents.get(&id).map(|(s, _)| *s);
            let kept = match state {
                AgentState::Idle
                    if matches!(was, Some(AgentState::Working | AgentState::Blocked)) =>
                {
                    Some(AgentState::Done)
                }
                AgentState::Working | AgentState::Blocked if was != Some(state) => Some(state),
                _ => None,
            };
            if kept == Some(AgentState::Done) && self.config.checkpoints {
                self.checkpoint(id);
            }
            if let Some(kept) = kept {
                self.activity.push_back((unix_now(), id, kept));
                if self.activity.len() > 500 {
                    self.activity.pop_front();
                }
            }
            // Nobody looks at anything while detached.
            let in_view = !self.clients.is_empty() && self.ws.contains(id) && self.ws.focus == id;
            if state == AgentState::Idle
                && matches!(was, Some(AgentState::Working | AgentState::Blocked))
                && !in_view
            {
                self.unseen.insert(id);
                self.notify(id, "is done");
            }
            if state == AgentState::Blocked && was != Some(AgentState::Blocked) && !in_view {
                self.flash = Some(format!("pane {id} needs you · ^B a"));
                self.broadcast(b"\x07");
                self.notify(id, "needs you");
            }
        }
        self.unseen.retain(|id| seen.contains_key(id));
        self.tell_taps(&seen);
        self.deliver_mail();
        if !self.clients.is_empty() {
            self.unseen.remove(&self.ws.focus);
        }
        self.agents = seen;
    }

    /// Takes a checkpoint of the repository the agent in pane `id` works
    /// in, in the background; outside a repository nothing happens.
    fn checkpoint(&self, id: PaneId) {
        let Some(cwd) = self.panes.get(&id).and_then(|pane| pane.cwd()) else {
            return;
        };
        let name = format!("{}-{id}", protocol::session_name());
        let who = format!(
            "{} › {}",
            self.window_name_of(id).unwrap_or_default(),
            self.pane_who(id).unwrap_or_default()
        );
        let time = protocol::chrono_like(unix_now()).unwrap_or_default();
        let message = format!("imkr: {who}, turn done at {time}");
        thread::spawn(move || {
            if let Err(e) = crate::checkpoint::take(&cwd, &name, &message)
                && !e.to_string().contains("not a git repository")
            {
                eprintln!("checkpoint of pane {id}: {e:#}");
            }
        });
    }

    /// Tells the user outside imkr that the agent in pane `id` `what`,
    /// e.g. "needs you", the way the settings say.
    fn notify(&mut self, id: PaneId, what: &str) {
        let Some(who) = self.pane_who(id) else {
            return;
        };
        let cwd = self.panes.get(&id).and_then(|pane| pane.cwd());
        let place = match (self.workspace_of(id), cwd) {
            (Some(n), Some(cwd)) => format!("window {n} · {}", tilde(&cwd)),
            (Some(n), None) => format!("window {n}"),
            _ => String::new(),
        };
        self.send_notification(&format!("{who} {what}"), &place);
    }

    /// The name pane `id` goes by in notifications: its name, else its
    /// program.
    fn pane_who(&self, id: PaneId) -> Option<String> {
        let pane = self.panes.get(&id)?;
        Some(
            pane.name
                .clone()
                .or_else(|| pane.program())
                .unwrap_or_else(|| "agent".into()),
        )
    }

    /// Sends a notification with `title` and `body` the way the settings
    /// say: through `notify-send`, as OSC 9 to the terminal, or not at all.
    fn send_notification(&mut self, title: &str, body: &str) {
        match self.config.notifications {
            Notifications::Off => {}
            Notifications::Terminal => {
                let text = match body {
                    "" => format!("imkr: {title}"),
                    body => format!("imkr: {title} · {body}"),
                };
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                self.broadcast(format!("\x1b]9;{text}\x07").as_bytes());
            }
            Notifications::System => {
                let spawned = std::process::Command::new("notify-send")
                    .args(["--app-name=imkr", title, body])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                // Reaped on a thread of its own, so the loop never waits.
                if let Ok(mut child) = spawned {
                    thread::spawn(move || child.wait());
                }
            }
        }
    }

    /// The state to show for pane `id`: done while an idle agent has not
    /// been looked at.
    fn shown_state(&self, id: PaneId) -> Option<AgentState> {
        let (state, _) = self.agents.get(&id)?;
        if *state == AgentState::Idle && self.unseen.contains(&id) {
            Some(AgentState::Done)
        } else {
            Some(*state)
        }
    }

    /// The name of the window pane `id` is in, else the name of its
    /// directory, which is what the sidebar calls a window without one.
    fn window_name_of(&self, id: PaneId) -> Option<String> {
        let n = self.workspace_of(id)?;
        let ws = self.workspace_ref(n)?;
        ws.name.clone().or_else(|| {
            let cwd = self.panes.get(&ws.focus)?.cwd()?;
            Some(cwd.file_name()?.to_string_lossy().into_owned())
        })
    }

    /// The number of the workspace that contains pane `id`.
    /// This session's agent dots: each agent's state and window.
    fn own_dots(&self) -> Vec<(AgentState, Option<u8>)> {
        let mut dots: Vec<(AgentState, Option<u8>)> = self
            .panes
            .keys()
            .filter_map(|&id| Some((self.shown_state(id)?, self.workspace_of(id))))
            .collect();
        protocol::sort_dots(&mut dots);
        dots
    }

    fn workspace_of(&self, id: PaneId) -> Option<u8> {
        if self.ws.contains(id) {
            return Some(self.workspace);
        }
        self.hidden
            .iter()
            .find_map(|(n, ws)| ws.contains(id).then_some(*n))
    }

    /// Shows pane `id`, switching to its workspace if needed.
    fn reveal(&mut self, id: PaneId) {
        if !self.ws.contains(id) {
            let Some(n) = self.workspace_of(id) else {
                return;
            };
            self.show(n);
        }
        self.ws.focus(id);
    }

    /// What the sidebar shows.
    fn sidebar_data(&self) -> sidebar::Data {
        let dir_name = |id: PaneId| {
            self.panes
                .get(&id)
                .and_then(Pane::cwd)
                .and_then(|cwd| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default()
        };
        let workspaces = self
            .workspaces()
            .into_iter()
            .map(|n| {
                let ws = if n == self.workspace {
                    &self.ws
                } else {
                    &self.hidden[&n]
                };
                let states: Vec<AgentState> = ws
                    .panes()
                    .iter()
                    .filter_map(|id| self.shown_state(*id))
                    .collect();
                sidebar::WorkspaceRow {
                    number: n,
                    name: ws.name.clone().unwrap_or_else(|| dir_name(ws.focus)),
                    active: n == self.workspace,
                    state: states.iter().copied().min_by_key(|s| sidebar::urgency(*s)),
                    agents: states.len(),
                }
            })
            .collect();

        let now = Instant::now();
        let mut agents: Vec<sidebar::AgentRow> = self
            .agents
            .iter()
            .filter_map(|(&id, &(_, since))| {
                let pane = self.panes.get(&id)?;
                Some(sidebar::AgentRow {
                    pane: id,
                    program: pane
                        .name
                        .clone()
                        .or_else(|| pane.program())
                        .unwrap_or_default(),
                    workspace: self.workspace_of(id)?,
                    dir: self.window_name_of(id).unwrap_or_else(|| dir_name(id)),
                    state: self.shown_state(id)?,
                    since: now - since,
                    focused: id == self.ws.focus,
                    meta: pane.meta_label(),
                })
            })
            .collect();
        // Those that need the user first, and among them the longest waiting.
        agents.sort_by_key(|a| {
            (
                sidebar::urgency(a.state),
                std::cmp::Reverse(a.since),
                a.pane,
            )
        });

        let branch = self.focused_cwd().and_then(|cwd| sidebar::git_branch(&cwd));
        // Taken apart from the rest: `machine_label` below takes the lock.
        let phones = self
            .places
            .lock()
            .unwrap()
            .web
            .iter()
            .map(|seen| (seen.device.clone(), seen.viewing.clone()))
            .collect();
        sidebar::Data {
            phones,
            workspaces,
            sessions: self.session_rows(),
            other_agents: self
                .other_agents()
                .into_iter()
                .enumerate()
                .filter_map(|(index, (place, agent))| {
                    Some(sidebar::OtherAgentRow {
                        machine: self.machine_label(place.host.as_deref()),
                        session: place.name,
                        index,
                        program: agent.program,
                        workspace: agent.workspace,
                        dir: agent.dir,
                        state: AgentState::from_name(&agent.state)?,
                        since: Duration::from_secs(agent.since),
                    })
                })
                .collect(),
            here: self.machine_label(None),
            session: protocol::session_name(),
            popups: self.popup_rows(),
            agents,
            branch,
            changes: self
                .git
                .as_ref()
                .map(|g| g.changes.clone())
                .unwrap_or_default(),
            files: self.file_rows(),
        }
    }

    /// The sessions for the sidebar: those here, this one with its own
    /// agents, then every other machine's. Only running ones: those that
    /// stopped or were closed are in `Ctrl+B S`.
    fn session_rows(&self) -> Vec<sidebar::SessionRow> {
        let current = protocol::session_name();
        let places = self.places.lock().unwrap().clone();
        let mut here = places.local.clone();
        let states: Vec<AgentState> = self
            .panes
            .keys()
            .filter_map(|id| self.shown_state(*id))
            .collect();
        let mut own = protocol::SessionInfo {
            name: current.clone(),
            ..Default::default()
        };
        for state in states {
            *own.agents.entry(state.name().to_owned()).or_default() += 1;
        }
        here.push(own);
        // What runs first, then what can come back.
        here.sort_by(|a, b| (a.dormant.is_some(), &a.name).cmp(&(b.dormant.is_some(), &b.name)));
        let row = |host: Option<String>, info: &protocol::SessionInfo| sidebar::SessionRow {
            current: host.is_none() && info.name == current,
            machine: self.machine_label(host.as_deref()),
            dots: if host.is_none() && info.name == current {
                self.own_dots()
            } else {
                info.dots()
            },
            host,
            name: info.name.clone(),
            problem: None,
            offline: false,
            dormant: info.dormant.is_some(),
        };
        let mut rows: Vec<sidebar::SessionRow> = here.iter().map(|info| row(None, info)).collect();
        if let Some(origin) = self.origin.clone() {
            for info in origin_infos(&origin) {
                rows.push(row(Some(origin.host.clone()), &info));
            }
        }
        for (host, sessions) in &self.known_hosts() {
            match sessions {
                // Nothing runs there: said so, not shown as a session.
                Ok(sessions) if sessions.iter().all(protocol::SessionInfo::is_stand_in) => {
                    rows.push(sidebar::SessionRow {
                        host: Some(host.clone()),
                        machine: self.machine_label(Some(host)),
                        name: String::new(),
                        current: false,
                        dots: Vec::new(),
                        problem: Some("no session yet".into()),
                        offline: false,
                        dormant: false,
                    });
                }
                Ok(sessions) => {
                    rows.extend(sessions.iter().map(|info| row(Some(host.clone()), info)));
                }
                Err(e) => {
                    rows.push(sidebar::SessionRow {
                        host: Some(host.clone()),
                        machine: self.machine_label(Some(host)),
                        name: String::new(),
                        current: false,
                        dots: Vec::new(),
                        problem: Some(short_problem(e)),
                        offline: true,
                        dormant: false,
                    });
                    // What ran there when it last answered, greyed out.
                    let seen = places.last_seen.get(host).cloned().unwrap_or_default();
                    for name in seen {
                        rows.push(sidebar::SessionRow {
                            host: Some(host.clone()),
                            machine: self.machine_label(Some(host)),
                            name,
                            current: false,
                            dots: Vec::new(),
                            problem: None,
                            offline: true,
                            dormant: false,
                        });
                    }
                }
            }
        }
        rows.retain(|row| !row.dormant);
        rows
    }

    /// This machine's name as the user knows it: the name their client
    /// reached it by, else its hostname.
    fn own_name(&self) -> String {
        self.origin
            .as_ref()
            .and_then(|o| o.via.clone())
            .unwrap_or_else(protocol::hostname)
    }

    /// Every other machine with its sessions: those this server asks
    /// itself, then those only the client's own machine knows. Neither
    /// this machine nor the client's.
    fn known_hosts(
        &self,
    ) -> Vec<(
        String,
        std::result::Result<Vec<protocol::SessionInfo>, String>,
    )> {
        let mut hosts = self.places.lock().unwrap().hosts.clone();
        if let Some(origin) = &self.origin {
            // The machine the user sits at shows as ⌂ already; listed here
            // as well (hosts both ways), its sessions came twice.
            hosts.retain(|(host, _)| *host != origin.host);
            let here = protocol::machine_name();
            for info in &origin.hosts {
                let known = hosts.iter().any(|(h, _)| *h == info.host);
                let itself = info.host == here || origin.via.as_deref() == Some(&info.host);
                if !known && !itself && info.host != origin.host {
                    hosts.push((info.host.clone(), Ok(info.sessions.clone())));
                }
            }
        }
        hosts
    }

    /// Tells about agents on other machines the way it tells about its own:
    /// the bar, a bell when one waits, and a notification.
    fn pass_on_alerts(&mut self) {
        let alerts = std::mem::take(&mut self.places.lock().unwrap().alerts);
        for (title, body) in alerts {
            if title.ends_with("needs you") {
                self.broadcast(b"\x07");
            }
            self.flash = Some(format!("{title} · {body}"));
            self.send_notification(&title, &body);
        }
    }

    /// The agents of the other sessions, here and elsewhere: grouped by
    /// session, those that need the user first.
    fn other_agents(&self) -> Vec<(Place, protocol::AgentInfo)> {
        let local = self.places.lock().unwrap().local.clone();
        let hosts = self.known_hosts();
        let mut groups: Vec<(Option<String>, &protocol::SessionInfo)> =
            local.iter().map(|s| (None, s)).collect();
        // The machine the user sits at, as their client last told.
        let origin = self.origin.clone();
        if let Some(origin) = &origin {
            for session in &origin.infos {
                groups.push((Some(origin.host.clone()), session));
            }
        }
        for (host, sessions) in &hosts {
            for session in sessions.iter().flatten() {
                groups.push((Some(host.clone()), session));
            }
        }
        // Agents shown here in a window of their own are this session's.
        let hosted: Vec<(String, String, PaneId)> = self
            .panes
            .values()
            .filter_map(|pane| hosted_target(&pane.foreground_argv()))
            .collect();
        let mut out = Vec::new();
        for (host, session) in groups {
            let mut agents = session.agent_panes.clone();
            agents.retain(|agent| {
                !host.as_ref().is_some_and(|host| {
                    hosted
                        .iter()
                        .any(|(h, s, p)| h == host && *s == session.name && *p == agent.pane)
                })
            });
            agents.sort_by_key(|a| AgentState::from_name(&a.state).map_or(9, sidebar::urgency));
            for agent in agents {
                let place = Place {
                    host: host.clone(),
                    name: session.name.clone(),
                };
                out.push((place, agent));
            }
        }
        out
    }

    /// Switches to the agent in row `i` of the other sessions' agents and
    /// shows its pane.
    fn go_to_other_agent(&mut self, i: usize) {
        let Some((place, agent)) = self.other_agents().into_iter().nth(i) else {
            return;
        };
        if place.host.is_some() && !self.active_caps().is_some_and(|caps| caps.hosts) {
            self.flash =
                Some("this terminal's imkr is too old for other machines, reattach".into());
            return;
        }
        // Clients from before only know sessions, not panes in them.
        if self.active_caps().is_some_and(|caps| caps.panes) {
            self.switch_session(format!("{}#{}", place.target(), agent.pane));
        } else {
            self.switch_session(place.target());
        }
    }

    /// Machine `host` (`None`: this one) as the user sees it: `⌂` for the
    /// one their client runs on, `⇄` for the others. `None` for this one
    /// when they sit at it and no other machine is known: nothing to tell
    /// apart then.
    fn machine_label(&self, host: Option<&str>) -> Option<String> {
        let home = self.origin.as_ref().map(|o| o.host.as_str());
        match (host, home) {
            (None, None) if self.places.lock().unwrap().hosts.is_empty() => None,
            (None, None) => Some(format!("⌂ {}", protocol::machine_name())),
            (None, Some(_)) => Some(format!("⇄ {}", self.own_name())),
            (Some(h), Some(home)) if h == home => Some(format!("⌂ {h}")),
            (Some(h), _) => Some(format!("⇄ {h}")),
        }
    }

    /// Switches to the session in row `i` of the sidebar.
    fn go_to_row(&mut self, i: usize) {
        let rows = self.session_rows();
        if let Some(row) = rows.get(i) {
            let place = Place {
                host: row.host.clone(),
                name: row.name.clone(),
            };
            self.go_to(&place);
        }
    }

    /// The file tree for the sidebar, with changed files marked.
    fn file_rows(&self) -> Option<(String, Vec<crate::filetree::Entry>)> {
        if !self.config.sidebar.files || self.files.root.as_os_str().is_empty() {
            return None;
        }
        let changed: HashSet<PathBuf> = self
            .git
            .as_ref()
            .map(|g| g.changes.iter().map(|c| g.root.join(&c.path)).collect())
            .unwrap_or_default();
        let mut entries = self.files.entries.clone();
        for entry in &mut entries {
            entry.changed = !entry.dir && changed.contains(&entry.path);
        }
        let root = self.files.root.file_name().map_or_else(
            || tilde(&self.files.root),
            |n| n.to_string_lossy().into_owned(),
        );
        Some((root, entries))
    }

    /// Keeps the file tree on the focused pane's project: its repository,
    /// or else its directory.
    fn refresh_files(&mut self) {
        if self.sidebar.is_none() || !self.config.sidebar.files {
            return;
        }
        let root = match (&self.git, self.focused_cwd()) {
            (Some(git), Some(cwd)) if cwd.starts_with(&git.root) => git.root.clone(),
            (_, Some(cwd)) => cwd,
            (_, None) => return,
        };
        self.files.refresh(&root);
    }

    /// Opens or closes a folder of the file tree, or opens a file in
    /// `$EDITOR`. True for a folder, where the sidebar keeps the keyboard.
    fn open_file(&mut self, i: usize) -> bool {
        if self.files.toggle(i) {
            return true;
        }
        let Some(entry) = self.files.entries.get(i) else {
            return false;
        };
        let path = entry.path.to_string_lossy().into_owned();
        let dir = entry.path.parent().map(PathBuf::from);
        let name = format!("edit {}", entry.name);
        self.float_script(&name, "\"${EDITOR:-nvim}\" \"$1\"", &[&path], dir);
        false
    }

    /// Keeps the row the sidebar's keyboard is on in view.
    fn scroll_sidebar_to_selection(&mut self) {
        let (Mode::Sidebar(selected), Some(area)) = (self.mode, self.sidebar) else {
            return;
        };
        let height = area.height.saturating_sub(2);
        let data = self.sidebar_data();
        self.sidebar_scroll = sidebar::scroll_to_show(&data, height, selected, self.sidebar_scroll)
            .min(sidebar::max_scroll(&data, height));
    }

    /// Focuses the agent that has been waiting longest, in any workspace.
    /// Pressed again, it goes on to the next one.
    fn jump_to_waiting(&mut self) {
        // Agents waiting for the user first, then those that finished
        // unseen, each oldest first.
        let mut waiting: Vec<(PaneId, Instant)> = self
            .agents
            .iter()
            .filter(|(id, (state, _))| {
                *state == AgentState::Blocked || self.shown_state(**id) == Some(AgentState::Done)
            })
            .map(|(&id, &(_, since))| (id, since))
            .collect();
        waiting.sort_by_key(|&(id, since)| {
            (self.shown_state(id) != Some(AgentState::Blocked), since, id)
        });
        let Some(&(target, _)) = waiting
            .iter()
            .find(|(id, _)| *id != self.ws.focus)
            .or(waiting.first())
        else {
            self.flash = Some("no agent is waiting or done".into());
            return;
        };
        self.reveal(target);
    }

    /// Adopts the active client's new terminal size.
    fn resize_screen(&mut self, cols: u16, rows: u16) -> Result<()> {
        let Some(id) = self.active else {
            return Ok(());
        };
        let Some(client) = self.client_mut(id) else {
            return Ok(());
        };
        client.size = Rect::new(0, 0, cols, rows);
        // Redrawn even at the same size: its screen may hold anything.
        client.terminal = Client::terminal_for(&client.stream, Rect::default())?;
        self.fit_screen(false);
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        if !self.list_key(key) {
            return Ok(());
        }

        match self.mode {
            Mode::Prefix(menu) => {
                // Enter, or ↑/↓ where they mean nothing else, picks from the
                // which-key menu with the arrows.
                let picks = self.config.which_key.enabled
                    && matches!(key.code, KeyCode::Enter | KeyCode::Up | KeyCode::Down)
                    && key.modifiers.is_empty()
                    && bindings::lookup(menu, key).is_none();
                let count = bindings::pick_keys(menu, &self.which_key_commands(menu)).len();
                if picks && count > 0 {
                    let at = if key.code == KeyCode::Up {
                        count - 1
                    } else {
                        0
                    };
                    self.mode = Mode::Pick(menu, at);
                    return Ok(());
                }
                self.mode = Mode::Normal;
                self.command(menu, key)
            }
            Mode::Pick(menu, at) => self.pick_key(menu, at, key),
            Mode::Help(scroll) => {
                self.mode = match key.code {
                    KeyCode::Char('j') | KeyCode::Down => Mode::Help(scroll.saturating_add(1)),
                    KeyCode::Char('k') | KeyCode::Up => Mode::Help(scroll.saturating_sub(1)),
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        Mode::Help(scroll.saturating_add(10))
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        Mode::Help(scroll.saturating_sub(10))
                    }
                    _ => Mode::Normal,
                };
                Ok(())
            }
            Mode::Menu(selected) => {
                self.menu_key(key, selected);
                Ok(())
            }
            Mode::Settings(selected) => {
                self.settings_key(key, selected);
                Ok(())
            }
            Mode::Copy => {
                self.copy_key(key);
                Ok(())
            }
            Mode::Prompt => {
                self.prompt_key(key);
                Ok(())
            }
            Mode::Sidebar(selected) => {
                self.sidebar_key(key, selected);
                Ok(())
            }
            Mode::Picker => {
                self.picker_key(key);
                Ok(())
            }
            Mode::Sessions(selected) => {
                self.sessions_key(key, selected);
                Ok(())
            }
            Mode::Hives(selected) => {
                self.hives_key(key, selected);
                Ok(())
            }
            Mode::Accounts(selected) => {
                self.accounts_key(key, selected);
                Ok(())
            }
            Mode::AgentAccount(selected) => {
                self.agent_account_key(key, selected);
                Ok(())
            }
            Mode::Jump => {
                self.jump_key(key);
                Ok(())
            }
            Mode::MoveTo(window, selected) => {
                self.move_to_key(key, window, selected);
                Ok(())
            }
            Mode::Agents(selected) => {
                self.agents_key(key, selected);
                Ok(())
            }
            Mode::Inbox(selected) => {
                self.inbox_key(key, selected);
                Ok(())
            }
            Mode::Review => {
                self.review_key(key);
                Ok(())
            }
            Mode::Notes => {
                self.notes_key(key);
                Ok(())
            }
            Mode::Layouts(selected) => {
                self.layouts_key(key, selected);
                Ok(())
            }
            Mode::Context(selected) => self.context_key(key, selected),
            Mode::Recap(since, selected) => {
                self.recap_key(key, since, selected);
                Ok(())
            }
            Mode::Windows(selected) => {
                self.spot_list_key(key, selected, false);
                Ok(())
            }
            Mode::Panes(selected) => {
                self.spot_list_key(key, selected, true);
                Ok(())
            }
            Mode::NewSessionWhere(selected) => {
                self.new_session_where_key(key, selected);
                Ok(())
            }
            Mode::NewSessionTemplate(selected) => {
                self.new_session_template_key(key, selected);
                Ok(())
            }
            Mode::NewWindowWhere(selected) => {
                self.new_window_where_key(key, selected)?;
                Ok(())
            }
            Mode::Marks(selected) => {
                self.marks_key(key, selected);
                Ok(())
            }
            Mode::Confirm(action) => {
                self.mode = Mode::Normal;
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    self.perform(action);
                }
                self.confirm_target = None;
                Ok(())
            }
            Mode::Repeat(until, menu)
                if Instant::now() < until
                    && bindings::lookup(menu, key).is_some_and(Command::repeatable) =>
            {
                self.command(menu, key)
            }
            _ => {
                self.mode = Mode::Normal;
                if is_prefix(key) {
                    self.mode = Mode::Prefix(Menu::Root);
                    return Ok(());
                }
                // Alt+1 … Alt+9: straight to a marked pane, like Harpoon.
                if self.config.marks_on_alt
                    && key.modifiers == KeyModifiers::ALT
                    && let KeyCode::Char(c @ '1'..='9') = key.code
                {
                    self.jump_to_mark(c as usize - '1' as usize);
                    return Ok(());
                }
                self.flash = None;
                self.selection = None;
                let target = self.input_pane();
                let Some(pane) = self.panes.get_mut(&target) else {
                    return Ok(());
                };
                // Typing brings a scrolled back pane to its live screen.
                if pane.scroll_offset() > 0 {
                    pane.scroll_to(0);
                }
                let app_cursor = pane.screen().screen().application_cursor();
                if let Some(bytes) = keys::encode(key, app_cursor) {
                    pane.note_input();
                    pane.write(&bytes)?;
                }
                Ok(())
            }
        }
    }

    /// What the user's commands or the plugins add to the which-key menu
    /// `menu`, as key and name.
    fn which_key_commands(&self, menu: Menu) -> Vec<(char, String)> {
        match menu {
            Menu::Plugins => self
                .plugins
                .lock()
                .unwrap()
                .iter()
                .flat_map(|p| {
                    let main = p.key.map(|k| (k, p.title().to_owned()));
                    let actions = p
                        .actions
                        .iter()
                        .map(|a| (a.key, format!("{} › {}", p.title(), a.title)));
                    main.into_iter().chain(actions)
                })
                .collect(),
            _ => self
                .config
                .commands
                .iter()
                .map(|c| (c.key, c.name.clone()))
                .collect(),
        }
    }

    /// A key while the which-key menu is picked from with the arrows: ↑/↓
    /// choose in a column, ←/→ the column beside, Enter or Space runs the
    /// entry (a submenu opens picked from too), Backspace goes back to the
    /// main menu, other keys work as ever.
    fn pick_key(&mut self, menu: Menu, at: usize, key: KeyEvent) -> Result<()> {
        let commands = self.which_key_commands(menu);
        let keys = bindings::pick_keys(menu, &commands);
        let count = keys.len().max(1);
        match key.code {
            KeyCode::Down => self.mode = Mode::Pick(menu, (at + 1) % count),
            KeyCode::Up => self.mode = Mode::Pick(menu, (at + count - 1) % count),
            KeyCode::Left | KeyCode::Right => {
                let columns = bindings::pick_columns(menu, &commands);
                let step = if key.code == KeyCode::Left { -1 } else { 1 };
                self.mode = Mode::Pick(menu, bindings::pick_beside(&columns, at, step));
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.mode = Mode::Normal;
                if let Some(&chosen) = keys.get(at) {
                    self.command(menu, chosen)?;
                    if let Mode::Prefix(submenu) = self.mode {
                        self.mode = Mode::Pick(submenu, 0);
                    }
                }
            }
            KeyCode::Backspace if menu != Menu::Root => self.mode = Mode::Pick(Menu::Root, 0),
            KeyCode::Backspace => {}
            KeyCode::Esc => self.mode = Mode::Normal,
            _ => {
                self.mode = Mode::Normal;
                self.command(menu, key)?;
            }
        }
        Ok(())
    }

    /// Starts the agent in pane `id` again in `account`, in the same
    /// conversation: it is ended, and once its shell is back in front
    /// (`finish_switches`), `imkr account run` starts it with that
    /// account's environment, so no key shows on the screen.
    fn switch_account(&mut self, id: PaneId, account: &crate::account::Account) -> Reply {
        let pane = self.panes.get(&id).ok_or(format!("no pane {id}"))?;
        if pane.program().as_deref() != Some(account.tool.as_str()) {
            return Err(format!(
                "no {tool} runs in pane {id}: start it there with `imkr account run {tool}:{name} -- {tool}`",
                tool = account.tool,
                name = account.name
            ));
        }
        let mut command = vec![
            imkr_path(),
            "account".to_owned(),
            "run".to_owned(),
            format!("{}:{}", account.tool, account.name),
            "--".to_owned(),
        ];
        command.extend(crate::account::resume_command(
            &account.tool,
            pane.session.as_deref(),
        ));
        let line = command
            .iter()
            .map(|w| crate::client::sh_quote(w))
            .collect::<Vec<_>>()
            .join(" ");
        if !pane.stop_foreground() {
            return Err(format!("could not end the agent in pane {id}"));
        }
        let pane = self.panes.get_mut(&id).expect("looked up above");
        set_account(&mut pane.accounts, &account.tool, &account.name);
        self.switching.insert(id, (line, Instant::now()));
        Ok(serde_json::json!({ "pane": id, "tool": account.tool, "account": account.name }))
    }

    /// Types the commands of `switching` into shells that are back in front,
    /// see `switch_account`; gives up on an agent that does not end.
    fn finish_switches(&mut self) {
        let ids: Vec<PaneId> = self.switching.keys().copied().collect();
        for id in ids {
            let Some(pane) = self.panes.get(&id) else {
                self.switching.remove(&id);
                continue;
            };
            let in_front = agent::is_shell(&pane.foreground_argv());
            let (line, since) = self.switching[&id].clone();
            if in_front {
                self.switching.remove(&id);
                let _ = self.type_into(id, &line, true);
            } else if since.elapsed() > SWITCH_WAIT {
                self.switching.remove(&id);
                self.flash = Some(format!("pane {id}: the agent did not end, not switched"));
            }
        }
    }

    /// Notices agents whose account is used up (`account::used_up_in` on
    /// the end of their screen), notes it, and goes on in the next free
    /// account or says which, as the settings say.
    fn check_used_up(&mut self) {
        if self.config.account_fallback == crate::config::AccountFallback::Off {
            return;
        }
        let now = crate::persist::now();
        let mut hits = Vec::new();
        for (&id, pane) in &self.panes {
            let Some(tool) = pane
                .program()
                .filter(|p| crate::account::TOOLS.contains(&p.as_str()))
            else {
                continue;
            };
            // A used up account stops the agent: one at work is not.
            if self.shown_state(id) == Some(AgentState::Working)
                || self
                    .used_up_seen
                    .get(&id)
                    .is_some_and(|at| at.elapsed() < USED_UP_QUIET)
            {
                continue;
            }
            let tail = agent::tail(&pane.screen(), USED_UP_LINES);
            if let Some(until) = crate::account::used_up_in(&tail, now) {
                let current = pane
                    .accounts
                    .get(&tool)
                    .cloned()
                    .unwrap_or_else(|| crate::account::STANDARD.to_owned());
                hits.push((id, tool, current, until));
            }
        }
        if hits.is_empty() {
            return;
        }
        let Ok(store) = crate::account::Store::here() else {
            return;
        };
        for (id, tool, current, until) in hits {
            self.used_up_seen.insert(id, Instant::now());
            if let Some(account) = store.find(&tool, &current) {
                let _ = store.mark_used_up(&account, until);
            }
            let when = crate::protocol::chrono_like(until).unwrap_or_default();
            let next = store.next_free(&tool, &current);
            let auto = self.config.account_fallback == crate::config::AccountFallback::Auto;
            let text = match next {
                Some(next) if auto => match self.switch_account(id, &next) {
                    Ok(_) => format!(
                        "account {current} used up until {when}: goes on in {}",
                        next.name
                    ),
                    Err(e) => format!("account {current} used up until {when}, not switched: {e}"),
                },
                Some(next) => format!(
                    "account {current} used up until {when}: Ctrl+B s c to go on in {}",
                    next.name
                ),
                None => format!("account {current} used up until {when}, no other free one"),
            };
            self.notify(id, &text);
            self.flash = Some(format!("pane {id}: {text}"));
        }
    }

    /// Runs the command bound to `key` after the prefix. Unbound keys just
    /// end the prefix.
    fn command(&mut self, menu: Menu, key: KeyEvent) -> Result<()> {
        let Some(command) = bindings::lookup(menu, key) else {
            if let KeyCode::Char(c) = key.code {
                match menu {
                    Menu::Commands => self.run_user_command(c),
                    Menu::Plugins => {
                        let plugins = self.plugins.lock().unwrap().clone();
                        if let Some(plugin) = plugins.iter().find(|p| p.key == Some(c)) {
                            self.run_plugin(plugin);
                        } else if let Some((plugin, action)) = plugins
                            .iter()
                            .find_map(|p| p.actions.iter().find(|a| a.key == c).map(|a| (p, a)))
                        {
                            self.run_plugin_action(plugin, action);
                        }
                    }
                    _ => {}
                }
            }
            return Ok(());
        };
        self.run_command(command)?;
        if command.repeatable() {
            self.mode = Mode::Repeat(Instant::now() + REPEAT_TIME, menu);
        }
        Ok(())
    }

    /// Carries out `command`, pressed as a key or clicked as a button.
    fn run_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::SplitRow => self.split(Axis::Row),
            Command::SplitColumn => self.split(Axis::Column),
            Command::ClosePane => self.mode = Mode::Confirm(Action::ClosePane(self.ws.focus)),
            Command::NextPane => self.ws.cycle_focus(),
            Command::Focus(dir) => self.ws.focus_direction(dir, self.body),
            Command::Resize(dir, cells) => {
                if !self.ws.resize_float(dir, cells, self.body) {
                    self.ws.layout.resize(self.ws.focus, dir, cells, self.body);
                }
            }
            Command::Move(dir, cells) => {
                self.ws.move_float(dir, cells, self.body);
            }
            Command::ToggleFloat => self.ws.toggle_float(self.body),
            Command::NewFloat => self.new_float(),
            Command::NextFloat => self.ws.next_float(),
            Command::Open(menu) => self.mode = Mode::Prefix(menu),
            Command::Back => self.mode = Mode::Prefix(Menu::Root),
            Command::CopyMode => self.enter_copy(),
            Command::JumpToWaiting => self.jump_to_waiting(),
            Command::RenamePane => self.start_prompt(RenameTarget::Pane(self.ws.focus)),
            // Over a popup, zoom makes it fill everything but the bars.
            Command::Zoom if self.shown_popup().is_some() => self.popup_full = !self.popup_full,
            Command::Zoom => self.ws.toggle_zoom(),
            // From full size, ! is the way back to the popup: what fills the
            // screen already looks like the window it would become.
            Command::BreakPopup if self.shown_popup().is_some() && self.popup_full => {
                self.popup_full = false
            }
            Command::BreakPopup if self.shown_popup().is_some() => self.break_popup(),
            Command::BreakPopup => self.pane_to_popup(),
            Command::PaneToWindow(n) => self.pane_to_window(Some(n)),
            Command::PaneToNewWindow => self.pane_to_window(None),
            Command::Review => self.open_review(),
            Command::Notes => self.open_notes(),
            Command::Layouts => self.mode = Mode::Layouts(0),
            Command::Recap => self.open_recap(),
            Command::WorktreeWindow => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::Worktree,
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
            }
            Command::HostedWindow => {
                self.hosting = true;
                self.mode = Mode::NewSessionWhere(0);
            }
            Command::Sessions => self.open_sessions(),
            Command::Archive => {
                self.show_archive = true;
                self.mode = Mode::Sessions(0);
            }
            Command::Usage => {
                let command = ["sh", "-c", "imkr account usage --wait"].map(str::to_owned);
                let title = Some("Accounts: usage and limits".to_owned());
                if let Err(e) = self.toggle_popup("usage", title, &command, None) {
                    self.flash = Some(format!("no usage: {e:#}"));
                }
            }
            Command::Hives => self.mode = Mode::Hives(0),
            Command::Accounts => self.mode = Mode::Accounts(0),
            Command::NewAgent => self.new_agent(),
            Command::Jump => {
                self.enter_copy();
                self.start_jump();
            }
            Command::PluginMarket => {
                let exe = std::env::current_exe()
                    .map_or("imkr".into(), |e| e.to_string_lossy().into_owned());
                let command = vec![exe, "plugin".into(), "market".into()];
                let title = Some("Plugin marketplace".to_owned());
                if let Err(e) = self.toggle_popup("marketplace", title, &command, None) {
                    self.flash = Some(format!("marketplace: {e:#}"));
                }
            }
            Command::NewSession => self.new_session(),
            Command::NewWindowElsewhere => self.mode = Mode::NewWindowWhere(0),
            Command::RenameSession => {
                let name = protocol::session_name();
                self.prompt = Some(Prompt {
                    purpose: PromptFor::RenameSession(name.clone()),
                    text: name,
                });
                self.mode = Mode::Prompt;
            }
            Command::EndSession => {
                self.confirm_target = Some(protocol::session_name());
                self.mode = Mode::Confirm(Action::EndSession);
            }
            Command::SaveHive => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::SaveHive,
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
            }
            Command::PickPane | Command::Search => {
                self.picker = Some(Picker {
                    query: String::new(),
                    selected: 0,
                    global: command == Command::Search,
                    hives: if command == Command::Search {
                        persist::hive_infos()
                    } else {
                        Vec::new()
                    },
                });
                self.mode = Mode::Picker;
            }
            Command::ReloadConfig => self.reload_config(),
            Command::ScrollbackEditor => self.scrollback_in_editor(),
            Command::Swap(forward) => self.ws.swap(forward),
            Command::Equalize => self.ws.layout.equalize(),
            Command::LastPane => {
                if let Some(last) = self.last_focus.filter(|id| self.panes.contains_key(id)) {
                    self.reveal(last);
                }
            }
            Command::RenameWorkspace => self.start_prompt(RenameTarget::Workspace(self.workspace)),
            Command::MoveWorkspace => self.mode = Mode::MoveTo(self.workspace, 0),
            Command::Agents => self.mode = Mode::Agents(0),
            Command::Inbox => {
                self.inbox_answer.clear();
                self.inbox_on = self.inbox_items().0.first().map(|(_, _, id)| id.clone());
                self.mode = Mode::Inbox(0);
            }
            Command::Windows => {
                let at = self.workspaces().iter().position(|&n| n == self.workspace);
                self.mode = Mode::Windows(at.unwrap_or(0));
            }
            Command::Panes => {
                let at = self
                    .pane_items()
                    .0
                    .iter()
                    .position(|s| matches!(s, Spot::Pane(id) if *id == self.ws.focus));
                self.mode = Mode::Panes(at.unwrap_or(0));
            }
            Command::Mark => self.mark_focused(),
            Command::Marks => self.mode = Mode::Marks(0),
            Command::TogglePopup => match self.popup.clone() {
                Some(_) => self.popup_shown = !self.popup_shown,
                None => {
                    self.flash =
                        Some("no popup open yet: plugins and `imkr popup` open them".into())
                }
            },
            Command::RenumberWorkspaces => {
                self.flash = Some(if self.renumber_windows() {
                    "windows renumbered".into()
                } else {
                    "windows are numbered without gaps already".into()
                });
            }
            Command::FocusSidebar => self.focus_sidebar(),
            Command::FocusMode => self.focus_mode = !self.focus_mode,
            Command::ToggleSidebar => {
                self.config.sidebar.enabled = !self.config.sidebar.enabled;
                if let Err(e) = self.config.save() {
                    self.config_note = Some(format!("not saved: {e:#}"));
                }
            }
            Command::ScrollBack => {
                self.enter_copy();
                let page = self.focused_inner().map_or(1, |r| r.height as isize);
                self.copy_scroll(page);
            }
            Command::Detach => self.detach_active(),
            Command::Quit => self.mode = Mode::Confirm(Action::KillServer),
            Command::Help => self.mode = Mode::Help(0),
            Command::SessionMenu => self.mode = Mode::Menu(0),
            Command::Settings => self.mode = Mode::Settings(0),
            Command::Workspace(n) => self.switch_workspace(n),
            // A new window asks for its name right away, Esc leaves it without one.
            Command::NewWorkspace => match self.free_workspace() {
                Some(n) => {
                    let back = self.workspace;
                    self.switch_workspace(n);
                    if self.workspace == n {
                        self.prompt = Some(Prompt {
                            purpose: PromptFor::NewWindow {
                                window: n,
                                pane: self.ws.focus,
                                back,
                            },
                            text: String::new(),
                        });
                        self.mode = Mode::Prompt;
                    }
                }
                None => self.flash = Some("all 9 windows are in use".into()),
            },
            Command::NextWorkspace => self.step_workspace(true),
            Command::PrevWorkspace => self.step_workspace(false),
            // Prefix twice sends the prefix key itself to the pane.
            Command::SendPrefix => {
                if let Some(pane) = self.panes.get_mut(&self.ws.focus) {
                    pane.write(&[0x02])?;
                }
            }
            Command::Cancel => {}
        }
        Ok(())
    }

    /// Moves through the quit menu with arrows or j/k and picks an entry
    /// with Enter or its letter. Choosing an entry there is already a
    /// deliberate decision, so it is not confirmed again.
    fn menu_key(&mut self, key: KeyEvent, selected: usize) {
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Menu((selected + MENU.len() - 1) % MENU.len());
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Menu((selected + 1) % MENU.len());
                return;
            }
            KeyCode::Enter => Some(MENU[selected].1),
            KeyCode::Char(c) => MENU.iter().find(|(k, _)| *k == c).map(|&(_, e)| e),
            _ => None,
        };
        let Some(entry) = pick else { return };
        self.mode = Mode::Normal;
        match entry {
            MenuEntry::Detach => self.detach_active(),
            MenuEntry::ClosePane => self.close(self.ws.focus),
            MenuEntry::EndSession => self.quit = true,
        }
    }

    /// Moves through the settings and changes the selected one. Every
    /// change applies right away and is saved to the config file.
    fn settings_key(&mut self, key: KeyEvent, selected: usize) {
        let count = SETTINGS.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | ',') => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Settings((selected + count - 1) % count);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Settings((selected + 1) % count);
            }
            KeyCode::Enter | KeyCode::Char(' ' | 'h' | 'l') | KeyCode::Left | KeyCode::Right => {
                let forward = !matches!(key.code, KeyCode::Left | KeyCode::Char('h'));
                (SETTINGS[selected].step)(&mut self.config, forward);
                self.apply_theme();
                self.config_note = Some(match self.config.save() {
                    Ok(path) => format!("saved to {}", tilde(&path)),
                    Err(e) => format!("not saved: {e:#}"),
                });
            }
            _ => {}
        }
    }

    /// The bar lines, the panes' area and the sidebar's. Focus mode leaves
    /// all of the screen to the panes; a focused sidebar still shows.
    fn areas(&self, screen: Rect) -> (ScreenLayout, Rect, Option<Rect>) {
        if self.focus_mode {
            let layout = ScreenLayout {
                top: None,
                body: screen,
                bottom: None,
            };
            let (body, sidebar) = if self.sidebar_focused() {
                split_sidebar(screen, &self.config.sidebar, true, 0)
            } else {
                (screen, None)
            };
            return (layout, body, sidebar);
        }
        let spacing = &self.config.spacing;
        crate::workspace::set_gaps(spacing.gap_x, spacing.gap_y);
        crate::workspace::set_shared_borders(self.config.shared_borders);
        let layout = screen_layout(screen, &self.config.bars, spacing);
        let (body, sidebar) = split_sidebar(
            layout.body,
            &self.config.sidebar,
            self.sidebar_focused(),
            spacing.gap_x,
        );
        (layout, body, sidebar)
    }

    /// The panes of `ws` that are shown, with their areas. In focus mode
    /// that is only the focused one, over all of the panes' area.
    fn rects_of(&self, ws: &Workspace) -> Vec<(PaneId, Rect)> {
        if self.focus_mode {
            vec![(ws.focus, self.body)]
        } else {
            ws.rects(self.body)
        }
    }

    /// The shown pane at `pos`, the topmost where floating panes overlap.
    fn pane_at(&self, pos: Position) -> Option<(PaneId, Rect)> {
        if self.focus_mode {
            return Some((self.ws.focus, self.body)).filter(|(_, r)| r.contains(pos));
        }
        self.ws.pane_at(pos, self.body)
    }

    /// The area of a pane's screen: inside its border, or all of it in
    /// focus mode, which has no borders.
    fn inner(&self, rect: Rect) -> Rect {
        if self.focus_mode {
            rect
        } else {
            pane_inner(rect)
        }
    }

    /// The area inside the focused pane's border.
    fn focused_inner(&self) -> Option<Rect> {
        self.inner_of(self.ws.focus)
    }

    fn inner_of(&self, id: PaneId) -> Option<Rect> {
        self.rects_of(&self.ws)
            .into_iter()
            .find_map(|(pane, rect)| (pane == id).then(|| self.inner(rect)))
    }

    /// The absolute position of a cell of pane `id` at `row`, `col` in its
    /// current view.
    fn to_abs(&self, id: PaneId, row: u16, col: u16) -> (usize, u16) {
        let Some(pane) = self.panes.get(&id) else {
            return (0, col);
        };
        let top = pane.history_len() - pane.scroll_offset();
        (top + usize::from(row), col)
    }

    /// The view row of absolute row `abs` in pane `id`, outside the view
    /// when negative or past the bottom.
    fn to_view(&self, id: PaneId, abs: usize) -> i64 {
        let Some(pane) = self.panes.get(&id) else {
            return -1;
        };
        let top = pane.history_len() - pane.scroll_offset();
        abs as i64 - top as i64
    }

    /// Copies the selection to the client's clipboard.
    fn copy_selection(&mut self) {
        let Some(sel) = self.selection else { return };
        let Some(pane) = self.panes.get(&sel.pane) else {
            return;
        };
        let text = pane.text(sel.anchor, sel.head);
        if text.is_empty() {
            return;
        }
        let chars = text.chars().count();
        self.send_active(&clipboard::osc52(&text));
        self.flash = Some(format!(
            "copied {chars} {}",
            if chars == 1 {
                "character"
            } else {
                "characters"
            }
        ));
    }

    fn enter_copy(&mut self) {
        let id = self.ws.focus;
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let (row, col) = pane.screen().screen().cursor_position();
        let pos = self.to_abs(id, row, col);
        self.copy = Some(CopyCursor { pane: id, pos });
        self.selection = None;
        self.mode = Mode::Copy;
    }

    fn leave_copy(&mut self) {
        if let Some(copy) = self.copy.take()
            && let Some(pane) = self.panes.get_mut(&copy.pane)
        {
            pane.scroll_to(0);
        }
        self.mode = Mode::Normal;
    }

    /// Moves the copy cursor by `rows` and `cols`, scrolling to keep it in
    /// view and dragging the selection's head along.
    fn copy_move(&mut self, rows: isize, cols: isize) {
        let Some(mut copy) = self.copy else { return };
        let Some(inner) = self.inner_of(copy.pane) else {
            return;
        };
        let Some(pane) = self.panes.get(&copy.pane) else {
            return;
        };
        let last_row = pane.history_len() + usize::from(inner.height).saturating_sub(1);
        copy.pos.0 = copy.pos.0.saturating_add_signed(rows).min(last_row);
        let col = (copy.pos.1 as isize + cols).clamp(0, inner.width.saturating_sub(1) as isize);
        copy.pos.1 = col as u16;
        self.copy = Some(copy);
        self.copy_show(copy);
    }

    /// Scrolls the copy mode view by `lines`, positive into the history,
    /// and moves the cursor along, like Ctrl-U and Ctrl-D in vim.
    fn copy_scroll(&mut self, lines: isize) {
        let Some(mut copy) = self.copy else { return };
        let Some(pane) = self.panes.get_mut(&copy.pane) else {
            return;
        };
        let before = pane.scroll_offset();
        pane.scroll(lines);
        let moved = pane.scroll_offset() as isize - before as isize;
        copy.pos.0 = copy.pos.0.saturating_add_signed(-moved);
        self.copy = Some(copy);
        if let Some(sel) = &mut self.selection {
            sel.head = copy.pos;
        }
    }

    fn start_jump(&mut self) {
        if self.copy.is_none() {
            return;
        }
        self.jump = Some(Jump::default());
        self.mode = Mode::Jump;
    }

    /// The matches of `query` in every pane on screen, the focused one
    /// first, top to bottom, each labelled with a key that does not go on
    /// any match, so that typing more narrows them down instead.
    fn jump_targets(&self, query: &str) -> Vec<JumpTarget> {
        if query.is_empty() {
            return Vec::new();
        }
        let fold = !query.chars().any(char::is_uppercase);
        let norm = |s: &str| if fold { s.to_lowercase() } else { s.to_owned() };
        let wanted = norm(query);
        let mut rects = self.rects_of(&self.ws);
        rects.sort_by_key(|(id, _)| *id != self.ws.focus);
        let mut targets = Vec::new();
        // Characters that follow a match: typing one of them goes on.
        let mut next = std::collections::HashSet::new();
        for (id, rect) in rects {
            let Some(pane) = self.panes.get(&id) else {
                continue;
            };
            let inner = self.inner(rect);
            let top = pane.history_len() - pane.scroll_offset();
            for (row, text) in pane
                .rows_from(top, usize::from(inner.height))
                .iter()
                .enumerate()
            {
                let text = norm(text);
                for (byte, _) in text.match_indices(&wanted) {
                    let col = text[..byte].chars().count();
                    if let Some(c) = text[byte + wanted.len()..].chars().next() {
                        next.insert(c.to_ascii_lowercase());
                    }
                    targets.push(JumpTarget {
                        pane: id,
                        pos: (top + row, col as u16),
                        row: row as u16,
                        label: None,
                    });
                }
            }
        }
        // Nearest the cursor first: Enter takes it, and near ones get the
        // first labels, as in flash.nvim. Other panes after, top down.
        let at = self.copy.filter(|c| c.pane == self.ws.focus).map(|c| c.pos);
        targets.sort_by_key(|t| {
            let here = t.pane == self.ws.focus;
            let away = at.filter(|_| here).map_or((t.pos.0, 0), |(row, col)| {
                (t.pos.0.abs_diff(row), usize::from(t.pos.1.abs_diff(col)))
            });
            (!here, away)
        });
        let mut labels = JUMP_LABELS.chars().filter(|c| !next.contains(c));
        for target in &mut targets {
            target.label = labels.next();
        }
        targets
    }

    /// A key while jumping: a label jumps there, other characters narrow the
    /// matches down, Enter takes the first, Backspace takes one back, Esc
    /// goes back to copy mode where it was.
    fn jump_key(&mut self, key: KeyEvent) {
        let Some(mut jump) = self.jump.take() else {
            self.mode = Mode::Copy;
            return;
        };
        let to = match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Copy;
                return;
            }
            // The marked one, nearest the cursor. Space too, as in the
            // which-key pick: a leading one would match every gap.
            KeyCode::Enter => jump.targets.first().copied(),
            KeyCode::Char(' ') if jump.query.is_empty() => None,
            KeyCode::Char(' ') => jump.targets.first().copied(),
            KeyCode::Backspace => {
                jump.query.pop();
                None
            }
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                match jump.targets.iter().find(|t| t.label == Some(c)) {
                    Some(target) => Some(*target),
                    None => {
                        jump.query.push(c);
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(target) = to {
            // Into that pane's copy mode, at the match.
            if let Some(copy) = self.copy
                && copy.pane != target.pane
                && let Some(pane) = self.panes.get_mut(&copy.pane)
            {
                pane.scroll_to(0);
            }
            self.ws.focus(target.pane);
            let copy = CopyCursor {
                pane: target.pane,
                pos: target.pos,
            };
            self.copy = Some(copy);
            // With a selection there, the jump moves its end: two jumps
            // mark any stretch of text. One in another pane starts over.
            if self.selection.is_some_and(|s| s.pane != target.pane) {
                self.selection = None;
            }
            self.copy_show(copy);
            self.mode = Mode::Copy;
            return;
        }
        jump.targets = self.jump_targets(&jump.query);
        self.jump = Some(jump);
    }

    /// The labels of a jump over the panes, the matches marked.
    fn draw_jump(&self, frame: &mut Frame) {
        let Some(jump) = &self.jump else { return };
        let t = theme::current();
        let matched = Style::new().fg(t.on_accent).bg(t.subtle);
        let label = Style::new()
            .fg(t.on_accent)
            .bg(t.accent)
            .add_modifier(Modifier::BOLD);
        let rects: HashMap<PaneId, Rect> = self
            .rects_of(&self.ws)
            .into_iter()
            .map(|(id, rect)| (id, self.inner(rect)))
            .collect();
        let first = Style::new()
            .fg(t.on_accent)
            .bg(t.warning)
            .add_modifier(Modifier::BOLD);
        let width = jump.query.chars().count() as u16;
        for (i, target) in jump.targets.iter().enumerate() {
            let Some(inner) = rects.get(&target.pane) else {
                continue;
            };
            let x = inner.x + target.pos.1;
            let y = inner.y + target.row;
            if y >= inner.bottom() || x >= inner.right() {
                continue;
            }
            let room = inner.right() - x;
            let area = Rect::new(x, y, width.min(room), 1);
            frame.buffer_mut().set_style(area, matched);
            if let Some(c) = target.label {
                let style = if i == 0 { first } else { label };
                frame.buffer_mut().set_string(x, y, c.to_string(), style);
            }
        }
    }

    /// Moves the copy cursor by words, as vim's `W`, `B` and `E` do: a word
    /// is anything between blanks, so a path or URL is one.
    fn copy_word(&mut self, motion: Word) {
        let Some(copy) = self.copy else { return };
        let Some(inner) = self.inner_of(copy.pane) else {
            return;
        };
        let Some(pane) = self.panes.get(&copy.pane) else {
            return;
        };
        let last = pane.history_len() + usize::from(inner.height).saturating_sub(1);
        // A stretch of rows around the cursor is enough to find the word.
        let first = match motion {
            Word::Back => copy.pos.0.saturating_sub(WORD_ROWS - 1),
            _ => copy.pos.0,
        };
        let count = (last + 1).saturating_sub(first).min(WORD_ROWS);
        let rows = pane.rows_from(first, count);
        let at = (copy.pos.0 - first, usize::from(copy.pos.1));
        let Some((row, col)) = word_motion(&rows, at, motion) else {
            return;
        };
        let copy = CopyCursor {
            pane: copy.pane,
            pos: (first + row, col as u16),
        };
        self.copy = Some(copy);
        self.copy_show(copy);
    }

    /// Moves the copy cursor to the next match of the last search, in its
    /// direction or, with `reverse`, the other way. Wraps around.
    fn search_next(&mut self, reverse: bool) {
        let (Some(copy), Some((query, forward))) = (self.copy, self.search.clone()) else {
            return;
        };
        let Some(pane) = self.panes.get(&copy.pane) else {
            return;
        };
        let rows = pane.rows_text();
        match find_in_rows(&rows, copy.pos, &query, forward != reverse) {
            Some(pos) => {
                let copy = CopyCursor {
                    pane: copy.pane,
                    pos,
                };
                self.copy = Some(copy);
                self.copy_show(copy);
            }
            None => self.flash = Some(format!("{query:?} not found")),
        }
    }

    /// Scrolls pane so that the copy cursor is in view.
    fn copy_show(&mut self, copy: CopyCursor) {
        let Some(inner) = self.inner_of(copy.pane) else {
            return;
        };
        let view = self.to_view(copy.pane, copy.pos.0);
        let Some(pane) = self.panes.get_mut(&copy.pane) else {
            return;
        };
        if view < 0 {
            pane.scroll(-view as isize);
        } else if view >= i64::from(inner.height) {
            pane.scroll(-((view - i64::from(inner.height) + 1) as isize));
        }
        if let Some(sel) = &mut self.selection {
            sel.head = copy.pos;
        }
    }

    /// Copy mode: vim-like keys move a cursor through the pane and its
    /// history, `v` starts a selection, `y` copies it.
    fn copy_key(&mut self, key: KeyEvent) {
        let Some(copy) = self.copy else {
            self.mode = Mode::Normal;
            return;
        };
        let half = self
            .inner_of(copy.pane)
            .map_or(1, |r| (r.height / 2).max(1) as isize);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.leave_copy(),
            KeyCode::Char('u') if ctrl => self.copy_scroll(half),
            KeyCode::Char('d') if ctrl => self.copy_scroll(-half),
            KeyCode::Char('b') if ctrl => self.copy_scroll(2 * half),
            KeyCode::Char('f') if ctrl => self.copy_scroll(-2 * half),
            KeyCode::PageUp => self.copy_scroll(2 * half),
            KeyCode::PageDown => self.copy_scroll(-2 * half),
            KeyCode::Char('k') | KeyCode::Up => self.copy_move(-1, 0),
            KeyCode::Char('j') | KeyCode::Down => self.copy_move(1, 0),
            KeyCode::Char('h') | KeyCode::Left => self.copy_move(0, -1),
            KeyCode::Char('l') | KeyCode::Right => self.copy_move(0, 1),
            KeyCode::Char('0') | KeyCode::Home => self.copy_move(0, -10_000),
            KeyCode::Char('$') | KeyCode::End => self.copy_move(0, 10_000),
            KeyCode::Char('g') => self.copy_move(-(isize::MAX / 2), 0),
            KeyCode::Char('G') => self.copy_move(isize::MAX / 2, 0),
            KeyCode::Char('/' | '?') => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::Search {
                        forward: key.code == KeyCode::Char('/'),
                    },
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
            }
            KeyCode::Char('n') => self.search_next(false),
            KeyCode::Char('N') => self.search_next(true),
            KeyCode::Char('s') => self.start_jump(),
            KeyCode::Char('i') => {
                self.config.which_key.copy_mode = !self.config.which_key.copy_mode;
                if let Err(e) = self.config.save() {
                    self.config_note = Some(format!("not saved: {e:#}"));
                }
            }
            KeyCode::Char('w') => self.copy_word(Word::Next),
            KeyCode::Char('b') if !ctrl => self.copy_word(Word::Back),
            KeyCode::Char('e') => self.copy_word(Word::End),
            // Whole lines: from this one's start, the cursor at its end;
            // j, k or a jump take more.
            KeyCode::Char('V') => {
                let width = self.inner_of(copy.pane).map_or(1, |r| r.width.max(1));
                let line_end = (copy.pos.0, width - 1);
                self.selection = Some(Selection {
                    pane: copy.pane,
                    anchor: (copy.pos.0, 0),
                    head: line_end,
                });
                let copy = CopyCursor {
                    pane: copy.pane,
                    pos: line_end,
                };
                self.copy = Some(copy);
            }
            KeyCode::Char('v' | ' ') => {
                self.selection = match self.selection {
                    Some(_) => None,
                    None => Some(Selection {
                        pane: copy.pane,
                        anchor: copy.pos,
                        head: copy.pos,
                    }),
                };
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                self.copy_selection();
                self.selection = None;
                self.leave_copy();
            }
            _ => {}
        }
    }

    /// Mouse: click focuses, dragging a floating pane's title bar moves it
    /// and its bottom right corner resizes it, dragging over text selects and
    /// copies it, the wheel scrolls back. Programs that asked for the mouse
    /// get the events instead, unless Shift is held.
    fn mouse(&mut self, event: MouseEvent) -> Result<()> {
        if let Mode::Context(_) = self.mode {
            return self.context_mouse(event);
        }
        if !matches!(self.mode, Mode::Normal | Mode::Repeat(..) | Mode::Copy) {
            return Ok(());
        }
        let pos = Position::new(event.column, event.row);
        // A popup takes the mouse inside it; a click outside hides it.
        if let Some(popup) = self.shown_popup() {
            let inner = pane_inner(self.popup_rect());
            if inner.contains(pos) {
                self.forward_mouse(popup, event, inner)?;
            } else if matches!(event.kind, MouseEventKind::Down(_)) {
                // A popup's row in the sidebar switches to it, or hides it.
                let row = self
                    .sidebar
                    .filter(|area| area.contains(pos))
                    .and_then(|area| {
                        sidebar::target_at(&self.sidebar_data(), area, pos, self.sidebar_scroll)
                    });
                match row {
                    Some(sidebar::Target::Popup(i)) => self.toggle_popup_at(i),
                    _ => self.popup_shown = false,
                }
            }
            return Ok(());
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.flash = None;
                // 0 is the + after the tabs.
                if let Some(n) = self.tab_at(pos) {
                    if n == 0 {
                        self.run_command(Command::NewWorkspace)?;
                    } else {
                        self.switch_workspace(n);
                    }
                    return Ok(());
                }
                if let Some(area) = self.sidebar
                    && let Some(button) = sidebar_buttons(area)
                        .into_iter()
                        .find(|b| b.area.contains(pos))
                {
                    return self.run_command(button.command);
                }
                if let Some(area) = self.sidebar.filter(|area| area.contains(pos)) {
                    let data = self.sidebar_data();
                    match sidebar::target_at(&data, area, pos, self.sidebar_scroll) {
                        Some(sidebar::Target::Workspace(n)) => self.switch_workspace(n),
                        Some(sidebar::Target::Session(i)) => self.go_to_row(i),
                        Some(sidebar::Target::Popup(i)) => self.toggle_popup_at(i),
                        Some(sidebar::Target::OtherAgent(i)) => self.go_to_other_agent(i),
                        Some(sidebar::Target::Pane(id)) => self.reveal(id),
                        Some(sidebar::Target::Change(i)) => self.open_change(i, false),
                        Some(sidebar::Target::File(i)) => {
                            self.open_file(i);
                        }
                        None => {}
                    }
                    return Ok(());
                }
                let Some((id, rect)) = self.pane_at(pos) else {
                    return Ok(());
                };
                if !self.focus_mode
                    && let Some(button) = pane_buttons(rect, self.ws.is_floating(id))
                        .into_iter()
                        .find(|b| b.area.contains(pos))
                {
                    self.ws.focus(id);
                    return self.run_command(button.command);
                }
                // A border where two tiled panes meet moves when dragged.
                if !self.ws.is_floating(id)
                    && self.ws.zoomed.is_none()
                    && !self.focus_mode
                    && let Some(path) = self.ws.layout.divider_at(pos, self.body)
                {
                    self.divider = path;
                    self.drag = Some(Drag::Divider);
                    return Ok(());
                }
                if self.copy.is_some_and(|c| c.pane != id) {
                    self.leave_copy();
                }
                self.ws.focus(id);
                if self.ws.is_floating(id) && !self.focus_mode {
                    if pos.y == rect.y {
                        self.drag = Some(Drag::Move {
                            id,
                            dx: pos.x - rect.x,
                        });
                        return Ok(());
                    }
                    if pos.x == rect.right() - 1 && pos.y == rect.bottom() - 1 {
                        self.drag = Some(Drag::Resize { id });
                        return Ok(());
                    }
                }
                let inner = self.inner(rect);
                // Ctrl+click opens a link, before the program sees the click.
                if event.modifiers.contains(KeyModifiers::CONTROL) && inner.contains(pos) {
                    let at = self.to_abs(id, pos.y - inner.y, pos.x - inner.x);
                    if let Some(url) = self.panes.get(&id).and_then(|pane| pane.link_at(at)) {
                        self.open_link(url);
                        return Ok(());
                    }
                }
                if !inner.contains(pos) || self.forward_mouse(id, event, inner)? {
                    return Ok(());
                }
                let at = self.to_abs(id, pos.y - inner.y, pos.x - inner.x);
                let double = self
                    .last_click
                    .is_some_and(|(when, at)| at == pos && when.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((Instant::now(), pos));
                if double {
                    self.select_word(id, at);
                    return Ok(());
                }
                self.selection = Some(Selection {
                    pane: id,
                    anchor: at,
                    head: at,
                });
                self.drag = Some(Drag::Select);
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Move { id, dx }) => {
                    if let Some((_, rect)) =
                        self.ws.rects(self.body).into_iter().find(|(p, _)| *p == id)
                    {
                        let moved = Rect {
                            x: pos.x.saturating_sub(dx),
                            y: pos.y,
                            ..rect
                        };
                        self.ws.set_float_rect(id, moved, self.body);
                    }
                }
                Some(Drag::Resize { id }) => {
                    if let Some((_, rect)) =
                        self.ws.rects(self.body).into_iter().find(|(p, _)| *p == id)
                    {
                        let resized = Rect {
                            width: (pos.x + 1).saturating_sub(rect.x),
                            height: (pos.y + 1).saturating_sub(rect.y),
                            ..rect
                        };
                        self.ws.set_float_rect(id, resized, self.body);
                    }
                }
                Some(Drag::Divider) => {
                    let path = self.divider.clone();
                    self.ws.layout.move_divider(&path, pos, self.body);
                }
                Some(Drag::Select) => {
                    let Some(sel) = self.selection else {
                        return Ok(());
                    };
                    let Some(inner) = self.inner_of(sel.pane) else {
                        return Ok(());
                    };
                    // Dragging past the top or bottom edge scrolls.
                    if let Some(pane) = self.panes.get_mut(&sel.pane) {
                        if pos.y < inner.y {
                            pane.scroll(1);
                        } else if pos.y >= inner.bottom() {
                            pane.scroll(-1);
                        }
                    }
                    let row = pos.y.clamp(inner.y, inner.bottom() - 1) - inner.y;
                    let col = pos.x.clamp(inner.x, inner.right() - 1) - inner.x;
                    let head = self.to_abs(sel.pane, row, col);
                    if let Some(sel) = &mut self.selection {
                        sel.head = head;
                    }
                }
                None => self.forward_to_focused(event)?,
            },
            MouseEventKind::Up(MouseButton::Left) => match self.drag.take() {
                Some(Drag::Select) => {
                    if self.selection.is_some_and(|s| s.anchor != s.head) {
                        self.copy_selection();
                    } else {
                        self.selection = None;
                    }
                }
                Some(_) => {}
                None => self.forward_to_focused(event)?,
            },
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if let Some(area) = self.sidebar.filter(|area| area.contains(pos)) {
                    let max = sidebar::max_scroll(&self.sidebar_data(), area.height - 2);
                    self.sidebar_scroll = if event.kind == MouseEventKind::ScrollUp {
                        self.sidebar_scroll.saturating_sub(3)
                    } else {
                        (self.sidebar_scroll + 3).min(max)
                    };
                    return Ok(());
                }
                let Some((id, rect)) = self.pane_at(pos) else {
                    return Ok(());
                };
                let inner = self.inner(rect);
                if self.forward_mouse(id, event, inner)? {
                    return Ok(());
                }
                let up = event.kind == MouseEventKind::ScrollUp;
                let Some(pane) = self.panes.get_mut(&id) else {
                    return Ok(());
                };
                let (alternate, app_cursor) = {
                    let parser = pane.screen();
                    (
                        parser.screen().alternate_screen(),
                        parser.screen().application_cursor(),
                    )
                };
                if alternate {
                    // Full screen programs without mouse support, like less,
                    // get arrow keys, as most terminals do.
                    let code = if up { KeyCode::Up } else { KeyCode::Down };
                    if let Some(bytes) =
                        keys::encode(KeyEvent::new(code, KeyModifiers::NONE), app_cursor)
                    {
                        for _ in 0..3 {
                            pane.write(&bytes)?;
                        }
                    }
                } else {
                    pane.scroll(if up { 3 } else { -3 });
                }
            }
            MouseEventKind::Down(MouseButton::Right) => self.open_context(event)?,
            _ => self.forward_to_focused(event)?,
        }
        Ok(())
    }

    /// Selects and copies the word at `at` in pane `id`, for a double click.
    fn select_word(&mut self, id: PaneId, at: (usize, u16)) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let Some(line) = pane.rows_text().into_iter().nth(at.0) else {
            return;
        };
        let Some((start, end)) = word_bounds(&line, usize::from(at.1)) else {
            return;
        };
        self.selection = Some(Selection {
            pane: id,
            anchor: (at.0, start as u16),
            head: (at.0, end as u16),
        });
        self.copy_selection();
    }

    fn forward_to_focused(&mut self, event: MouseEvent) -> Result<()> {
        if let Some(inner) = self.focused_inner() {
            self.forward_mouse(self.ws.focus, event, inner)?;
        }
        Ok(())
    }

    /// Sends `event` to the program in pane `id` if it asked for mouse
    /// events and the pointer is inside the pane. Returns whether it did.
    fn forward_mouse(&mut self, id: PaneId, event: MouseEvent, inner: Rect) -> Result<bool> {
        let pos = Position::new(event.column, event.row);
        if event.modifiers.contains(KeyModifiers::SHIFT) || !inner.contains(pos) {
            return Ok(false);
        }
        let Some(pane) = self.panes.get_mut(&id) else {
            return Ok(false);
        };
        let (mode, encoding) = {
            let parser = pane.screen();
            (
                parser.screen().mouse_protocol_mode(),
                parser.screen().mouse_protocol_encoding(),
            )
        };
        let Some(bytes) = mouse::encode(
            event.kind,
            event.modifiers,
            pos.x - inner.x,
            pos.y - inner.y,
            mode,
            encoding,
        ) else {
            return Ok(mode != crate::term::MouseProtocolMode::None);
        };
        pane.write(&bytes)?;
        Ok(true)
    }

    /// The workspace whose tab is at `pos`, if any.
    fn tab_at(&self, pos: Position) -> Option<u8> {
        let bars = &self.config.bars;
        let (layout, ..) = self.areas(self.screen);
        let side = if layout.top.is_some_and(|r| r.contains(pos)) {
            Placement::Top
        } else if layout.bottom.is_some_and(|r| r.contains(pos)) {
            Placement::Bottom
        } else {
            return None;
        };
        if bars.tabs != side {
            return None;
        }
        let mut x = if bars.control() == side {
            badge_text().chars().count() as u16 + 1
        } else {
            0
        };
        for n in self.workspaces() {
            let Some(ws) = self.workspace_ref(n) else {
                continue;
            };
            let width = tab_label(n, ws).chars().count() as u16;
            if (x..x + width).contains(&pos.x) {
                return Some(n);
            }
            x += width;
        }
        (self.free_workspace().is_some() && (x..x + NEW_TAB.len() as u16).contains(&pos.x))
            .then_some(0)
    }

    /// The first workspace number not in use.
    fn free_workspace(&self) -> Option<u8> {
        (1..=9).find(|n| !self.workspace_exists(*n))
    }

    /// Remembers the previously focused pane whenever focus moves, however
    /// it moved: keys, mouse, sidebar or workspace switch.
    fn track_focus(&mut self) {
        let now = Some(self.ws.focus);
        if now != self.current_focus {
            if self
                .current_focus
                .is_some_and(|id| self.panes.contains_key(&id))
            {
                self.last_focus = self.current_focus;
            }
            self.current_focus = now;
        }
    }

    /// Every pane matching the picker's query: workspace, name, program,
    /// state and directory are searched, words in any order.
    fn picker_entries(&self, query: &str) -> Vec<PickerEntry> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut all: Vec<(u8, PaneId)> = Vec::new();
        for n in self.workspaces() {
            if let Some(ws) = self.workspace_ref(n) {
                all.extend(ws.panes().into_iter().map(|id| (n, id)));
            }
        }
        all.into_iter()
            .filter_map(|(n, id)| {
                let pane = self.panes.get(&id)?;
                let program = pane.program().unwrap_or_default();
                let label = match &pane.name {
                    Some(name) => format!("{name} ({program})"),
                    None => program,
                };
                let dir = pane.cwd().map(|c| tilde(&c)).unwrap_or_default();
                let state = self.shown_state(id);
                let ws_name = self
                    .workspace_ref(n)
                    .and_then(|w| w.name.clone())
                    .unwrap_or_default();
                let haystack = format!(
                    "{n} {ws_name} {id} {label} {} {dir}",
                    state.map(AgentState::name).unwrap_or_default()
                )
                .to_lowercase();
                words
                    .iter()
                    .all(|w| haystack.contains(w))
                    .then_some(PickerEntry {
                        pane: id,
                        workspace: n,
                        label,
                        state,
                        dir,
                    })
            })
            .collect()
    }

    /// The switcher's entries: every running session, then a new one.
    /// The session list: this machine's sessions, then those of every
    /// other machine known, and the client's own machine if it is another.
    fn session_items(&self) -> (Vec<Place>, Vec<menu::MenuItem>) {
        let current = protocol::session_name();
        let places = self.places.lock().unwrap().clone();
        let mut entries: Vec<SessionEntry> = Vec::new();
        let mut here: Vec<String> = places.local.iter().map(|s| s.name.clone()).collect();
        here.push(current.clone());
        // What runs first, then what can come back.
        let asleep = |name: &str| {
            places
                .local
                .iter()
                .any(|s| s.name == name && s.dormant.is_some())
        };
        here.sort_by(|a, b| (asleep(a), a).cmp(&(asleep(b), b)));
        // "stopped 21:57 · 2 windows" for a session that does not run.
        let dormant_of = |infos: &[protocol::SessionInfo], name: &str| {
            let info = infos.iter().find(|s| s.name == name)?;
            let label = info.dormant_label()?;
            Some(format!("{label} · {} windows", info.windows))
        };
        let agents_of = |infos: &[protocol::SessionInfo], name: &str| {
            infos
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.agents.clone())
                .unwrap_or_default()
        };
        let quiet_of = |infos: &[protocol::SessionInfo], name: &str| {
            infos
                .iter()
                .find(|s| s.name == name)
                .and_then(protocol::SessionInfo::quiet_for)
        };
        let mut own = BTreeMap::new();
        for &id in self.panes.keys() {
            if let Some(state) = self.shown_state(id) {
                *own.entry(state.name().to_owned()).or_insert(0) += 1;
            }
        }
        let own_quiet = self.quiet_for();
        // This machine's sessions carry its name if the user is not at it.
        let here_label = self.machine_label(None);
        for name in here {
            let hint = match (&here_label, name == current) {
                (Some(machine), true) => format!("this session · {machine}"),
                (None, true) => "this session".to_owned(),
                (Some(machine), false) => machine.clone(),
                (None, false) => String::new(),
            };
            let agents = if name == current {
                own.clone()
            } else {
                agents_of(&places.local, &name)
            };
            let dormant = dormant_of(&places.local, &name);
            let quiet = if name == current {
                own_quiet
            } else {
                quiet_of(&places.local, &name)
            };
            entries.push((Place { host: None, name }, hint, agents, dormant, quiet));
        }
        // The machine the active client sits at, to go back to.
        if let Some(origin) = self.origin.clone() {
            for name in origin.sessions {
                let place = Place {
                    host: Some(origin.host.clone()),
                    name,
                };
                let agents = agents_of(&origin.infos, &place.name);
                let dormant = dormant_of(&origin.infos, &place.name);
                let quiet = quiet_of(&origin.infos, &place.name);
                entries.push((place, format!("⌂ {}", origin.host), agents, dormant, quiet));
            }
        }
        // Machines that answer but run nothing yet: going there starts one.
        let mut empty = Vec::new();
        for (host, sessions) in &self.known_hosts() {
            for session in sessions.iter().flatten() {
                let place = Place {
                    host: Some(host.clone()),
                    name: session.name.clone(),
                };
                let dormant = dormant_of(std::slice::from_ref(session), &session.name);
                let mut hint = format!("⇄ {host}");
                if session.is_stand_in() {
                    empty.push(place.clone());
                    hint.push_str(" · no session yet, Enter starts one");
                }
                entries.push((
                    place,
                    hint,
                    session.agents.clone(),
                    dormant,
                    session.quiet_for(),
                ));
            }
        }
        let old: Vec<Place> = entries
            .iter()
            .filter(|(place, _, _, dormant, _)| {
                let current = place.host.is_none() && place.name == current;
                is_old_session(current, empty.contains(place), dormant.is_some())
            })
            .map(|(place, ..)| place.clone())
            .collect();
        let hidden =
            if self.hide_old && !(self.show_archive && matches!(self.mode, Mode::Sessions(_))) {
                old.len()
            } else {
                0
            };
        if self.hide_old {
            entries.retain(|(place, ..)| !old.contains(place));
        }
        // The archive of this machine: the active hive's first, then the
        // other hives', then those ended outside a hive (#506). One entry
        // per name, the one `persist::revive` would bring back.
        // The archive of every machine of the hives: this one's, and the
        // others' as last fetched, so a machine that is off shows too (#506).
        // The active hive first, then the other hives, then sessions ended
        // outside a hive; this machine first in each.
        let replicas = persist::replicas();
        let mut archived: Vec<(Option<String>, persist::Archived)> = persist::archived()
            .into_iter()
            .map(|a| (None, a))
            .chain(replicas.into_iter().flat_map(|r| {
                r.sessions
                    .into_iter()
                    .map(move |a| (Some(r.host.clone()), a))
            }))
            .collect();
        let archived_count = archived.len();
        let active = persist::active_hive();
        archived.sort_by_key(|(host, a)| {
            (
                a.hive != active,
                a.hive.is_none(),
                a.hive.clone(),
                host.is_some(),
                host.clone(),
            )
        });
        let mut archive_of: BTreeMap<(Option<String>, String), String> = BTreeMap::new();
        // The archive is a view of its own, only in the session list.
        let archive_view = self.show_archive && matches!(self.mode, Mode::Sessions(_));
        if archive_view {
            entries.clear();
            let runs = |host: &Option<String>, name: &str| match host {
                None => places.local.iter().any(|s| s.name == name),
                Some(host) => places.hosts.iter().any(|(h, sessions)| {
                    h == host
                        && sessions
                            .as_ref()
                            .is_ok_and(|s| s.iter().any(|s| s.name == name && s.dormant.is_none()))
                }),
            };
            let here = protocol::machine_name();
            for (host, entry) in archived {
                let key = (host.clone(), entry.name.clone());
                if archive_of.contains_key(&key) || runs(&host, &entry.name) {
                    continue;
                }
                let day = protocol::chrono_like(entry.saved_at).unwrap_or_default();
                let hive = entry.hive.as_deref().unwrap_or("no hive");
                let machine = host.clone().unwrap_or_else(|| here.clone());
                archive_of.insert(key, format!("▣ {hive} · {machine}"));
                entries.push((
                    Place {
                        host,
                        name: entry.name,
                    },
                    String::new(),
                    BTreeMap::new(),
                    Some(format!("archived {day}").trim_end().to_owned()),
                    None,
                ));
            }
        }
        let (places, items): (Vec<Place>, Vec<menu::MenuItem>) = entries
            .into_iter()
            .enumerate()
            .map(|(i, (place, hint, agents, dormant, quiet))| {
                if let Some(dormant) = dormant {
                    let hint = if hint.is_empty() {
                        format!("{dormant} · Enter brings it back")
                    } else {
                        format!("{dormant} · {hint}")
                    };
                    let item = menu::MenuItem {
                        key: list_key_char(i),
                        title: format!("◌ {}", place.name),
                        hint,
                        danger: false,
                        apart: false,
                        color: Some(theme::current().muted),
                        group: None,
                        marks: Vec::new(),
                    };
                    return (place, item);
                }
                // What its agents do, as the sidebar marks them: a dot per
                // agent in its state's colour.
                let info = protocol::SessionInfo {
                    agents,
                    ..Default::default()
                };
                let urgent = info.urgent().map(|(state, _)| state);
                let dots = if place.host.is_none() && place.name == protocol::session_name() {
                    self.own_dots()
                } else {
                    self.session_info(&place)
                        .map_or_else(|| info.dots(), |found| found.dots())
                };
                let marks: Vec<(ratatui::style::Color, String)> = sidebar::agent_dots(&dots)
                    .into_iter()
                    .filter_map(|(style, dot)| Some((style.fg?, dot)))
                    .collect();
                // How long its agents have been quiet: "2h ago".
                let mut counts = String::new();
                if let Some(quiet) = quiet.filter(|&q| q >= 60) {
                    let ago = sidebar::age(Duration::from_secs(quiet));
                    counts = format!("{ago} ago");
                }
                let hint = match (counts.is_empty(), hint.is_empty()) {
                    (true, _) => hint,
                    (false, true) => counts,
                    (false, false) => format!("{counts} · {hint}"),
                };
                let item = menu::MenuItem {
                    key: if i < 9 {
                        char::from_digit(i as u32 + 1, 10).unwrap_or(' ')
                    } else {
                        ' '
                    },
                    title: match urgent {
                        Some(state) => format!("{} {}", state.symbol(), place.name),
                        None => format!("  {}", place.name),
                    },
                    hint,
                    danger: false,
                    apart: false,
                    color: urgent.and_then(|state| sidebar::state_style(state).fg),
                    group: None,
                    marks,
                };
                (place, item)
            })
            .unzip();
        // Grouped by machine under its name, which leaves the hints; what
        // does not run is a group of its own at the end.
        let mut grouped: Vec<(Place, menu::MenuItem)> = places
            .into_iter()
            .zip(items)
            .map(|(place, mut item)| {
                let asleep = item.title.starts_with('◌');
                let machine = self.machine_label(place.host.as_deref());
                let archive = archive_view
                    .then(|| {
                        archive_of
                            .get(&(place.host.clone(), place.name.clone()))
                            .cloned()
                    })
                    .flatten();
                if let Some(group) = archive {
                    item.group = Some(group);
                } else if asleep {
                    item.group = Some("◌ not running".into());
                } else if let Some(machine) = machine {
                    item.hint = item
                        .hint
                        .replace(&format!(" · {machine}"), "")
                        .replace(&machine, "")
                        .trim_start_matches(" · ")
                        .to_owned();
                    item.group = Some(machine);
                }
                (place, item)
            })
            .collect();
        // Running, then not running, then the archive, which keeps its order.
        grouped.sort_by_key(|(_, item)| {
            let archived = item.group.as_deref().is_some_and(|g| g.starts_with('▣'));
            (item.title.starts_with('◌'), archived)
        });
        let (places, mut items): (Vec<Place>, Vec<menu::MenuItem>) = grouped
            .into_iter()
            .enumerate()
            .map(|(i, (place, mut item))| {
                item.key = list_key_char(i);
                (place, item)
            })
            .unzip();
        if hidden > 0 {
            items.push(menu::MenuItem {
                key: 'o',
                title: format!("{hidden} old hidden"),
                hint: "not running, Enter on one brings it back".into(),
                danger: false,
                apart: true,
                color: Some(theme::current().muted),
                group: None,
                marks: Vec::new(),
            });
        }
        if archive_view {
            if places.is_empty() {
                items.push(menu::MenuItem {
                    key: ' ',
                    title: "the archive is empty".into(),
                    hint: "sessions come here once more than ten have ended".into(),
                    danger: false,
                    apart: true,
                    color: Some(theme::current().muted),
                    group: None,
                    marks: Vec::new(),
                });
            }
            items.push(menu::MenuItem {
                key: 'a',
                title: "back to the sessions".into(),
                hint: String::new(),
                danger: false,
                apart: true,
                color: Some(theme::current().muted),
                group: None,
                marks: Vec::new(),
            });
            return (places, items);
        }
        items.push(menu::MenuItem {
            key: 'a',
            title: format!("archive ({archived_count})"),
            hint: "ended before the ten in `last`, by hive".into(),
            danger: false,
            apart: true,
            color: Some(theme::current().muted),
            group: None,
            marks: Vec::new(),
        });
        items.push(menu::MenuItem {
            key: 'n',
            title: "new session…".into(),
            hint: "`imkr -s NAME` does the same".into(),
            danger: false,
            apart: true,
            color: None,
            group: None,
            marks: Vec::new(),
        });
        (places, items)
    }

    /// Where the user sits, as their client says: the active client's, if
    /// it tells everything, else that of a client that does. A client from
    /// before tells only session names and would make the sidebar poorer.
    fn newest_origin(&self) -> Option<protocol::Origin> {
        let full = |o: &protocol::Origin| o.via.is_some();
        let active = self.active_caps().and_then(|caps| caps.origin);
        if active.as_ref().is_some_and(full) {
            return active;
        }
        self.clients
            .iter()
            .filter_map(|c| c.caps.origin.clone())
            .find(full)
            .or(active)
    }

    /// What the client whose input is handled now can do.
    fn active_caps(&self) -> Option<Caps> {
        let client = match self.active {
            Some(id) => self.clients.iter().find(|c| c.id == id),
            None => None,
        };
        client.or(self.clients.first()).map(|c| c.caps.clone())
    }

    /// Asks where a new session goes when other machines are known, else
    /// straight for its name.
    fn new_session(&mut self) {
        self.hosting = false;
        let (machines, _) = self.machine_choices();
        if machines.len() > 1 {
            self.mode = Mode::NewSessionWhere(0);
        } else {
            self.ask_session_name(None);
        }
    }

    fn ask_session_name(&mut self, host: Option<String>) {
        self.prompt = Some(Prompt {
            purpose: PromptFor::NewSession(host),
            text: String::new(),
        });
        self.mode = Mode::Prompt;
    }

    /// The machines a session can be started on: this one, the one the
    /// user sits at if that is another, and every other one that answers.
    fn machine_choices(&self) -> (Vec<Option<String>>, Vec<menu::MenuItem>) {
        let mut machines: Vec<Option<String>> = vec![None];
        if let Some(origin) = &self.origin {
            machines.push(Some(origin.host.clone()));
        }
        for (host, sessions) in self.known_hosts() {
            if sessions.is_ok() && !machines.contains(&Some(host.clone())) {
                machines.push(Some(host));
            }
        }
        // Machines of the tailnet not known yet, as suggestions after them.
        let known: Vec<String> = self.known_hosts().into_iter().map(|(h, _)| h).collect();
        let here = protocol::machine_name().to_lowercase();
        let suggested: Vec<String> = self
            .places
            .lock()
            .unwrap()
            .tailnet
            .iter()
            .filter(|peer| **peer != here && !known.contains(peer))
            .filter(|peer| !machines.contains(&Some((*peer).clone())))
            .cloned()
            .collect();
        let first_suggested = machines.len();
        machines.extend(suggested.into_iter().map(Some));
        let items = machines
            .iter()
            .enumerate()
            .map(|(i, host)| {
                let label = self
                    .machine_label(host.as_deref())
                    .unwrap_or_else(|| format!("⌂ {}", protocol::machine_name()));
                menu::MenuItem {
                    key: char::from_digit((i as u32 + 1) % 10, 10).unwrap_or(' '),
                    title: label,
                    hint: if host.is_none() {
                        "this machine".into()
                    } else if i >= first_suggested {
                        "in your tailnet, not used with imkr yet".into()
                    } else {
                        String::new()
                    },
                    danger: false,
                    apart: false,
                    color: None,
                    group: (i >= first_suggested).then(|| "Tailscale".to_owned()),
                    marks: Vec::new(),
                }
            })
            .collect();
        (machines, items)
    }

    /// A new window in the chosen session: here right away, elsewhere after
    /// switching there, which the client asks for with `#new`.
    fn new_window_where_key(&mut self, key: KeyEvent, selected: usize) -> Result<()> {
        let (places, _) = self.session_items();
        let count = places.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return Ok(());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::NewWindowWhere((selected + count - 1) % count);
                return Ok(());
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::NewWindowWhere((selected + 1) % count);
                return Ok(());
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c @ '1'..='9') => c as usize - '1' as usize,
            _ => return Ok(()),
        };
        self.mode = Mode::Normal;
        let Some(place) = places.get(pick) else {
            return Ok(());
        };
        if place.host.is_none() && place.name == protocol::session_name() {
            return self.run_command(Command::NewWorkspace);
        }
        if !self.active_caps().is_some_and(|caps| caps.windows) {
            self.flash = Some("this terminal's imkr is too old for that, it updates itself".into());
            return Ok(());
        }
        if place.host.is_some() && !self.active_caps().is_some_and(|caps| caps.hosts) {
            return Ok(());
        }
        self.switch_session(format!("{}#new", place.target()));
        Ok(())
    }

    /// The templates a new session can start from: this machine's, then
    /// those of the user's own machine it does not have. "empty" first.
    fn templates(&self) -> Vec<crate::template::Template> {
        let mut all = crate::template::load().0;
        if let Some(origin) = &self.origin {
            for template in &origin.templates {
                if !all.iter().any(|t| t.name == template.name) {
                    all.push(template.clone());
                }
            }
        }
        all
    }

    fn template_choices(&self) -> (Vec<Option<crate::template::Template>>, Vec<menu::MenuItem>) {
        let mut templates: Vec<Option<crate::template::Template>> = vec![None];
        templates.extend(self.templates().into_iter().map(Some));
        let items = templates
            .iter()
            .enumerate()
            .map(|(i, template)| menu::MenuItem {
                key: char::from_digit((i as u32 + 1) % 10, 10).unwrap_or(' '),
                title: template
                    .as_ref()
                    .map_or("empty".into(), |t| t.title().to_owned()),
                hint: template
                    .as_ref()
                    .map_or("one shell".into(), |t| t.summary()),
                danger: false,
                apart: false,
                color: None,
                group: None,
                marks: Vec::new(),
            })
            .collect();
        (templates, items)
    }

    /// Asks for the template of the new session `place` when there are
    /// any, else goes there right away.
    fn choose_template(&mut self, place: Place) {
        let archived =
            place.host.is_none() && persist::archived().iter().any(|a| a.name == place.name);
        if self.session_exists(&place) || archived {
            self.go_to(&place);
            return;
        }
        if self.templates().is_empty() {
            self.ask_session_dir(place, None);
            return;
        }
        self.new_session = Some(place);
        self.mode = Mode::NewSessionTemplate(0);
    }

    /// Whether session `place` runs already, as far as this server knows.
    fn session_exists(&self, place: &Place) -> bool {
        let places = self.places.lock().unwrap();
        match &place.host {
            None => {
                place.name == protocol::session_name()
                    || places.local.iter().any(|s| s.name == place.name)
            }
            Some(host) => places.hosts.iter().any(|(h, sessions)| {
                h == host
                    && sessions
                        .as_ref()
                        .is_ok_and(|s| s.iter().any(|s| s.name == place.name))
            }),
        }
    }

    fn new_session_template_key(&mut self, key: KeyEvent, selected: usize) {
        let (templates, _) = self.template_choices();
        let count = templates.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.new_session = None;
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::NewSessionTemplate((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::NewSessionTemplate((selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c @ '1'..='9') => c as usize - '1' as usize,
            _ => return,
        };
        self.mode = Mode::Normal;
        let Some(place) = self.new_session.take() else {
            return;
        };
        if let Some(template) = templates.get(pick).cloned() {
            self.ask_session_dir(place, template);
        }
    }

    /// Asks where session `place` starts, suggesting the template's root,
    /// else where the user is here, else the home directory there.
    fn ask_session_dir(&mut self, place: Place, template: Option<crate::template::Template>) {
        let root = template.as_ref().and_then(|t| t.root.as_ref());
        let suggested = match (root, &place.host) {
            (Some(root), _) => root.replace("{session}", &place.name),
            (None, None) => self
                .panes
                .get(&self.ws.focus)
                .and_then(|p| p.cwd())
                .map_or("~".into(), |cwd| crate::template::with_tilde(&cwd)),
            (None, Some(_)) => "~".into(),
        };
        self.prompt = Some(Prompt {
            purpose: PromptFor::SessionDir(place, template),
            text: suggested,
        });
        self.dir_pick = None;
        // Directories there may have changed since the last time.
        self.dir_lists.lock().unwrap().clear();
        self.mode = Mode::Prompt;
    }

    /// The directories where the directory prompt's text is typed, on the
    /// machine the session starts on; `None` while another machine is
    /// still asked, through ssh in the background.
    fn dir_names(&self) -> Option<Vec<String>> {
        let Some(Prompt {
            purpose: PromptFor::SessionDir(place, _),
            text,
        }) = &self.prompt
        else {
            return Some(Vec::new());
        };
        let parent = crate::template::split_dir(text).0.to_owned();
        let Some(host) = place.host.clone() else {
            return Some(crate::template::subdirs(&parent).unwrap_or_default());
        };
        let key = (host.clone(), parent.clone());
        let mut lists = self.dir_lists.lock().unwrap();
        if let Some(listed) = lists.get(&key) {
            return listed.clone();
        }
        lists.insert(key.clone(), None);
        let lists = Arc::clone(&self.dir_lists);
        let events = self.events.clone();
        thread::spawn(move || {
            let names = crate::client::remote_output(&host, &["dirs"], &parent)
                .ok()
                .and_then(|out| serde_json::from_str::<Vec<String>>(&out).ok())
                .unwrap_or_default();
            lists.lock().unwrap().insert(key, Some(names));
            // Draws the list.
            let _ = events.send(AppEvent::PtyOutput);
        });
        None
    }

    /// The directories the directory prompt's text may complete to.
    fn dir_choices(&self) -> Option<Vec<String>> {
        let names = self.dir_names()?;
        let text = self.prompt.as_ref().map_or("", |p| p.text.as_str());
        let prefix = crate::template::split_dir(text).1;
        Some(
            crate::template::matching(&names, prefix)
                .into_iter()
                .cloned()
                .collect(),
        )
    }

    /// Tab and the arrows in the directory prompt: the arrows choose from
    /// the directories under it, Tab takes the chosen one, else completes
    /// like a shell; Enter takes the chosen one and then starts. False for
    /// keys the prompt handles itself.
    fn dir_key(&mut self, key: KeyEvent) -> bool {
        let Some(Prompt {
            purpose: PromptFor::SessionDir(..),
            ..
        }) = &self.prompt
        else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let count = self.dir_choices().map_or(0, |c| c.len().min(DIR_ROWS));
                self.dir_pick = match (self.dir_pick, key.code) {
                    _ if count == 0 => None,
                    (None, KeyCode::Down) => Some(0),
                    (None, _) => Some(count - 1),
                    (Some(i), KeyCode::Down) => Some((i + 1) % count),
                    (Some(i), _) => Some((i + count - 1) % count),
                };
                true
            }
            KeyCode::Tab => {
                let picked = self
                    .dir_pick
                    .and_then(|i| self.dir_choices()?.into_iter().nth(i));
                let names = self.dir_names();
                if let Some(prompt) = &mut self.prompt {
                    let parent = crate::template::split_dir(&prompt.text).0.to_owned();
                    match (picked, names) {
                        (Some(name), _) => prompt.text = format!("{parent}{name}/"),
                        (None, Some(names)) => {
                            prompt.text = crate::template::complete_with(&prompt.text, &names)
                        }
                        (None, None) => {}
                    }
                }
                self.dir_pick = None;
                true
            }
            KeyCode::Enter => {
                // A chosen directory is the one meant, then Enter starts as usual.
                let picked = self
                    .dir_pick
                    .and_then(|i| self.dir_choices()?.into_iter().nth(i));
                if let (Some(name), Some(prompt)) = (picked, &mut self.prompt) {
                    let parent = crate::template::split_dir(&prompt.text).0.to_owned();
                    prompt.text = format!("{parent}{name}");
                }
                self.dir_pick = None;
                false
            }
            _ => {
                // Typing changes what is listed.
                self.dir_pick = None;
                false
            }
        }
    }

    /// Starts session `place` from `template` and goes there: here right
    /// away, on another machine once it runs there.
    fn start_from_template(&mut self, place: Place, template: &crate::template::Template) {
        // Without a root of its own it starts where the user is, on this
        // machine; elsewhere that directory means nothing.
        let here = match place.host {
            None => self.panes.get(&self.ws.focus).and_then(|p| p.cwd()),
            Some(_) => None,
        };
        let saved = template.to_saved(&place.name, here.as_deref());
        match place.host.clone() {
            None => match crate::client::start_from(&place.name, saved) {
                Ok(_) => self.go_to(&place),
                Err(e) => self.flash = Some(format!("{} did not start: {e:#}", place.name)),
            },
            Some(host) => {
                if !self.active_caps().is_some_and(|caps| caps.hosts) {
                    self.go_to(&place);
                    return;
                }
                self.flash = Some(format!("starting {} on {host} …", place.name));
                let socket = self.socket.clone();
                let target = place.target();
                thread::spawn(move || {
                    let request = match crate::client::start_from_on(&host, &place.name, saved) {
                        Ok(_) => Request::Go { target },
                        Err(e) => Request::Flash {
                            text: format!("{} did not start on {host}: {e:#}", place.name),
                        },
                    };
                    let _ = crate::client::request_at(&socket, request);
                });
            }
        }
    }

    fn new_session_where_key(&mut self, key: KeyEvent, selected: usize) {
        let (machines, _) = self.machine_choices();
        let count = machines.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::NewSessionWhere((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::NewSessionWhere((selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c @ '1'..='9') => c as usize - '1' as usize,
            _ => return,
        };
        match machines.get(pick).cloned() {
            Some(host) if self.hosting => {
                self.mode = Mode::Normal;
                match host {
                    Some(host) => self.hosted_window(host),
                    None => self.switch_workspace(self.free_workspace().unwrap_or(9)),
                }
            }
            Some(host) => self.ask_session_name(host),
            None => {}
        }
    }

    /// Opens a window whose program runs on `host`, see `cli::new_hosted`,
    /// in the background: ssh may take a while.
    fn hosted_window(&mut self, host: String) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let socket = self.socket.clone();
        self.flash = Some(format!("opening a window on {host}…"));
        let _ = thread::Builder::new()
            .name("hosted-window".into())
            .spawn(move || {
                let output = std::process::Command::new(exe)
                    .args(["--host", &host, "new", "--focus"])
                    .env("IMKR_SOCKET", &socket)
                    .env_remove("IMKR_SESSION")
                    .stdin(std::process::Stdio::null())
                    .output();
                let message = match output {
                    Ok(o) if o.status.success() => return,
                    Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_owned(),
                    Err(e) => e.to_string(),
                };
                let _ = crate::client::request_at(
                    &socket,
                    Request::Flash {
                        text: format!("no window on {host}: {message}"),
                    },
                );
            });
    }

    /// Marks the focused pane for Alt+1 … Alt+9, after the marks there are.
    fn mark_focused(&mut self) {
        let id = self.ws.focus;
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let session = protocol::session_name();
        if self
            .marks
            .iter()
            .any(|m| m.host.is_none() && m.session == session && m.pane == id)
        {
            self.flash = Some("marked already, Ctrl+B M lists the marks".into());
            return;
        }
        if self.marks.len() >= 9 {
            self.flash = Some("9 marks already, Ctrl+B M to remove one".into());
            return;
        }
        let label = pane
            .name
            .clone()
            .or_else(|| pane.program())
            .unwrap_or_else(|| "pane".into());
        self.marks.push(Mark {
            host: None,
            session,
            pane: id,
            window: self.workspace,
            label: label.clone(),
            agent: pane.session.clone(),
        });
        self.save_marks();
        self.flash = Some(format!("{label} is Alt+{}", self.marks.len()));
    }

    /// Marks the agent chosen in the agent list, wherever it runs.
    fn mark_agent(&mut self, at: Option<AgentAt>) {
        let mark = match at {
            Some(AgentAt::Here(id)) => {
                let Some(pane) = self.panes.get(&id) else {
                    return;
                };
                Mark {
                    host: None,
                    session: protocol::session_name(),
                    pane: id,
                    window: self.workspace_of(id).unwrap_or(self.workspace),
                    label: pane
                        .name
                        .clone()
                        .or_else(|| pane.program())
                        .unwrap_or_default(),
                    agent: pane.session.clone(),
                }
            }
            Some(AgentAt::Other(i)) => {
                let Some((place, agent)) = self.other_agents().into_iter().nth(i) else {
                    return;
                };
                Mark {
                    host: place.host,
                    session: place.name,
                    pane: agent.pane,
                    window: agent.workspace,
                    label: agent.program,
                    agent: None,
                }
            }
            None => return,
        };
        let known = self
            .marks
            .iter()
            .any(|m| m.host == mark.host && m.session == mark.session && m.pane == mark.pane);
        if known {
            self.flash = Some("marked already".into());
        } else if self.marks.len() >= 9 {
            self.flash = Some("9 marks already, Ctrl+B M to remove one".into());
        } else {
            let label = mark.label.clone();
            self.marks.push(mark);
            self.save_marks();
            self.flash = Some(format!("{label} is Alt+{}", self.marks.len()));
        }
    }

    fn save_marks(&mut self) {
        let Some(path) = marks_path() else {
            return;
        };
        let written = serde_json::to_string_pretty(&self.marks)
            .map_err(anyhow::Error::from)
            .and_then(|json| persist::write(&path, &json));
        if let Err(e) = written {
            self.flash = Some(format!("marks not saved: {e:#}"));
        }
    }

    /// Goes to mark `i`: a pane here, in another session or on another
    /// machine. A pane here that got another id since, after a restart, is
    /// found by its agent's session or by its name in its window.
    fn jump_to_mark(&mut self, i: usize) {
        let Some(mark) = self.marks.get(i).cloned() else {
            self.flash = Some(format!("no mark {}, Ctrl+B m marks a pane", i + 1));
            return;
        };
        let here = mark.host.is_none() && mark.session == protocol::session_name();
        if !here {
            let place = Place {
                host: mark.host.clone(),
                name: mark.session.clone(),
            };
            if place.host.is_some() && !self.active_caps().is_some_and(|caps| caps.hosts) {
                self.flash =
                    Some("this terminal's imkr is too old for other machines, reattach".into());
                return;
            }
            self.switch_session(format!("{}#{}", place.target(), mark.pane));
            return;
        }
        let found = if self.panes.contains_key(&mark.pane) && self.workspace_of(mark.pane).is_some()
        {
            Some(mark.pane)
        } else {
            let by_agent = mark.agent.as_ref().and_then(|agent| {
                self.panes
                    .iter()
                    .find(|(_, p)| p.session.as_ref() == Some(agent))
                    .map(|(&id, _)| id)
            });
            by_agent.or_else(|| {
                self.panes
                    .iter()
                    .find(|&(&id, p)| {
                        self.workspace_of(id) == Some(mark.window)
                            && p.name.clone().or_else(|| p.program()).as_ref() == Some(&mark.label)
                    })
                    .map(|(&id, _)| id)
            })
        };
        match found {
            Some(id) => {
                if id != mark.pane {
                    self.marks[i].pane = id;
                    self.save_marks();
                }
                self.reveal(id);
            }
            None => self.flash = Some(format!("{} is gone, Ctrl+B M to remove it", mark.label)),
        }
    }

    /// The marks for their list, each with where it runs.
    fn mark_items(&self) -> Vec<menu::MenuItem> {
        self.marks
            .iter()
            .enumerate()
            .map(|(i, mark)| {
                let machine = match self.machine_label(mark.host.as_deref()) {
                    Some(machine) => format!("{machine} · "),
                    None => String::new(),
                };
                // Its window by name while it is in this session.
                let here = mark.host.is_none() && mark.session == protocol::session_name();
                let window = here
                    .then(|| self.window_name_of(mark.pane))
                    .flatten()
                    .map_or(mark.window.to_string(), |name| {
                        format!(
                            "{} {name}",
                            self.workspace_of(mark.pane).unwrap_or(mark.window)
                        )
                    });
                menu::MenuItem {
                    key: char::from_digit(i as u32 + 1, 10).unwrap_or(' '),
                    title: format!("{window} › {}", mark.label),
                    hint: format!("Alt+{} · {machine}{}", i + 1, mark.session),
                    danger: false,
                    apart: false,
                    color: None,
                    group: None,
                    marks: Vec::new(),
                }
            })
            .collect()
    }

    /// Enter or a digit goes to a mark, J and K move it, x removes it.
    fn marks_key(&mut self, key: KeyEvent, selected: usize) {
        let count = self.marks.len().max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Marks((selected + count - 1) % count);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Marks((selected + 1) % count);
            }
            KeyCode::Char('K') if selected > 0 && selected < self.marks.len() => {
                self.marks.swap(selected, selected - 1);
                self.save_marks();
                self.mode = Mode::Marks(selected - 1);
            }
            KeyCode::Char('J') if selected + 1 < self.marks.len() => {
                self.marks.swap(selected, selected + 1);
                self.save_marks();
                self.mode = Mode::Marks(selected + 1);
            }
            KeyCode::Char('x') if selected < self.marks.len() => {
                self.marks.remove(selected);
                self.save_marks();
                let last = self.marks.len().saturating_sub(1);
                self.mode = Mode::Marks(selected.min(last));
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.jump_to_mark(selected);
            }
            KeyCode::Char(c @ '1'..='9') => {
                self.mode = Mode::Normal;
                self.jump_to_mark(c as usize - '1' as usize);
            }
            _ => {}
        }
    }

    /// Every agent, those that need the user first and among them the
    /// longest waiting: this session's, then by where they run. Each with
    /// where it is, for `agents_key`.
    fn agent_choices(&self) -> (Vec<AgentAt>, Vec<menu::MenuItem>) {
        self.agent_entries()
            .into_iter()
            .map(|(_, at, item)| (at, item))
            .unzip()
    }

    /// `agent_choices` with each agent's state.
    fn agent_entries(&self) -> Vec<(AgentState, AgentAt, menu::MenuItem)> {
        let now = Instant::now();
        let mut all: Vec<AgentEntry> = Vec::new();
        let here = self.machine_label(None);
        for (&id, &(_, since)) in &self.agents {
            let (Some(pane), Some(state), Some(n)) = (
                self.panes.get(&id),
                self.shown_state(id),
                self.workspace_of(id),
            ) else {
                continue;
            };
            let program = pane
                .name
                .clone()
                .or_else(|| pane.program())
                .unwrap_or_default();
            let dir = self.window_name_of(id).unwrap_or_else(|| {
                pane.cwd()
                    .and_then(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_default()
            });
            let program = format!("{n} {dir} › {program}");
            let place = match pane.meta() {
                Some(meta) => format!("this session · {}", meta.label()),
                None => "this session".into(),
            };
            all.push((
                state,
                now - since,
                AgentAt::Here(id),
                program,
                place,
                here.clone(),
            ));
        }
        for (i, (place, agent)) in self.other_agents().into_iter().enumerate() {
            let Some(state) = AgentState::from_name(&agent.state) else {
                continue;
            };
            let group = self.machine_label(place.host.as_deref());
            let program = format!("{} {} › {}", agent.workspace, agent.dir, agent.program);
            let since = Duration::from_secs(agent.since);
            let place = if agent.meta.is_empty() {
                place.name
            } else {
                format!("{} · {}", place.name, agent.meta)
            };
            all.push((state, since, AgentAt::Other(i), program, place, group));
        }
        // By machine, this one first, and within each those waiting first.
        let mut machines: Vec<Option<String>> = Vec::new();
        for entry in &all {
            if !machines.contains(&entry.5) {
                machines.push(entry.5.clone());
            }
        }
        all.sort_by_key(|(state, since, .., group)| {
            (
                machines.iter().position(|m| m == group),
                sidebar::urgency(*state),
                std::cmp::Reverse(*since),
            )
        });
        all.into_iter()
            .enumerate()
            .map(|(i, (state, since, at, program, place, group))| {
                let item = menu::MenuItem {
                    key: if i < 9 {
                        char::from_digit(i as u32 + 1, 10).unwrap_or(' ')
                    } else {
                        ' '
                    },
                    title: format!("{} {program}", state.symbol()),
                    hint: format!("{place} · {}", sidebar::age(since)),
                    danger: state == AgentState::Blocked,
                    apart: false,
                    color: sidebar::state_style(state).fg,
                    group,
                    marks: Vec::new(),
                };
                (state, at, item)
            })
            .collect()
    }

    /// The inbox: the agents everywhere that wait for the user, then those
    /// done, by machine like `agent_choices`, each with the bottom of its
    /// screen.
    fn inbox_items(&self) -> (Vec<(InboxAt, String, InboxId)>, Vec<menu::MenuItem>) {
        // Contacts' messages first: nothing reaches its agent until accepted.
        let (mut targets, mut items): (Vec<_>, Vec<_>) = crate::contacts::waiting()
            .into_iter()
            .map(|m| {
                let age = std::time::Duration::from_secs(crate::web::now().saturating_sub(m.at));
                let item = menu::MenuItem {
                    key: ' ',
                    title: format!("✉ {}", m.from),
                    hint: format!("to {} · {}", m.to, sidebar::age(age)),
                    danger: false,
                    apart: false,
                    color: Some(theme::current().accent),
                    group: Some("Messages from contacts".into()),
                    marks: Vec::new(),
                };
                (
                    (InboxAt::Message(m.id), m.text, InboxId::Message(m.id)),
                    item,
                )
            })
            .unzip();
        let (agents, agent_items) = self.inbox_agents();
        targets.extend(agents);
        items.extend(agent_items);
        (targets, items)
    }

    /// The inbox's agents: those waiting for the user, then those done.
    fn inbox_agents(&self) -> (Vec<(InboxAt, String, InboxId)>, Vec<menu::MenuItem>) {
        let others = self.other_agents();
        let mut entries: Vec<_> = self
            .agent_entries()
            .into_iter()
            .filter(|(state, ..)| matches!(state, AgentState::Blocked | AgentState::Done))
            .collect();
        // Machines in the order of `agent_entries`, those waiting first in
        // each.
        let mut groups: Vec<Option<String>> = Vec::new();
        for (_, _, item) in &entries {
            if !groups.contains(&item.group) {
                groups.push(item.group.clone());
            }
        }
        entries.sort_by_key(|(state, _, item)| {
            (
                groups.iter().position(|g| *g == item.group),
                *state != AgentState::Blocked,
            )
        });
        entries
            .into_iter()
            .map(|(_, at, mut item)| {
                let (tail, id) = match at {
                    AgentAt::Here(id) => (
                        self.panes
                            .get(&id)
                            .map(|pane| agent::tail(pane.screen().screen(), INBOX_LINES))
                            .unwrap_or_default(),
                        InboxId::Agent(None, protocol::session_name(), id),
                    ),
                    AgentAt::Other(i) => match others.get(i) {
                        Some((place, agent)) => (
                            agent.tail.clone(),
                            InboxId::Agent(place.host.clone(), place.name.clone(), agent.pane),
                        ),
                        None => (String::new(), InboxId::Agent(None, String::new(), 0)),
                    },
                };
                item.key = ' ';
                ((InboxAt::Agent(at), tail, id), item)
            })
            .unzip()
    }

    /// Where the agent selected in the inbox is now, `None` once it left
    /// the list; the one at `fallback` before anything was selected.
    fn inbox_selected(
        &self,
        targets: &[(InboxAt, String, InboxId)],
        fallback: usize,
    ) -> Option<usize> {
        match &self.inbox_on {
            Some(on) => targets.iter().position(|(_, _, id)| id == on),
            None => (!targets.is_empty()).then(|| fallback.min(targets.len() - 1)),
        }
    }

    /// Up and down choose an agent, a digit chooses that option of its
    /// question, typed text and Enter answer it, Enter alone goes there.
    /// Answers are always the user's own: the inbox only carries them, and
    /// only to the agent whose question is shown.
    fn inbox_key(&mut self, key: KeyEvent, fallback: usize) {
        let (targets, items) = self.inbox_items();
        let count = targets.len().max(1);
        let selected = self.inbox_selected(&targets, fallback);
        let at = selected.map(|i| targets[i].0);
        let message = match at {
            Some(InboxAt::Message(id)) => Some(id),
            _ => None,
        };
        let agent_at = match at {
            Some(InboxAt::Agent(at)) => Some(at),
            _ => None,
        };
        let waiting = selected.is_some_and(|i| items[i].danger);
        let select = |app: &mut Self, i: usize| {
            app.inbox_answer.clear();
            app.inbox_on = targets.get(i).map(|(_, _, id)| id.clone());
            app.mode = Mode::Inbox(i);
        };
        let here = selected.unwrap_or(fallback.min(count - 1));
        match key.code {
            KeyCode::Esc if !self.inbox_answer.is_empty() => self.inbox_answer.clear(),
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::BackTab => select(self, (here + count - 1) % count),
            KeyCode::Down | KeyCode::Tab => select(self, (here + 1) % count),
            // A contact's message: only taken or thrown away, never answered
            // here, the agent it is for answers.
            KeyCode::Enter if let Some(id) = message => self.accept_message(id),
            KeyCode::Char('x') | KeyCode::Delete if let Some(id) = message => {
                self.flash = Some(match crate::contacts::decline(id) {
                    Ok(()) => "message thrown away".into(),
                    Err(e) => format!("{e:#}"),
                });
            }
            KeyCode::Char(_) | KeyCode::Backspace if message.is_some() => {}
            KeyCode::Backspace => {
                self.inbox_answer.pop();
            }
            // Gone from the list: say so, and show the one now in its place.
            KeyCode::Enter | KeyCode::Char('1'..='9') if at.is_none() && !targets.is_empty() => {
                select(self, here);
                self.flash = Some("that agent no longer waits, nothing sent".into());
            }
            KeyCode::Enter if self.inbox_answer.is_empty() => {
                self.mode = Mode::Normal;
                match agent_at {
                    Some(AgentAt::Here(id)) => self.reveal(id),
                    Some(AgentAt::Other(i)) => self.go_to_other_agent(i),
                    None => {}
                }
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.inbox_answer);
                if let Some(at) = agent_at {
                    self.answer(at, &text, true);
                }
            }
            KeyCode::Char(c @ '1'..='9') if waiting && self.inbox_answer.is_empty() => {
                if let Some(at) = agent_at {
                    self.answer(at, &c.to_string(), false);
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.inbox_answer.push(c);
            }
            _ => {}
        }
    }

    /// Accepts a contact's message: typed into its pane in the background,
    /// as the session it is for may be this one.
    fn accept_message(&mut self, id: u64) {
        self.flash = Some("accepted, typing it in".into());
        let socket = self.socket.clone();
        thread::spawn(move || {
            let text = match crate::contacts::accept(id) {
                Ok(done) => format!("message {done}"),
                Err(e) => format!("message kept, not delivered: {e:#}"),
            };
            let _ = crate::client::request_at(&socket, Request::Flash { text });
        });
    }

    /// Types `text` into the agent at `at`, then Enter if `enter`: here
    /// directly, in another session through its socket, on another machine
    /// through ssh. Waits for neither, failures show in the control bar.
    fn answer(&mut self, at: AgentAt, text: &str, enter: bool) {
        let (place, pane) = match at {
            AgentAt::Here(id) => {
                if let Err(e) = self.type_into(id, text, enter) {
                    self.flash = Some(format!("could not answer: {e}"));
                }
                return;
            }
            AgentAt::Other(i) => match self.other_agents().into_iter().nth(i) {
                Some((place, agent)) => (place, agent.pane),
                None => return,
            },
        };
        self.flash = Some(format!("answered {}", place.name));
        let text = text.to_owned();
        let socket = self.socket.clone();
        thread::spawn(move || {
            let result = match &place.host {
                None => protocol::session_socket(&place.name).and_then(|path| {
                    crate::client::request_at(&path, Request::Send { pane, text, enter })
                }),
                Some(host) => {
                    let pane = pane.to_string();
                    let mut args = vec!["-s", &place.name, "send", "--stdin", "--pane", &pane];
                    if !enter {
                        args.push("--no-enter");
                    }
                    crate::client::remote_output(host, &args, &text)
                        .map(|_| serde_json::Value::Null)
                }
            };
            if let Err(e) = result {
                let text = format!("could not answer {}: {e:#}", place.name);
                let _ = crate::client::request_at(&socket, Request::Flash { text });
            }
        });
    }

    /// The panes of every other session, here and on the other machines,
    /// with where they are. Sessions from versions before list only their
    /// agents.
    fn other_panes(&self) -> Vec<(Place, protocol::AgentInfo)> {
        let local = self.places.lock().unwrap().local.clone();
        let hosts = self.known_hosts();
        let mut groups: Vec<(Option<String>, protocol::SessionInfo)> =
            local.into_iter().map(|s| (None, s)).collect();
        if let Some(origin) = &self.origin {
            for session in &origin.infos {
                groups.push((Some(origin.host.clone()), session.clone()));
            }
        }
        for (host, sessions) in hosts {
            for session in sessions.into_iter().flatten() {
                groups.push((Some(host.clone()), session));
            }
        }
        let mut out = Vec::new();
        for (host, session) in groups {
            let panes = if session.all_panes.is_empty() {
                session.agent_panes.clone()
            } else {
                session.all_panes.clone()
            };
            for pane in panes {
                let place = Place {
                    host: host.clone(),
                    name: session.name.clone(),
                };
                out.push((place, pane));
            }
        }
        out
    }

    /// Every window everywhere: this session's first, then those of the
    /// other sessions on every machine. Each with what its agents do, in
    /// the sidebar's colours, and its programs.
    fn window_items(&self) -> (Vec<Spot>, Vec<menu::MenuItem>) {
        let mut entries: Vec<WindowEntry> = Vec::new();
        for n in self.workspaces() {
            let Some(ws) = self.workspace_ref(n) else {
                continue;
            };
            let panes = ws.panes();
            let name = ws
                .name
                .clone()
                .or_else(|| {
                    let cwd = self.panes.get(&ws.focus)?.cwd()?;
                    Some(cwd.file_name()?.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            let states = panes
                .iter()
                .filter_map(|&id| self.shown_state(id))
                .collect();
            let programs = panes
                .iter()
                .filter_map(|id| {
                    let pane = self.panes.get(id)?;
                    pane.name.clone().or_else(|| pane.program())
                })
                .collect();
            let at = if n == self.workspace {
                "shown".to_owned()
            } else {
                "this session".to_owned()
            };
            entries.push(WindowEntry {
                spot: Spot::Window(n),
                number: n,
                name,
                states,
                programs,
                at,
                group: self.machine_label(None),
            });
        }
        let mut groups: Vec<(Place, u8, Vec<protocol::AgentInfo>)> = Vec::new();
        for (place, pane) in self.other_panes() {
            match groups
                .iter_mut()
                .find(|(p, n, _)| *p == place && *n == pane.workspace)
            {
                Some((_, _, panes)) => panes.push(pane),
                None => groups.push((place, pane.workspace, vec![pane])),
            }
        }
        for (place, n, panes) in groups {
            let states = panes
                .iter()
                .filter_map(|p| AgentState::from_name(&p.state))
                .collect();
            let programs = panes.iter().map(|p| p.program.clone()).collect();
            entries.push(WindowEntry {
                at: place.name.clone(),
                group: self.machine_label(place.host.as_deref()),
                spot: Spot::Elsewhere(place, panes[0].pane),
                number: n,
                name: panes[0].dir.clone(),
                states,
                programs,
            });
        }
        entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let WindowEntry {
                    spot,
                    number: n,
                    name,
                    states,
                    programs,
                    at,
                    group,
                } = entry;
                let urgent = states.iter().copied().min_by_key(|s| sidebar::urgency(*s));
                let mut hint = vec![at];
                let counts = state_counts(&states);
                if !counts.is_empty() {
                    hint.push(counts);
                }
                hint.push(programs.join(", "));
                let item = menu::MenuItem {
                    key: list_key_char(i),
                    title: match urgent {
                        Some(state) => format!("{} {n} {name}", state.symbol()),
                        None => format!("  {n} {name}"),
                    },
                    hint: hint.join(" · "),
                    danger: false,
                    apart: false,
                    color: urgent.and_then(|state| sidebar::state_style(state).fg),
                    group,
                    marks: Vec::new(),
                };
                (spot, item)
            })
            .unzip()
    }

    /// Every pane everywhere: this session's, then those of the other
    /// sessions on every machine, each after its window.
    fn pane_items(&self) -> (Vec<Spot>, Vec<menu::MenuItem>) {
        let mut entries: Vec<PaneEntry> = Vec::new();
        let here = self.machine_label(None);
        for n in self.workspaces() {
            let Some(ws) = self.workspace_ref(n) else {
                continue;
            };
            for id in ws.panes() {
                let Some(pane) = self.panes.get(&id) else {
                    continue;
                };
                let program = pane
                    .name
                    .clone()
                    .or_else(|| pane.program())
                    .unwrap_or_default();
                let window = self.window_name_of(id).unwrap_or_default();
                let dir = pane.cwd().map(|c| tilde(&c)).unwrap_or_default();
                entries.push((
                    Spot::Pane(id),
                    format!("{n} {window} › {program}"),
                    self.shown_state(id),
                    format!("this session · pane {id} · {dir}"),
                    here.clone(),
                ));
            }
        }
        for (place, pane) in self.other_panes() {
            let group = self.machine_label(place.host.as_deref());
            entries.push((
                Spot::Elsewhere(place.clone(), pane.pane),
                format!("{} {} › {}", pane.workspace, pane.dir, pane.program),
                AgentState::from_name(&pane.state),
                place.name,
                group,
            ));
        }
        entries
            .into_iter()
            .enumerate()
            .map(|(i, (spot, label, state, hint, group))| {
                let item = menu::MenuItem {
                    key: list_key_char(i),
                    title: match state {
                        Some(state) => format!("{} {label}", state.symbol()),
                        None => format!("  {label}"),
                    },
                    hint,
                    danger: false,
                    apart: false,
                    color: state.and_then(|state| sidebar::state_style(state).fg),
                    group,
                    marks: Vec::new(),
                };
                (spot, item)
            })
            .unzip()
    }

    /// Keys of the window and pane lists: Enter or a digit goes there.
    fn spot_list_key(&mut self, key: KeyEvent, selected: usize, panes: bool) {
        let spots = if panes {
            self.pane_items().0
        } else {
            self.window_items().0
        };
        let mode = |i| {
            if panes {
                Mode::Panes(i)
            } else {
                Mode::Windows(i)
            }
        };
        let count = spots.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = mode((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = mode((selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c @ '1'..='9') => c as usize - '1' as usize,
            _ => return,
        };
        self.mode = Mode::Normal;
        if let Some(spot) = spots.into_iter().nth(pick) {
            self.go_spot(spot);
        }
    }

    fn go_spot(&mut self, spot: Spot) {
        match spot {
            Spot::Window(n) => self.switch_workspace(n),
            Spot::Pane(id) => self.reveal(id),
            Spot::Elsewhere(place, pane) => {
                if place.host.is_some() && !self.active_caps().is_some_and(|caps| caps.hosts) {
                    self.flash =
                        Some("this terminal's imkr is too old for other machines, reattach".into());
                } else if self.active_caps().is_some_and(|caps| caps.panes) {
                    self.switch_session(format!("{}#{pane}", place.target()));
                } else {
                    self.switch_session(place.target());
                }
            }
        }
    }

    /// Enter or a digit goes to that agent, here or wherever it runs.
    fn agents_key(&mut self, key: KeyEvent, selected: usize) {
        let (targets, _) = self.agent_choices();
        let count = targets.len().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Agents((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Agents((selected + 1) % count);
                return;
            }
            KeyCode::Char('m') => {
                self.mark_agent(targets.get(selected).copied());
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => c as usize - '1' as usize,
            _ => return,
        };
        self.mode = Mode::Normal;
        match targets.get(pick) {
            Some(AgentAt::Here(id)) => self.reveal(*id),
            Some(AgentAt::Other(i)) => self.go_to_other_agent(*i),
            None => {}
        }
    }

    /// Where a window can move: every other running session, then a new
    /// one.
    fn move_to_items(&self) -> (Vec<String>, Vec<menu::MenuItem>) {
        let current = protocol::session_name();
        let names: Vec<String> = protocol::sessions()
            .into_iter()
            .filter(|name| *name != current)
            .collect();
        let mut items: Vec<menu::MenuItem> = names
            .iter()
            .enumerate()
            .map(|(i, name)| menu::MenuItem {
                key: char::from_digit((i as u32 + 1) % 10, 10).unwrap_or(' '),
                title: name.clone(),
                hint: String::new(),
                danger: false,
                apart: false,
                color: None,
                group: None,
                marks: Vec::new(),
            })
            .collect();
        items.push(menu::MenuItem {
            key: 'n',
            title: "new session…".into(),
            hint: "started for this window".into(),
            danger: false,
            apart: true,
            color: None,
            group: None,
            marks: Vec::new(),
        });
        (names, items)
    }

    /// Enter or a digit moves window `window` to that session, n asks for
    /// the name of a new one.
    fn move_to_key(&mut self, key: KeyEvent, window: u8, selected: usize) {
        let (names, items) = self.move_to_items();
        // The arrows stay on the running sessions: a new one takes n.
        let count = items.iter().filter(|item| !item.apart).count().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::MoveTo(window, (selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::MoveTo(window, (selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char(c) => match items.iter().position(|item| item.key == c) {
                Some(i) => i,
                None => return,
            },
            _ => return,
        };
        self.mode = Mode::Normal;
        match names.get(pick) {
            Some(name) => {
                if let Err(e) = self.move_window(window, &name.clone()) {
                    self.flash = Some(e);
                }
            }
            None => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::MoveWindow(window),
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
            }
        }
    }

    /// The hives menu's entries: every hive with its sessions and age.
    fn hive_items(&self) -> (Vec<String>, Vec<menu::MenuItem>) {
        let infos = persist::hive_infos();
        let running = protocol::sessions();
        let active = persist::active_hive();
        let items = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                let age = persist::now().saturating_sub(info.saved_at);
                let age = crate::sidebar::age(Duration::from_secs(age));
                let mut sessions: Vec<String> = info
                    .sessions
                    .iter()
                    .map(|s| {
                        if running.contains(s) {
                            format!("{s}●")
                        } else {
                            s.clone()
                        }
                    })
                    .collect();
                if !info.elsewhere.is_empty() {
                    sessions.push(format!("⇄ {}", info.elsewhere.join(", ")));
                }
                let auto = if info.name == persist::LAST {
                    " · ended sessions"
                } else {
                    ""
                };
                let title = if active.as_deref() == Some(info.name.as_str()) {
                    format!("{} ● active", info.name)
                } else {
                    info.name.clone()
                };
                menu::MenuItem {
                    key: char::from_digit((i as u32 + 1) % 10, 10).unwrap_or(' '),
                    title,
                    hint: format!("{} · {age} ago{auto}", sessions.join(", ")),
                    danger: false,
                    apart: false,
                    color: None,
                    group: None,
                    marks: Vec::new(),
                }
            })
            .collect();
        (infos.into_iter().map(|i| i.name).collect(), items)
    }

    /// The session switcher: arrows or j/k choose, Enter or the digit
    /// switches, n starts a new session.
    fn sessions_key(&mut self, key: KeyEvent, selected: usize) {
        let (places, items) = self.session_items();
        // Ending and renaming reach only the sessions on this machine.
        let names: Vec<String> = places
            .iter()
            .map(|p| match p.host {
                None => p.name.clone(),
                Some(_) => String::new(),
            })
            .collect();
        // The arrows stay on what runs: a new session takes n or a click.
        let count = items.iter().filter(|item| !item.apart).count().max(1);
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.show_archive = false;
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Sessions((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Sessions((selected + 1) % count);
                return;
            }
            KeyCode::Enter => selected,
            KeyCode::Char('n') => names.len(),
            KeyCode::Char('o') => {
                self.hide_old = !self.hide_old;
                self.mode = Mode::Sessions(0);
                return;
            }
            KeyCode::Char('a') => {
                self.show_archive = !self.show_archive;
                self.mode = Mode::Sessions(0);
                return;
            }
            KeyCode::Char('x')
                if self.show_archive && places.get(selected).is_some_and(|p| p.host.is_some()) =>
            {
                if let Some(place) = places.get(selected) {
                    self.forget_elsewhere(place);
                }
                return;
            }
            KeyCode::Char('x') => {
                if let Some(name) = names.get(selected).filter(|n| !n.is_empty()) {
                    let place = Place {
                        host: None,
                        name: name.clone(),
                    };
                    let archived = !self.session_exists(&place)
                        && persist::archived().iter().any(|a| a.name == *name);
                    let asleep = self.dormant_of(&place).is_some() || archived;
                    self.confirm_target = Some(name.clone());
                    self.mode = Mode::Confirm(if asleep {
                        Action::ForgetSession
                    } else {
                        Action::EndSession
                    });
                }
                return;
            }
            KeyCode::Char('s') => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::SaveHive,
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
                return;
            }
            KeyCode::Char('h') => {
                self.mode = Mode::Hives(0);
                return;
            }
            KeyCode::Char('r') => {
                if let Some(name) = names.get(selected).filter(|n| !n.is_empty()) {
                    self.prompt = Some(Prompt {
                        purpose: PromptFor::RenameSession(name.clone()),
                        text: name.clone(),
                    });
                    self.mode = Mode::Prompt;
                }
                return;
            }
            KeyCode::Char(c) => match items.iter().position(|item| item.key == c) {
                Some(i) => i,
                None => return,
            },
            _ => return,
        };
        // In the archive only its entries are picked, nothing starts anew.
        let in_archive = self.show_archive;
        if in_archive && places.get(pick).is_none() {
            return;
        }
        self.mode = Mode::Normal;
        self.show_archive = false;
        match places.get(pick) {
            Some(place) if in_archive && place.host.is_some() => self.revive_elsewhere(place),
            Some(place) if place.host.is_none() && place.name == protocol::session_name() => {}
            Some(place) => self.go_to(place),
            None if pick == places.len() => {
                self.new_session();
            }
            None => {}
        }
    }

    /// The hives menu: Enter or the digit brings a hive back, s saves the
    /// running sessions as a new one, u over the selected one, r renames,
    /// x deletes.
    fn hives_key(&mut self, key: KeyEvent, selected: usize) {
        let (names, _) = self.hive_items();
        let count = names.len().max(1);
        let selected = selected.min(count - 1);
        let chosen = names.get(selected).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Hives((selected + count - 1) % count);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Hives((selected + 1) % count);
            }
            KeyCode::Char('s') => {
                self.prompt = Some(Prompt {
                    purpose: PromptFor::SaveHive,
                    text: String::new(),
                });
                self.mode = Mode::Prompt;
            }
            KeyCode::Char('u') => {
                if let Some(name) = chosen {
                    self.confirm_target = Some(name);
                    self.mode = Mode::Confirm(Action::OverwriteHive);
                }
            }
            KeyCode::Char('r') => {
                if let Some(name) = chosen {
                    self.prompt = Some(Prompt {
                        purpose: PromptFor::RenameHive(name.clone()),
                        text: name,
                    });
                    self.mode = Mode::Prompt;
                }
            }
            KeyCode::Char('x') => {
                if let Some(name) = chosen {
                    self.confirm_target = Some(name);
                    self.mode = Mode::Confirm(Action::DeleteHive);
                }
            }
            KeyCode::Char('w') => {
                if let Some(name) = chosen {
                    self.confirm_list = self.hive_switch_ends(&name);
                    self.confirm_target = Some(name);
                    self.mode = Mode::Confirm(Action::SwitchHive);
                }
            }
            KeyCode::Enter => {
                if let Some(name) = chosen {
                    self.mode = Mode::Normal;
                    self.restore_hive(&name);
                }
            }
            KeyCode::Char(c) => {
                if let Some(i) = c.to_digit(10)
                    && let Some(name) = names.get((i as usize + 9) % 10)
                {
                    let name = name.clone();
                    self.mode = Mode::Normal;
                    self.restore_hive(&name);
                }
            }
            _ => {}
        }
    }

    /// What switching to hive `name` would end, as the session lists know
    /// it now: running sessions of its machines that it does not have. The
    /// switch itself asks the machines again.
    fn hive_switch_ends(&self, name: &str) -> Vec<String> {
        let Ok(Some(hive)) = persist::load_hive(name) else {
            return Vec::new();
        };
        let machines: Vec<(bool, Vec<String>, String)> = crate::client::hive_machines(hive)
            .into_iter()
            .map(|m| {
                let here = crate::client::is_this_machine(&m);
                (here, m.sessions.keys().cloned().collect(), m.name)
            })
            .collect();
        let in_hive_here: Vec<String> = machines
            .iter()
            .filter(|m| m.0)
            .flat_map(|m| m.1.clone())
            .collect();
        let mut ends = Vec::new();
        let own = protocol::session_name();
        if !in_hive_here.contains(&own) {
            ends.push(format!("{own} (this one)"));
        }
        let places = self.places.lock().unwrap().clone();
        for session in places.local.iter().filter(|s| s.dormant.is_none()) {
            if !in_hive_here.contains(&session.name) {
                ends.push(session.name.clone());
            }
        }
        for (_, sessions, host) in machines.iter().filter(|m| !m.0) {
            if let Some((_, Ok(running))) = places.hosts.iter().find(|(h, _)| h == host) {
                for session in running.iter().filter(|s| s.dormant.is_none()) {
                    if !sessions.contains(&session.name) {
                        ends.push(format!("{host}: {}", session.name));
                    }
                }
            }
        }
        ends
    }

    /// Switches to hive `name`: brings it back, ends what is not in it,
    /// this session last, on a thread that says how it went.
    fn switch_hive(&mut self, name: &str) {
        let socket = self.socket.clone();
        let own = protocol::session_name();
        // This session's layout from here, as `save_hive` takes it.
        let layout = self.saved();
        let name = name.to_owned();
        self.flash = Some(format!("switching to hive {name}…"));
        thread::spawn(move || {
            let result = crate::client::hive_outsiders(&name).and_then(|outsiders| {
                let lines = crate::client::switch_hive(
                    &name,
                    &outsiders,
                    Some(&own),
                    Some((own.as_str(), layout)),
                )?;
                let ends_here = outsiders.iter().any(|(h, s)| h.is_none() && *s == own);
                Ok((lines, ends_here))
            });
            match result {
                Ok((lines, ends_here)) => {
                    let text = format!("hive {name}: {}", lines.join(" · "));
                    let _ = crate::client::request_at(&socket, Request::Flash { text });
                    // Its clients go on to a session of the hive.
                    if ends_here {
                        let _ = crate::client::kill_server_at(&socket);
                    }
                }
                Err(e) => {
                    let text = format!("not switched: {e:#}");
                    let _ = crate::client::request_at(&socket, Request::Flash { text });
                }
            }
        });
    }

    /// Brings hive `name` back on every machine in it, see
    /// `client::restore_hive`. Machines take a moment to answer, so it runs
    /// on a thread, which says how it went through the socket.
    fn restore_hive(&mut self, name: &str) {
        let socket = self.socket.clone();
        let name = name.to_owned();
        self.flash = Some(format!("bringing back hive {name}…"));
        thread::spawn(move || {
            let text = match crate::client::restore_hive(&name, None, None) {
                Ok(lines) => format!("hive {name}: {}", lines.join(" · ")),
                Err(e) => format!("not restored: {e:#}"),
            };
            let _ = crate::client::request_at(&socket, Request::Flash { text });
        });
    }

    /// Saves every running session as hive `name`, here and on every known
    /// machine, this one from memory: asking its own socket would wait on
    /// itself. Asking the other machines takes a moment, so it happens on
    /// a thread, which says how it went through the socket.
    fn save_hive(&mut self, name: &str) {
        let own = (protocol::session_name(), self.saved());
        let socket = self.socket.clone();
        let name = name.to_owned();
        self.flash = Some(format!("saving hive {name}…"));
        thread::spawn(move || {
            let previous = persist::load_hive(&name).ok().flatten();
            let result = crate::client::snapshot_everywhere(
                Some((own.0.as_str(), own.1)),
                previous.as_ref(),
            )
            .and_then(|hive| {
                persist::write_hive(&name, &hive)?;
                // Every machine gets it, to bring back from anywhere.
                if name != persist::LAST {
                    crate::client::push_hive(&name, &hive);
                    persist::set_active_hive(Some(&name))?;
                }
                Ok(hive)
            });
            let text = match result {
                Ok(hive) => {
                    let elsewhere: usize = hive.machines.iter().map(|m| m.sessions.len()).sum();
                    format!(
                        "hive {name} saved: {} here, {elsewhere} on {} other machines",
                        hive.sessions.len(),
                        hive.machines.len()
                    )
                }
                Err(e) => format!("not saved: {e:#}"),
            };
            let _ = crate::client::request_at(&socket, Request::Flash { text });
        });
    }

    /// Ends session `name`: this one like the quit menu does, another one
    /// through its socket. Either way it lands in the hive `last`.
    fn end_session(&mut self, name: &str) {
        if name == protocol::session_name() {
            self.quit = true;
            return;
        }
        match protocol::session_socket(name) {
            Ok(socket) => {
                // Waiting for it to go would hold up this session's loop.
                thread::spawn(move || crate::client::kill_server_at(&socket));
                self.flash = Some(format!("session {name} ended, ^B H brings it back"));
            }
            Err(e) => self.flash = Some(format!("not ended: {e:#}")),
        }
    }

    /// Renames this session: its socket and saved state move to the new
    /// name, the old socket name stays as a link for the panes' hooks.
    fn rename_session(&mut self, name: &str) -> std::result::Result<(), String> {
        if !protocol::valid_session_name(name) {
            return Err("session names use letters, digits, - and _".into());
        }
        if name == protocol::session_name() {
            return Ok(());
        }
        if std::env::var_os("IMKR_SOCKET").is_some() {
            return Err("a session on a socket from IMKR_SOCKET has no name to change".into());
        }
        let socket = protocol::session_socket(name).map_err(|e| e.to_string())?;
        if UnixStream::connect(&socket).is_ok() {
            return Err(format!("there is a session called {name} already"));
        }
        let old = self.socket.clone();
        std::fs::rename(&old, &socket).map_err(|e| format!("cannot rename the socket: {e}"))?;
        let _ = std::fs::rename(old.with_extension("log"), socket.with_extension("log"));
        if std::os::unix::fs::symlink(&socket, &old).is_ok() {
            self.old_sockets.push(old);
        }
        protocol::set_session_name(name);
        self.socket = socket;
        let state = persist::path().ok();
        if let (Some(from), Some(to)) = (&self.state_path, &state) {
            let _ = std::fs::rename(from, to);
            let _ = std::fs::rename(persist::scrollback_dir(from), persist::scrollback_dir(to));
        }
        self.state_path = state;
        self.last_saved.clear();
        Ok(())
    }

    /// Asks the server of session `old` to be called `name`.
    fn rename_other_session(&mut self, old: &str, name: &str) {
        let result = if old == protocol::session_name() {
            self.rename_session(name)
        } else {
            protocol::session_socket(old)
                .and_then(|socket| {
                    crate::client::request_at(
                        &socket,
                        Request::RenameSession {
                            name: name.to_owned(),
                        },
                    )
                })
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        };
        self.flash = Some(match result {
            Ok(()) => format!("session {old} is now {name}"),
            Err(e) => format!("not renamed: {e}"),
        });
    }

    /// Opens the session list with this session selected.
    fn open_sessions(&mut self) {
        self.show_archive = false;
        let current = protocol::session_name();
        let selected = self
            .session_items()
            .0
            .iter()
            .position(|p| p.host.is_none() && p.name == current)
            .unwrap_or(0);
        self.mode = Mode::Sessions(selected);
    }

    /// Brings back `place`, archived on another machine: there if it
    /// answers, else it asks whether to start it here (#506).
    fn revive_elsewhere(&mut self, place: &Place) {
        let Some(host) = place.host.clone() else {
            return;
        };
        let answers = self
            .places
            .lock()
            .unwrap()
            .hosts
            .iter()
            .any(|(h, sessions)| *h == host && sessions.is_ok());
        if !answers {
            self.confirm_target = Some(format!("{host}:{}", place.name));
            self.mode = Mode::Confirm(Action::StartArchivedHere);
            return;
        }
        let socket = self.socket.clone();
        let (name, target) = (place.name.clone(), place.target());
        self.flash = Some(format!("bringing {name} back on {host} …"));
        thread::spawn(move || {
            let request = match crate::client::remote_output(&host, &["revive", &name], "") {
                Ok(_) => {
                    let _ = persist::drop_from_replica(&host, &name);
                    Request::Go { target }
                }
                Err(e) => Request::Flash {
                    text: format!("{name} cannot come back on {host}: {e:#}"),
                },
            };
            let _ = crate::client::request_at(&socket, request);
        });
    }

    /// Starts here the archived session `host:name` of a machine that does
    /// not answer, from the layout kept of its archive.
    fn start_archived_here(&mut self, target: &str) {
        let Some((host, name)) = target.split_once(':') else {
            return;
        };
        let entry = persist::replicas()
            .into_iter()
            .find(|r| r.host == host)
            .and_then(|r| r.sessions.into_iter().find(|a| a.name == name));
        let Some(entry) = entry else {
            self.flash = Some(format!("{name} is no longer in the archive of {host}"));
            return;
        };
        let here = Place {
            host: None,
            name: name.to_owned(),
        };
        if self.session_exists(&here) || persist::archived().iter().any(|a| a.name == name) {
            self.flash = Some(format!("a session {name} is here already, rename it first"));
            return;
        }
        let socket = self.socket.clone();
        let (host, name) = (host.to_owned(), name.to_owned());
        self.flash = Some(format!("starting {name} here …"));
        thread::spawn(move || {
            let taken = entry.clone();
            let request = match crate::client::start_from(&name, entry.layout) {
                Ok(_) => {
                    // Its machine drops it from its archive once it answers.
                    let _ = persist::note_taken(&host, &taken);
                    let _ = persist::drop_from_replica(&host, &name);
                    Request::Go { target: name }
                }
                Err(e) => Request::Flash {
                    text: format!("{name} did not start here: {e:#}"),
                },
            };
            let _ = crate::client::request_at(&socket, request);
        });
    }

    /// Forgets `place`, archived on another machine, there; that machine
    /// has to answer.
    fn forget_elsewhere(&mut self, place: &Place) {
        let Some(host) = place.host.clone() else {
            return;
        };
        let socket = self.socket.clone();
        let name = place.name.clone();
        self.flash = Some(format!("forgetting {name} on {host} …"));
        thread::spawn(move || {
            let text = match crate::client::remote_output(&host, &["forget", &name], "") {
                Ok(_) => {
                    let _ = persist::drop_from_replica(&host, &name);
                    format!("{name} forgotten on {host}")
                }
                Err(e) => format!("{name} stays: {host} does not answer ({e:#})"),
            };
            let _ = crate::client::request_at(&socket, Request::Flash { text });
        });
    }

    /// Sends the active client to `place`, on this machine or another.
    fn go_to(&mut self, place: &Place) {
        if place.host.is_some() && !self.active_caps().is_some_and(|caps| caps.hosts) {
            self.flash =
                Some("this terminal's imkr is too old for other machines, reattach".into());
            return;
        }
        // A session closed on purpose comes back from the hive `last` first,
        // an older one from the archive; a stopped one brings its layout
        // back by itself when it starts.
        let archived = place.host.is_none()
            && !self.session_exists(place)
            && persist::archived().iter().any(|a| a.name == place.name);
        if archived || self.dormant_of(place).as_deref() == Some("closed") {
            match place.host.clone() {
                None => {
                    if let Err(e) = persist::revive(&place.name) {
                        self.flash = Some(format!("{} cannot come back: {e:#}", place.name));
                        return;
                    }
                }
                Some(host) => {
                    let socket = self.socket.clone();
                    let (name, target) = (place.name.clone(), place.target());
                    self.flash = Some(format!("bringing {name} back on {host} …"));
                    thread::spawn(move || {
                        let request =
                            match crate::client::remote_output(&host, &["revive", &name], "") {
                                Ok(_) => Request::Go { target },
                                Err(e) => Request::Flash {
                                    text: format!("{name} cannot come back: {e:#}"),
                                },
                            };
                        let _ = crate::client::request_at(&socket, request);
                    });
                    return;
                }
            }
        }
        self.switch_session(place.target());
    }

    /// The session info of `place` as this server last heard of it.
    fn session_info(&self, place: &Place) -> Option<protocol::SessionInfo> {
        let find =
            |infos: &[protocol::SessionInfo]| infos.iter().find(|s| s.name == place.name).cloned();
        match &place.host {
            None => find(&self.places.lock().unwrap().local),
            Some(host) => {
                if let Some(origin) = self.origin.as_ref().filter(|o| o.host == *host)
                    && let Some(info) = find(&origin.infos)
                {
                    return Some(info);
                }
                self.known_hosts()
                    .into_iter()
                    .find(|(h, _)| h == host)
                    .and_then(|(_, sessions)| find(&sessions.ok()?))
            }
        }
    }

    /// `stopped` or `closed` when session `place` does not run.
    fn dormant_of(&self, place: &Place) -> Option<String> {
        self.session_info(place)?.dormant
    }

    /// Sends the active client on to session `name`; it starts it if
    /// needed. The other clients stay.
    fn switch_session(&mut self, name: String) {
        let Some(id) = self.active else {
            return;
        };
        if let Some(mut client) = self.take_client(id) {
            let _ = client.send(&ServerMsg::Switch(name));
            self.fit_screen(false);
        }
    }

    /// Typing filters, arrows or Ctrl-N/P move, Enter goes to the pane.
    fn picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.picker else {
            self.mode = Mode::Normal;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.picker = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let selected = picker.selected;
                let found = self.found().into_iter().nth(selected);
                self.picker = None;
                self.mode = Mode::Normal;
                if let Some(found) = found {
                    self.go(found.go);
                }
            }
            KeyCode::Down | KeyCode::Tab => picker.selected += 1,
            KeyCode::Char('n' | 'j') if ctrl => picker.selected += 1,
            KeyCode::Up | KeyCode::BackTab => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('p' | 'k') if ctrl => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Char('u' | 'w') if ctrl => {
                picker.query.clear();
                picker.selected = 0;
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                picker.query.push(c);
                picker.selected = 0;
            }
            _ => {}
        }
        // Keep the selection on an entry.
        if self.picker.is_some() {
            let count = self.found().len();
            if let Some(picker) = &mut self.picker {
                picker.selected = picker.selected.min(count.saturating_sub(1));
            }
        }
    }

    /// What the picker shows for its query: the panes, or with `global`
    /// everything there is to go to, words in any order.
    fn found(&self) -> Vec<Found> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let panes = self.picker_entries(&picker.query).into_iter().map(|entry| {
            let state = entry
                .state
                .map(|s| format!("  {} {}", s.symbol(), s.name()))
                .unwrap_or_default();
            Found {
                kind: if picker.global { "pane" } else { "" },
                label: format!(
                    "{} {} › {}{state}",
                    entry.workspace,
                    self.window_name_of(entry.pane).unwrap_or_default(),
                    entry.label
                ),
                hint: format!("pane {} · {}", entry.pane, entry.dir),
                style: entry.state.map(state_style).unwrap_or_default(),
                go: Go::Pane(entry.pane),
            }
        });
        if !picker.global {
            return panes.collect();
        }
        let mut all: Vec<Found> = Vec::new();
        for n in self.workspaces() {
            if let Some(ws) = self.workspace_ref(n) {
                all.push(Found {
                    kind: "window",
                    label: tab_label(n, ws).trim().to_owned(),
                    hint: format!("{} panes", ws.panes().len()),
                    style: Style::new().fg(theme::current().blue),
                    go: Go::Window(n),
                });
            }
        }
        let (places, items) = self.session_items();
        let current = protocol::session_name();
        for (place, item) in places.into_iter().zip(items) {
            if item.apart || (place.host.is_none() && place.name == current) {
                continue;
            }
            all.push(Found {
                kind: "session",
                label: item.title,
                hint: item.hint,
                style: Style::new().fg(theme::current().magenta),
                go: Go::Place(place),
            });
        }
        let (agents, items) = self.agent_choices();
        for (at, item) in agents.into_iter().zip(items) {
            if let AgentAt::Other(i) = at {
                all.push(Found {
                    kind: "agent",
                    label: item.title,
                    hint: item.hint,
                    style: item.color.map(|c| Style::new().fg(c)).unwrap_or_default(),
                    go: Go::Agent(i),
                });
            }
        }
        for plugin in self.plugins.lock().unwrap().iter() {
            all.push(Found {
                kind: "plugin",
                label: plugin.title().to_owned(),
                hint: plugin.key.map(|k| format!("^B p {k}")).unwrap_or_default(),
                style: Style::new().fg(theme::current().magenta),
                go: Go::Plugin(plugin.name.clone()),
            });
        }
        for (i, mark) in self.marks.iter().enumerate() {
            all.push(Found {
                kind: "mark",
                label: mark.label.clone(),
                hint: format!("Alt+{} · {}", i + 1, mark.session),
                style: Style::new().fg(theme::current().magenta),
                go: Go::Mark(i),
            });
        }
        for hive in &picker.hives {
            let mut sessions = hive.sessions.clone();
            sessions.extend(hive.elsewhere.iter().cloned());
            all.push(Found {
                kind: "hive",
                label: hive.name.clone(),
                hint: format!("bring back {}", sessions.join(", ")),
                style: Style::new().fg(theme::current().orange),
                go: Go::Hive(hive.name.clone()),
            });
        }
        for binding in bindings::BINDINGS {
            // Keys for one command each; ranges like 1-9 are not one.
            let [(_, command)] = binding.keys else {
                continue;
            };
            if *command == Command::Search {
                continue;
            }
            let path = binding.group.menu().path();
            let keys = if path.is_empty() {
                format!("^B {}", binding.label)
            } else {
                format!("^B {path} {}", binding.label)
            };
            all.push(Found {
                kind: "command",
                label: binding.description.trim_end_matches('…').to_owned(),
                hint: keys,
                style: Style::new().fg(theme::current().cyan),
                go: Go::Command(*command),
            });
        }
        let words: Vec<String> = picker
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        all.retain(|found| {
            let haystack = format!("{} {} {}", found.kind, found.label, found.hint).to_lowercase();
            words.iter().all(|w| haystack.contains(w))
        });
        panes.chain(all).collect()
    }

    /// Goes where a found thing is, or does it.
    fn go(&mut self, go: Go) {
        match go {
            Go::Pane(id) => self.reveal(id),
            Go::Window(n) => self.switch_workspace(n),
            Go::Place(place) => self.go_to(&place),
            Go::Agent(i) => self.go_to_other_agent(i),
            Go::Plugin(name) => {
                let plugin = self
                    .plugins
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| p.name == name)
                    .cloned();
                if let Some(plugin) = plugin {
                    self.run_plugin(&plugin);
                }
            }
            Go::Hive(name) => self.restore_hive(&name),
            Go::Mark(i) => self.jump_to_mark(i),
            Go::Command(command) => {
                if let Err(e) = self.run_command(command) {
                    self.flash = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// The picker in the middle of the screen.
    fn draw_picker(&self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &self.picker else { return };
        let entries = self.found();
        let accent = theme::current().accent;
        let dim = Style::new().add_modifier(Modifier::DIM);
        let width = area.width.saturating_sub(4).min(90);
        let rows = usize::from(area.height.saturating_sub(8)).max(3);
        let inner_width = usize::from(width.saturating_sub(6));

        let mut lines = vec![
            Line::from(vec![
                Span::styled("› ", Style::new().fg(accent).add_modifier(Modifier::BOLD)),
                Span::raw(picker.query.clone()),
            ]),
            Line::default(),
        ];
        // Scroll the list so the selection stays visible.
        let first = picker.selected.saturating_sub(rows - 1);
        for (i, entry) in entries.iter().enumerate().skip(first).take(rows) {
            let kind = if entry.kind.is_empty() {
                String::new()
            } else {
                format!("{:<8}", entry.kind)
            };
            let label: String = entry
                .label
                .chars()
                .take(inner_width.saturating_sub(kind.chars().count() + 2))
                .collect();
            let used = kind.chars().count() + label.chars().count() + 2;
            let hint = shorten(&entry.hint, inner_width.saturating_sub(used));
            let pad = inner_width.saturating_sub(used + hint.chars().count());
            let mut line = Line::from(vec![
                Span::styled(kind, dim),
                Span::styled(label, entry.style),
                Span::raw("  "),
                Span::raw(" ".repeat(pad)),
                Span::styled(hint, dim),
            ]);
            if i == picker.selected {
                line = line.patch_style(Style::new().bg(theme::current().subtle));
            }
            lines.push(line);
        }
        if entries.is_empty() {
            lines.push(Line::styled(
                if picker.global {
                    "nothing matches"
                } else {
                    "no pane matches"
                },
                dim,
            ));
        }
        lines.push(Line::default());
        lines.push(Line::styled(
            "type to filter · ↑↓ choose · Enter go · Esc cancel",
            dim,
        ));

        let popup = centered(area, width, lines.len() as u16 + 2);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(accent))
            .title(Span::styled(
                if picker.global {
                    format!(" Search · {} ", entries.len())
                } else {
                    format!(" Go to pane · {} ", entries.len())
                },
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ))
            .padding(Padding::horizontal(2));
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new(lines).block(block), popup);
        let x = inner.x + 2 + picker.query.chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(inner.right().saturating_sub(1)),
            inner.y,
        ));
    }

    fn sidebar_focused(&self) -> bool {
        matches!(self.mode, Mode::Sidebar(_))
    }

    /// Gives the sidebar the keyboard, with the row of the focused pane's
    /// agent, or else the active workspace, selected.
    fn focus_sidebar(&mut self) {
        let targets = sidebar::targets(&self.sidebar_data());
        let selected = targets
            .iter()
            .position(|t| *t == sidebar::Target::Pane(self.ws.focus))
            .or_else(|| {
                targets
                    .iter()
                    .position(|t| *t == sidebar::Target::Workspace(self.workspace))
            })
            .unwrap_or(0);
        self.mode = Mode::Sidebar(selected);
    }

    /// The sidebar with the keyboard: j/k move, Enter goes there, r names
    /// the row's workspace or pane, x closes the row's pane, Esc leaves. In
    /// the file tree Enter opens a folder or a file, h closes the folder, f
    /// jumps there.
    fn sidebar_key(&mut self, key: KeyEvent, selected: usize) {
        let targets = sidebar::targets(&self.sidebar_data());
        if targets.is_empty() {
            self.mode = Mode::Normal;
            return;
        }
        let selected = selected.min(targets.len() - 1);
        let target = targets[selected];
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'e') => self.mode = Mode::Normal,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Sidebar((selected + 1) % targets.len());
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.mode = Mode::Sidebar((selected + targets.len() - 1) % targets.len());
            }
            KeyCode::Char('g') | KeyCode::Home => self.mode = Mode::Sidebar(0),
            KeyCode::Char('G') | KeyCode::End => self.mode = Mode::Sidebar(targets.len() - 1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.mode = Mode::Normal;
                match target {
                    sidebar::Target::Workspace(n) => self.switch_workspace(n),
                    sidebar::Target::Session(i) => self.go_to_row(i),
                    sidebar::Target::Popup(i) => self.toggle_popup_at(i),
                    sidebar::Target::OtherAgent(i) => self.go_to_other_agent(i),
                    sidebar::Target::Pane(id) => self.reveal(id),
                    sidebar::Target::Change(i) => self.open_change(i, false),
                    sidebar::Target::File(i) => {
                        if self.open_file(i) {
                            self.mode = Mode::Sidebar(selected);
                        }
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let sidebar::Target::File(i) = target
                    && let Some(to) = self.files.collapse(i)
                {
                    let row = targets
                        .iter()
                        .position(|t| *t == sidebar::Target::File(to))
                        .unwrap_or(selected);
                    self.mode = Mode::Sidebar(row);
                }
            }
            KeyCode::Char('f') => {
                if let Some(row) = targets
                    .iter()
                    .position(|t| matches!(t, sidebar::Target::File(_)))
                {
                    self.mode = Mode::Sidebar(row);
                }
            }
            KeyCode::Char('o') => {
                if let sidebar::Target::Change(i) = target {
                    self.mode = Mode::Normal;
                    self.open_change(i, true);
                }
            }
            KeyCode::Char('r') => match target {
                sidebar::Target::Workspace(n) => self.start_prompt(RenameTarget::Workspace(n)),
                sidebar::Target::Pane(id) => self.start_prompt(RenameTarget::Pane(id)),
                sidebar::Target::Change(_)
                | sidebar::Target::File(_)
                | sidebar::Target::Session(_)
                | sidebar::Target::Popup(_)
                | sidebar::Target::OtherAgent(_) => {}
            },
            KeyCode::Char('x') => match target {
                sidebar::Target::Pane(id) => self.mode = Mode::Confirm(Action::ClosePane(id)),
                sidebar::Target::Popup(i) => {
                    let name = self
                        .popup_entries()
                        .into_iter()
                        .nth(i)
                        .map(|(name, _)| name);
                    if let Some(popup) = name.and_then(|name| self.popups.get(&name)) {
                        self.mode = Mode::Confirm(Action::ClosePane(popup.pane));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Opens the name prompt for `target`, filled with its current name.
    fn start_prompt(&mut self, target: RenameTarget) {
        let text = self.name_of(target).unwrap_or_default();
        self.prompt = Some(Prompt {
            purpose: PromptFor::Rename(target),
            text,
        });
        self.mode = Mode::Prompt;
    }

    fn name_of(&self, target: RenameTarget) -> Option<String> {
        match target {
            RenameTarget::Pane(id) => self.panes.get(&id)?.name.clone(),
            RenameTarget::Workspace(n) => self.workspace_ref(n)?.name.clone(),
        }
    }

    fn workspace_ref(&self, n: u8) -> Option<&Workspace> {
        if n == self.workspace {
            Some(&self.ws)
        } else {
            self.hidden.get(&n)
        }
    }

    /// Typing a name: Enter saves it, an empty name clears it, Esc cancels.
    fn prompt_key(&mut self, key: KeyEvent) {
        if self.dir_key(key) {
            return;
        }
        let Some(prompt) = &mut self.prompt else {
            self.mode = Mode::Normal;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                let searching = matches!(prompt.purpose, PromptFor::Search { .. });
                let noting = prompt.purpose == PromptFor::ReviewNote;
                let purpose = self.prompt.take().map(|p| p.purpose);
                self.mode = if searching {
                    Mode::Copy
                } else if noting {
                    Mode::Review
                } else {
                    Mode::Normal
                };
                if let Some(PromptFor::NewWindow { pane, back, .. }) = purpose {
                    self.cancel_new_window(pane, back);
                }
            }
            KeyCode::Enter => {
                let Prompt { purpose, text } = self.prompt.take().expect("checked above");
                self.mode = Mode::Normal;
                match purpose {
                    PromptFor::NewWindow { window, .. } => {
                        let name = Some(text).filter(|t| !t.trim().is_empty());
                        if let Err(e) = self.rename(RenameTarget::Workspace(window), name) {
                            self.flash = Some(e);
                        }
                    }
                    PromptFor::Rename(target) => {
                        let name = Some(text).filter(|t| !t.trim().is_empty());
                        if let Err(e) = self.rename(target, name) {
                            self.flash = Some(e);
                        }
                    }
                    PromptFor::RenameSession(old) => {
                        self.mode = Mode::Normal;
                        self.rename_other_session(&old, text.trim());
                    }
                    PromptFor::MoveWindow(n) => {
                        if let Err(e) = self.move_window(n, text.trim()) {
                            self.flash = Some(e);
                        }
                    }
                    PromptFor::Worktree => {
                        if let Err(e) = self.worktree_window(text.trim(), &[], None) {
                            self.flash = Some(e);
                        }
                    }
                    PromptFor::SaveLayout => {
                        if let Err(e) = self.layout(text.trim(), true) {
                            self.flash = Some(e);
                        }
                    }
                    PromptFor::ReviewNote => {
                        self.mode = Mode::Review;
                        if let Some(review) = &mut self.review
                            && let Some(line) = review.lines.get(review.cursor)
                        {
                            review.notes.retain(|n| !n.is_on(line));
                            if !text.trim().is_empty() {
                                let note = crate::review::Note::on(line, text.trim().to_owned());
                                review.notes.push(note);
                            }
                        }
                    }
                    PromptFor::SessionDir(place, template) => {
                        // Without a template: one shell, in that directory.
                        let mut template = template.unwrap_or_else(|| crate::template::Template {
                            windows: vec![crate::template::Window::default()],
                            ..Default::default()
                        });
                        let dir = text.trim().trim_end_matches('/');
                        template.root = Some(if dir.is_empty() { "~" } else { dir }.to_owned());
                        self.start_from_template(place, &template);
                    }
                    PromptFor::NewSession(host) => {
                        let name = text.trim().to_owned();
                        if protocol::valid_session_name(&name) {
                            self.choose_template(Place { host, name });
                        } else {
                            self.flash = Some("session names use letters, digits, - and _".into());
                        }
                    }
                    PromptFor::SaveHive => {
                        let name = text.trim().to_owned();
                        if !protocol::valid_session_name(&name) {
                            self.flash = Some("hive names use letters, digits, - and _".into());
                        } else if persist::hive_infos().iter().any(|h| h.name == name) {
                            // Saving over a hive is what u is for, confirmed.
                            self.confirm_target = Some(name);
                            self.mode = Mode::Confirm(Action::OverwriteHive);
                        } else {
                            self.save_hive(&name);
                        }
                    }
                    PromptFor::RenameHive(old) => {
                        let name = text.trim();
                        self.flash = Some(match persist::rename_hive(&old, name) {
                            Ok(()) => format!("hive {old} is now {name}"),
                            Err(e) => format!("not renamed: {e:#}"),
                        });
                    }
                    PromptFor::Search { forward } => {
                        self.mode = Mode::Copy;
                        if !text.is_empty() {
                            self.search = Some((text, forward));
                            self.search_next(false);
                        }
                    }
                }
            }
            KeyCode::Char('w') if ctrl && matches!(prompt.purpose, PromptFor::SessionDir(..)) => {
                // Back to the directory above, as Ctrl+W does for a word.
                let trimmed = prompt.text.trim_end_matches('/');
                let cut = trimmed.rfind('/').map_or(0, |i| i + 1);
                prompt.text.truncate(cut);
            }
            KeyCode::Char(c)
                if !ctrl
                    && matches!(prompt.purpose, PromptFor::SessionDir(..))
                    && prompt.text.len() < MAX_PATH =>
            {
                prompt.text.push(c);
            }
            KeyCode::Backspace if ctrl => prompt.text.clear(),
            KeyCode::Char('u' | 'w') if ctrl => prompt.text.clear(),
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            KeyCode::Char(c) if !ctrl && prompt.text.chars().count() < MAX_NAME => {
                prompt.text.push(c);
            }
            _ => {}
        }
    }

    /// Names a pane or a workspace, or clears the name with `None`.
    fn rename(&mut self, target: RenameTarget, name: Option<String>) -> Reply {
        let name = name
            .map(|n| {
                n.chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_NAME)
                    .collect::<String>()
            })
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty());
        match target {
            RenameTarget::Pane(id) => {
                let pane = self.panes.get_mut(&id).ok_or(format!("no pane {id}"))?;
                pane.name = name.clone();
            }
            RenameTarget::Workspace(n) => {
                let ws = if n == self.workspace {
                    &mut self.ws
                } else {
                    self.hidden.get_mut(&n).ok_or(format!("no window {n}"))?
                };
                ws.name = name.clone();
            }
        }
        Ok(serde_json::json!({ "name": name }))
    }

    /// Whether images are shown: an attached client whose terminal can, and
    /// the setting on.
    fn graphics_enabled(&self) -> bool {
        self.config.images && self.graphics_caps().is_some()
    }

    /// What the first client that shows images can do. Programs are told
    /// its cell size, the others may scale a little differently.
    fn graphics_caps(&self) -> Option<Caps> {
        self.clients
            .iter()
            .map(|c| &c.caps)
            .find(|c| c.graphics)
            .cloned()
    }

    /// Tells every pane whether to handle images and the cell size, and
    /// writes the answers to programs' graphics queries.
    fn sync_graphics(&mut self) {
        let enabled = self.graphics_enabled();
        let caps = self.graphics_caps().unwrap_or_default();
        let cell = (caps.cell_width, caps.cell_height);
        for pane in self.panes.values_mut() {
            let replies = {
                let mut g = pane.graphics.lock().unwrap();
                g.enabled = enabled;
                g.cell = cell;
                std::mem::take(&mut g.replies)
            };
            for reply in replies {
                let _ = pane.write(&reply);
            }
        }
    }

    /// What every client that shows images needs after a frame: the new
    /// image data, every image with `resend`, and where the images go, see
    /// `placements`. Images show only in panes nothing is drawn over.
    fn image_state(&self, enabled: bool, resend: bool) -> (Vec<u8>, Placements) {
        let mut out = Vec::new();
        for pane in self.panes.values() {
            let mut g = pane.graphics.lock().unwrap();
            if enabled && resend {
                g.resend();
            }
            for command in g.outbox.drain(..) {
                if enabled {
                    out.extend_from_slice(&command);
                }
            }
        }

        (out, self.image_places(enabled))
    }

    /// Where the images of the shown window go, see `image_state`.
    fn image_places(&self, enabled: bool) -> Placements {
        let mut wanted: Placements = HashMap::new();
        // Nothing may be drawn over them, a popup included.
        let quiet = matches!(self.mode, Mode::Normal | Mode::Repeat(..) | Mode::Copy)
            && self.shown_popup().is_none();
        if enabled && quiet {
            let rects = self.rects_of(&self.ws);
            for (i, &(id, rect)) in rects.iter().enumerate() {
                // Anything drawn later, i.e. a floating pane, hides it.
                if rects[i + 1..].iter().any(|(_, r)| r.intersects(rect)) {
                    continue;
                }
                let Some(pane) = self.panes.get(&id) else {
                    continue;
                };
                let inner = pane_inner(rect);
                let top = (pane.history_len() - pane.scroll_offset()) as i64;
                let placements = pane.graphics.lock().unwrap().placements.clone();
                for p in placements {
                    let row = p.row as i64 - top;
                    let fits = row >= 0
                        && row + i64::from(p.rows) <= i64::from(inner.height)
                        && p.col + p.cols <= inner.width;
                    if fits {
                        let at = (inner.x + p.col, inner.y + row as u16, p.cols, p.rows);
                        wanted.insert((p.image, p.id), at);
                    }
                }
            }
        }
        wanted
    }

    /// `imkr · 2 api · claude`: the workspace, then the focused pane.
    fn window_title_text(&self) -> String {
        let ws = match &self.ws.name {
            Some(name) => format!("{} {name}", self.workspace),
            None => self.workspace.to_string(),
        };
        let pane = self
            .panes
            .get(&self.ws.focus)
            .and_then(|p| p.name.clone().or_else(|| p.program()))
            .unwrap_or_default();
        format!("imkr · {ws} · {pane}")
    }

    /// Moves window `n` with its running panes to session `to`, started
    /// if needed. The processes go on there, untouched: their ptys are
    /// passed over the session's socket.
    fn move_window(&mut self, n: u8, to: &str) -> Reply {
        if !self.workspace_exists(n) {
            return Err(format!("no window {n}"));
        }
        if !protocol::valid_session_name(to) {
            return Err(format!(
                "{to:?} is no session name: letters, digits, - and _ only"
            ));
        }
        if to == protocol::session_name() {
            return Err(format!("window {n} is in session {to} already"));
        }
        let socket = protocol::session_socket(to).map_err(|e| format!("{e:#}"))?;
        let (stream, fresh) = match UnixStream::connect(&socket) {
            Ok(stream) => (stream, false),
            Err(_) => (
                crate::client::start_server(&socket, Some(to)).map_err(|e| format!("{e:#}"))?,
                true,
            ),
        };
        let ws = if n == self.workspace {
            self.ws.clone()
        } else {
            self.hidden[&n].clone()
        };
        let ids = ws.panes();

        // Nothing may be read from the ptys here anymore: what comes next
        // is the other session's to read.
        for id in &ids {
            if let Some(pane) = self.panes.get_mut(id) {
                pane.stop_reading();
            }
        }
        let mut panes = BTreeMap::new();
        let mut fds = Vec::new();
        for id in &ids {
            let handed = self.panes.get(id).and_then(|pane| {
                let mut handed = pane.handover()?;
                fds.push(handed.fd);
                handed.fd = (fds.len() - 1) as RawFd;
                Some(upgrade::MovedPane {
                    pane: handed,
                    name: pane.name.clone(),
                    session: pane.session.clone(),
                    notes: pane.notes.clone(),
                })
            });
            match handed {
                Some(handed) => {
                    panes.insert(*id, handed);
                }
                None => {
                    self.read_on(&ids);
                    return Err(format!("pane {id} cannot be moved"));
                }
            }
        }
        let window = upgrade::MovedWindow {
            number: n,
            workspace: ws,
            panes,
            fresh,
        };
        let sent = (|| -> Result<serde_json::Value> {
            let mut stream = stream;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            ClientMsg::Move(serde_json::to_vec(&window)?).write_to(&mut stream)?;
            crate::fdpass::send(&stream, &fds)?;
            loop {
                match ServerMsg::read_from(&mut stream)? {
                    Some(ServerMsg::Reply(Ok(value))) => return Ok(value),
                    Some(ServerMsg::Reply(Err(message))) => anyhow::bail!(message),
                    Some(_) => {}
                    None => anyhow::bail!("session {to} closed the connection"),
                }
            }
        })();
        let reply = match sent {
            Ok(reply) => reply,
            Err(e) => {
                self.read_on(&ids);
                return Err(format!("not moved: {e:#}"));
            }
        };

        // The panes live on there. Here they only go away.
        let new_ids: HashMap<PaneId, PaneId> =
            serde_json::from_value(reply["panes"].clone()).unwrap_or_default();
        for id in &ids {
            if let Some(pane) = self.panes.get_mut(id) {
                pane.release();
            }
            if let Some(&there) = new_ids.get(id) {
                self.moved.insert(*id, (socket.clone(), there));
            }
            self.close(*id);
        }
        let there = reply["window"].as_u64().unwrap_or(0);
        self.flash = Some(format!("window {n} is now window {there} of session {to}"));
        Ok(serde_json::json!({ "window": n, "to": to, "as": there, "panes": new_ids }))
    }

    /// Reads the ptys of `ids` again, after a move that did not happen.
    fn read_on(&mut self, ids: &[PaneId]) {
        for id in ids {
            if let Some(pane) = self.panes.get_mut(id)
                && let Err(e) = pane.read_on()
            {
                eprintln!("pane {id} cannot be read anymore: {e:#}");
            }
        }
    }

    /// Takes in a window another session moved here, keeping its number
    /// and its panes' ids where they are free.
    fn move_in(&mut self, window: upgrade::MovedWindow, fds: Vec<OwnedFd>) -> Reply {
        let keep = (1..=9).contains(&window.number) && !self.workspace_exists(window.number);
        let Some(n) = keep
            .then_some(window.number)
            .or_else(|| self.free_workspace())
        else {
            return Err(format!(
                "session {} has all 9 windows in use",
                protocol::session_name()
            ));
        };
        if fds.len() != window.panes.len() {
            return Err("the panes' terminals did not all arrive".into());
        }
        let mut new_ids: HashMap<PaneId, PaneId> = HashMap::new();
        for &old in window.panes.keys() {
            let taken = |id: PaneId, new_ids: &HashMap<PaneId, PaneId>| {
                self.panes.contains_key(&id) || new_ids.values().any(|&v| v == id)
            };
            let mut new = old;
            if taken(new, &new_ids) {
                new = self.next_id;
                while taken(new, &new_ids) {
                    new += 1;
                }
            }
            new_ids.insert(old, new);
        }
        self.next_id = self
            .next_id
            .max(new_ids.values().max().map_or(0, |id| id + 1));

        let first_shell = self
            .first_shell
            .filter(|_| window.fresh && self.panes.len() == 1);
        let mut fds: Vec<Option<OwnedFd>> = fds.into_iter().map(Some).collect();
        let mut ws = window.workspace;
        ws.renumber(&|id| new_ids.get(&id).copied().unwrap_or(id));
        for (old, mut moved) in window.panes {
            let new = new_ids[&old];
            let fd = usize::try_from(moved.pane.fd)
                .ok()
                .and_then(|i| fds.get_mut(i)?.take());
            let Some(fd) = fd else {
                ws.remove(new);
                continue;
            };
            moved.pane.fd = fd.into_raw_fd();
            match Pane::take_over(new, &moved.pane, self.events.clone()) {
                Ok(mut pane) => {
                    pane.name = moved.name;
                    pane.session = moved.session;
                    pane.notes = moved.notes;
                    self.panes.insert(new, pane);
                }
                Err(e) => {
                    eprintln!("could not take over pane {old}: {e:#}");
                    ws.remove(new);
                }
            }
        }
        if ws.is_empty() {
            return Err("no pane could be taken over".into());
        }
        self.hidden.insert(n, ws);
        // Started for this window: its own first shell makes way.
        if let Some(id) = first_shell {
            self.close(id);
        }
        self.first_shell = None;
        Ok(serde_json::json!({ "window": n, "panes": new_ids }))
    }

    /// Draws with the configured theme from now on. When it follows
    /// Omarchy, remembers which Omarchy theme that was.
    fn apply_theme(&mut self) {
        agent::configure(&self.config.extra_agents, &self.config.question_phrases);
        sidebar::set_nerd_font(self.config.nerd_font);
        crate::pane::set_tmux_shim(if self.config.tmux_shim {
            crate::tmux::shim_dir().ok()
        } else {
            None
        });
        self.followed = theme::omarchy_stamp();
        theme::set(theme::load(&self.config.theme));
    }

    /// Picks up a theme switch on the desktop when following Omarchy.
    fn follow_omarchy(&mut self) {
        if self.config.theme == theme::FOLLOW && theme::omarchy_stamp() != self.followed {
            self.apply_theme();
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::ClosePane(id) => self.close(id),
            Action::KillServer => self.quit = true,
            Action::EndSession => {
                if let Some(name) = self.confirm_target.take() {
                    self.end_session(&name);
                }
            }
            Action::ForgetSession => {
                if let Some(name) = self.confirm_target.take() {
                    self.flash = Some(match persist::forget(&name) {
                        Ok(_) => format!("{name} forgotten"),
                        Err(e) => format!("not forgotten: {e:#}"),
                    });
                }
            }
            Action::DeleteHive => {
                if let Some(name) = self.confirm_target.take() {
                    self.flash = Some(match persist::delete_hive(&name) {
                        Ok(()) => format!("hive {name} deleted"),
                        Err(e) => format!("not deleted: {e:#}"),
                    });
                }
            }
            Action::OverwriteHive => {
                if let Some(name) = self.confirm_target.take() {
                    self.save_hive(&name);
                }
            }
            Action::SwitchHive => {
                if let Some(name) = self.confirm_target.take() {
                    self.switch_hive(&name);
                }
            }
            Action::StartArchivedHere => {
                if let Some(target) = self.confirm_target.take() {
                    self.start_archived_here(&target);
                }
            }
        }
    }

    /// Splits the focused pane and focuses the new one. Does nothing if the
    /// pane floats or is too small to be split.
    fn split(&mut self, axis: Axis) {
        let Some((_, rect)) = self
            .ws
            .layout
            .rects(self.body)
            .into_iter()
            .find(|(id, _)| *id == self.ws.focus)
        else {
            return;
        };
        let room = match axis {
            Axis::Row => rect.width,
            Axis::Column => rect.height,
        };
        if room < 2 * MIN_PANE_SIZE {
            return;
        }

        let cwd = self.focused_cwd();
        let id = self.next_id;
        self.next_id += 1;
        self.ws.split(axis, id);
        self.spawn_into_ws(id, cwd, &[]);
    }

    /// Runs `script` with `sh -c` in a new floating pane, in `cwd`. When it
    /// fails, the pane stays until Enter, so the error can be read.
    fn float_script(&mut self, name: &str, script: &str, args: &[&str], cwd: Option<PathBuf>) {
        let wrapped = format!(
            "{script}\nstatus=$?\nif [ $status -ne 0 ]; then printf '\\n[exited with %s, Enter closes]' $status; read _; fi"
        );
        let mut command = vec!["sh".to_owned(), "-c".to_owned(), wrapped, "sh".to_owned()];
        command.extend(args.iter().map(|a| a.to_string()));
        let id = self.next_id;
        self.next_id += 1;
        self.ws.add_float(id, self.body);
        self.spawn_into_ws(id, cwd, &command);
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.name = Some(name.to_owned());
        }
    }

    /// Runs the user's command bound to `key` in the commands menu.
    fn run_user_command(&mut self, key: char) {
        let Some(command) = self.config.commands.iter().find(|c| c.key == key).cloned() else {
            return;
        };
        let cwd = self.focused_cwd();
        if command.float {
            self.float_script(&command.name, &command.command, &[], cwd);
            return;
        }
        let id = self.next_id;
        if !self.ws.split(Axis::Row, id) {
            return;
        }
        self.next_id += 1;
        let script = vec!["sh".to_owned(), "-c".to_owned(), command.command.clone()];
        self.spawn_into_ws(id, cwd, &script);
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.name = Some(command.name);
        }
    }

    /// The diff of changed file `i` in a floating pane, or with `editor` the
    /// file itself in `$EDITOR`.
    fn open_change(&mut self, i: usize, editor: bool) {
        let Some(git) = &self.git else { return };
        let Some(change) = git.changes.get(i) else {
            return;
        };
        let (root, path) = (git.root.clone(), change.path.clone());
        if editor {
            let name = format!("edit {path}");
            self.float_script(&name, "\"${EDITOR:-nvim}\" \"$1\"", &[&path], Some(root));
        } else {
            let name = format!("diff {path}");
            self.float_script(&name, DIFF_SCRIPT, &[&path], Some(root));
        }
    }

    /// The focused pane's whole history in `$EDITOR`, like herdr's prefix+e.
    fn scrollback_in_editor(&mut self) {
        let id = self.ws.focus;
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let text = pane.read(Some(usize::MAX));
        let dir = self
            .socket
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let file = dir.join(format!("scrollback-{id}.txt"));
        if let Err(e) = std::fs::write(&file, text + "\n") {
            self.flash = Some(format!("cannot write the scrollback: {e}"));
            return;
        }
        let file = file.to_string_lossy().into_owned();
        self.float_script(
            &format!("history {id}"),
            "\"${EDITOR:-less}\" \"$1\"; rm -f \"$1\"",
            &[&file],
            self.focused_cwd(),
        );
    }

    /// Keeps the focused pane's `git status` for the sidebar, at most every
    /// two seconds and only while the sidebar shows.
    fn refresh_git(&mut self) {
        if self.sidebar.is_none() {
            return;
        }
        let cwd = self.focused_cwd();
        let fresh = self.git.as_ref().is_some_and(|g| {
            Some(&g.cwd) == cwd.as_ref() && g.at.elapsed() < Duration::from_secs(2)
        });
        if fresh {
            return;
        }
        self.git = cwd.and_then(|cwd| git_state(&cwd));
    }

    /// Starts a shell in a new floating pane in the middle of the screen.
    fn new_float(&mut self) {
        let cwd = self.focused_cwd();
        let id = self.next_id;
        self.next_id += 1;
        self.ws.add_float(id, self.body);
        self.spawn_into_ws(id, cwd, &[]);
    }

    /// Starts the shell for pane `id`, which was just added to the active
    /// workspace, sized to the area it got there. Takes the pane out again
    /// if the shell cannot be started.
    fn spawn_into_ws(&mut self, id: PaneId, cwd: Option<PathBuf>, command: &[String]) {
        let rect = self
            .ws
            .rects(self.body)
            .into_iter()
            .find_map(|(pane, rect)| (pane == id).then_some(rect))
            .unwrap_or(self.body);
        let inner = pane_inner(rect);
        let spawned = Pane::spawn(
            Spawn {
                id,
                rows: inner.height,
                cols: inner.width,
                cwd: cwd.as_deref(),
                command,
                socket: &self.socket,
                accounts: &self.accounts,
            },
            self.events.clone(),
        );
        match spawned {
            Ok(pane) => {
                self.panes.insert(id, pane);
            }
            Err(e) => {
                eprintln!("failed to start a shell: {e:#}");
                self.ws.remove(id);
            }
        }
    }

    /// The working directory of the focused pane, where new panes start.
    fn focused_cwd(&self) -> Option<PathBuf> {
        self.panes.get(&self.ws.focus).and_then(Pane::cwd)
    }

    /// Removes pane `id`, killing its process if it still runs. A workspace
    /// without panes disappears. When the active one does, another one is
    /// shown, and when none is left, imkr quits.
    /// Esc on a new window's name: it was not wanted after all. Back to the
    /// window it was opened from, then its shell goes.
    fn cancel_new_window(&mut self, pane: PaneId, back: u8) {
        if self.workspace_exists(back) {
            self.switch_workspace(back);
        }
        self.close(pane);
    }

    fn close(&mut self, id: PaneId) {
        if self.panes.remove(&id).is_none() {
            return;
        }
        // A popup whose program ended goes with it.
        if let Some(name) = self
            .popups
            .iter()
            .find_map(|(name, p)| (p.pane == id).then(|| name.clone()))
        {
            self.popups.remove(&name);
            if self.popup.as_deref() == Some(name.as_str()) {
                self.popup = self.popups.keys().next().cloned();
                self.popup_shown = false;
            }
            return;
        }
        self.unplace(id);
    }

    /// Takes pane `id` out of its window, which goes when it is empty; the
    /// pane itself stays. With no window left the session ends.
    fn unplace(&mut self, id: PaneId) {
        if !self.ws.contains(id) {
            let Some(n) = self
                .hidden
                .iter()
                .find_map(|(n, ws)| ws.contains(id).then_some(*n))
            else {
                return;
            };
            let ws = self.hidden.get_mut(&n).expect("found above");
            ws.remove(id);
            if ws.is_empty() {
                self.hidden.remove(&n);
            }
            return;
        }

        self.ws.remove(id);
        if self.ws.is_empty() {
            // Show the nearest remaining workspace, preferring a lower number.
            let current = self.workspace;
            let nearest = self
                .hidden
                .keys()
                .copied()
                .min_by_key(|n| (n.abs_diff(current), *n > current));
            match nearest {
                Some(n) => self.show(n),
                None => self.quit = true,
            }
        }
    }

    /// Moves the focused pane into window `to`, made if needed, or into the
    /// first free one, and shows it there. Its program runs on; the window
    /// it leaves goes if that was its last pane.
    fn pane_to_window(&mut self, to: Option<u8>) {
        if self.shown_popup().is_some() {
            self.flash = Some("a popup becomes a window with Ctrl+B !".into());
            return;
        }
        let id = self.ws.focus;
        if !self.panes.contains_key(&id) {
            return;
        }
        let Some(n) = to.or_else(|| self.free_workspace()) else {
            self.flash = Some("all 9 windows are in use".into());
            return;
        };
        if n == self.workspace {
            self.flash = Some(format!("the pane is in window {n} already"));
            return;
        }
        if !self.hidden.contains_key(&n) && self.ws.panes().len() == 1 {
            self.flash = Some("it is the only pane of its window".into());
            return;
        }
        // Into the target first, so the session never looks empty.
        let floating = self.ws.is_floating(id);
        let body = self.body;
        match self.hidden.get_mut(&n) {
            Some(ws) if floating => ws.add_float(id, body),
            Some(ws) => {
                if !ws.split(Axis::Row, id) {
                    ws.add_float(id, body);
                }
            }
            None => {
                self.hidden.insert(n, Workspace::new(id));
            }
        }
        self.unplace(id);
        self.switch_workspace(n);
        self.ws.focus(id);
    }

    fn workspace_exists(&self, n: u8) -> bool {
        n == self.workspace || self.hidden.contains_key(&n)
    }

    /// Numbers of all workspaces that have panes, in order.
    fn workspaces(&self) -> Vec<u8> {
        let mut all: Vec<u8> = self.hidden.keys().copied().collect();
        all.push(self.workspace);
        all.sort_unstable();
        all
    }

    /// Makes workspace `n` the active one. An empty workspace gets a new
    /// shell, like an empty workspace in i3.
    fn switch_workspace(&mut self, n: u8) {
        if n == self.workspace {
            return;
        }
        if self.hidden.contains_key(&n) {
            self.show(n);
            return;
        }

        let cwd = self.focused_cwd();
        let id = self.next_id;
        let inner = pane_inner(self.body);
        let spawned = Pane::spawn(
            Spawn {
                id,
                rows: inner.height,
                cols: inner.width,
                cwd: cwd.as_deref(),
                command: &[],
                socket: &self.socket,
                accounts: &self.accounts,
            },
            self.events.clone(),
        );
        let pane = match spawned {
            Ok(pane) => pane,
            Err(e) => {
                eprintln!("failed to start a shell for window {n}: {e:#}");
                return;
            }
        };
        self.next_id += 1;
        self.panes.insert(id, pane);
        self.stash();
        self.workspace = n;
        self.ws = Workspace::new(id);
    }

    /// Goes to the next or previous workspace that exists, wrapping around.
    fn step_workspace(&mut self, forward: bool) {
        let all = self.workspaces();
        let Some(i) = all.iter().position(|&n| n == self.workspace) else {
            return;
        };
        let len = all.len();
        let next = if forward {
            (i + 1) % len
        } else {
            (i + len - 1) % len
        };
        self.switch_workspace(all[next]);
    }

    /// Brings hidden workspace `n` to the front, hiding the active one unless
    /// it is empty.
    fn show(&mut self, n: u8) {
        let Some(ws) = self.hidden.remove(&n) else {
            return;
        };
        self.stash();
        self.workspace = n;
        self.ws = ws;
    }

    /// The popup shown now, if any.
    fn shown_popup(&self) -> Option<PaneId> {
        let name = self.popup.as_ref().filter(|_| self.popup_shown)?;
        self.popups.get(name).map(|p| p.pane)
    }

    /// The pane that keys and pastes go to: the shown popup, else the
    /// focused pane.
    fn input_pane(&self) -> PaneId {
        self.shown_popup().unwrap_or(self.ws.focus)
    }

    /// Where a popup is drawn: most of the panes' area, centred, or with
    /// `popup_full` the panes' area and the sidebar's.
    fn popup_rect(&self) -> Rect {
        let area = self.body;
        if self.popup_full {
            return match self.sidebar {
                Some(sidebar) => area.union(sidebar),
                None => area,
            };
        }
        let width = (area.width * 17 / 20).max(20).min(area.width);
        let height = (area.height * 17 / 20).max(6).min(area.height);
        Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        )
    }

    /// Shows popup `name`, or hides it if it is shown. The first time it
    /// starts `command` in the focused pane's directory, or in `cwd`.
    fn toggle_popup(
        &mut self,
        name: &str,
        title: Option<String>,
        command: &[String],
        cwd: Option<&std::path::Path>,
    ) -> Result<()> {
        if self.popups.contains_key(name) {
            let same = self.popup.as_deref() == Some(name);
            self.popup_shown = !(same && self.popup_shown);
            self.popup = Some(name.to_owned());
            return Ok(());
        }
        let inner = pane_inner(self.popup_rect());
        let id = self.next_id;
        let here = self.focused_cwd();
        let pane = Pane::spawn(
            Spawn {
                id,
                rows: inner.height,
                cols: inner.width,
                cwd: cwd.or(here.as_deref()),
                command,
                socket: &self.socket,
                accounts: &self.accounts,
            },
            self.events.clone(),
        )?;
        self.next_id += 1;
        self.panes.insert(id, pane);
        let title = title.unwrap_or_else(|| name.to_owned());
        self.popups
            .insert(name.to_owned(), Popup { pane: id, title });
        self.popup = Some(name.to_owned());
        self.popup_shown = true;
        Ok(())
    }

    /// Turns the shown popup into the only pane of a new window, named
    /// after it and shown. Its program runs on.
    fn break_popup(&mut self) {
        let Some(id) = self.shown_popup() else {
            self.flash = Some("no popup is shown".into());
            return;
        };
        let Some(n) = self.free_workspace() else {
            self.flash = Some("all 9 windows are in use".into());
            return;
        };
        let Some(name) = self.popup.take() else {
            return;
        };
        let title = self.popups.remove(&name).map(|p| p.title);
        self.popup = self.popups.keys().next().cloned();
        self.popup_shown = false;
        let mut ws = Workspace::new(id);
        ws.name = title;
        self.hidden.insert(n, ws);
        self.switch_workspace(n);
    }

    /// Shows the popup at index `i` of the sidebar, or hides it if shown.
    fn toggle_popup_at(&mut self, i: usize) {
        let Some((name, plugin)) = self.popup_entries().into_iter().nth(i) else {
            return;
        };
        // A plugin not running yet starts.
        if let Some(plugin) = plugin
            && !self.popups.contains_key(&name)
        {
            self.run_plugin(&plugin);
            if matches!(self.mode, Mode::Sidebar(_)) {
                self.mode = Mode::Normal;
            }
            return;
        }
        let shown = self.popup_shown && self.popup.as_deref() == Some(name.as_str());
        self.popup = Some(name);
        self.popup_shown = !shown;
        // The keys go to the popup, not on in the sidebar.
        if self.popup_shown && matches!(self.mode, Mode::Sidebar(_)) {
            self.mode = Mode::Normal;
        }
    }

    /// Reads the plugin files again.
    fn load_plugins(&mut self) {
        let (plugins, broken) = crate::plugin::load();
        self.own_plugins = plugins;
        self.broken_plugins = broken;
        self.merge_plugins();
    }

    /// The plugins shown: this machine's, then those of the user's own
    /// machine that it has not of the same name.
    fn merge_plugins(&mut self) {
        let mut all = self.own_plugins.clone();
        if let Some(origin) = &self.origin {
            for plugin in &origin.plugins {
                if !all.iter().any(|p| p.name == plugin.name) {
                    all.push(crate::plugin::Plugin {
                        from: Some(origin.host.clone()),
                        ..plugin.clone()
                    });
                }
            }
        }
        let mut shared = self.plugins.lock().unwrap();
        if *shared != all {
            *shared = all;
        }
    }

    /// Opens plugin `plugin` as a popup, starting it the first time, or
    /// hides it if it is shown.
    fn run_plugin(&mut self, plugin: &crate::plugin::Plugin) {
        // Brought along from the user's machine: it runs here, if it can.
        if !self.popups.contains_key(&plugin.name) && !plugin.installed() {
            let program = plugin.command.split_whitespace().next().unwrap_or("");
            self.flash = Some(format!(
                "{program} is not installed on {}, plugin {} cannot start here",
                self.own_name(),
                plugin.title()
            ));
            return;
        }
        let command = vec!["sh".to_owned(), "-c".to_owned(), plugin.command.clone()];
        let title = Some(plugin.title().to_owned());
        match self.toggle_popup(&plugin.name, title, &command, None) {
            Ok(()) if self.popup_shown => {
                self.popup_full = plugin.mode == crate::plugin::Mode::Fullscreen;
            }
            Ok(()) => {}
            Err(e) => self.flash = Some(format!("plugin {}: {e:#}", plugin.name)),
        }
    }

    /// Runs an action of `plugin`: in a popup of its own, or in the
    /// background with what it did said in the control bar.
    fn run_plugin_action(
        &mut self,
        plugin: &crate::plugin::Plugin,
        action: &crate::plugin::Action,
    ) {
        let title = format!("{} › {}", plugin.title(), action.title);
        if action.popup {
            let name = format!("{}-{}", plugin.name, action.key);
            let command = vec!["sh".to_owned(), "-c".to_owned(), action.run.clone()];
            if let Err(e) = self.toggle_popup(&name, Some(title), &command, None) {
                self.flash = Some(format!("{}: {e:#}", plugin.name));
            }
            return;
        }
        let spawned = std::process::Command::new("sh")
            .arg("-c")
            .arg(&action.run)
            .env("IMKR_PLUGIN", &plugin.name)
            .env("IMKR_SOCKET", &self.socket)
            .current_dir(self.focused_cwd().unwrap_or_else(|| PathBuf::from("/")))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let events = self.socket.clone();
        match spawned {
            Ok(child) => {
                self.flash = Some(format!("{title} …"));
                // Says when it is done, or why it failed.
                thread::spawn(move || {
                    let Ok(output) = child.wait_with_output() else {
                        return;
                    };
                    let text = if output.status.success() {
                        format!("{title} done")
                    } else {
                        let why = String::from_utf8_lossy(&output.stderr);
                        format!(
                            "{title} failed: {}",
                            why.lines().last().unwrap_or("").trim()
                        )
                    };
                    let _ = crate::client::request_at(&events, Request::Flash { text });
                });
            }
            Err(e) => self.flash = Some(format!("{title}: {e}")),
        }
    }

    /// What the sidebar lists under plugins: every plugin, running or not,
    /// then the popups opened by hand. Names, with the plugin if it is one.
    fn popup_entries(&self) -> Vec<(String, Option<crate::plugin::Plugin>)> {
        let plugins = self.plugins.lock().unwrap().clone();
        let mut entries: Vec<(String, Option<crate::plugin::Plugin>)> = plugins
            .iter()
            .map(|p| (p.name.clone(), Some(p.clone())))
            .collect();
        for name in self.popups.keys() {
            if !plugins.iter().any(|p| p.name == *name) {
                entries.push((name.clone(), None));
            }
        }
        entries
    }

    /// The sidebar's rows for `popup_entries`, broken plugin files last.
    fn popup_rows(&self) -> Vec<sidebar::PopupRow> {
        let statuses = self.plugin_status.lock().unwrap().clone();
        let mut rows: Vec<sidebar::PopupRow> = self
            .popup_entries()
            .into_iter()
            .map(|(name, plugin)| sidebar::PopupRow {
                title: match (&plugin, self.popups.get(&name)) {
                    (Some(plugin), _) => plugin.title().to_owned(),
                    (None, Some(popup)) => popup.title.clone(),
                    (None, None) => name.clone(),
                },
                shown: self.popup_shown && self.popup.as_ref() == Some(&name),
                running: self.popups.contains_key(&name),
                plugin: plugin.is_some(),
                key: plugin.as_ref().and_then(|p| {
                    let key = format!("^B p {}", p.key?);
                    // Brought along from the user's own machine.
                    Some(match p.from {
                        Some(_) => format!("⌂ {key}"),
                        None => key,
                    })
                }),
                status: statuses.get(&name).cloned().unwrap_or_default(),
            })
            .collect();
        for broken in &self.broken_plugins {
            rows.push(sidebar::PopupRow {
                title: format!("{} (broken)", broken.name),
                shown: false,
                running: false,
                plugin: true,
                key: None,
                status: vec![broken.problem.clone()],
            });
        }
        rows
    }

    /// Turns the focused pane into a popup, shown, named after its window if
    /// it is alone there, else after itself. Its program runs on.
    fn pane_to_popup(&mut self) {
        let id = self.ws.focus;
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let popups: HashSet<PaneId> = self.popups.values().map(|p| p.pane).collect();
        if self.panes.keys().filter(|id| !popups.contains(id)).count() < 2 {
            self.flash = Some("the last pane cannot become a popup".into());
            return;
        }
        let title = match &self.ws.name {
            Some(name) if self.ws.panes().len() == 1 => name.clone(),
            _ => pane
                .name
                .clone()
                .or_else(|| pane.program())
                .unwrap_or_else(|| "popup".into()),
        };
        let mut name = title.to_lowercase();
        let mut n = 2;
        while self.popups.contains_key(&name) {
            name = format!("{}-{n}", title.to_lowercase());
            n += 1;
        }
        self.unplace(id);
        self.popups.insert(name.clone(), Popup { pane: id, title });
        self.popup = Some(name);
        self.popup_shown = true;
        // Back as a popup should look like one, not fill the screen like
        // the window it was.
        self.popup_full = false;
    }

    /// Draws the shown popup over the panes, with the cursor in it.
    fn draw_popup(&self, frame: &mut Frame) {
        let Some(id) = self.shown_popup() else {
            return;
        };
        let (Some(pane), Some(name)) = (self.panes.get(&id), &self.popup) else {
            return;
        };
        let rect = self.popup_rect();
        let inner = pane_inner(rect);
        let t = theme::current();
        let title = self
            .popups
            .get(name)
            .map_or(name.as_str(), |p| p.title.as_str());
        frame.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.accent))
            // Full size it could pass for a window: it says what it is.
            .title(Line::from(vec![
                Span::styled(
                    format!(" ◆ {title} "),
                    Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if self.popup_full {
                        "popup · full "
                    } else {
                        "popup "
                    },
                    Style::new().fg(t.muted),
                ),
            ]))
            .title_bottom(
                Line::styled(
                    if self.popup_full {
                        " ^B Space hide · z/! smaller "
                    } else {
                        " ^B Space hide · z full · ! window "
                    },
                    Style::new().fg(t.muted),
                )
                .right_aligned(),
            );
        frame.render_widget(block, rect);
        let parser = pane.screen();
        let view = ScreenView::new(parser.screen());
        let cursor = view.cursor(inner);
        frame.render_widget(view, inner);
        if matches!(self.mode, Mode::Normal | Mode::Repeat(..))
            && let Some(position) = cursor
        {
            frame.set_cursor_position(position);
        }
    }

    /// Numbers the windows 1, 2, 3 … in their order, closing gaps. Whatever
    /// refers to a window by number follows. False if nothing changed.
    fn renumber_windows(&mut self) -> bool {
        let old = self.workspaces();
        let map: HashMap<u8, u8> = old.iter().zip(1..).map(|(&o, n)| (o, n)).collect();
        if map.iter().all(|(o, n)| o == n) {
            return false;
        }
        let new = |n: u8| map.get(&n).copied().unwrap_or(n);
        self.hidden = std::mem::take(&mut self.hidden)
            .into_iter()
            .map(|(n, ws)| (new(n), ws))
            .collect();
        self.workspace = new(self.workspace);
        for client in &mut self.clients {
            client.workspace = new(client.workspace);
        }
        if let Mode::MoveTo(window, _) = &mut self.mode {
            *window = new(*window);
        }
        if let Some(prompt) = &mut self.prompt {
            match &mut prompt.purpose {
                PromptFor::MoveWindow(window)
                | PromptFor::Rename(RenameTarget::Workspace(window)) => *window = new(*window),
                PromptFor::NewWindow { window, back, .. } => {
                    *window = new(*window);
                    *back = new(*back);
                }
                _ => {}
            }
        }
        true
    }

    /// Moves the active workspace into `hidden`, if it has any panes.
    fn stash(&mut self) {
        let ws = std::mem::replace(&mut self.ws, Workspace::empty());
        if !ws.is_empty() {
            self.hidden.insert(self.workspace, ws);
        }
    }

    fn paste(&mut self, text: &str) -> Result<()> {
        // Pasted into imkr's own lines, not the pane behind them.
        if self.mode == Mode::Notes
            && let Some(pad) = &mut self.notepad
            && let Some(pane) = self.panes.get_mut(&pad.pane)
        {
            // Terminals send line breaks of a paste as \r as well as \n.
            let lines: Vec<&str> = text
                .split(['\n', '\r'])
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .collect();
            match lines.as_slice() {
                [] => {}
                [one] => pad.input.push_str(one),
                // Several lines: each one a note, the typed line first.
                many => {
                    let first = std::mem::take(&mut pad.input);
                    for (i, line) in many.iter().enumerate() {
                        let text = if i == 0 {
                            format!("{first}{line}")
                        } else {
                            (*line).to_owned()
                        };
                        if !text.trim().is_empty() {
                            pane.notes.push(crate::persist::Note {
                                text: text.trim().to_owned(),
                                sent: false,
                            });
                        }
                    }
                }
            }
            pad.selected = None;
            return Ok(());
        }
        if self.mode == Mode::Prompt
            && let Some(prompt) = &mut self.prompt
        {
            let joined: Vec<&str> = text
                .split(['\n', '\r'])
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            prompt.text.push_str(&joined.join(" "));
            return Ok(());
        }
        let target = self.input_pane();
        let Some(pane) = self.panes.get_mut(&target) else {
            return Ok(());
        };
        pane.note_input();
        let bracketed = pane.screen().screen().bracketed_paste();
        if bracketed {
            pane.write(b"\x1b[200~")?;
            pane.write(text.as_bytes())?;
            pane.write(b"\x1b[201~")
        } else {
            pane.write(text.replace('\n', "\r").as_bytes())
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let (screen, body, sidebar_area) = self.areas(frame.area());

        let mut rects = self.rects_of(&self.ws);
        let shared = self.config.shared_borders && self.ws.zoomed.is_none();
        if shared {
            // The focused tiled pane last, so its border colour wins where
            // it is shared; floating panes stay on top.
            rects.sort_by_key(|(id, _)| {
                (
                    self.ws.is_floating(*id),
                    *id == self.ws.focus && !self.ws.is_floating(*id),
                )
            });
        }
        // What the border joining leaves alone: floating panes, and what
        // programs draw inside panes, which may be boxes too.
        let keep_out: Vec<Rect> = rects
            .iter()
            .map(|(id, r)| {
                if self.ws.is_floating(*id) {
                    *r
                } else {
                    pane_inner(*r)
                }
            })
            .collect();
        for (id, rect) in rects {
            let Some(pane) = self.panes.get(&id) else {
                continue;
            };
            let focused = id == self.ws.focus;
            let floating = self.ws.is_floating(id);
            let state = self.shown_state(id);
            let t = theme::current();
            // Floating panes get their own colour, so they stand apart from
            // the tiling even without focus.
            let border = match (focused, state) {
                (_, Some(AgentState::Blocked)) => Style::new().fg(t.danger),
                (true, _) => Style::new().fg(t.accent),
                (false, _) if floating => Style::new().fg(t.cyan),
                (false, _) => Style::new().fg(t.subtle),
            };
            let label = pane
                .name
                .clone()
                .or_else(|| pane.program())
                .unwrap_or_else(|| self.title.clone());
            let label_style = if focused {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let mut title = vec![
                Span::styled(format!(" {id} "), Style::new().fg(t.muted)),
                Span::styled(format!("{label} "), label_style),
            ];
            if let Some(state) = state {
                title.push(Span::styled(
                    format!("{} {} ", state.symbol(), state.name()),
                    state_style(state),
                ));
                // Which subscription and model it runs on: limits and
                // costs are theirs.
                let account = pane.shown_account.clone();
                let model = pane
                    .meta()
                    .map(|m| crate::agent::short_model(&m.model))
                    // A provider's account names its model already.
                    .filter(|m| !account.as_deref().is_some_and(|a| a.contains(m.as_str())));
                let about: Vec<String> = account.into_iter().chain(model).collect();
                if !about.is_empty() {
                    title.push(Span::styled(
                        format!("· {} ", about.join(" · ")),
                        Style::new().fg(t.muted),
                    ));
                }
            }
            // Thoughts waiting on its notepad (Ctrl+B t).
            let open_notes = pane.notes.iter().filter(|n| !n.sent).count();
            if open_notes > 0 {
                title.push(Span::styled(
                    format!("✎{open_notes} "),
                    Style::new().fg(t.warning),
                ));
            }
            if floating {
                title.push(Span::styled("⧉ ", Style::new().fg(t.cyan)));
            }
            if self.ws.zoomed == Some(id) {
                title.push(Span::styled(
                    "⤢ zoom ",
                    Style::new()
                        .fg(theme::current().accent)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let offset = pane.scroll_offset();
            if offset > 0 {
                title.push(Span::styled(
                    format!("⇡{offset}/{} ", pane.history_len()),
                    Style::new().fg(t.magenta),
                ));
            }
            let inner = self.inner(rect);
            if floating {
                frame.render_widget(Clear, rect);
            }
            if !self.focus_mode {
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border)
                    .title(Line::from(title));
                frame.render_widget(block, rect);
                draw_buttons(frame, &pane_buttons(rect, floating));
            }

            let selection = self.selection.filter(|s| s.pane == id).map(|s| {
                (
                    (self.to_view(id, s.anchor.0), s.anchor.1),
                    (self.to_view(id, s.head.0), s.head.1),
                )
            });
            let copy_cursor = self.copy.filter(|c| c.pane == id).and_then(|c| {
                let row = u16::try_from(self.to_view(id, c.pos.0)).ok()?;
                Some(Position::new(inner.x + c.pos.1, inner.y + row))
            });
            let parser = pane.screen();
            let view = ScreenView::new(parser.screen()).selection(selection);
            let cursor = copy_cursor.or_else(|| view.cursor(inner));
            frame.render_widget(view, inner);
            if focused
                && !matches!(
                    self.mode,
                    Mode::Confirm(_)
                        | Mode::Help(_)
                        | Mode::Menu(_)
                        | Mode::Settings(_)
                        | Mode::Prompt
                        | Mode::Sidebar(_)
                        | Mode::Picker
                        | Mode::Sessions(_)
                        | Mode::Hives(_)
                        | Mode::MoveTo(..)
                        | Mode::Agents(_)
                        | Mode::Inbox(_)
                        | Mode::Review
                        | Mode::Notes
                        | Mode::Layouts(_)
                        | Mode::Context(_)
                        | Mode::Recap(..)
                        | Mode::Windows(_)
                        | Mode::Panes(_)
                        | Mode::NewSessionWhere(_)
                        | Mode::NewSessionTemplate(_)
                        | Mode::NewWindowWhere(_)
                        | Mode::Marks(_)
                )
                && (self.mode != Mode::Copy || copy_cursor.is_some())
                && let Some(position) = cursor
            {
                frame.set_cursor_position(position);
            }
        }
        if shared {
            crate::render::join_borders(frame.buffer_mut(), body, &keep_out);
        }

        if let Some(area) = sidebar_area {
            let selected = match self.mode {
                Mode::Sidebar(i) => Some(i),
                _ => None,
            };
            sidebar::draw(
                frame,
                area,
                &self.sidebar_data(),
                selected,
                self.sidebar_scroll,
            );
            draw_buttons(frame, &sidebar_buttons(area));
        }
        if let Some(area) = screen.top {
            self.draw_bar(frame, area, Placement::Top);
        }
        if let Some(area) = screen.bottom {
            self.draw_bar(frame, area, Placement::Bottom);
        }
        self.draw_popup(frame);
        if self.focus_mode {
            self.draw_focus_overlay(frame, frame.area());
        }

        match self.mode {
            Mode::Prefix(menu) if self.config.which_key.enabled => {
                let commands = self.which_key_commands(menu);
                let side = self.config.which_key.position;
                menu::draw_which_key(frame, body, side, menu, &commands, None);
            }
            Mode::Copy if self.config.which_key.copy_mode => {
                let cursor = self.copy.and_then(|c| {
                    let inner = self.inner_of(c.pane)?;
                    let row = u16::try_from(self.to_view(c.pane, c.pos.0)).ok()?;
                    Some(inner.y + row)
                });
                menu::draw_copy_keys(frame, body, self.config.which_key.position, cursor);
            }
            Mode::Pick(menu, at) => {
                let commands = self.which_key_commands(menu);
                let side = self.config.which_key.position;
                menu::draw_which_key(frame, body, side, menu, &commands, Some(at));
            }
            Mode::Settings(selected) => menu::draw_settings(
                frame,
                body,
                &self.config,
                selected,
                self.config_note.as_deref(),
            ),
            Mode::Help(scroll) => menu::draw_help(frame, body, scroll),
            Mode::Menu(selected) => menu::draw_quit_menu(frame, body, &self.menu_items(), selected),
            Mode::Confirm(action) => self.draw_confirm(frame, body, action),
            Mode::Prompt => {
                if self
                    .prompt
                    .as_ref()
                    .is_some_and(|p| p.purpose == PromptFor::ReviewNote)
                {
                    self.draw_review(frame, body);
                }
                self.draw_prompt(frame, body)
            }
            Mode::Picker => self.draw_picker(frame, body),
            Mode::Sessions(selected) => {
                let (_, items) = self.session_items();
                let title = if self.show_archive {
                    " ⬢ Archive "
                } else {
                    " ⬢ Sessions "
                };
                self.draw_list(
                    frame,
                    body,
                    title,
                    &items,
                    selected,
                    bindings::SESSION_LIST_KEYS,
                );
            }
            Mode::NewWindowWhere(selected) => {
                let (_, mut items) = self.session_items();
                items.retain(|item| !item.apart);
                self.draw_list(
                    frame,
                    body,
                    " ⬢ New window in ",
                    &items,
                    selected,
                    bindings::CHOOSE_LIST_KEYS,
                );
            }
            Mode::NewSessionWhere(selected) => {
                let (_, items) = self.machine_choices();
                self.draw_list(
                    frame,
                    body,
                    if self.hosting {
                        " ⬢ New window running on "
                    } else {
                        " ⬢ New session on "
                    },
                    &items,
                    selected,
                    bindings::CHOOSE_LIST_KEYS,
                );
            }
            Mode::NewSessionTemplate(selected) => {
                let (_, items) = self.template_choices();
                let name = self.new_session.as_ref().map_or("", |p| p.name.as_str());
                self.draw_list(
                    frame,
                    body,
                    &format!(" ⬢ {name} starts with "),
                    &items,
                    selected,
                    bindings::CHOOSE_LIST_KEYS,
                );
            }
            Mode::Marks(selected) => {
                let mut items = self.mark_items();
                if items.is_empty() {
                    items.push(menu::MenuItem {
                        key: ' ',
                        title: "no marks yet".into(),
                        hint: "Ctrl+B m marks the focused pane".into(),
                        danger: false,
                        apart: false,
                        color: None,
                        group: None,
                        marks: Vec::new(),
                    });
                }
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Marks · Alt+1 … 9 ",
                    &items,
                    selected,
                    bindings::MARK_LIST_KEYS,
                );
            }
            Mode::Windows(selected) => {
                let (_, items) = self.window_items();
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Windows everywhere ",
                    &items,
                    selected,
                    bindings::CHOOSE_LIST_KEYS,
                );
            }
            Mode::Panes(selected) => {
                let (_, items) = self.pane_items();
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Panes everywhere ",
                    &items,
                    selected,
                    bindings::CHOOSE_LIST_KEYS,
                );
            }
            Mode::Agents(selected) => {
                let (_, mut items) = self.agent_choices();
                if items.is_empty() {
                    items.push(menu::MenuItem {
                        key: ' ',
                        title: "no agents running".into(),
                        hint: "here or on any machine known".into(),
                        danger: false,
                        apart: false,
                        color: None,
                        group: None,
                        marks: Vec::new(),
                    });
                }
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Agents ",
                    &items,
                    selected,
                    bindings::AGENT_LIST_KEYS,
                );
            }
            Mode::Inbox(selected) => self.draw_inbox(frame, body, selected),
            Mode::Review => self.draw_review(frame, body),
            Mode::Notes => self.draw_notes(frame, body),
            Mode::Context(selected) => self.draw_context(frame, selected),
            Mode::Layouts(selected) => {
                let (_, items) = self.layout_items();
                self.draw_list(
                    frame,
                    body,
                    &format!(" ⬢ Layout for window {} ", self.workspace),
                    &items,
                    selected.min(items.len() - 1),
                    bindings::LAYOUT_LIST_KEYS,
                );
            }
            Mode::Recap(since, selected) => {
                let (_, items) = self.recap_items(since);
                let when = protocol::chrono_like(since).unwrap_or_default();
                if !items.is_empty() {
                    self.draw_list(
                        frame,
                        body,
                        &format!(" ⬢ While you were away, since {when} "),
                        &items,
                        selected.min(items.len() - 1),
                        bindings::CHOOSE_LIST_KEYS,
                    );
                }
            }
            Mode::MoveTo(window, selected) => {
                let (_, items) = self.move_to_items();
                self.draw_list(
                    frame,
                    body,
                    &format!(" ⬢ Move window {window} to "),
                    &items,
                    selected,
                    bindings::MOVE_LIST_KEYS,
                );
            }
            Mode::Hives(selected) => {
                let (names, mut items) = self.hive_items();
                if names.is_empty() {
                    items.push(menu::MenuItem {
                        key: ' ',
                        title: "no hives yet".into(),
                        hint: "s saves the running sessions as one".into(),
                        danger: false,
                        apart: false,
                        color: None,
                        group: None,
                        marks: Vec::new(),
                    });
                }
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Hives ",
                    &items,
                    selected.min(items.len() - 1),
                    bindings::HIVE_LIST_KEYS,
                );
            }
            Mode::Jump => self.draw_jump(frame),
            Mode::Accounts(selected) => {
                let (_, items) = self.account_items();
                self.draw_list(
                    frame,
                    body,
                    " ⬢ Accounts ",
                    &items,
                    selected.min(items.len().saturating_sub(1)),
                    bindings::ACCOUNT_LIST_KEYS,
                );
            }
            Mode::AgentAccount(selected) => {
                let title = match &self.agent_start {
                    Some(start) if !start.panes.is_empty() => {
                        format!(" ⬢ {} of the template: which account? ", start.tools[0])
                    }
                    _ => " ⬢ New agent: which account? ".to_owned(),
                };
                let (_, items) = self.agent_start_items();
                self.draw_list(
                    frame,
                    body,
                    &title,
                    &items,
                    selected.min(items.len().saturating_sub(1)),
                    bindings::AGENT_START_KEYS,
                );
            }
            Mode::Prefix(_) | Mode::Normal | Mode::Repeat(..) | Mode::Copy | Mode::Sidebar(_) => {}
        }
        self.draw_banner(frame);
    }

    /// The session's name in the middle of the screen for a moment after a
    /// client switched here (`Caps::banner`), over everything else.
    fn draw_banner(&self, frame: &mut Frame) {
        let Some((text, at, since)) = &self.banner else {
            return;
        };
        if since.elapsed() >= BANNER_FOR {
            return;
        }
        let t = theme::current();
        let area = frame.area();
        let label = format!("⬢ {text}");
        let width = (label.chars().count() as u16 + 8).min(area.width);
        let height = 3.min(area.height);
        // The middle of the screen, or of its top or bottom third.
        let third = area.height / 3;
        let (top, span) = match at {
            crate::config::BannerAt::Middle => (area.y, area.height),
            crate::config::BannerAt::Top => (area.y, third),
            crate::config::BannerAt::Bottom => (area.bottom() - third, third),
        };
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            top + span.saturating_sub(height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(t.accent));
        frame.render_widget(
            Paragraph::new(Line::styled(
                label,
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            ))
            .alignment(ratatui::layout::Alignment::Center)
            .block(block),
            rect,
        );
    }

    /// Reads the config and the plugins again; the control bar says how
    /// that went.
    fn reload_config(&mut self) {
        let (config, error) = Config::load();
        self.config = config;
        self.apply_theme();
        self.load_plugins();
        self.flash = Some(match (error, self.config.hook_problems()) {
            (Some(e), _) => format!("config has an error, using defaults: {e}"),
            (None, Some(problem)) => problem,
            (None, None) => "config reloaded".into(),
        });
    }

    /// Every account of every agent, grouped by agent, with what uses it:
    /// ● new shells of this session, ▸ the focused agent.
    fn account_items(&self) -> (Vec<crate::account::Account>, Vec<menu::MenuItem>) {
        let Ok(store) = crate::account::Store::here() else {
            return (Vec::new(), Vec::new());
        };
        let focused = self.panes.get(&self.ws.focus);
        let mut accounts = Vec::new();
        let mut items = Vec::new();
        for tool in crate::account::TOOLS {
            let default = store.default_of(tool);
            for account in store.of_tool(tool) {
                let session = self
                    .accounts
                    .get(*tool)
                    .map_or(account.name == default, |name| *name == account.name);
                let agent = focused.is_some_and(|pane| {
                    pane.program().as_deref() == Some(*tool)
                        && pane
                            .accounts
                            .get(*tool)
                            .map_or(account.is_standard(), |name| *name == account.name)
                });
                let mut hint = match (&account.url, store.identity(&account)) {
                    (None, Some(who)) => who.shown,
                    _ => account.kind(),
                };
                if !store.logged_in(&account) {
                    hint.push_str(" · not logged in");
                }
                if let Some(until) = store.used_up_until(&account) {
                    hint.push_str(&format!(" · used up until {until}"));
                }
                let marks = format!(
                    "{}{}",
                    if agent { "▸" } else { " " },
                    if session { "●" } else { " " }
                );
                items.push(menu::MenuItem {
                    key: char::from_digit((items.len() as u32 + 1) % 10, 10).unwrap_or(' '),
                    title: format!(
                        "{marks} {}{}",
                        if account.url.is_some() {
                            account.name.clone()
                        } else {
                            crate::account::shown(&store, &account)
                        },
                        if account.name == default {
                            " (standard)"
                        } else {
                            ""
                        }
                    ),
                    hint,
                    danger: false,
                    apart: false,
                    color: None,
                    group: Some((*tool).to_owned()),
                    marks: Vec::new(),
                });
                accounts.push(account);
            }
        }
        (accounts, items)
    }

    /// The account list: Enter puts the focused agent into the chosen
    /// account if it is that agent, else (and with `u`) new shells.
    fn accounts_key(&mut self, key: KeyEvent, selected: usize) {
        let (accounts, _) = self.account_items();
        let count = accounts.len().max(1);
        let selected = selected.min(count - 1);
        let chosen = |i: usize| accounts.get(i).cloned();
        let pick = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Accounts((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::Accounts((selected + 1) % count);
                return;
            }
            KeyCode::Enter => chosen(selected).map(|a| (a, true)),
            KeyCode::Char('u') => chosen(selected).map(|a| (a, false)),
            KeyCode::Char(c) => c
                .to_digit(10)
                .and_then(|d| chosen((d as usize + 9) % 10))
                .map(|a| (a, true)),
            _ => None,
        };
        let Some((account, now)) = pick else {
            return;
        };
        self.mode = Mode::Normal;
        let focus = self.ws.focus;
        let runs_it = self
            .panes
            .get(&focus)
            .is_some_and(|p| p.program().as_deref() == Some(account.tool.as_str()));
        let pane = (now && runs_it).then_some(focus);
        let request = Request::UseAccount {
            tool: account.tool.clone(),
            name: account.name.clone(),
            pane,
        };
        self.flash = Some(match self.request(request) {
            Ok(_) if pane.is_some() => {
                format!("{} goes on in account {}", account.tool, account.name)
            }
            Ok(_) => format!(
                "new shells of this session use {} account {}",
                account.tool, account.name
            ),
            Err(e) => e,
        });
    }

    /// `Ctrl+B k`: a new window with an agent. With more than one account
    /// logged in, asks which one; with one, starts it right away.
    fn new_agent(&mut self) {
        let tools: Vec<String> = crate::account::TOOLS
            .iter()
            .filter(|tool| on_path(tool))
            .map(|tool| (*tool).to_owned())
            .collect();
        if tools.is_empty() {
            self.flash = Some("neither claude nor codex is installed here".into());
            return;
        }
        self.ask_agent_account(AgentStart {
            tools,
            panes: Vec::new(),
        });
    }

    /// Asks which account `start` begins in (`Mode::AgentAccount`), with
    /// the default of its first agent selected; with only one account
    /// logged in, starts in it without asking.
    fn ask_agent_account(&mut self, start: AgentStart) {
        self.agent_start = Some(start);
        let (accounts, _) = self.agent_start_items();
        let Ok(store) = crate::account::Store::here() else {
            self.agent_start = None;
            return;
        };
        let usable: Vec<usize> = (0..accounts.len())
            .filter(|&i| store.logged_in(&accounts[i]))
            .collect();
        if let [only] = usable.as_slice() {
            let account = accounts[*only].clone();
            self.start_agent(&account);
            return;
        }
        let tool = self.agent_start.as_ref().map(|s| s.tools[0].clone());
        let default = tool.as_deref().map(|tool| store.default_of(tool));
        let selected = accounts
            .iter()
            .position(|a| Some(&a.tool) == tool.as_ref() && Some(&a.name) == default.as_ref())
            .unwrap_or(0);
        self.mode = Mode::AgentAccount(selected);
    }

    /// The accounts a waiting agent can start in, by agent: logged in ones
    /// as they are, the others with why not.
    fn agent_start_items(&self) -> (Vec<crate::account::Account>, Vec<menu::MenuItem>) {
        let (Some(start), Ok(store)) = (&self.agent_start, crate::account::Store::here()) else {
            return (Vec::new(), Vec::new());
        };
        let mut accounts = Vec::new();
        let mut items = Vec::new();
        for tool in &start.tools {
            let default = store.default_of(tool);
            for account in store.of_tool(tool) {
                let mut hint = match (&account.url, store.identity(&account)) {
                    (None, Some(who)) => who.shown,
                    (None, None) if account.is_standard() => format!("~/.{tool}"),
                    _ => account.kind(),
                };
                if store.login_dead(&account) {
                    hint.push_str(" · logged out");
                } else if !store.logged_in(&account) {
                    hint.push_str(" · not logged in");
                }
                if let Some(until) = store.used_up_until(&account) {
                    hint.push_str(&format!(" · used up until {until}"));
                }
                let name = if account.url.is_some() {
                    account.name.clone()
                } else {
                    crate::account::shown(&store, &account)
                };
                items.push(menu::MenuItem {
                    key: char::from_digit((items.len() as u32 + 1) % 10, 10).unwrap_or(' '),
                    title: format!(
                        "{name}{}",
                        if account.name == default {
                            " (standard)"
                        } else {
                            ""
                        }
                    ),
                    hint,
                    danger: false,
                    apart: false,
                    color: None,
                    group: Some(tool.clone()),
                    marks: Vec::new(),
                });
                accounts.push(account);
            }
        }
        (accounts, items)
    }

    /// The account choice for a waiting agent: Enter or its number starts
    /// it there; Esc drops a new window's, and starts a template's as the
    /// template says, in the default account.
    fn agent_account_key(&mut self, key: KeyEvent, selected: usize) {
        let (accounts, _) = self.agent_start_items();
        let count = accounts.len().max(1);
        let selected = selected.min(count - 1);
        let chosen = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                if let Some(start) = self.agent_start.take() {
                    for (id, command) in start.panes {
                        let _ = self.type_into(id, &command, true);
                    }
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::AgentAccount((selected + count - 1) % count);
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.mode = Mode::AgentAccount((selected + 1) % count);
                return;
            }
            KeyCode::Enter => accounts.get(selected).cloned(),
            KeyCode::Char(c) => c
                .to_digit(10)
                .and_then(|d| accounts.get((d as usize + 9) % 10).cloned()),
            _ => None,
        };
        if let Some(account) = chosen {
            self.mode = Mode::Normal;
            self.start_agent(&account);
        }
    }

    /// Starts the waiting agent in `account`: in a new window named after
    /// it, or in the template's panes of that agent. Typed into the shell
    /// as `imkr account run`, like a switch, so the pane notes the account.
    fn start_agent(&mut self, account: &crate::account::Account) {
        let Some(start) = self.agent_start.take() else {
            return;
        };
        let tool = account.tool.clone();
        let mut panes = start.panes;
        if panes.is_empty() {
            let Some(n) = self.free_workspace() else {
                self.flash = Some("all 9 windows are in use".into());
                return;
            };
            self.switch_workspace(n);
            if self.workspace != n {
                return;
            }
            let _ = self.rename(RenameTarget::Workspace(n), Some(tool.clone()));
            panes.push((self.ws.focus, self.config.agent_command(&tool)));
        }
        for (id, command) in panes {
            if let Some(pane) = self.panes.get_mut(&id) {
                set_account(&mut pane.accounts, &tool, &account.name);
            }
            let line = format!(
                "{} account run {} -- {command}",
                crate::client::sh_quote(&imkr_path()),
                crate::client::sh_quote(&format!("{tool}:{}", account.name)),
            );
            let _ = self.type_into(id, &line, true);
        }
        self.flash = Some(format!(
            "{tool} starts in account {}",
            crate::account::Store::here()
                .map(|store| crate::account::shown(&store, account))
                .unwrap_or_else(|_| account.name.clone())
        ));
        // A template with another agent waiting: its account next.
        if let Some(rest) = self.waiting_agents.take() {
            self.ask_agent_account(rest);
        }
    }

    /// The list shown now, if one is: its items and the one selected.
    fn list_state(&self) -> Option<(Vec<menu::MenuItem>, usize)> {
        Some(match self.mode {
            Mode::Sessions(s) => (self.session_items().1, s),
            Mode::Agents(s) => (self.agent_choices().1, s),
            Mode::Windows(s) => (self.window_items().1, s),
            Mode::Panes(s) => (self.pane_items().1, s),
            Mode::Hives(s) => (self.hive_items().1, s),
            Mode::MoveTo(_, s) => (self.move_to_items().1, s),
            Mode::Marks(s) => (self.mark_items(), s),
            Mode::NewWindowWhere(s) => {
                let mut items = self.session_items().1;
                items.retain(|item| !item.apart);
                (items, s)
            }
            Mode::NewSessionWhere(s) => (self.machine_choices().1, s),
            Mode::NewSessionTemplate(s) => (self.template_choices().1, s),
            _ => return None,
        })
    }

    fn set_list_selected(&mut self, i: usize) {
        match &mut self.mode {
            Mode::Sessions(s)
            | Mode::Agents(s)
            | Mode::Windows(s)
            | Mode::Panes(s)
            | Mode::Hives(s)
            | Mode::Marks(s)
            | Mode::NewWindowWhere(s)
            | Mode::NewSessionWhere(s)
            | Mode::NewSessionTemplate(s)
            | Mode::MoveTo(_, s) => *s = i,
            _ => {}
        }
    }

    /// The search in every list: `/` starts it, typing narrows the list,
    /// the arrows move through what matches, Enter picks, Esc clears the
    /// search and then closes the list. False when the key is handled.
    fn list_key(&mut self, key: KeyEvent) -> bool {
        let Some((items, selected)) = self.list_state() else {
            self.list_query = None;
            return true;
        };
        let Some(mut query) = self.list_query.clone() else {
            if key.code == KeyCode::Char('/') {
                self.list_query = Some(String::new());
                return false;
            }
            return true;
        };
        let visible: Vec<usize> = (0..items.len())
            .filter(|&i| list_matches(&items[i], &query))
            .collect();
        let at = visible.iter().position(|&i| i == selected);
        match key.code {
            KeyCode::Esc if query.is_empty() => {
                self.list_query = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Esc => self.list_query = Some(String::new()),
            KeyCode::Up | KeyCode::Down | KeyCode::Tab if !visible.is_empty() => {
                let n = visible.len();
                let next = match (key.code, at) {
                    (KeyCode::Up, Some(at)) => (at + n - 1) % n,
                    (_, Some(at)) => (at + 1) % n,
                    (_, None) => 0,
                };
                self.set_list_selected(visible[next]);
            }
            KeyCode::Enter => {
                let Some(&pick) = at.map(|at| &visible[at]).or(visible.first()) else {
                    return false;
                };
                self.set_list_selected(pick);
                self.list_query = None;
                // The list itself takes it from here.
                return true;
            }
            KeyCode::Backspace => {
                query.pop();
                self.list_query = Some(query);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(c);
                if let Some(&first) = (0..items.len())
                    .filter(|&i| list_matches(&items[i], &query))
                    .collect::<Vec<_>>()
                    .first()
                {
                    self.set_list_selected(first);
                }
                self.list_query = Some(query);
            }
            _ => {}
        }
        false
    }

    /// Draws a list, narrowed to what matches the search if there is one,
    /// with `/` among its keys.
    /// The inbox: the list above, the selected agent's question below
    /// with the answer typed so far.
    fn draw_inbox(&self, frame: &mut Frame, body: Rect, selected: usize) {
        let (targets, mut items) = self.inbox_items();
        if items.is_empty() {
            items.push(menu::MenuItem {
                key: ' ',
                title: "nothing waits for you".into(),
                hint: "no agent asks or is done, here or elsewhere".into(),
                danger: false,
                apart: false,
                color: None,
                group: None,
                marks: Vec::new(),
            });
        }
        // Once the agent selected left the list, the one in its place, but
        // `inbox_key` sends nothing until it is chosen.
        let selected = self
            .inbox_selected(&targets, selected)
            .unwrap_or(selected)
            .min(items.len() - 1);
        let tail = targets
            .get(selected)
            .map(|(_, tail, _)| tail.as_str())
            .unwrap_or_default();
        let lines = tail.lines().count() as u16;
        let ask = if targets.is_empty() { 0 } else { lines + 3 };
        let ask = ask.min(body.height / 2);
        let list = Rect {
            height: body.height - ask,
            ..body
        };
        let body = Rect {
            height: body.height.saturating_sub(1),
            ..body
        };
        menu::draw_choices(
            frame,
            list,
            " ⬢ Inbox ",
            &items,
            selected,
            bindings::INBOX_KEYS,
        );
        if ask == 0 {
            return;
        }
        let t = theme::current();
        let width = body.width.saturating_sub(4).min(110);
        let area = Rect {
            x: body.x + (body.width - width) / 2,
            y: body.y + body.height - ask,
            width,
            height: ask,
        };
        let title = items[selected].title.clone();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if items[selected].danger {
                t.danger
            } else {
                t.accent
            }))
            .title(format!(" {title} "))
            .padding(Padding::horizontal(1));
        let mut text: Vec<Line> = tail
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect();
        let message = matches!(targets.get(selected), Some((InboxAt::Message(_), ..)));
        let answer = if message {
            Span::styled(
                "Enter accept: typed in like your own message · x decline",
                Style::new().fg(t.subtle),
            )
        } else if self.inbox_answer.is_empty() {
            Span::styled(
                if items[selected].danger {
                    "type the answer, or a digit for that option"
                } else {
                    "type the next prompt"
                },
                Style::new().fg(t.subtle),
            )
        } else {
            Span::raw(self.inbox_answer.clone())
        };
        text.push(Line::from(vec![
            Span::styled("› ", Style::new().fg(t.accent)),
            answer,
        ]));
        let skip = (text.len() as u16).saturating_sub(ask.saturating_sub(2));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(text).block(block).scroll((skip, 0)), area);
    }

    fn draw_list(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        items: &[menu::MenuItem],
        selected: usize,
        keys: &[bindings::KeyHint],
    ) {
        let mut keys = keys.to_vec();
        keys.insert(keys.len().saturating_sub(1), ("/", "search"));
        let Some(query) = &self.list_query else {
            let selected = selected.min(items.len().saturating_sub(1));
            menu::draw_choices(frame, area, title, items, selected, &keys);
            return;
        };
        let mut shown = Vec::new();
        let mut at = 0;
        for (i, item) in items.iter().enumerate() {
            if list_matches(item, query) {
                if i == selected {
                    at = shown.len();
                }
                shown.push(item.clone());
            }
        }
        if shown.is_empty() {
            shown.push(menu::MenuItem {
                key: ' ',
                title: "no match".into(),
                hint: "Esc clears the search".into(),
                danger: false,
                apart: false,
                color: None,
                group: None,
                marks: Vec::new(),
            });
        }
        let title = format!("{title}· /{query}▏ ");
        menu::draw_choices(frame, area, &title, &shown, at, &keys);
    }

    fn menu_items(&self) -> Vec<menu::MenuItem> {
        let shells = self.panes.len();
        MENU.iter()
            .map(|&(key, entry)| match entry {
                MenuEntry::Detach => menu::MenuItem {
                    key,
                    title: "Detach".into(),
                    hint: "shells keep running, `imkr` brings you back".into(),
                    danger: false,
                    apart: false,
                    color: None,
                    group: None,
                    marks: Vec::new(),
                },
                MenuEntry::ClosePane => menu::MenuItem {
                    key,
                    title: format!("Close pane {}", self.ws.focus),
                    hint: "ends the program running in it".into(),
                    danger: true,
                    apart: false,
                    color: None,
                    group: None,
                    marks: Vec::new(),
                },
                MenuEntry::EndSession => menu::MenuItem {
                    key,
                    title: "End session".into(),
                    hint: format!(
                        "closes all {shells} {}",
                        if shells == 1 { "shell" } else { "shells" }
                    ),
                    danger: true,
                    apart: false,
                    color: None,
                    group: None,
                    marks: Vec::new(),
                },
            })
            .collect()
    }

    /// The name prompt in the middle of the screen, with the terminal cursor
    /// at the end of the text.
    fn draw_prompt(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.prompt else { return };
        let title = match &prompt.purpose {
            PromptFor::Rename(RenameTarget::Pane(id)) => format!(" Name pane {id} "),
            PromptFor::Rename(RenameTarget::Workspace(n)) => format!(" Name window {n} "),
            PromptFor::NewWindow { window, .. } => format!(" Name new window {window} "),
            PromptFor::Search { forward: true } => " Search down ".to_owned(),
            PromptFor::Search { forward: false } => " Search up ".to_owned(),
            PromptFor::SessionDir(place, _) => match &place.host {
                Some(host) => format!(" Directory of {} on {host} ", place.name),
                None => format!(" Directory of {} ", place.name),
            },
            PromptFor::NewSession(None) => " New session ".to_owned(),
            PromptFor::NewSession(Some(host)) => format!(" New session on {host} "),
            PromptFor::MoveWindow(n) => format!(" Move window {n} to session "),
            PromptFor::Worktree => " New window in a git worktree, branch ".to_owned(),
            PromptFor::ReviewNote => " Note on this line ".to_owned(),
            PromptFor::SaveLayout => " Save this window's layout as ".to_owned(),
            PromptFor::SaveHive => " Save running sessions as hive ".to_owned(),
            PromptFor::RenameHive(old) => format!(" Rename hive {old} "),
            PromptFor::RenameSession(old) => format!(" Rename session {old} "),
        };
        let accent = theme::current().accent;
        let hint = Style::new().add_modifier(Modifier::DIM);
        let width = (MAX_NAME as u16 + 8).max(72);
        // A long directory shows its end, where the typing is.
        let room = usize::from(width.saturating_sub(9));
        let count = prompt.text.chars().count();
        let shown = if count > room {
            let tail: String = prompt.text.chars().skip(count + 1 - room).collect();
            format!("…{tail}")
        } else {
            prompt.text.clone()
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled("› ", Style::new().fg(accent).add_modifier(Modifier::BOLD)),
                Span::raw(shown.clone()),
            ]),
            Line::default(),
            Line::styled(
                match prompt.purpose {
                    PromptFor::Rename(_) => "Enter save · empty clears the name · Esc cancel",
                    PromptFor::NewWindow { .. } => "Enter save · Esc closes the window again",
                    PromptFor::Search { .. } => "Enter find · then n next, N previous · Esc cancel",
                    PromptFor::NewSession(_) => "Enter start and switch · letters, digits, - and _",
                    PromptFor::SessionDir(..) => {
                        "Enter start · Tab completes · ↑↓ choose · Ctrl+W up · Esc cancel"
                    }
                    PromptFor::MoveWindow(_) => "Enter move · a new name starts it · Esc cancel",
                    PromptFor::RenameSession(_) => "Enter rename · letters, digits, - and _",
                    PromptFor::SaveHive => "Enter save · letters, digits, - and _ · Esc cancel",
                    PromptFor::RenameHive(_) => "Enter rename · letters, digits, - and _",
                    PromptFor::Worktree => {
                        "Enter open · new branch from here, or the one of that name · Esc cancel"
                    }
                    PromptFor::ReviewNote => "Enter keep · empty removes it · Esc cancel",
                    PromptFor::SaveLayout => "Enter save · the same name replaces it · Esc cancel",
                },
                hint,
            ),
        ];
        if let PromptFor::SessionDir(place, _) = &prompt.purpose {
            // The directories it may complete to, between text and hint.
            let mut listed = Vec::new();
            match self.dir_choices() {
                None => listed.push(Line::styled(
                    format!("  listing on {}…", place.host.as_deref().unwrap_or("")),
                    hint,
                )),
                Some(choices) => {
                    for (i, name) in choices.iter().take(DIR_ROWS).enumerate() {
                        let style = if self.dir_pick == Some(i) {
                            Style::new().fg(theme::current().on_accent).bg(accent)
                        } else {
                            Style::new()
                        };
                        listed.push(Line::styled(format!("  {name}/"), style));
                    }
                    if choices.len() > DIR_ROWS {
                        listed.push(Line::styled(
                            format!("  … {} more", choices.len() - DIR_ROWS),
                            hint,
                        ));
                    }
                }
            }
            if !listed.is_empty() {
                listed.push(Line::default());
                lines.splice(2..2, listed);
            }
        }
        let popup = centered(area, width, lines.len() as u16 + 2);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(accent))
            .title(Span::styled(
                title,
                Style::new().fg(accent).add_modifier(Modifier::BOLD),
            ))
            .padding(Padding::horizontal(2));
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new(lines).block(block), popup);
        let x = inner.x + 2 + shown.chars().count() as u16;
        frame.set_cursor_position(Position::new(
            x.min(inner.right().saturating_sub(1)),
            inner.y,
        ));
    }

    /// A warning box in the middle of the screen, asking to confirm `action`.
    fn draw_confirm(&self, frame: &mut Frame, area: Rect, action: Action) {
        let shells = self.panes.len();
        let target = || self.confirm_target.clone().unwrap_or_default();
        let (title, lines) = match action {
            Action::ClosePane(id) => (
                " Close pane ",
                vec![Line::from(format!(
                    "Close pane {id} and end the program running in it?"
                ))],
            ),
            Action::KillServer => (
                " Quit imkr ",
                vec![
                    Line::from(format!(
                        "End the session and all {shells} {} in it?",
                        if shells == 1 { "shell" } else { "shells" }
                    )),
                    Line::from(Span::styled(
                        "To keep them running, detach with ^B q instead.",
                        Style::new().add_modifier(Modifier::DIM),
                    )),
                ],
            ),
            Action::EndSession => (
                " End session ",
                vec![
                    Line::from(format!(
                        "End session {} and every program running in it?",
                        target()
                    )),
                    Line::from(Span::styled(
                        "Its layout goes into the hive `last`, ^B H brings it back.",
                        Style::new().add_modifier(Modifier::DIM),
                    )),
                ],
            ),
            Action::ForgetSession => (
                " Forget session ",
                vec![
                    Line::from(format!(
                        "Forget session {}? It does not run, its layout is deleted.",
                        target()
                    )),
                    Line::from(Span::styled(
                        "Hives that hold it keep it.",
                        Style::new().add_modifier(Modifier::DIM),
                    )),
                ],
            ),
            Action::DeleteHive => (
                " Delete hive ",
                vec![Line::from(format!(
                    "Delete the hive {}? Running sessions keep running.",
                    target()
                ))],
            ),
            Action::SwitchHive => (" Switch hive ", {
                let mut lines = if self.confirm_list.is_empty() {
                    vec![Line::from(format!(
                        "Switch to the hive {}? Nothing else runs, nothing ends.",
                        target()
                    ))]
                } else {
                    let mut lines = vec![Line::from(format!(
                        "Switch to the hive {}? These sessions end, into the hive `last`:",
                        target()
                    ))];
                    for name in self.confirm_list.iter().take(8) {
                        lines.push(Line::from(format!("  {name}")));
                    }
                    if self.confirm_list.len() > 8 {
                        lines.push(Line::from(format!(
                            "  … {} more",
                            self.confirm_list.len() - 8
                        )));
                    }
                    lines
                };
                // The hive left behind is saved as it is now (#507).
                if let Some(active) = persist::active_hive()
                    .filter(|a| Some(a.as_str()) != self.confirm_target.as_deref())
                {
                    lines.insert(
                        0,
                        Line::from(format!("The hive {active} is saved first, as it is now.")),
                    );
                }
                lines
            }),
            Action::StartArchivedHere => {
                let (host, name) = target().split_once(':').map_or_else(
                    || (String::new(), target()),
                    |(h, n)| (h.to_owned(), n.to_owned()),
                );
                (
                    " Start here ",
                    vec![
                        Line::from(format!("{host} does not answer.")),
                        Line::from(format!(
                            "Start {name} on this machine, from its archived layout?"
                        )),
                        Line::from(format!(
                            "Directories {host} has and this one not become its start directory."
                        )),
                    ],
                )
            }
            Action::OverwriteHive => (
                " Update hive ",
                vec![Line::from(format!(
                    "Replace the hive {} with the sessions running now?",
                    target()
                ))],
            ),
        };

        let warning = Style::new()
            .fg(theme::current().danger)
            .add_modifier(Modifier::BOLD);
        let mut text = lines;
        text.push(Line::default());
        text.push(Line::from(vec![
            Span::styled(
                " y ",
                Style::new()
                    .fg(theme::current().on_accent)
                    .bg(theme::current().danger)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" yes    "),
            Span::styled("any other key", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(" cancel"),
        ]));

        let width = text.iter().map(Line::width).max().unwrap_or(0) as u16 + 6;
        let height = text.len() as u16 + 2;
        let popup = centered(area, width, height);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(warning)
            .title(Span::styled(format!(" ⚠{title}"), warning))
            .padding(Padding::horizontal(2));
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new(text).block(block), popup);
    }

    /// One bar line: control bar and workspace tabs on the left, the path
    /// on the right, whichever of them the settings put on `side`.
    /// What focus mode keeps of the bars: the control line while a menu or
    /// the prefix is up, and a corner note when an agent elsewhere waits.
    fn draw_focus_overlay(&self, frame: &mut Frame, screen: Rect) {
        if !matches!(self.mode, Mode::Normal) {
            let side = self.config.bars.control();
            let y = match side {
                Placement::Top => screen.y,
                _ => screen.bottom().saturating_sub(1),
            };
            let line = Rect {
                y,
                height: 1.min(screen.height),
                ..screen
            };
            frame.render_widget(Clear, line);
            self.draw_bar(frame, line, side);
        }
        let waiting = self
            .panes
            .keys()
            .filter(|&&id| id != self.ws.focus)
            .filter(|&&id| self.shown_state(id) == Some(AgentState::Blocked))
            .count();
        if waiting > 0 {
            let text = format!(" {} {waiting} waiting ", AgentState::Blocked.symbol());
            let width = (text.chars().count() as u16).min(screen.width);
            let area = Rect {
                x: screen.right() - width,
                y: screen.y,
                width,
                height: 1.min(screen.height),
            };
            let t = theme::current();
            frame.render_widget(
                Paragraph::new(text).style(Style::new().fg(t.on_accent).bg(t.danger)),
                area,
            );
        }
    }

    fn draw_bar(&self, frame: &mut Frame, area: Rect, side: Placement) {
        let bars = &self.config.bars;
        let t = theme::current();
        let hint = Style::new().fg(t.muted);
        // The bar gets a surface of its own, set apart from the panes.
        frame.render_widget(Block::new().style(Style::new().bg(t.surface)), area);
        let control = bars.control() == side;

        let mut spans = Vec::new();
        if control {
            spans.push(Span::styled(badge_text(), badge_style()));
            spans.push(Span::raw(" "));
        }
        if bars.tabs == side {
            spans.extend(self.tab_spans());
            if self.free_workspace().is_some() {
                spans.push(Span::styled(NEW_TAB, Style::new().fg(t.success)));
            }
            spans.push(Span::styled(if control { "│ " } else { " " }, hint));
        }
        if control {
            spans.extend(self.mode_spans());
            // Working on a machine the user does not sit at: worth knowing.
            // With the path in this bar the machine stands next to it.
            if let Some(machine) = self
                .machine_label(None)
                .filter(|_| self.origin.is_some() && bars.path != side)
            {
                spans.push(Span::styled(
                    format!(" · {machine}"),
                    Style::new().fg(t.cyan).add_modifier(Modifier::BOLD),
                ));
            }
            // Others see and type into the same session: worth knowing.
            if self.attached > 1 {
                spans.push(Span::styled(
                    format!(" · {} clients", self.attached),
                    Style::new().fg(t.cyan).add_modifier(Modifier::BOLD),
                ));
            }
        }
        let left = Line::from(spans);

        if bars.path == side
            && let Some(cwd) = self.focused_cwd()
        {
            // Which machine this is, always: ⌂ the one the user sits at,
            // ⇄ another.
            let machine = self
                .machine_label(None)
                .unwrap_or_else(|| format!("⌂ {}", protocol::machine_name()));
            let machine_style = if self.origin.is_some() {
                Style::new().fg(t.cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let room =
                usize::from(area.width).saturating_sub(left.width() + 8 + machine.chars().count());
            let path = shorten(&tilde(&cwd), room);
            if !path.is_empty() {
                // Plain text with a coloured marker: dimmed text is close to
                // invisible in some colour schemes.
                let line = Line::from(vec![
                    Span::styled("▸ ", Style::new().fg(t.cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(path),
                    Span::styled(" · ", Style::new().fg(t.muted)),
                    Span::styled(machine, machine_style),
                    Span::raw(" "),
                ]);
                // Alone in its line the path starts on the left, where the
                // eye is. Next to the control bar or tabs it goes right.
                if left.width() == 0 {
                    let area = Rect {
                        x: area.x + 1,
                        width: area.width.saturating_sub(1),
                        ..area
                    };
                    frame.render_widget(line, area);
                } else {
                    frame.render_widget(line.right_aligned(), area);
                }
            }
        }
        frame.render_widget(left, area);
    }

    /// Workspace tabs, each in its workspace's colour: filled when it is
    /// the active one, red when an agent there waits.
    fn tab_spans(&self) -> Vec<Span<'static>> {
        let t = theme::current();
        self.workspaces()
            .into_iter()
            .map(|n| {
                let ws = if n == self.workspace {
                    &self.ws
                } else {
                    &self.hidden[&n]
                };
                let waiting = self
                    .agents
                    .iter()
                    .any(|(id, (state, _))| *state == AgentState::Blocked && ws.contains(*id));
                if waiting {
                    Span::styled(
                        tab_label(n, ws),
                        Style::new()
                            .fg(theme::current().on_accent)
                            .bg(theme::current().danger)
                            .add_modifier(Modifier::BOLD),
                    )
                } else if n == self.workspace {
                    Span::styled(
                        tab_label(n, ws),
                        Style::new()
                            .fg(t.on_accent)
                            .bg(t.workspace(n))
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(tab_label(n, ws), Style::new().fg(t.workspace(n)))
                }
            })
            .collect()
    }

    /// The mode badge and key hints of the control bar. Every mode has its
    /// own colour, so a glance at the bar tells which one is on.
    fn mode_spans(&self) -> Vec<Span<'static>> {
        let t = theme::current();
        let hint = Style::new().fg(t.muted);
        let mut spans = Vec::new();
        let searching = self
            .prompt
            .as_ref()
            .is_some_and(|p| matches!(p.purpose, PromptFor::Search { .. }));
        let (label, color, keys) = match self.mode {
            Mode::Prefix(_) => ("PREFIX", t.accent, "? all keys · Enter pick · Esc cancel"),
            Mode::Pick(..) => (
                "PREFIX",
                t.accent,
                "↑↓←→ choose · Enter or Space run · ⌫ back · or the key · Esc cancel",
            ),
            Mode::Menu(_) => (
                "QUIT",
                t.magenta,
                "↑↓ select · Enter or letter choose · Esc cancel",
            ),
            Mode::Settings(_) => (
                "SETTINGS",
                t.cyan,
                "↑↓ select · ←→ Enter change · Esc close",
            ),
            Mode::NewWindowWhere(_) => (
                "NEW WINDOW",
                t.magenta,
                "↑↓ choose the session, on any machine · Enter or digit · Esc cancel",
            ),
            Mode::NewSessionWhere(_) => (
                "NEW SESSION",
                t.magenta,
                "↑↓ choose the machine · Enter or digit · Esc cancel",
            ),
            Mode::NewSessionTemplate(_) => (
                "NEW SESSION",
                t.magenta,
                "↑↓ choose the template · Enter or digit · Esc cancel",
            ),
            Mode::Marks(_) => (
                "MARKS",
                t.magenta,
                "↑↓ choose · Enter or digit go there · J K move · x remove · Esc cancel",
            ),
            Mode::Recap(..) => (
                "AWAY",
                t.magenta,
                "● started · ◆ asked · ✦ finished · Enter or digit go there · Esc close",
            ),
            Mode::Context(_) => (
                "MENU",
                t.blue,
                "click or ↑↓ Enter · its key · Esc or a click outside closes",
            ),
            Mode::Layouts(_) => (
                "LAYOUT",
                t.blue,
                "↑↓ choose · Enter or digit give it this window · s save · x remove · Esc cancel",
            ),
            Mode::Notes => (
                "NOTES",
                t.warning,
                "type, Enter keeps it · ↑↓ pick · Enter send to the agent · y copy · e edit · x remove · Esc close",
            ),
            Mode::Review => (
                "REVIEW",
                t.magenta,
                "j k move · n p file · c note · x remove · s send to the agent · Esc close",
            ),
            Mode::Inbox(_) => (
                "INBOX",
                t.magenta,
                "↑↓ choose · digit option · type + Enter answer · Enter go there · Esc cancel",
            ),
            Mode::Agents(_) => (
                "AGENTS",
                t.magenta,
                "↑↓ choose · Enter or digit go there · m mark for Alt+1-9 · Esc cancel",
            ),
            Mode::Windows(_) => (
                "WINDOWS",
                t.blue,
                "↑↓ choose · Enter or digit go there · / search · Esc cancel",
            ),
            Mode::Panes(_) => (
                "PANES",
                t.blue,
                "↑↓ choose · Enter or digit go there · / search · Esc cancel",
            ),
            Mode::MoveTo(..) => (
                "MOVE",
                t.magenta,
                "↑↓ choose · Enter or digit move there · n new session · Esc cancel",
            ),
            Mode::Sessions(_) => (
                "SESSIONS",
                t.magenta,
                "↑↓ choose · Enter switch · n new · x end · r rename · o old · s save as hive · h hives · Esc",
            ),
            Mode::Hives(_) => (
                "HIVES",
                t.accent,
                "↑↓ choose · Enter bring back · s save new · u update · r rename · x delete · Esc cancel",
            ),
            Mode::Jump => (
                "JUMP",
                t.accent,
                match &self.jump {
                    Some(jump) if jump.query.is_empty() => {
                        "type a few letters of where to jump · Esc back"
                    }
                    Some(jump) if jump.targets.is_empty() => {
                        "no match · ⌫ fewer letters · Esc back"
                    }
                    _ => {
                        "press a label to jump there · Enter/Space the yellow one · more letters narrow it · Esc back"
                    }
                },
            ),
            Mode::Accounts(_) => (
                "ACCOUNTS",
                t.accent,
                "↑↓ choose · Enter focused agent goes on in it · u new shells here · Esc cancel",
            ),
            Mode::AgentAccount(_) => (
                "NEW AGENT",
                t.accent,
                "↑↓ choose · Enter or 1-9 start it in that account · Esc cancel",
            ),
            Mode::Picker => ("GO TO", t.cyan, "type to filter · Enter go · Esc cancel"),
            Mode::Sidebar(_) => (
                "SIDEBAR",
                t.blue,
                "j k move · Enter go/open · h fold · f files · o edit · r name · x close · Esc back",
            ),
            Mode::Prompt if searching => ("SEARCH", t.orange, "Enter find · Esc cancel"),
            Mode::Prompt => ("NAME", t.orange, "Enter save · Esc cancel"),
            Mode::Copy => (
                "COPY",
                t.magenta,
                if self.config.which_key.copy_mode {
                    "v select · s jump · y copy · i hide keys · q quit"
                } else {
                    "hjkl w b e move · s jump · / search · v V select · y copy · i show all keys · q quit"
                },
            ),
            Mode::Help(_) => ("HELP", t.blue, "j k scroll · any other key closes"),
            Mode::Confirm(_) => ("CONFIRM", t.danger, "y yes · any other key cancels"),
            Mode::Repeat(..) => ("REPEAT", t.orange, "arrows again, without the prefix"),
            Mode::Normal => ("", t.accent, ""),
        };
        if !label.is_empty() {
            spans.push(Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(t.on_accent)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!("  {keys}"), Style::new().fg(color)));
            return spans;
        }
        match self.mode {
            Mode::Normal if self.flash.is_some() => {
                let text = self.flash.as_deref().unwrap_or_default();
                // Messages about waiting agents are warnings, the rest are
                // confirmations like "copied".
                if self.agents.values().any(|(s, _)| *s == AgentState::Blocked)
                    && text.contains("needs you")
                {
                    spans.push(Span::styled(
                        format!("◆ {text}"),
                        state_style(AgentState::Blocked),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!("✓ {text}"),
                        Style::new().fg(theme::current().success),
                    ));
                }
            }
            Mode::Normal => {
                for state in [
                    AgentState::Blocked,
                    AgentState::Done,
                    AgentState::Working,
                    AgentState::Idle,
                ] {
                    let count = self
                        .agents
                        .keys()
                        .filter(|id| self.shown_state(**id) == Some(state))
                        .count();
                    if count > 0 {
                        spans.push(Span::styled(
                            format!("{} {count} {}", state.symbol(), state.name()),
                            state_style(state),
                        ));
                        spans.push(Span::styled(" · ", hint));
                    }
                }
                spans.push(Span::styled(
                    format!(
                        "{} {} · ^B menu · ^B ? all keys",
                        self.panes.len(),
                        if self.panes.len() == 1 {
                            "pane"
                        } else {
                            "panes"
                        }
                    ),
                    hint,
                ));
            }
            // Every other mode returned its badge above.
            _ => {}
        }
        spans
    }
}

/// A `width` x `height` rect in the middle of `area`, shrunk to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn is_prefix(key: KeyEvent) -> bool {
    bindings::PREFIX.matches(key)
}

/// The screen cut into the bar lines the settings ask for and the panes.
struct ScreenLayout {
    top: Option<Rect>,
    body: Rect,
    bottom: Option<Rect>,
}

fn screen_layout(screen: Rect, bars: &Bars, spacing: &Spacing) -> ScreenLayout {
    let mut rest = screen;
    let mut top = None;
    let mut bottom = None;
    // A bar at the edge takes the outermost line over the whole width...
    let top_edge = bars.uses(Placement::Top) && spacing.top_bar == BarEdge::Edge;
    let bottom_edge = bars.uses(Placement::Bottom) && spacing.bottom_bar == BarEdge::Edge;
    if top_edge && rest.height > 0 {
        top = Some(Rect { height: 1, ..rest });
        rest.y += 1;
        rest.height -= 1;
    }
    if bottom_edge && rest.height > 0 {
        rest.height -= 1;
        bottom = Some(Rect {
            y: rest.bottom(),
            height: 1,
            ..rest
        });
    }
    // ...then the margin, as long as the panes keep room for a border and
    // a line...
    let (x, y) = (spacing.outer_x, spacing.outer_y);
    if rest.width > 2 * x + 2 && rest.height > 2 * y + 2 {
        rest = Rect::new(
            rest.x + x,
            rest.y + y,
            rest.width - 2 * x,
            rest.height - 2 * y,
        );
    }
    // ...and an inset bar sits inside it, a gap away from the panes.
    let gap = |rest: Rect| spacing.gap_y.min(rest.height.saturating_sub(4) / 2);
    if bars.uses(Placement::Top) && !top_edge && rest.height > 0 {
        let gap = gap(rest);
        top = Some(Rect { height: 1, ..rest });
        rest.y += 1 + gap;
        rest.height -= 1 + gap;
    }
    if bars.uses(Placement::Bottom) && !bottom_edge && rest.height > 0 {
        let gap = gap(rest);
        rest.height -= 1 + gap;
        bottom = Some(Rect {
            y: rest.bottom() + gap,
            height: 1,
            ..rest
        });
    }
    ScreenLayout {
        top,
        body: rest,
        bottom,
    }
}

/// ` ⬢ imkr `, or the session's name for a named one.
/// Why a host's sessions are unknown, in a few words for the sidebar.
fn short_problem(error: &str) -> String {
    if error.contains("command not found") || error.contains("unknown arguments") {
        "imkr there is too old or missing".into()
    } else if error.contains("no sessions") {
        "no sessions".into()
    } else {
        "offline".into()
    }
}

/// Lists the other sessions here every `LOCAL_EVERY` and those on the hosts
/// from the settings and from `imkr --host` every `HOSTS_EVERY`, into
/// `places`. Hosts are asked one after the other, through ssh without
/// prompting; one that does not answer only says so.
fn watch_places(places: Arc<Mutex<Places>>, hosts: Arc<Mutex<Vec<String>>>) {
    let _ = thread::Builder::new().name("places".into()).spawn(move || {
        let mut hosts_at: Option<Instant> = None;
        let mut hives_at: Option<Instant> = None;
        // The state each agent elsewhere was last seen in, by host, session
        // and pane.
        let mut states: HashMap<(String, String, PaneId), String> = HashMap::new();
        let mut noted: Vec<String> = Vec::new();
        let mut web_at: Option<Instant> = None;
        let mut brain_at: Option<Instant> = None;
        loop {
            let own = protocol::session_name();
            let local: Vec<_> = crate::client::local_sessions()
                .into_iter()
                .filter(|s| s.name != own)
                .collect();
            // What runs, for bringing it all back after a restart. Only the
            // sessions of the user's own runtime directory, not tests'.
            let running = to_bring_back(&own, &local, &persist::were_running());
            if running != noted && std::env::var_os("IMKR_SOCKET").is_none() {
                let _ = persist::note_running(&running);
                noted = running;
            }
            if web_at.is_none_or(|at| at.elapsed() >= WEB_EVERY) {
                web_at = Some(Instant::now());
                crate::web::keep_running();
            }
            // Contacts' messages through AI Brain (none without such a
            // contact); test servers leave them to the user's own.
            if brain_at.is_none_or(|at| at.elapsed() >= BRAIN_EVERY)
                && std::env::var_os("IMKR_SOCKET").is_none()
            {
                brain_at = Some(Instant::now());
                let _ = crate::contacts::fetch_brain();
            }
            let web = crate::web::connected();
            let mut known = places.lock().unwrap();
            known.local = local;
            known.web = web;
            drop(known);

            if hives_at.is_none_or(|at| at.elapsed() >= HIVES_EVERY) {
                let online = crate::client::tailnet_peers()
                    .into_iter()
                    .filter(|p| p.online)
                    .map(|p| p.name)
                    .collect();
                places.lock().unwrap().tailnet = online;
            }
            if hosts_at.is_none_or(|at| at.elapsed() >= HOSTS_EVERY) {
                hosts_at = Some(Instant::now());
                let here = protocol::machine_name();
                let mut all = hosts.lock().unwrap().clone();
                all.extend(crate::client::remembered_hosts());
                let mut seen = HashSet::new();
                all.retain(|h| *h != here && seen.insert(h.clone()));
                let found: Vec<_> = all
                    .into_iter()
                    .map(|host| {
                        let sessions =
                            crate::client::remote_sessions(&host).map_err(|e| format!("{e:#}"));
                        (host, sessions)
                    })
                    .collect();
                let mut alerts = Vec::new();
                let mut now = HashMap::new();
                for (host, sessions) in &found {
                    for session in sessions.iter().flatten() {
                        for agent in &session.agent_panes {
                            let key = (host.clone(), session.name.clone(), agent.pane);
                            if let Some(what) = remote_change(states.get(&key), &agent.state) {
                                alerts.push((
                                    format!("{} {what}", agent.program),
                                    format!(
                                        "⇄ {host} · {} · window {}",
                                        session.name, agent.workspace
                                    ),
                                ));
                            }
                            now.insert(key, agent.state.clone());
                        }
                    }
                }
                states = now;
                // Messages there for agents here, from machines that cannot
                // reach this one (see `client::fetch_outbox`).
                for (host, sessions) in &found {
                    if sessions.is_ok() {
                        let _ = crate::client::fetch_outbox(host);
                    }
                }
                // Hives saved elsewhere come here too, from those that answer.
                if hives_at.is_none_or(|at| at.elapsed() >= HIVES_EVERY) {
                    hives_at = Some(Instant::now());
                    for (host, sessions) in &found {
                        if sessions.is_ok() {
                            let _ = crate::client::pull_hives(host);
                            let _ = crate::client::pull_archive(host);
                        }
                    }
                }
                let mut places = places.lock().unwrap();
                for (host, sessions) in &found {
                    match sessions {
                        Ok(sessions) => {
                            let names = sessions.iter().map(|s| s.name.clone()).collect();
                            places.last_seen.insert(host.clone(), names);
                        }
                        // Never seen from here: what the newest hive knows.
                        Err(_) if !places.last_seen.contains_key(host) => {
                            if let Some(names) = sessions_in_hives(host) {
                                places.last_seen.insert(host.clone(), names);
                            }
                        }
                        Err(_) => {}
                    }
                }
                places.hosts = found;
                places.alerts.extend(alerts);
            }
            thread::sleep(LOCAL_EVERY);
        }
    });
}

/// The sessions on the client's machine, with their agents if the client
/// sent them, else just by name, as clients from before do.
fn origin_infos(origin: &protocol::Origin) -> Vec<protocol::SessionInfo> {
    if !origin.infos.is_empty() {
        return origin.infos.clone();
    }
    origin
        .sessions
        .iter()
        .map(|name| protocol::SessionInfo {
            name: name.clone(),
            ..Default::default()
        })
        .collect()
}

/// The sessions machine `host` had in the newest hive that has it.
fn sessions_in_hives(host: &str) -> Option<Vec<String>> {
    persist::shared_hives()
        .into_values()
        .filter_map(|hive| {
            let time = persist::hive_time(&hive);
            let sessions = if hive.machine == host {
                hive.sessions
            } else {
                hive.machines.into_iter().find(|m| m.name == host)?.sessions
            };
            Some((time, sessions.into_keys().collect()))
        })
        .max_by_key(|(time, _)| *time)
        .map(|(_, names)| names)
}

/// Whether a list item matches a search: every word of it somewhere in
/// its title or hint, ignoring case.
fn list_matches(item: &menu::MenuItem, query: &str) -> bool {
    let text = format!("{} {}", item.title, item.hint).to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| text.contains(word))
}

/// What an agent elsewhere did since it was last seen `before`: came to
/// need the user, or finished. Nothing for one seen the first time, so a
/// newly known host does not ring for everything already waiting there.
fn remote_change(before: Option<&String>, now: &str) -> Option<&'static str> {
    let before = before?;
    match (before.as_str(), now) {
        (old, "blocked") if old != "blocked" => Some("needs you"),
        ("working" | "blocked", "idle" | "done") => Some("is done"),
        _ => None,
    }
}

/// Reads a moved window and the descriptors of its ptys that follow it on
/// `stream`.
fn receive_window(
    stream: &UnixStream,
    json: &[u8],
) -> std::result::Result<(upgrade::MovedWindow, Vec<OwnedFd>), String> {
    let window: upgrade::MovedWindow =
        serde_json::from_slice(json).map_err(|e| format!("broken window: {e}"))?;
    let fds = crate::fdpass::recv(stream, window.panes.len())
        .map_err(|e| format!("the panes' terminals did not arrive: {e}"))?;
    Ok((window, fds))
}

/// Fills what lies outside the shared `screen` on a client of `size` with
/// dots, so it is clear the session ends there.
fn outside(size: Rect, screen: Rect) -> Vec<u8> {
    let mut out = b"\x1b[0;2m".to_vec();
    for row in 0..size.height {
        let (col, width) = if row < screen.height {
            (screen.width, size.width.saturating_sub(screen.width))
        } else {
            (0, size.width)
        };
        if width > 0 {
            out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
            out.extend_from_slice("·".repeat(usize::from(width)).as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

/// Moves a client's images from where they are, `shown`, to where they go,
/// `wanted`: placements moved, added and removed.
fn placements(out: &mut Vec<u8>, shown: &Placements, wanted: &Placements) {
    for (&(image, id), &at) in wanted {
        if shown.get(&(image, id)) == Some(&at) {
            continue;
        }
        let (x, y, cols, rows) = at;
        // Save the cursor, place the image without moving it, restore.
        out.extend_from_slice(
            format!(
                "\x1b7\x1b[{};{}H\x1b_Ga=p,i={image},p={id},c={cols},r={rows},C=1,q=2\x1b\\\x1b8",
                y + 1,
                x + 1
            )
            .as_bytes(),
        );
    }
    for &(image, id) in shown.keys() {
        if !wanted.contains_key(&(image, id)) {
            out.extend_from_slice(format!("\x1b_Ga=d,d=i,i={image},p={id},q=2\x1b\\").as_bytes());
        }
    }
}

fn badge_text() -> String {
    let name = protocol::session_name();
    if name == protocol::DEFAULT_SESSION {
        " ⬢ imkr ".to_owned()
    } else {
        format!(" ⬢ {name} ")
    }
}

/// The panes' area and the sidebar's, if it is on and there is room for it
/// next to panes at least as wide.
/// A focused sidebar is shown even when it is turned off.
/// `gap` columns keep the sidebar apart from the panes.
fn split_sidebar(
    body: Rect,
    config: &config::Sidebar,
    focused: bool,
    gap: u16,
) -> (Rect, Option<Rect>) {
    let width = sidebar::WIDTH;
    if !(config.enabled || focused) || body.width < 2 * width + gap {
        return (body, None);
    }
    let rest = body.width - width - gap;
    match config.side {
        Side::Left => (
            Rect {
                x: body.x + width + gap,
                width: rest,
                ..body
            },
            Some(Rect { width, ..body }),
        ),
        Side::Right => (
            Rect {
                width: rest,
                ..body
            },
            Some(Rect {
                x: body.x + rest + gap,
                width,
                ..body
            }),
        ),
    }
}

/// Shows a file's changes against the last commit, or all of it when git
/// does not track it yet, in a pager.
const DIFF_SCRIPT: &str = "if git ls-files --error-unmatch -- \"$1\" >/dev/null 2>&1; \
then git diff --color=always HEAD -- \"$1\"; \
else git diff --color=always --no-index -- /dev/null \"$1\"; fi | less -R";

/// A repository's changed files, as last read.
struct GitState {
    /// The directory it was read for.
    cwd: PathBuf,
    root: PathBuf,
    changes: Vec<sidebar::Change>,
    at: Instant,
}

/// Reads the repository containing `cwd`, `None` outside one.
fn git_state(cwd: &std::path::Path) -> Option<GitState> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let root = PathBuf::from(git(&["rev-parse", "--show-toplevel"])?.trim());
    let status = git(&["status", "--porcelain"])?;
    Some(GitState {
        cwd: cwd.to_owned(),
        root,
        changes: sidebar::parse_changes(&status),
        at: Instant::now(),
    })
}

/// Two clicks on the same cell within this time make a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The first and last column of the word around column `col` in `line`,
/// `None` on a space.
fn word_bounds(line: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: &char| !c.is_whitespace() && !"\"'`()[]{}<>|,;".contains(*c);
    if !chars.get(col).is_some_and(is_word) {
        return None;
    }
    let start = chars[..col]
        .iter()
        .rposition(|c| !is_word(c))
        .map_or(0, |i| i + 1);
    let end = chars[col..]
        .iter()
        .position(|c| !is_word(c))
        .map_or(chars.len(), |i| col + i)
        - 1;
    Some((start, end))
}

/// The next place `query` appears in `rows` after `from` (before it when
/// not `forward`), wrapping around. Ignores case unless the query has
/// capitals, like vim's smartcase.
fn find_in_rows(
    rows: &[String],
    from: (usize, u16),
    query: &str,
    forward: bool,
) -> Option<(usize, u16)> {
    let fold = !query.chars().any(char::is_uppercase);
    let norm = |s: &str| if fold { s.to_lowercase() } else { s.to_owned() };
    let query = norm(query);
    let len = rows.len();
    if len == 0 || query.is_empty() {
        return None;
    }
    // Columns of every match in a row, in characters.
    let matches = |row: &str| -> Vec<usize> {
        let row = norm(row);
        row.match_indices(&query)
            .map(|(byte, _)| row[..byte].chars().count())
            .collect()
    };
    let (row0, col0) = (from.0.min(len - 1), usize::from(from.1));
    for step in 0..=len {
        let row = if forward {
            (row0 + step) % len
        } else {
            (row0 + len - step % len) % len
        };
        let cols = matches(&rows[row]);
        let hit = match (step, forward) {
            (0, true) => cols.into_iter().find(|&c| c > col0),
            (0, false) => cols.into_iter().rev().find(|&c| c < col0),
            (_, true) => cols.into_iter().next(),
            (_, false) => cols.into_iter().next_back(),
        };
        if let Some(col) = hit {
            return Some((row, col as u16));
        }
    }
    None
}

/// Names are cut to this many characters.
const MAX_NAME: usize = 32;
/// Whether `Ctrl+B S` hides a session as old until `o`: only one that is
/// not running. Every running session stays, however long its agents have
/// been quiet, as in the sidebar (#7921): one that vanished after 12 quiet
/// hours looked gone while it ran.
fn is_old_session(current: bool, stand_in: bool, dormant: bool) -> bool {
    !current && !stand_in && dormant
}
/// How long agents may take after an update or restart to be idle again
/// and keep how long they were before, see `App::carried`.
const CARRIED_FOR: Duration = Duration::from_secs(120);
/// The longest directory typed in a prompt.
/// The sessions to bring back after a restart, sorted: those running here
/// (`own` and the running ones of `local`), and those of `before` that
/// stopped without being ended. When the machine shuts down, the servers
/// go one by one, and one still running would otherwise drop the others
/// that went first. One ended on purpose has no state left (or is closed,
/// in the hive `last`) and drops out.
fn to_bring_back(own: &str, local: &[protocol::SessionInfo], before: &[String]) -> Vec<String> {
    let mut names: Vec<String> = local
        .iter()
        .filter(|s| match s.dormant.as_deref() {
            None => true,
            Some("stopped") => before.contains(&s.name),
            Some(_) => false,
        })
        .map(|s| s.name.clone())
        .chain([own.to_owned()])
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Directories listed on other machines, by machine and directory.
type DirLists = Arc<Mutex<HashMap<(String, String), Option<Vec<String>>>>>;

/// How many of the last lines of an agent's screen say whether its
/// account is used up.
const USED_UP_LINES: usize = 8;

/// After noticing a used up account, how long its message on the screen
/// does not count again.
const USED_UP_QUIET: Duration = Duration::from_secs(600);

/// How long `switch_account` waits for an agent to end.
const SWITCH_WAIT: Duration = Duration::from_secs(15);

/// An agent waiting for the account it starts in: a new window of one of
/// `tools` (`Ctrl+B k`, `panes` empty), or the panes of a template that
/// start `tools[0]`, with the command each would have run.
struct AgentStart {
    tools: Vec<String>,
    panes: Vec<(PaneId, String)>,
}

/// Whether `program` is on the PATH.
fn on_path(program: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", "command -v \"$0\" >/dev/null", program])
        .stdin(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Notes account `name` of `tool` in `accounts`: `standard` is no entry.
fn set_account(accounts: &mut BTreeMap<String, String>, tool: &str, name: &str) {
    if name == crate::account::STANDARD {
        accounts.remove(tool);
    } else {
        accounts.insert(tool.to_owned(), name.to_owned());
    }
}

/// This imkr, for commands typed into panes; `imkr` when it cannot say.
fn imkr_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|exe| {
            let exe = exe.to_string_lossy().into_owned();
            // Replaced by an update while running: the new one is there.
            exe.trim_end_matches(" (deleted)").to_owned()
        })
        .unwrap_or_else(|| "imkr".to_owned())
}

/// How many directories the directory prompt lists under the text.
const DIR_ROWS: usize = 8;
const MAX_PATH: usize = 400;

/// The button after the tabs that opens a new workspace.
const NEW_TAB: &str = " + ";

/// A label in a border that runs a command when clicked.
struct Button {
    area: Rect,
    label: &'static str,
    command: Command,
    color: Color,
}

/// Puts `items` side by side in the one-row `row`, from its right end with
/// `right`, leaving all out if they do not fit.
fn place_buttons(row: Rect, items: &[(&'static str, Command, Color)], right: bool) -> Vec<Button> {
    let width: u16 = items.iter().map(|(l, ..)| l.chars().count() as u16).sum();
    if width > row.width {
        return Vec::new();
    }
    let mut x = if right { row.right() - width } else { row.x };
    items
        .iter()
        .map(|&(label, command, color)| {
            let w = label.chars().count() as u16;
            let area = Rect::new(x, row.y, w, 1);
            x += w;
            Button {
                area,
                label,
                command,
                color,
            }
        })
        .collect()
}

/// Split, zoom and close in the top border of a pane at `rect`, when it
/// is wide enough to keep room for its title. Floating panes only close.
fn pane_buttons(rect: Rect, floating: bool) -> Vec<Button> {
    if rect.width < 36 || rect.height < 3 {
        return Vec::new();
    }
    let t = theme::current();
    let row = Rect::new(rect.x + 1, rect.y, rect.width - 3, 1);
    let all = [
        (" ┃ ", Command::SplitRow, t.blue),
        (" ━ ", Command::SplitColumn, t.blue),
        (" ⤢ ", Command::Zoom, t.cyan),
        (" × ", Command::ClosePane, t.danger),
    ];
    place_buttons(row, if floating { &all[3..] } else { &all }, true)
}

/// New workspace, new floating pane and the settings in the sidebar's
/// bottom border.
fn sidebar_buttons(area: Rect) -> Vec<Button> {
    if area.height < 3 {
        return Vec::new();
    }
    let t = theme::current();
    let row = Rect::new(
        area.x + 1,
        area.bottom() - 1,
        area.width.saturating_sub(2),
        1,
    );
    place_buttons(
        row,
        &[
            (" + window ", Command::NewWorkspace, t.success),
            (" ⧉ float ", Command::NewFloat, t.cyan),
            (" ⚙ ", Command::Settings, t.orange),
        ],
        false,
    )
}

fn draw_buttons(frame: &mut Frame, buttons: &[Button]) {
    for button in buttons {
        frame.render_widget(
            Span::styled(
                button.label,
                Style::new().fg(button.color).add_modifier(Modifier::BOLD),
            ),
            button.area,
        );
    }
}

/// A workspace tab: its number, and its name if it has one.
fn tab_label(n: u8, ws: &Workspace) -> String {
    match &ws.name {
        Some(name) => format!(" {n} {name} "),
        None => format!(" {n} "),
    }
}

fn badge_style() -> Style {
    Style::new()
        .fg(theme::current().on_accent)
        .bg(theme::current().accent)
        .add_modifier(Modifier::BOLD)
}

/// `path` cut down to `room` characters, keeping its end, e.g.
/// `…/imkr/src`.
fn shorten(path: &str, room: usize) -> String {
    let len = path.chars().count();
    if len <= room {
        return path.to_owned();
    }
    if room < 2 {
        return String::new();
    }
    let tail: String = path.chars().skip(len - (room - 1)).collect();
    format!("…{tail}")
}

/// The area inside a pane's border, i.e. the size of its pty.
fn pane_inner(rect: Rect) -> Rect {
    Block::bordered().inner(rect)
}

/// `path` with the home directory written as `~`.
fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME") {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|shell| shell.rsplit('/').next().map(str::to_owned))
        .unwrap_or_else(|| "shell".into())
}

/// The pane a window shows from another machine (`imkr hosted H PANE
/// [SESSION]`): host, session and pane there.
fn hosted_target(argv: &[String]) -> Option<(String, String, PaneId)> {
    hosted_command(argv)?;
    let host = argv.get(2)?.clone();
    let pane = argv.get(3)?.parse().ok()?;
    let session = argv
        .get(4)
        .cloned()
        .unwrap_or_else(|| protocol::DEFAULT_SESSION.to_owned());
    Some((host, session, pane))
}

/// For a window whose program runs on another machine (`imkr hosted`):
/// the line that shows it again after a restart here. Started again on
/// its own, unlike other programs: it restarts nothing there.
fn hosted_command(argv: &[String]) -> Option<String> {
    let imkr = argv.first()?.rsplit('/').next() == Some("imkr");
    if !imkr || argv.get(1).map(String::as_str) != Some("hosted") {
        return None;
    }
    let words: Vec<String> = argv.iter().map(|w| crate::client::sh_quote(w)).collect();
    Some(format!("exec {}", words.join(" ")))
}

#[cfg(test)]
mod tests {

    #[test]
    fn words_move_like_vim_big_words() {
        let rows: Vec<String> = ["cargo test --release", "", "  src/app.rs:42 failed"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        // w: the next word, over the blank row.
        assert_eq!(word_motion(&rows, (0, 0), Word::Next), Some((0, 6)));
        assert_eq!(word_motion(&rows, (0, 11), Word::Next), Some((2, 2)));
        // e: the end of this word, then of the next; a path is one word.
        assert_eq!(word_motion(&rows, (0, 0), Word::End), Some((0, 4)));
        assert_eq!(word_motion(&rows, (2, 2), Word::End), Some((2, 14)));
        // b: back to the start of the word before.
        assert_eq!(word_motion(&rows, (2, 2), Word::Back), Some((0, 11)));
        assert_eq!(word_motion(&rows, (0, 8), Word::Back), Some((0, 6)));
        assert_eq!(word_motion(&rows, (0, 0), Word::Back), None);
    }

    #[test]
    fn only_sessions_not_running_are_old() {
        // Running, its agents quiet for days: stays (#7921).
        assert!(!is_old_session(false, false, false));
        // Not running: hidden until `o`.
        assert!(is_old_session(false, false, true));
        // The session you are in, and a machine without one yet, never.
        assert!(!is_old_session(true, false, true));
        assert!(!is_old_session(false, true, true));
    }

    #[test]
    fn sessions_stopped_by_a_shutdown_come_back() {
        let session = |name: &str, dormant: Option<&str>| protocol::SessionInfo {
            name: name.into(),
            dormant: dormant.map(str::to_owned),
            ..Default::default()
        };
        let local = [
            session("api", None),
            // Went down first when the machine shut down.
            session("deckbox", Some("stopped")),
            // Ended on purpose: closed, in the hive `last`.
            session("old", Some("closed")),
            // Stopped long ago, not running before either.
            session("verwaltung", Some("stopped")),
        ];
        let before = ["deckbox".to_owned(), "imkr".to_owned(), "old".to_owned()];
        assert_eq!(
            to_bring_back("imkr", &local, &before),
            ["api", "deckbox", "imkr"]
        );
        // Once it was ended on purpose, its state is gone.
        assert_eq!(to_bring_back("imkr", &local[..1], &before), ["api", "imkr"]);
    }

    #[test]
    fn pane_buttons_sit_right_in_the_top_border() {
        let buttons = pane_buttons(Rect::new(10, 5, 40, 10), false);
        let labels: Vec<&str> = buttons.iter().map(|b| b.label.trim()).collect();
        assert_eq!(labels, ["┃", "━", "⤢", "×"]);
        let close = buttons.last().unwrap();
        // One cell of border before the corner.
        assert_eq!((close.area.right(), close.area.y), (48, 5));
        assert_eq!(close.command, Command::ClosePane);
        assert_eq!(pane_buttons(Rect::new(0, 0, 40, 10), true).len(), 1);
        assert!(pane_buttons(Rect::new(0, 0, 20, 10), false).is_empty());
    }
    use super::*;

    #[test]
    fn hosted_windows_come_back_after_a_restart() {
        let argv: Vec<String> = ["/home/u/.cargo/bin/imkr", "hosted", "box", "3", "dev"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            hosted_command(&argv).as_deref(),
            Some("exec /home/u/.cargo/bin/imkr hosted box 3 dev")
        );
        let shell: Vec<String> = ["zsh"].map(String::from).to_vec();
        assert_eq!(hosted_command(&shell), None);
    }

    #[test]
    fn edge_bars_stay_at_the_edge_around_the_margin() {
        let screen = Rect::new(0, 0, 80, 24);
        let spacing = Spacing {
            outer_x: 2,
            outer_y: 1,
            ..Spacing::default()
        };
        let layout = screen_layout(screen, &Bars::default(), &spacing);
        assert_eq!(layout.bottom, Some(Rect::new(0, 23, 80, 1)));
        assert_eq!(layout.body, Rect::new(2, 1, 76, 21));
    }

    #[test]
    fn search_matches_every_word_in_title_or_hint() {
        let item = menu::MenuItem {
            key: '1',
            title: "brain".into(),
            hint: "⇄ codingmachine".into(),
            danger: false,
            apart: false,
            color: None,
            group: None,
            marks: Vec::new(),
        };
        assert!(list_matches(&item, ""));
        assert!(list_matches(&item, "BRA"));
        assert!(list_matches(&item, "coding brain"));
        assert!(!list_matches(&item, "brain laptop"));
    }

    #[test]
    fn agents_elsewhere_alert_on_waiting_and_finishing_only() {
        let was = |s: &str| Some(s.to_owned());
        assert_eq!(
            remote_change(was("working").as_ref(), "blocked"),
            Some("needs you")
        );
        assert_eq!(
            remote_change(was("idle").as_ref(), "blocked"),
            Some("needs you")
        );
        assert_eq!(
            remote_change(was("working").as_ref(), "idle"),
            Some("is done")
        );
        assert_eq!(
            remote_change(was("blocked").as_ref(), "done"),
            Some("is done")
        );
        // Still waiting, just busy, or seen for the first time: quiet.
        assert_eq!(remote_change(was("blocked").as_ref(), "blocked"), None);
        assert_eq!(remote_change(was("idle").as_ref(), "working"), None);
        assert_eq!(remote_change(None, "blocked"), None);
    }

    #[test]
    fn outside_fills_only_what_the_shared_screen_leaves() {
        let text =
            String::from_utf8(outside(Rect::new(0, 0, 5, 3), Rect::new(0, 0, 3, 2))).unwrap();
        // Two dots right of the first two rows, a full row below.
        assert!(text.contains("\x1b[1;4H··"));
        assert!(text.contains("\x1b[2;4H··"));
        assert!(text.contains("\x1b[3;1H·····"));
        // A client exactly the shared size gets nothing but the reset.
        assert_eq!(
            outside(Rect::new(0, 0, 3, 2), Rect::new(0, 0, 3, 2)),
            b"\x1b[0;2m\x1b[0m"
        );
    }

    #[test]
    fn inset_bars_move_in_with_the_margin_a_gap_from_the_panes() {
        let screen = Rect::new(0, 0, 80, 24);
        let spacing = Spacing {
            outer_x: 2,
            outer_y: 1,
            gap_y: 1,
            bottom_bar: BarEdge::Inset,
            ..Spacing::default()
        };
        let layout = screen_layout(screen, &Bars::default(), &spacing);
        assert_eq!(layout.bottom, Some(Rect::new(2, 22, 76, 1)));
        assert_eq!(layout.body, Rect::new(2, 1, 76, 20));
    }

    #[test]
    fn sidebar_keeps_the_gap_from_the_panes() {
        let body = Rect::new(0, 0, 100, 20);
        let config = config::Sidebar::default();
        let (panes, side) = split_sidebar(body, &config, false, 2);
        let side = side.unwrap();
        assert_eq!(panes.right() + 2, side.x);
        assert_eq!(side.right(), body.right());
    }

    #[test]
    fn bars_take_lines_on_their_side() {
        let screen = Rect::new(0, 0, 80, 24);
        let layout = screen_layout(screen, &Bars::default(), &Spacing::default());
        assert_eq!(layout.top, None);
        assert_eq!(layout.body, Rect::new(0, 0, 80, 23));
        assert_eq!(layout.bottom, Some(Rect::new(0, 23, 80, 1)));

        let split = Bars {
            control: Placement::Bottom,
            tabs: Placement::Top,
            path: Placement::Off,
        };
        let layout = screen_layout(screen, &split, &Spacing::default());
        assert_eq!(layout.top, Some(Rect::new(0, 0, 80, 1)));
        assert_eq!(layout.body, Rect::new(0, 1, 80, 22));
        assert_eq!(layout.bottom, Some(Rect::new(0, 23, 80, 1)));
    }

    #[test]
    fn sidebar_takes_its_side_when_there_is_room() {
        let body = Rect::new(0, 1, 120, 30);
        let mut config = config::Sidebar::default();
        let (panes, side) = split_sidebar(body, &config, false, 0);
        assert_eq!(panes, Rect::new(0, 1, 90, 30));
        assert_eq!(side, Some(Rect::new(90, 1, 30, 30)));

        config.side = Side::Left;
        let (panes, side) = split_sidebar(body, &config, false, 0);
        assert_eq!(panes, Rect::new(30, 1, 90, 30));
        assert_eq!(side, Some(Rect::new(0, 1, 30, 30)));

        assert_eq!(
            split_sidebar(Rect::new(0, 0, 50, 20), &config, false, 0).1,
            None
        );
        config.enabled = false;
        assert_eq!(split_sidebar(body, &config, false, 0), (body, None));
        assert!(split_sidebar(body, &config, true, 0).1.is_some());
    }

    #[test]
    fn double_click_finds_the_word() {
        let line = "cargo test --lib (src/app.rs) done";
        assert_eq!(word_bounds(line, 2), Some((0, 4)));
        assert_eq!(word_bounds(line, 12), Some((11, 15)));
        assert_eq!(word_bounds(line, 20), Some((18, 27)));
        assert_eq!(word_bounds(line, 5), None);
    }

    #[test]
    fn search_finds_matches_both_ways_and_wraps() {
        let rows: Vec<String> = ["error one", "ok", "Error two", "ok"]
            .map(String::from)
            .to_vec();
        assert_eq!(find_in_rows(&rows, (0, 0), "error", true), Some((2, 0)));
        assert_eq!(find_in_rows(&rows, (2, 0), "error", true), Some((0, 0)));
        assert_eq!(find_in_rows(&rows, (3, 0), "error", false), Some((2, 0)));
        assert_eq!(find_in_rows(&rows, (3, 0), "Error", false), Some((2, 0)));
        assert_eq!(find_in_rows(&rows, (2, 0), "Error", true), Some((2, 0)));
        assert_eq!(find_in_rows(&rows, (0, 0), "missing", true), None);
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(
            shorten("~/Development/beehive", 40),
            "~/Development/beehive"
        );
        assert_eq!(shorten("~/Development/beehive", 8), "…beehive");
        assert_eq!(shorten("~/x", 1), "");
    }
}
