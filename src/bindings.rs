//! The key bindings after the prefix. This table is the single source for
//! what a key does and for every place that shows keys to the user: the
//! which-key menu, the help overlay and `imkr keys`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::layout::Direction;

/// The prefix key, Ctrl+B like tmux and herdr.
pub const PREFIX: Key = Key::ctrl(KeyCode::Char('b'));
pub const PREFIX_LABEL: &str = "Ctrl+B";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    SplitRow,
    SplitColumn,
    ClosePane,
    NextPane,
    Focus(Direction),
    Resize(Direction, u16),
    Move(Direction, u16),
    ToggleFloat,
    NewFloat,
    Detach,
    SessionMenu,
    Settings,
    Workspace(u8),
    NewWorkspace,
    NextWorkspace,
    PrevWorkspace,
    NextFloat,
    RenamePane,
    Sessions,
    Hives,
    /// Start a new session, asking for its name.
    NewSession,
    /// Rename this session.
    RenameSession,
    /// End this session, asking first.
    EndSession,
    /// Save every running session as a hive, asking for its name.
    SaveHive,
    /// What the agents did while no terminal was attached.
    Recap,
    PickPane,
    /// The list of every window, in every session on every machine.
    Windows,
    /// The list of every pane, in every session on every machine.
    Panes,
    /// Search everything: panes, windows, sessions, agents, plugins,
    /// hives, marks and commands.
    Search,
    ReloadConfig,
    ScrollbackEditor,
    Zoom,
    Swap(bool),
    Equalize,
    LastPane,
    RenameWorkspace,
    /// Move the active window with its panes to another session.
    MoveWorkspace,
    /// Number the windows 1, 2, 3 … again, closing the gaps.
    RenumberWorkspaces,
    /// The layouts to give this window: presets and saved ones.
    Layouts,
    /// Show or hide the popup used last.
    TogglePopup,
    /// Every agent here and elsewhere, to jump to.
    Agents,
    /// The agents everywhere that wait for the user or are done, with what
    /// they ask, to answer from the list.
    Inbox,
    /// A new window in a session chosen from all, on any machine.
    NewWindowElsewhere,
    /// Mark the focused pane for Alt+1 … Alt+9, like Harpoon.
    Mark,
    /// The marks, to jump to, reorder and remove.
    Marks,
    /// Turn the shown popup into a pane of a window of its own, or the
    /// focused pane into a popup.
    BreakPopup,
    /// Give the sidebar the keyboard.
    FocusSidebar,
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Hide bars, sidebar and borders and show only the focused pane.
    FocusMode,
    /// Focus the agent waiting longest for the user.
    JumpToWaiting,
    /// Scroll back and select text with the keyboard.
    CopyMode,
    /// Copy mode, scrolled up half a page right away.
    ScrollBack,
    Quit,
    Help,
    SendPrefix,
    /// Opens a submenu, like a group in which-key.nvim.
    Open(Menu),
    /// Moves the focused pane into window n, made if needed; its program
    /// runs on.
    PaneToWindow(u8),
    /// Moves the focused pane into a new window of its own.
    PaneToNewWindow,
    /// Asks for a branch and opens a window in a git worktree of it.
    WorktreeWindow,
    /// Asks for a machine and opens a window whose program runs there.
    HostedWindow,
    /// The changes of the focused pane's repository, to note on lines and
    /// send the notes to its agent.
    Review,
    /// The notepad of the focused pane: thoughts for its agent, kept with
    /// the pane.
    Notes,
    /// Goes back from a submenu to the main menu.
    Back,
    Cancel,
    /// The plugins of the catalogs, to add.
    PluginMarket,
    /// The agents' accounts: for this session, or the focused agent now.
    Accounts,
    /// Jump like flash.nvim: type a few characters, every match on screen
    /// gets a label, the label's key puts the copy cursor there.
    Jump,
}

impl Command {
    /// Repeatable commands keep the prefix active for a moment, so arrow keys
    /// can be pressed again without it.
    pub fn repeatable(self) -> bool {
        matches!(
            self,
            Command::Focus(_)
                | Command::Resize(..)
                | Command::Move(..)
                | Command::NextFloat
                | Command::NextWorkspace
                | Command::PrevWorkspace
        )
    }
}

/// The menus after the prefix: the main one and its submenus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Root,
    Floating,
    Workspaces,
    /// The user's own commands from `[[commands]]` in the config.
    Commands,
    /// Sessions and hives.
    Session,
    /// The plugins, from `plugins/*.toml` next to the config.
    Plugins,
    /// Where to move the focused pane.
    PaneTo,
}

impl Menu {
    pub const ALL: [Menu; 7] = [
        Menu::Root,
        Menu::Floating,
        Menu::Workspaces,
        Menu::PaneTo,
        Menu::Session,
        Menu::Commands,
        Menu::Plugins,
    ];

