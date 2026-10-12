//! User settings, stored as TOML in `$XDG_CONFIG_HOME/imkr/config.toml`.
//!
//! The server loads them at start. The settings menu changes them live and
//! writes them back right away.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhichKey {
    /// Show the key menu after the prefix.
    pub enabled: bool,
    /// Which bottom corner the menu opens in.
    pub position: Side,
}

impl Default for WhichKey {
    fn default() -> Self {
        Self {
            enabled: true,
            position: Side::Left,
        }
    }
}

/// Where a bar element is shown: in a line above or below the panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Top,
    Bottom,
    Off,
}

impl Placement {
    fn name(self) -> &'static str {
        match self {
            Placement::Top => "top",
            Placement::Bottom => "bottom",
            Placement::Off => "off",
        }
    }
}

/// Where a bar line sits: at the very edge of the screen over its full
/// width, or inside the outer margin like the panes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarEdge {
    #[default]
    Edge,
    Inset,
}

impl BarEdge {
    fn name(self) -> &'static str {
        match self {
            BarEdge::Edge => "edge",
            BarEdge::Inset => "inset",
        }
    }

    fn toggled(self) -> Self {
        match self {
            BarEdge::Edge => BarEdge::Inset,
            BarEdge::Inset => BarEdge::Edge,
        }
    }
}

/// Space around and between the panes, in cells. A cell is about twice as
/// tall as it is wide, so columns and rows are set apart: two columns look
/// like one row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spacing {
    /// Columns left and right of the panes.
    pub outer_x: u16,
    /// Rows above and below the panes.
    pub outer_y: u16,
    /// Columns between panes side by side, and before the sidebar.
    pub gap_x: u16,
    /// Rows between panes one above the other, and between an inset bar
    /// and the panes.
    pub gap_y: u16,
    pub top_bar: BarEdge,
    pub bottom_bar: BarEdge,
}

/// The largest spacing the settings menu steps to, in columns and rows.
const MAX_COLUMNS: u16 = 4;
const MAX_ROWS: u16 = 2;

/// `value` one step up or down, wrapping between 0 and `max`.
fn step_cells(value: u16, max: u16, forward: bool) -> u16 {
    let value = value.min(max);
    if forward {
        if value == max { 0 } else { value + 1 }
    } else if value == 0 {
        max
    } else {
        value - 1
    }
}

/// The elements of the bars around the panes. Elements on the same side
/// share one line: the control bar and the workspace tabs on the left, the
/// path on the right.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bars {
    /// The imkr badge, mode and key hints. Never off, it is the only
    /// sign of the prefix when the which-key menu is off.
    pub control: Placement,
    /// The workspace tabs.
    pub tabs: Placement,
    /// The working directory of the focused pane.
    pub path: Placement,
}

impl Default for Bars {
    fn default() -> Self {
        Self {
            control: Placement::Bottom,
            tabs: Placement::Bottom,
            path: Placement::Bottom,
        }
    }
}

impl Bars {
    /// Whether anything is shown on `side`.
    pub fn uses(&self, side: Placement) -> bool {
        self.control() == side || self.tabs == side || self.path == side
    }