    pub fn groups(self) -> &'static [Group] {
        match self {
            Menu::Root => &[Group::Panes, Group::Navigate, Group::View, Group::Session],
            Menu::Floating => &[Group::Floating],
            Menu::Workspaces => &[Group::Workspaces],
            Menu::Commands => &[Group::Commands],
            Menu::Session => &[Group::Sessions],
            Menu::Plugins => &[Group::Plugins],
            Menu::PaneTo => &[Group::PaneTo],
        }
    }

    /// The keys that open this menu, after the prefix, e.g. `f`.
    pub fn path(self) -> &'static str {
        match self {
            Menu::Root => "",
            Menu::Floating => "f",
            Menu::Workspaces => "w",
            Menu::Commands => "c",
            Menu::Session => "s",
            Menu::Plugins => "p",
            Menu::PaneTo => ">",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Panes,
    Navigate,
    View,
    Session,
    /// Keys of the main menu that the which-key popup lists only in its
    /// footer: rarely needed, or variations of listed ones.
    More,
    Floating,
    Workspaces,
    Commands,
    /// The session submenu.
    Sessions,
    Plugins,
    PaneTo,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Panes => "Panes",
            Group::Navigate => "Go to",
            Group::View => "View",
            Group::Session => "Session",
            Group::More => "More",
            Group::Floating => "Floating panes",
            Group::Workspaces => "Windows",
            Group::Commands => "Your commands",
            Group::Sessions => "Sessions and hives",
            Group::Plugins => "Plugins",
            Group::PaneTo => "Move this pane to",
        }
    }

    /// The colour of the group's heading in menus.
    pub fn color(self) -> ratatui::style::Color {
        let t = crate::theme::current();
        match self {
            Group::Panes | Group::Workspaces => t.blue,
            Group::Navigate | Group::Floating => t.cyan,
            Group::View => t.orange,
            Group::Session | Group::Commands | Group::Sessions | Group::Plugins => t.magenta,
            Group::PaneTo => t.blue,
            Group::More => t.muted,
        }
    }

    pub fn menu(self) -> Menu {
        match self {
            Group::Panes | Group::Navigate | Group::View | Group::Session | Group::More => {
                Menu::Root
            }
            Group::Floating => Menu::Floating,
            Group::Workspaces => Menu::Workspaces,
            Group::Commands => Menu::Commands,
            Group::Sessions => Menu::Session,
            Group::Plugins => Menu::Plugins,
            Group::PaneTo => Menu::PaneTo,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Key {
    /// The key as a terminal reports it.
    pub fn event(&self) -> KeyEvent {
        KeyEvent::new(self.code, self.mods)
    }

    const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::NONE,
        }
    }

    const fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::CONTROL,
        }
    }

    const fn shift(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::SHIFT,
        }
    }

    const fn alt(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::ALT,
        }
    }

    /// Whether `event` is this key. For characters Shift is ignored: it is
    /// part of the character already (`%`, `Q`), and terminals disagree on
    /// whether they report it on top. For other keys, like arrows, Shift
    /// counts.
    pub fn matches(self, event: KeyEvent) -> bool {
        let relevant = match self.code {
            KeyCode::Char(_) => KeyModifiers::CONTROL | KeyModifiers::ALT,
            _ => KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        };
        event.code == self.code && event.modifiers & relevant == self.mods
    }
}

/// One line in the menu. Several keys can share a line, like the four arrows.
pub struct Binding {
    pub label: &'static str,
    pub description: &'static str,
    pub group: Group,
    pub keys: &'static [(Key, Command)],
}

use Command as C;
use Direction::{Down, Left, Right, Up};
use KeyCode::{Char, Esc};

pub const BINDINGS: &[Binding] = &[
    // The main menu, after the prefix.
    // tmux's keys, and keys without Shift on a German keyboard.
    Binding {
        label: "% # |",
        description: "split side by side",
        group: Group::Panes,
        keys: &[
            (Key::plain(Char('%')), C::SplitRow),
            (Key::plain(Char('#')), C::SplitRow),
            (Key::plain(Char('|')), C::SplitRow),
        ],
    },
    Binding {
        label: "\" -",
        description: "split top and bottom",
        group: Group::Panes,
        keys: &[
            (Key::plain(Char('"')), C::SplitColumn),
            (Key::plain(Char('-')), C::SplitColumn),
        ],
    },
    Binding {
        label: "x",
        description: "close pane",
        group: Group::Panes,
        keys: &[(Key::plain(Char('x')), C::ClosePane)],
    },
    Binding {
        label: "z",
        description: "zoom pane / popup",
        group: Group::Panes,
        keys: &[(Key::plain(Char('z')), C::Zoom)],
    },
    Binding {
        label: "{ }",
        description: "swap with prev/next",
        group: Group::Panes,
        keys: &[
            (Key::plain(Char('{')), C::Swap(false)),
            (Key::plain(Char('}')), C::Swap(true)),
        ],
    },
    Binding {
        label: "=",
        description: "equal sizes",
        group: Group::Panes,
        keys: &[(Key::plain(Char('=')), C::Equalize)],
    },
    Binding {
        label: "r",
        description: "name pane",
        group: Group::Panes,
        keys: &[(Key::plain(Char('r')), C::RenamePane)],
    },
    Binding {
        label: "t",
        description: "notepad of the pane…",
        group: Group::Panes,
        keys: &[(Key::plain(Char('t')), C::Notes)],
    },
    Binding {
        label: ">",
        description: "move pane to window…",
        group: Group::Panes,
        keys: &[(Key::plain(Char('>')), C::Open(Menu::PaneTo))],
    },
    Binding {
        label: "f",
        description: "floating…",
        group: Group::Panes,
        keys: &[(Key::plain(Char('f')), C::Open(Menu::Floating))],
    },
    Binding {
        label: "←↑↓→",
        description: "pane there",
        group: Group::Navigate,
        keys: &[
            (Key::plain(KeyCode::Left), C::Focus(Left)),
            (Key::plain(KeyCode::Right), C::Focus(Right)),
            (Key::plain(KeyCode::Up), C::Focus(Up)),
            (Key::plain(KeyCode::Down), C::Focus(Down)),
        ],
    },
    Binding {
        label: "o ;",
        description: "next / last pane",
        group: Group::Navigate,
        keys: &[
            (Key::plain(Char('o')), C::NextPane),
            (Key::plain(Char(';')), C::LastPane),
        ],
    },
    Binding {
        label: "g",
        description: "any pane…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('g')), C::PickPane)],
    },
    Binding {
        label: "/",
        description: "search all…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('/')), C::Search)],
    },
    Binding {
        label: "a",
        description: "waiting agent",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('a')), C::JumpToWaiting)],
    },
    Binding {
        label: "i",
        description: "inbox: answer agents…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('i')), C::Inbox)],
    },
    Binding {
        label: "v",
        description: "review changes…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('v')), C::Review)],
    },
    Binding {
        label: "A",
        description: "all agents…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('A')), C::Agents)],
    },
    Binding {
        label: "W",
        description: "all windows…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('W')), C::Windows)],
    },
    Binding {
        label: "P",
        description: "all panes…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('P')), C::Panes)],
    },
    Binding {
        label: "m M",
        description: "mark pane / marks…",
        group: Group::Navigate,
        keys: &[
            (Key::plain(Char('m')), C::Mark),
            (Key::plain(Char('M')), C::Marks),
        ],
    },
    Binding {
        label: "1-9",
        description: "window",
        group: Group::Navigate,
        keys: &[
            (Key::plain(Char('1')), C::Workspace(1)),
            (Key::plain(Char('2')), C::Workspace(2)),
            (Key::plain(Char('3')), C::Workspace(3)),
            (Key::plain(Char('4')), C::Workspace(4)),
            (Key::plain(Char('5')), C::Workspace(5)),
            (Key::plain(Char('6')), C::Workspace(6)),
            (Key::plain(Char('7')), C::Workspace(7)),
            (Key::plain(Char('8')), C::Workspace(8)),
            (Key::plain(Char('9')), C::Workspace(9)),
        ],
    },
    Binding {
        label: "w",
        description: "windows…",
        group: Group::Navigate,
        keys: &[(Key::plain(Char('w')), C::Open(Menu::Workspaces))],
    },
    Binding {
        label: "n N",
        description: "new window / elsewhere…",
        group: Group::Navigate,
        keys: &[
            (Key::plain(Char('n')), C::NewWorkspace),
            (Key::plain(Char('N')), C::NewWindowElsewhere),
        ],
    },
    // `y` as well: `[` needs AltGr on a German keyboard.
    Binding {
        label: "[ y",
        description: "copy mode",
        group: Group::View,
        keys: &[
            (Key::plain(Char('[')), C::CopyMode),
            (Key::plain(Char('y')), C::CopyMode),
        ],
    },
    Binding {
        label: "j",
        description: "jump to text on screen",
        group: Group::View,
        keys: &[(Key::plain(Char('j')), C::Jump)],
    },
    Binding {
        label: "u",
        description: "page up",
        group: Group::View,
        keys: &[
            (Key::plain(Char('u')), C::ScrollBack),
            (Key::plain(KeyCode::PageUp), C::ScrollBack),
        ],
    },
    Binding {
        label: "E",
        description: "history in $EDITOR",
        group: Group::View,
        keys: &[(Key::plain(Char('E')), C::ScrollbackEditor)],
    },
    Binding {
        label: "b",
        description: "sidebar on/off",
        group: Group::View,
        keys: &[(Key::plain(Char('b')), C::ToggleSidebar)],
    },
    Binding {
        label: "e",
        description: "into the sidebar",
        group: Group::View,
        keys: &[(Key::plain(Char('e')), C::FocusSidebar)],
    },
    Binding {
        label: "F",
        description: "focus mode",
        group: Group::View,
        keys: &[(Key::plain(Char('F')), C::FocusMode)],
    },
    Binding {
        label: "Space",
        description: "popup show/hide",
        group: Group::View,
        keys: &[(Key::plain(KeyCode::Char(' ')), C::TogglePopup)],
    },
    Binding {
        label: "!",
        description: "popup ⇄ window",
        group: Group::View,
        keys: &[(Key::plain(Char('!')), C::BreakPopup)],
    },
    Binding {
        label: "q",
        description: "quit…",
        group: Group::Session,
        keys: &[(Key::plain(Char('q')), C::SessionMenu)],
    },
    Binding {
        label: "d",
        description: "detach",
        group: Group::Session,
        keys: &[(Key::plain(Char('d')), C::Detach)],
    },
    Binding {
        label: "s",
        description: "session…",
        group: Group::Session,
        keys: &[(Key::plain(Char('s')), C::Open(Menu::Session))],
    },
    Binding {
        label: "c",
        description: "commands…",
        group: Group::Session,
        keys: &[(Key::plain(Char('c')), C::Open(Menu::Commands))],
    },
    Binding {
        label: "p",
        description: "plugins…",
        group: Group::Session,
        keys: &[(Key::plain(Char('p')), C::Open(Menu::Plugins))],
    },
    Binding {
        label: ",",
        description: "settings",
        group: Group::Session,
        keys: &[(Key::plain(Char(',')), C::Settings)],
    },
    Binding {
        label: "?",
        description: "all keys",
        group: Group::Session,
        keys: &[(Key::plain(Char('?')), C::Help)],
    },
    // Listed in the footer only.
    Binding {
        label: "S",
        description: "session list",
        group: Group::More,
        keys: &[(Key::plain(Char('S')), C::Sessions)],
    },
    Binding {
        label: "H",
        description: "hives",
        group: Group::More,
        keys: &[(Key::plain(Char('H')), C::Hives)],
    },
    Binding {
        label: "Ctrl/Alt ←↑↓→",
        description: "resize",
        group: Group::More,
        keys: &[
            (Key::ctrl(KeyCode::Left), C::Resize(Left, 1)),
            (Key::ctrl(KeyCode::Right), C::Resize(Right, 1)),
            (Key::ctrl(KeyCode::Up), C::Resize(Up, 1)),
            (Key::ctrl(KeyCode::Down), C::Resize(Down, 1)),
            (Key::alt(KeyCode::Left), C::Resize(Left, 5)),
            (Key::alt(KeyCode::Right), C::Resize(Right, 5)),
            (Key::alt(KeyCode::Up), C::Resize(Up, 5)),
            (Key::alt(KeyCode::Down), C::Resize(Down, 5)),
        ],
    },
    Binding {
        label: "Shift ←↑↓→",
        description: "move float",
        group: Group::More,
        keys: &[
            (Key::shift(KeyCode::Left), C::Move(Left, 1)),
            (Key::shift(KeyCode::Right), C::Move(Right, 1)),
            (Key::shift(KeyCode::Up), C::Move(Up, 1)),
            (Key::shift(KeyCode::Down), C::Move(Down, 1)),
        ],
    },
    Binding {
        label: "Q",
        description: "quit imkr",
        group: Group::More,
        keys: &[(Key::plain(Char('Q')), C::Quit)],
    },
    Binding {
        label: "R",
        description: "reload config",
        group: Group::More,
        keys: &[(Key::plain(Char('R')), C::ReloadConfig)],
    },
    Binding {
        label: PREFIX_LABEL,
        description: "sends Ctrl+B",
        group: Group::More,
        keys: &[(PREFIX, C::SendPrefix)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::More,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // Floating panes, after the prefix and `f`.
    Binding {
        label: "f",
        description: "float / tile this pane",
        group: Group::Floating,
        keys: &[(Key::plain(Char('f')), C::ToggleFloat)],
    },
    Binding {
        label: "n",
        description: "new floating pane",
        group: Group::Floating,
        keys: &[(Key::plain(Char('n')), C::NewFloat)],
    },
    Binding {
        label: "o",
        description: "next floating pane",
        group: Group::Floating,
        keys: &[
            (Key::plain(Char('o')), C::NextFloat),
            (Key::plain(KeyCode::Tab), C::NextFloat),
        ],
    },
    Binding {
        label: "←↑↓→",
        description: "move",
        group: Group::Floating,
        keys: &[
            (Key::plain(KeyCode::Left), C::Move(Left, 1)),
            (Key::plain(KeyCode::Right), C::Move(Right, 1)),
            (Key::plain(KeyCode::Up), C::Move(Up, 1)),
            (Key::plain(KeyCode::Down), C::Move(Down, 1)),
        ],
    },
    Binding {
        label: "Ctrl ←↑↓→",
        description: "resize by 1",
        group: Group::Floating,
        keys: &[
            (Key::ctrl(KeyCode::Left), C::Resize(Left, 1)),
            (Key::ctrl(KeyCode::Right), C::Resize(Right, 1)),
            (Key::ctrl(KeyCode::Up), C::Resize(Up, 1)),
            (Key::ctrl(KeyCode::Down), C::Resize(Down, 1)),
        ],
    },
    Binding {
        label: "Alt ←↑↓→",
        description: "resize by 5",
        group: Group::Floating,
        keys: &[
            (Key::alt(KeyCode::Left), C::Resize(Left, 5)),
            (Key::alt(KeyCode::Right), C::Resize(Right, 5)),
            (Key::alt(KeyCode::Up), C::Resize(Up, 5)),
            (Key::alt(KeyCode::Down), C::Resize(Down, 5)),
        ],
    },
    Binding {
        label: "⌫",
        description: "back",
        group: Group::Floating,
        keys: &[(Key::plain(KeyCode::Backspace), C::Back)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::Floating,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // The user's commands, after the prefix and `c`. The commands themselves
    // come from the config, these are only the keys every menu has.
    Binding {
        label: "⌫",
        description: "back",
        group: Group::Commands,
        keys: &[(Key::plain(KeyCode::Backspace), C::Back)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::Commands,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // The plugins' own keys come from their files, like the commands'.
    Binding {
        label: "+",
        description: "marketplace…",
        group: Group::Plugins,
        keys: &[(Key::plain(Char('+')), C::PluginMarket)],
    },
    Binding {
        label: "⌫",
        description: "back",
        group: Group::Plugins,
        keys: &[(Key::plain(KeyCode::Backspace), C::Back)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::Plugins,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // Moving the focused pane, after the prefix and `>`.
    Binding {
        label: "1-9",
        description: "that window",
        group: Group::PaneTo,
        keys: &[
            (Key::plain(Char('1')), C::PaneToWindow(1)),
            (Key::plain(Char('2')), C::PaneToWindow(2)),
            (Key::plain(Char('3')), C::PaneToWindow(3)),
            (Key::plain(Char('4')), C::PaneToWindow(4)),
            (Key::plain(Char('5')), C::PaneToWindow(5)),
            (Key::plain(Char('6')), C::PaneToWindow(6)),
            (Key::plain(Char('7')), C::PaneToWindow(7)),
            (Key::plain(Char('8')), C::PaneToWindow(8)),
            (Key::plain(Char('9')), C::PaneToWindow(9)),
        ],
    },
    Binding {
        label: "n",
        description: "a new window",
        group: Group::PaneTo,
        keys: &[(Key::plain(Char('n')), C::PaneToNewWindow)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::PaneTo,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // Workspaces, after the prefix and `w`.
    Binding {
        label: "1-9",
        description: "go to window",
        group: Group::Workspaces,
        keys: &[
            (Key::plain(Char('1')), C::Workspace(1)),
            (Key::plain(Char('2')), C::Workspace(2)),
            (Key::plain(Char('3')), C::Workspace(3)),
            (Key::plain(Char('4')), C::Workspace(4)),
            (Key::plain(Char('5')), C::Workspace(5)),
            (Key::plain(Char('6')), C::Workspace(6)),
            (Key::plain(Char('7')), C::Workspace(7)),
            (Key::plain(Char('8')), C::Workspace(8)),
            (Key::plain(Char('9')), C::Workspace(9)),
        ],
    },
    Binding {
        label: "r",
        description: "name window",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('r')), C::RenameWorkspace)],
    },
    Binding {
        label: "c",
        description: "new window",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('c')), C::NewWorkspace)],
    },
    Binding {
        label: "t",
        description: "new window in a git worktree…",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('t')), C::WorktreeWindow)],
    },
    Binding {
        label: "h",
        description: "new window on another machine…",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('h')), C::HostedWindow)],
    },
    Binding {
        label: "m",
        description: "move to session",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('m')), C::MoveWorkspace)],
    },
    Binding {
        label: "l",
        description: "layout…",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('l')), C::Layouts)],
    },
    Binding {
        label: "=",
        description: "renumber windows",
        group: Group::Workspaces,
        keys: &[(Key::plain(Char('=')), C::RenumberWorkspaces)],
    },
    Binding {
        label: "n p ←→",
        description: "next / previous",
        group: Group::Workspaces,
        keys: &[
            (Key::plain(Char('n')), C::NextWorkspace),
            (Key::plain(Char('p')), C::PrevWorkspace),
            (Key::plain(KeyCode::Right), C::NextWorkspace),
            (Key::plain(KeyCode::Left), C::PrevWorkspace),
        ],
    },
    Binding {
        label: "⌫",
        description: "back",
        group: Group::Workspaces,
        keys: &[(Key::plain(KeyCode::Backspace), C::Back)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::Workspaces,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
    // The session submenu.
    Binding {
        label: "s S",
        description: "switch…",
        group: Group::Sessions,
        keys: &[
            (Key::plain(Char('s')), C::Sessions),
            (Key::plain(Char('S')), C::Sessions),
        ],
    },
    Binding {
        label: "n",
        description: "new session…",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('n')), C::NewSession)],
    },
    Binding {
        label: "r",
        description: "rename this session",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('r')), C::RenameSession)],
    },
    Binding {
        label: "x",
        description: "end this session",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('x')), C::EndSession)],
    },
    Binding {
        label: "d",
        description: "detach",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('d')), C::Detach)],
    },
    Binding {
        label: "c",
        description: "accounts of the agents…",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('c')), C::Accounts)],
    },
    Binding {
        label: "h H",
        description: "hives…",
        group: Group::Sessions,
        keys: &[
            (Key::plain(Char('h')), C::Hives),
            (Key::plain(Char('H')), C::Hives),
        ],
    },
    Binding {
        label: "a",
        description: "save all as hive…",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('a')), C::SaveHive)],
    },
    Binding {
        label: "l",
        description: "while you were away…",
        group: Group::Sessions,
        keys: &[(Key::plain(Char('l')), C::Recap)],
    },
    Binding {
        label: "⌫",
        description: "back",
        group: Group::Sessions,
        keys: &[(Key::plain(KeyCode::Backspace), C::Back)],
    },
    Binding {
        label: "Esc",
        description: "cancel",
        group: Group::Sessions,
        keys: &[(Key::plain(Esc), C::Cancel)],
    },
];