    /// The control bar's side, bottom if the config says off.
    pub fn control(&self) -> Placement {
        match self.control {
            Placement::Off => Placement::Bottom,
            side => side,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `imkr`, `omarchy` to follow the desktop, or an Omarchy theme name.
    pub theme: String,
    pub which_key: WhichKey,
    pub bars: Bars,
    pub spacing: Spacing,
    pub sidebar: Sidebar,
    /// How to tell the user that an agent waits or finished while they
    /// looked elsewhere.
    pub notifications: Notifications,
    /// The user's own commands, in the `Ctrl+B c` menu.
    pub commands: Vec<UserCommand>,
    /// Whether agents are resumed after a restart: typed in and started,
    /// typed in for the user to start with Enter, or not at all.
    pub resume: Resume,
    /// The command that resumes each agent, `{session}` for the session id
    /// its hooks report; over `DEFAULT_RESUME`, an empty one turns an agent
    /// off.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub resume_commands: BTreeMap<String, String>,
    /// Programs to treat as coding agents besides those imkr knows, by the
    /// name they run under, e.g. `my-agent`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_agents: Vec<String>,
    /// Text that means an agent asks something, besides imkr's own
    /// ("Do you want to", "(y/n)" …), for agents without hooks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub question_phrases: Vec<String>,
    /// Shell commands run when something happens, see `Hook`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<Hook>,
    /// Images from programs, with the kitty graphics protocol, where the
    /// terminal shows them.
    pub images: bool,
    /// Windows move up to fill the gap a closed one leaves, so they are
    /// always numbered 1, 2, 3 …, as tmux's `renumber-windows`. Without
    /// it a window keeps its number for good.
    pub renumber_windows: bool,
    /// Alt+1 … Alt+9 jump to the panes marked with `Ctrl+B m`, without the
    /// prefix. Off leaves those keys to the programs.
    pub marks_on_alt: bool,
    /// Panes find imkr as `tmux`, so programs that drive tmux, like Claude
    /// Code's agent teams, open imkr panes. Off by default: with `$TMUX`
    /// set, programs act as inside tmux.
    pub tmux_shim: bool,
    /// `imkr` opens the session list when it starts, to pick where to go.
    /// Without it, it goes straight to the session used last.
    pub sessions_on_start: bool,
    /// This machine's name for you and your other machines, e.g. how they
    /// reach it by ssh (`laptop`). Without it, its hostname, which machines
    /// set up alike may share.
    pub name: Option<String>,
    /// Other machines, as `ssh` knows them, whose sessions the session list
    /// and the sidebar show. Hosts used with `imkr --host` are added by
    /// themselves.
    pub hosts: Vec<String>,
}

/// A shell command run in the background when `on` happens: an agent
/// becomes `blocked`, `done`, `working` or `idle`, a pane opens
/// (`pane-opened`) or closes (`pane-closed`), another window is shown
/// (`window`), a terminal attaches or detaches (`attach`, `detach`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    pub on: String,
    /// Run with `sh -c`, with `IMKR_EVENT`, `IMKR_SESSION` and, for a pane,
    /// `IMKR_HOOK_PANE`, `IMKR_HOOK_PROGRAM`, `IMKR_HOOK_STATE` and
    /// `IMKR_HOOK_WINDOW` set.
    pub run: String,
}

/// The events a `Hook` can be `on`.
pub const HOOK_EVENTS: &[&str] = &[
    "blocked",
    "done",
    "working",
    "idle",
    "pane-opened",
    "pane-closed",
    "window",
    "attach",
    "detach",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resume {
    /// Typed in and started.
    #[default]
    Auto,
    /// Typed in, Enter is the user's.
    Ask,
    Off,
}

/// How agents pick up their conversation again: by the session id their
/// hooks report (Claude Code, Codex), else the last one in the directory.
pub const DEFAULT_RESUME: &[(&str, &str)] = &[
    ("claude", "claude --resume {session}"),
    ("codex", "codex resume {session}"),
    ("opencode", "opencode --continue"),
    ("aider", "aider --restore-chat-history"),
    ("goose", "goose session --resume"),
    ("cursor-agent", "cursor-agent resume"),
];

impl Config {
    /// The resume command of every agent: the defaults with the user's own
    /// over them, empty ones left out.
    pub fn resume_commands(&self) -> BTreeMap<String, String> {
        let mut all: BTreeMap<String, String> = DEFAULT_RESUME
            .iter()
            .map(|(agent, command)| ((*agent).to_owned(), (*command).to_owned()))
            .collect();
        all.extend(self.resume_commands.clone());
        all.retain(|_, command| !command.trim().is_empty());
        all
    }

    /// Hooks on events there are none of, e.g. misspelt, as a note.
    pub fn hook_problems(&self) -> Option<String> {
        let unknown: Vec<&str> = self
            .hooks
            .iter()
            .map(|hook| hook.on.as_str())
            .filter(|on| !HOOK_EVENTS.contains(on))
            .collect();
        (!unknown.is_empty()).then(|| {
            format!(
                "hooks on unknown events: {} (known: {})",
                unknown.join(", "),
                HOOK_EVENTS.join(", ")
            )
        })
    }
}

/// A command of the user's, e.g. lazygit, started from the `Ctrl+B c` menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCommand {
    pub key: char,
    pub name: String,
    /// Run with `sh -c` in the focused pane's directory.
    pub command: String,
    /// In a floating pane, or else split off the focused one.
    #[serde(default = "yes")]
    pub float: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Notifications {
    /// A desktop notification through `notify-send`.
    #[default]
    System,
    /// OSC 9, which terminals like Ghostty and WezTerm turn into one.
    Terminal,
    Off,
}

/// The sidebar with workspaces and agents, like the rail in TUIOS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidebar {
    pub enabled: bool,
    pub side: Side,
    /// The file tree of the focused pane's project.
    pub files: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            enabled: true,
            side: Side::Right,
            files: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: theme::default_name(),
            which_key: WhichKey::default(),
            bars: Bars::default(),
            spacing: Spacing::default(),
            sidebar: Sidebar::default(),
            notifications: Notifications::default(),
            commands: vec![UserCommand {
                key: 'g',
                name: "lazygit".into(),
                command: "lazygit".into(),
                float: true,
            }],
            hooks: Vec::new(),
            extra_agents: Vec::new(),
            question_phrases: Vec::new(),
            resume: Resume::default(),
            resume_commands: BTreeMap::new(),
            images: true,
            renumber_windows: false,
            marks_on_alt: true,
            tmux_shim: false,
            sessions_on_start: false,
            name: None,
            hosts: Vec::new(),
        }
    }
}