/// A key and what it does, as a menu's footer shows it.
pub type KeyHint = (&'static str, &'static str);

/// The keys of the session list (`Ctrl+B S`) besides its entries. Its
/// popup, the help and `imkr keys` all show this list.
pub const SESSION_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key switch"),
    ("x", "end"),
    ("r", "rename"),
    ("s", "save all as hive"),
    ("h", "hives"),
    ("o", "hide/show old"),
    ("Esc", "cancel"),
];

/// The keys of the accounts (`Ctrl+B s c`).
pub const ACCOUNT_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    (
        "Enter",
        "use: the focused agent goes on in it, else new shells",
    ),
    ("u", "new shells of this session"),
    ("Esc", "cancel"),
];

/// The keys of the marks (`Ctrl+B M`).
pub const MARK_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key go there"),
    ("J K", "move down/up"),
    ("x", "remove"),
    ("Esc", "cancel"),
];

/// The keys of the layout list (`Ctrl+B w l`).
pub const LAYOUT_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key give it this window"),
    ("s", "save this window's as…"),
    ("x", "remove a saved one"),
    ("Esc", "cancel"),
];

/// The keys of the lists that only pick a place: machine, session.
pub const CHOOSE_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key choose"),
    ("Esc", "cancel"),
];

/// The keys of the agent list (`Ctrl+B A`).
pub const AGENT_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key go there"),
    ("m", "mark for Alt+1-9"),
    ("Esc", "cancel"),
];

/// The keys of the inbox (`Ctrl+B i`).
pub const INBOX_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("digit", "choose that option"),
    ("text Enter", "answer"),
    ("Enter", "go there"),
    ("✉ Enter x", "accept, decline a contact's message"),
    ("Esc", "cancel"),
];

/// The keys of the list of sessions to move a window to (`Ctrl+B w m`).
pub const MOVE_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or key move there"),
    ("Esc", "cancel"),
];

/// The keys of the hives list (`Ctrl+B H`) besides its entries.
pub const HIVE_LIST_KEYS: &[KeyHint] = &[
    ("↑↓", "select"),
    ("Enter", "or digit bring back"),
    ("s", "save new"),
    ("u", "update"),
    ("w", "switch: only this hive runs"),
    ("r", "rename"),
    ("x", "delete"),
    ("Esc", "cancel"),
];

/// The keys of copy mode (`Ctrl+B y`), in groups: title, keys. Its key
/// board, the help and `imkr keys` all show this table.
pub const COPY_KEYS: &[(&str, &[KeyHint])] = &[
    (
        "Move",
        &[
            ("hjkl", "by one"),
            ("w b e", "words"),
            ("0 $", "line ends"),
            ("g G", "top, bottom"),
            ("^U ^D", "half page"),
            ("^B ^F", "page"),
        ],
    ),
    (
        "Jump & search",
        &[
            ("s", "jump (again: end there)"),
            ("/ ?", "search down, up"),
            ("n N", "next, previous"),
        ],
    ),
    (
        "Select",
        &[
            ("v", "start"),
            ("V", "whole lines"),
            ("y", "copy"),
            ("i", "hide keys"),
            ("q", "leave"),
        ],
    ),
];