impl Config {
    /// Parses a config file. Missing keys take their default.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// Loads the config file. A missing file means all defaults. A broken
    /// one also falls back to the defaults, with the reason returned
    /// alongside, so a typo never keeps imkr from starting.
    pub fn load() -> (Self, Option<String>) {
        let path = match path() {
            Ok(path) => path,
            Err(e) => return (Self::default(), Some(format!("{e:#}"))),
        };
        match fs::read_to_string(&path) {
            Ok(text) => match Self::parse(&text) {
                Ok(config) => (config, None),
                Err(e) => (Self::default(), Some(format!("{}: {e:#}", path.display()))),
            },
            Err(_) => (Self::default(), None),
        }
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }
}

/// `$IMKR_CONFIG` if set, otherwise `imkr/config.toml` in
/// `$XDG_CONFIG_HOME`, or in `~/.config` without it.
pub fn path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("IMKR_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("imkr").join("config.toml"))
}

/// One line in the settings menu. `step` moves to the next value, or the
/// previous one when `forward` is false.
pub struct Setting {
    pub name: &'static str,
    pub value: fn(&Config) -> String,
    pub step: fn(&mut Config, bool),
}

/// bottom → top → off → bottom, or without off for elements that must stay.
fn step_placement(p: Placement, allow_off: bool, forward: bool) -> Placement {
    let order: &[Placement] = if allow_off {
        &[Placement::Bottom, Placement::Top, Placement::Off]
    } else {
        &[Placement::Bottom, Placement::Top]
    };
    step_in(order, &p, forward)
}

/// The item after (or before) `current` in `items`, wrapping around. The
/// first item when `current` is not among them.
fn step_in<T: Clone + PartialEq>(items: &[T], current: &T, forward: bool) -> T {
    let len = items.len();
    let next = match items.iter().position(|item| item == current) {
        Some(i) if forward => (i + 1) % len,
        Some(i) => (i + len - 1) % len,
        None => 0,
    };
    items[next].clone()
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        name: "Theme",
        value: |c| match c.theme.as_str() {
            theme::FOLLOW => match theme::omarchy_current() {
                Some(name) => format!("omarchy: {name}"),
                None => theme::FOLLOW.to_owned(),
            },
            name => name.to_owned(),
        },
        step: |c, forward| c.theme = step_in(&theme::available(), &c.theme, forward),
    },
    Setting {
        name: "Sidebar",
        value: |c| if c.sidebar.enabled { "on" } else { "off" }.to_owned(),
        step: |c, _| c.sidebar.enabled = !c.sidebar.enabled,
    },
    Setting {
        name: "Sidebar files",
        value: |c| if c.sidebar.files { "on" } else { "off" }.to_owned(),
        step: |c, _| c.sidebar.files = !c.sidebar.files,
    },
    Setting {
        name: "Sidebar side",
        value: |c| {
            match c.sidebar.side {
                Side::Left => "left",
                Side::Right => "right",
            }
            .to_owned()
        },
        step: |c, _| {
            c.sidebar.side = match c.sidebar.side {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            }
        },
    },
    Setting {
        name: "Notifications",
        value: |c| {
            match c.notifications {
                Notifications::System => "system",
                Notifications::Terminal => "terminal",
                Notifications::Off => "off",
            }
            .to_owned()
        },
        step: |c, forward| {
            c.notifications = step_in(
                &[
                    Notifications::System,
                    Notifications::Terminal,
                    Notifications::Off,
                ],
                &c.notifications,
                forward,
            )
        },
    },
    Setting {
        name: "Resume agents after a restart",
        value: |c| {
            match c.resume {
                Resume::Auto => "start them",
                Resume::Ask => "type, Enter starts",
                Resume::Off => "off",
            }
            .to_owned()
        },
        step: |c, forward| {
            c.resume = step_in(
                &[Resume::Auto, Resume::Ask, Resume::Off],
                &c.resume,
                forward,
            )
        },
    },
    Setting {
        name: "tmux for agent teams (new panes)",
        value: |c| if c.tmux_shim { "on" } else { "off" }.to_owned(),
        step: |c, _| c.tmux_shim = !c.tmux_shim,
    },
    Setting {
        name: "Marks on Alt+1-9",
        value: |c| if c.marks_on_alt { "on" } else { "off" }.to_owned(),
        step: |c, _| c.marks_on_alt = !c.marks_on_alt,
    },
    Setting {
        name: "Renumber windows",
        value: |c| if c.renumber_windows { "on" } else { "off" }.to_owned(),
        step: |c, _| c.renumber_windows = !c.renumber_windows,
    },
    Setting {
        name: "Session list on start",
        value: |c| if c.sessions_on_start { "on" } else { "off" }.to_owned(),
        step: |c, _| c.sessions_on_start = !c.sessions_on_start,
    },
    Setting {
        name: "Images",
        value: |c| if c.images { "on" } else { "off" }.to_owned(),
        step: |c, _| c.images = !c.images,
    },
    Setting {
        name: "Which-key menu",
        value: |c| if c.which_key.enabled { "on" } else { "off" }.to_owned(),
        step: |c, _| c.which_key.enabled = !c.which_key.enabled,
    },
    Setting {
        name: "Which-key position",
        value: |c| {
            match c.which_key.position {
                Side::Left => "left",
                Side::Right => "right",
            }
            .to_owned()
        },
        step: |c, _| {
            c.which_key.position = match c.which_key.position {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            }
        },
    },
    Setting {
        name: "Control bar",
        value: |c| c.bars.control().name().to_owned(),
        step: |c, forward| c.bars.control = step_placement(c.bars.control(), false, forward),
    },
    Setting {
        name: "Window tabs",
        value: |c| c.bars.tabs.name().to_owned(),
        step: |c, forward| c.bars.tabs = step_placement(c.bars.tabs, true, forward),
    },
    Setting {
        name: "Path",
        value: |c| c.bars.path.name().to_owned(),
        step: |c, forward| c.bars.path = step_placement(c.bars.path, true, forward),
    },
    Setting {
        name: "Outer margin ↔",
        value: |c| format!("{} col", c.spacing.outer_x),
        step: |c, forward| c.spacing.outer_x = step_cells(c.spacing.outer_x, MAX_COLUMNS, forward),
    },
    Setting {
        name: "Outer margin ↕",
        value: |c| format!("{} row", c.spacing.outer_y),
        step: |c, forward| c.spacing.outer_y = step_cells(c.spacing.outer_y, MAX_ROWS, forward),
    },
    Setting {
        name: "Gap side by side",
        value: |c| format!("{} col", c.spacing.gap_x),
        step: |c, forward| c.spacing.gap_x = step_cells(c.spacing.gap_x, MAX_COLUMNS, forward),
    },
    Setting {
        name: "Gap above/below",
        value: |c| format!("{} row", c.spacing.gap_y),
        step: |c, forward| c.spacing.gap_y = step_cells(c.spacing.gap_y, MAX_ROWS, forward),
    },
    Setting {
        name: "Top bar",
        value: |c| c.spacing.top_bar.name().to_owned(),
        step: |c, _| c.spacing.top_bar = c.spacing.top_bar.toggled(),
    },
    Setting {
        name: "Bottom bar",
        value: |c| c.spacing.bottom_bar.name().to_owned(),
        step: |c, _| c.spacing.bottom_bar = c.spacing.bottom_bar.toggled(),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_defaults_to_none_and_parses() {
        assert_eq!(Config::default().spacing, Spacing::default());
        let config =
            Config::parse("[spacing]\nouter_x = 2\ngap_y = 1\nbottom_bar = \"inset\"\n").unwrap();
        assert_eq!(config.spacing.outer_x, 2);
        assert_eq!(config.spacing.gap_y, 1);
        assert_eq!(config.spacing.bottom_bar, BarEdge::Inset);
        assert_eq!(config.spacing.top_bar, BarEdge::Edge);
    }

    #[test]
    fn spacing_steps_wrap_around() {
        assert_eq!(step_cells(0, 2, true), 1);
        assert_eq!(step_cells(2, 2, true), 0);
        assert_eq!(step_cells(0, 2, false), 2);
    }

    #[test]
    fn hooks_come_from_the_config() {
        let config: Config = toml::from_str(
            "[[hooks]]\non = \"blocked\"\nrun = \"notify-send waiting\"\n\n[[hooks]]\non = \"blockd\"\nrun = \"true\"\n",
        )
        .unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].run, "notify-send waiting");
        assert!(config.hook_problems().unwrap().contains("blockd"));
        assert!(Config::default().hook_problems().is_none());
        // No empty `hooks = []` in a saved config.
        assert!(
            !toml::to_string_pretty(&Config::default())
                .unwrap()
                .contains("hooks")
        );
    }

    #[test]
    fn commands_come_from_the_config() {
        let config = Config::parse(
            "[[commands]]\nkey = \"t\"\nname = \"btop\"\ncommand = \"btop\"\nfloat = false\n",
        )
        .unwrap();
        assert_eq!(config.commands.len(), 1);
        assert_eq!(config.commands[0].key, 't');
        assert!(!config.commands[0].float);
        assert_eq!(Config::default().commands[0].name, "lazygit");
    }

    #[test]
    fn empty_file_means_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config, Config::default());
        assert!(config.which_key.enabled);
        assert_eq!(config.which_key.position, Side::Left);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config = Config::parse("[which_key]\nposition = \"right\"\n").unwrap();
        assert_eq!(config.which_key.position, Side::Right);
        assert!(config.which_key.enabled);
    }

    #[test]
    fn invalid_value_is_an_error() {
        assert!(Config::parse("[which_key]\nposition = \"middle\"\n").is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let mut config = Config::default();
        for setting in SETTINGS {
            (setting.step)(&mut config, true);
        }
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), config);
        assert!(text.contains("position = \"right\""));
    }

    #[test]
    fn cycling_comes_back_to_the_start() {
        for setting in SETTINGS {
            let mut config = Config::default();
            let before = (setting.value)(&config);
            (setting.step)(&mut config, true);
            assert_ne!((setting.value)(&config), before, "{}", setting.name);
            let mut steps = 1;
            while (setting.value)(&config) != before {
                (setting.step)(&mut config, true);
                steps += 1;
                assert!(steps <= 50, "{} never comes back", setting.name);
            }
        }
    }

    #[test]
    fn stepping_back_undoes_stepping_forward() {
        for setting in SETTINGS {
            let mut config = Config::default();
            let before = (setting.value)(&config);
            (setting.step)(&mut config, true);
            (setting.step)(&mut config, false);
            assert_eq!((setting.value)(&config), before, "{}", setting.name);
        }
    }

    #[test]
    fn control_bar_is_never_off() {
        let mut config = Config::parse("[bars]\ncontrol = \"off\"\n").unwrap();
        assert_eq!(config.bars.control(), Placement::Bottom);
        let setting = SETTINGS.iter().find(|s| s.name == "Control bar").unwrap();
        for _ in 0..4 {
            (setting.step)(&mut config, true);
            assert_ne!(config.bars.control, Placement::Off);
        }
    }

    #[test]
    fn bars_tell_which_sides_are_used() {
        let config = Config::parse("[bars]\ntabs = \"top\"\npath = \"off\"\n").unwrap();
        assert!(config.bars.uses(Placement::Top));
        assert!(config.bars.uses(Placement::Bottom));
        let config =
            Config::parse("[bars]\ncontrol = \"top\"\ntabs = \"top\"\npath = \"top\"\n").unwrap();
        assert!(!config.bars.uses(Placement::Bottom));
    }
}