/// The lists with keys of their own, for the help: title, how to open it,
/// its keys.
pub const LISTS: &[(&str, &str, &[KeyHint])] = &[
    ("Session list", "S", SESSION_LIST_KEYS),
    ("Hives", "H", HIVE_LIST_KEYS),
    ("Accounts", "s c", ACCOUNT_LIST_KEYS),
    ("Move window", "w m", MOVE_LIST_KEYS),
    ("Agents", "A", AGENT_LIST_KEYS),
    ("Inbox (answer waiting agents)", "i", INBOX_KEYS),
    ("Windows everywhere", "W", CHOOSE_LIST_KEYS),
    ("Panes everywhere", "P", CHOOSE_LIST_KEYS),
    ("Marks (Alt+1 … 9 jumps)", "M", MARK_LIST_KEYS),
];

/// The command bound to `event` in `menu`.
pub fn lookup(menu: Menu, event: KeyEvent) -> Option<Command> {
    BINDINGS
        .iter()
        .filter(|binding| binding.group.menu() == menu)
        .flat_map(|binding| binding.keys)
        .find(|(key, _)| key.matches(event))
        .map(|&(_, command)| command)
}

/// Whether a binding is one thing to pick with the arrows in the which-key
/// menu: all its keys do the same (not `1-9` or `←↑↓→`).
pub fn pickable(binding: &Binding) -> bool {
    binding
        .keys
        .first()
        .is_some_and(|(_, first)| binding.keys.iter().all(|(_, c)| c == first))
}

/// The keys of what the which-key menu of `menu` lets pick with the arrows,
/// in the order shown: `commands` (the user's or the plugins') first, then
/// the menu's bindings group by group.
pub fn pick_keys(menu: Menu, commands: &[(char, String)]) -> Vec<KeyEvent> {
    let own = matches!(menu, Menu::Commands | Menu::Plugins);
    let mut keys: Vec<KeyEvent> = if own {
        commands
            .iter()
            .map(|(c, _)| KeyEvent::new(Char(*c), KeyModifiers::NONE))
            .collect()
    } else {
        Vec::new()
    };
    for group in menu.groups() {
        keys.extend(
            in_group(*group)
                .filter(|b| pickable(b))
                .map(|b| b.keys[0].0.event()),
        );
    }
    keys
}

/// How many of `pick_keys` each column of the which-key menu holds, in
/// order: a group each, the user's commands or plugins with their group.
pub fn pick_columns(menu: Menu, commands: &[(char, String)]) -> Vec<usize> {
    let mut columns: Vec<usize> = menu
        .groups()
        .iter()
        .map(|g| in_group(*g).filter(|b| pickable(b)).count())
        .collect();
    if matches!(menu, Menu::Commands | Menu::Plugins)
        && let Some(first) = columns.first_mut()
    {
        *first += commands.len();
    }
    columns
}

/// Where `at` of `pick_keys` lands one column to the side (`step` -1 or
/// 1, round), at the same height or the column's last entry.
pub fn pick_beside(columns: &[usize], at: usize, step: isize) -> usize {
    let starts: Vec<usize> = columns
        .iter()
        .scan(0, |sum, n| {
            let start = *sum;
            *sum += n;
            Some(start)
        })
        .collect();
    let Some(column) = (0..columns.len())
        .rev()
        .find(|&c| columns[c] > 0 && starts[c] <= at)
    else {
        return at;
    };
    let row = at - starts[column];
    let count = columns.len() as isize;
    let mut next = column as isize;
    for _ in 0..columns.len() {
        next = (next + step).rem_euclid(count);
        if columns[next as usize] > 0 {
            break;
        }
    }
    let next = next as usize;
    starts[next] + row.min(columns[next] - 1)
}

pub fn in_group(group: Group) -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(move |binding| binding.group == group)
}

/// The key reference as plain text, for `imkr keys`.
pub fn reference() -> String {
    let width = BINDINGS
        .iter()
        .map(|b| b.label.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("Press {PREFIX_LABEL}, then:\n");
    let groups = Menu::ALL
        .iter()
        .flat_map(|m| m.groups().iter().copied())
        .chain([Group::More]);
    for group in groups {
        match group.menu() {
            Menu::Root => out.push_str(&format!("\n{}\n", group.title())),
            menu => out.push_str(&format!(
                "\n{} ({PREFIX_LABEL}, {}, then)\n",
                group.title(),
                menu.path()
            )),
        }
        for binding in in_group(group) {
            let pad = width - binding.label.chars().count();
            out.push_str(&format!(
                "  {}{}  {}\n",
                binding.label,
                " ".repeat(pad),
                binding.description
            ));
        }
    }
    for (title, key, keys) in LISTS {
        out.push_str(&format!("\n{title} ({PREFIX_LABEL}, {key}), in the list\n"));
        for (label, description) in *keys {
            let pad = width.saturating_sub(label.chars().count());
            out.push_str(&format!("  {label}{}  {description}\n", " ".repeat(pad)));
        }
    }
    out.push_str(&format!("\nCopy mode ({PREFIX_LABEL}, y)\n"));
    for (title, keys) in COPY_KEYS {
        out.push_str(&format!("  {title}\n"));
        for (label, description) in *keys {
            let pad = width.saturating_sub(label.chars().count() + 2);
            out.push_str(&format!("    {label}{}  {description}\n", " ".repeat(pad)));
        }
    }
    out.push_str(
        "\nFocus, move and resize repeat: for a moment afterwards, the same keys work without the prefix.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn looks_up_commands() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            lookup(Menu::Root, event(Char('%'), none)),
            Some(C::SplitRow)
        );
        // Without Shift on a German keyboard, besides tmux's.
        assert_eq!(
            lookup(Menu::Root, event(Char('#'), none)),
            Some(C::SplitRow)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('|'), none)),
            Some(C::SplitRow)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('-'), none)),
            Some(C::SplitColumn)
        );
        assert_eq!(
            lookup(Menu::Root, event(KeyCode::Left, none)),
            Some(C::Focus(Left))
        );
        assert_eq!(
            lookup(Menu::Root, event(KeyCode::Up, KeyModifiers::CONTROL)),
            Some(C::Resize(Up, 1))
        );
        assert_eq!(
            lookup(Menu::Root, event(KeyCode::Up, KeyModifiers::ALT)),
            Some(C::Resize(Up, 5))
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('b'), KeyModifiers::CONTROL)),
            Some(C::SendPrefix)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('y'), none)),
            Some(C::CopyMode)
        );
        assert_eq!(lookup(Menu::Root, event(Char('j'), none)), Some(C::Jump));
        assert_eq!(lookup(Menu::Root, event(Char('k'), none)), None);
        assert_eq!(
            lookup(Menu::Root, event(Char(';'), none)),
            Some(C::LastPane)
        );
        assert_eq!(lookup(Menu::Root, event(Char('z'), none)), Some(C::Zoom));
        assert_eq!(
            lookup(Menu::Root, event(Char('n'), none)),
            Some(C::NewWorkspace)
        );
        assert_eq!(
            lookup(Menu::Workspaces, event(Char('c'), none)),
            Some(C::NewWorkspace)
        );
    }

    #[test]
    fn shift_is_ignored_but_ctrl_and_alt_are_not() {
        assert_eq!(
            lookup(Menu::Root, event(Char('%'), KeyModifiers::SHIFT)),
            Some(C::SplitRow)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('Q'), KeyModifiers::SHIFT)),
            Some(C::Quit)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('q'), KeyModifiers::NONE)),
            Some(C::SessionMenu)
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('x'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn shift_counts_for_arrows() {
        assert_eq!(
            lookup(Menu::Root, event(KeyCode::Left, KeyModifiers::NONE)),
            Some(C::Focus(Left))
        );
        assert_eq!(
            lookup(Menu::Root, event(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(C::Move(Left, 1))
        );
    }

    #[test]
    fn submenus_have_their_own_keys() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            lookup(Menu::Root, event(Char('f'), none)),
            Some(C::Open(Menu::Floating))
        );
        assert_eq!(
            lookup(Menu::Floating, event(Char('f'), none)),
            Some(C::ToggleFloat)
        );
        assert_eq!(
            lookup(Menu::Floating, event(KeyCode::Left, none)),
            Some(C::Move(Left, 1))
        );
        assert_eq!(
            lookup(Menu::Workspaces, event(Char('3'), none)),
            Some(C::Workspace(3))
        );
        assert_eq!(
            lookup(Menu::Workspaces, event(KeyCode::Backspace, none)),
            Some(C::Back)
        );
        assert_eq!(lookup(Menu::Workspaces, event(Char('%'), none)), None);
    }

    #[test]
    fn session_menu_has_its_commands_and_the_old_keys_still_work() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            lookup(Menu::Root, event(Char('s'), none)),
            Some(C::Open(Menu::Session))
        );
        assert_eq!(
            lookup(Menu::Root, event(Char('S'), none)),
            Some(C::Sessions)
        );
        assert_eq!(lookup(Menu::Root, event(Char('H'), none)), Some(C::Hives));
        assert_eq!(
            lookup(Menu::Session, event(Char('r'), none)),
            Some(C::RenameSession)
        );
        assert_eq!(
            lookup(Menu::Session, event(Char('x'), none)),
            Some(C::EndSession)
        );
        assert_eq!(
            lookup(Menu::Session, event(Char('a'), none)),
            Some(C::SaveHive)
        );
        assert!(reference().contains("save new") && reference().contains("or key switch"));
    }

    #[test]
    fn every_key_is_bound_once_per_menu() {
        for menu in Menu::ALL {
            let keys: Vec<Key> = BINDINGS
                .iter()
                .filter(|b| b.group.menu() == menu)
                .flat_map(|b| b.keys)
                .map(|&(k, _)| k)
                .collect();
            for (i, key) in keys.iter().enumerate() {
                assert!(
                    !keys[i + 1..].contains(key),
                    "{key:?} is bound twice in {menu:?}"
                );
            }
        }
    }

    /// docs/src/keys.md is `imkr keys` as a page; written anew with
    /// `IMKR_WRITE_DOCS=1 cargo test docs_keys`.
    #[test]
    fn docs_keys_are_current() {
        let want = format!(
            "# Keys\n\nEvery key, as `imkr keys` prints it. Generated from `src/bindings.rs`.\n\n```text\n{}```\n",
            reference()
        );
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/keys.md");
        if std::env::var_os("IMKR_WRITE_DOCS").is_some() {
            std::fs::write(path, &want).unwrap();
        }
        let have = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            have == want,
            "docs/src/keys.md is out of date: IMKR_WRITE_DOCS=1 cargo test docs_keys"
        );
    }

    #[test]
    fn reference_lists_every_binding() {
        let text = reference();
        for binding in BINDINGS {
            assert!(
                text.contains(binding.description),
                "{} missing",
                binding.description
            );
        }
    }
}
