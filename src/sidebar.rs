//! The sidebar, like the rail in TUIOS and herdr's sidebar: every workspace
//! with what its agents are doing, every agent across all workspaces, and
//! the git branch of the focused pane and the files of its project. Rows are
//! clickable.

use std::fs;
use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::agent::AgentState;
use crate::filetree::Entry;
use crate::layout::PaneId;
use crate::theme;

/// The sidebar's width, border included.
pub const WIDTH: u16 = 30;

pub struct WorkspaceRow {
    pub number: u8,
    /// The project: the directory name of the workspace's focused pane.
    pub name: String,
    pub active: bool,
    /// The state that needs attention most among its agents.
    pub state: Option<AgentState>,
    pub agents: usize,
}

/// A plugin or popup in the sidebar.
pub struct PopupRow {
    pub title: String,
    pub shown: bool,
    /// Its program runs, shown or not.
    pub running: bool,
    /// A plugin, as opposed to a popup opened by hand.
    pub plugin: bool,
    /// Its key, e.g. `^B p p`.
    pub key: Option<String>,
    /// What its status command last printed.
    pub status: Vec<String>,
}

/// A session in the sidebar, here or on another machine.
pub struct SessionRow {
    /// The machine, `None` for this one.
    pub host: Option<String>,
    /// The machine as the user sees it, e.g. `⇄ codingmachine` or
    /// `⌂ omarchy`; `None` for the one they sit at when that is this one.
    pub machine: Option<String>,
    pub name: String,
    pub current: bool,
    /// A dot per agent: its state and its window, see `agent_dots`.
    pub dots: Vec<(AgentState, Option<u8>)>,
    /// Why the host's sessions are unknown; the row is then about the host.
    pub problem: Option<String>,
    /// Last seen on a machine that is off now: shown, but not to go to.
    pub offline: bool,
    /// Not running, but it can come back: stopped or closed.
    pub dormant: bool,
}

pub struct AgentRow {
    pub pane: PaneId,
    pub program: String,
    pub workspace: u8,
    pub dir: String,
    pub state: AgentState,
    /// How long it has been in this state.
    pub since: Duration,
    pub focused: bool,
    /// Its model and context (`opus 5.5 · 300k`), shown under it while it
    /// is focused.
    pub meta: String,
    /// What it says it does (its terminal title), shown under it.
    pub title: String,
}

/// An agent of another session, here or on another machine.
pub struct OtherAgentRow {
    /// The machine as the user sees it (`⌂ …`, `⇄ …`), `None` for this one
    /// when no other is known.
    pub machine: Option<String>,
    pub session: String,
    /// Its index among `App::other_agents`, which a click goes to.
    pub index: usize,
    pub program: String,
    pub workspace: u8,
    pub dir: String,
    pub state: AgentState,
    pub since: Duration,
    /// What it says it does, as `AgentRow::title`.
    pub title: String,
}

pub struct Data {
    pub workspaces: Vec<WorkspaceRow>,
    /// The agents of the other sessions, those that need the user first.
    pub other_agents: Vec<OtherAgentRow>,
    /// Every session, this one included, grouped by machine. Shown only
    /// when there is more than this one.
    pub sessions: Vec<SessionRow>,
    /// The popups, shown only when there are any.
    pub popups: Vec<PopupRow>,
    /// This session's agents, and this machine and session as their
    /// heading reads.
    pub agents: Vec<AgentRow>,
    pub here: Option<String>,
    pub session: String,
    pub branch: Option<String>,
    /// Changed files in the focused pane's repository, as `git status`
    /// shows them: two status letters and the path from the repository root.
    pub changes: Vec<Change>,
    /// The file tree: the root's name and what is shown of it.
    pub files: Option<(String, Vec<Entry>)>,
    /// Devices with `imkr web` of this machine open, and the pane each
    /// shows as a terminal. Shown only when there are any.
    pub phones: Vec<(String, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub status: String,
    pub path: String,
}

/// At most this many changed files are listed, the rest as a count.
const MAX_CHANGES: usize = 12;

/// What a click on a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Workspace(u8),
    /// The session at this index in `Data::sessions`.
    Session(usize),
    /// The popup at this index in `Data::popups`.
    Popup(usize),
    /// The agent at this index in `Data::other_agents`.
    OtherAgent(usize),
    Pane(PaneId),
    /// The changed file at this index in `Data::changes`.
    Change(usize),
    /// The file tree's entry at this index.
    File(usize),
}

/// The order agents are listed in: those that need the user first.
pub fn urgency(state: AgentState) -> u8 {
    match state {
        AgentState::Blocked => 0,
        AgentState::Failed => 1,
        AgentState::Done => 2,
        AgentState::Working => 3,
        AgentState::Idle => 4,
    }
}

/// Whether the terminal's font is a Nerd Font, see `Config::nerd_font`.
static NERD_FONT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_nerd_font(on: bool) {
    NERD_FONT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A dot per agent in its state's colour with the number of its window
/// inside, waiting first: `❶ ❷` is window 1 working and window 2 waiting,
/// two dots with one number are two agents in that window. Quiet ones are
/// dim; past window 10, or without a known window, the number follows a
/// plain dot. The sidebar and `Ctrl+B S` both show them.
pub fn agent_dots(dots: &[(AgentState, Option<u8>)]) -> Vec<(Style, String)> {
    let nerd = NERD_FONT.load(std::sync::atomic::Ordering::Relaxed);
    let dim = Style::new().fg(theme::current().muted);
    dots.iter()
        .map(|&(state, window)| {
            let style = match state {
                AgentState::Idle => dim,
                state => state_style(state),
            };
            let dot = match window {
                // Nerd Font `md-numeric_N_circle`, drawn round for one cell;
                // each is followed by its outline, hence two apart.
                Some(n @ 1..=9) if nerd => {
                    char::from_u32(0xF0CA0 + 2 * (u32::from(n) - 1)).map(String::from)
                }
                // ❶ to ❿: dingbat negative circled digits.
                Some(n @ 1..=10) if !nerd => {
                    char::from_u32(0x2775 + u32::from(n)).map(String::from)
                }
                _ => None,
            };
            let dot = dot.unwrap_or_else(|| match window {
                Some(n) => format!("●{n}"),
                None => "●".to_owned(),
            });
            // A space after each, not before: Ghostty draws a Nerd Font
            // symbol up to two cells wide when a space follows it.
            (style, format!("{dot} "))
        })
        .collect()
}

pub fn state_style(state: AgentState) -> Style {
    let t = theme::current();
    match state {
        AgentState::Working => Style::new().fg(t.warning),
        AgentState::Blocked => Style::new().fg(t.danger).add_modifier(Modifier::BOLD),
        AgentState::Done => Style::new().fg(t.success).add_modifier(Modifier::BOLD),
        AgentState::Idle => Style::new().fg(t.success),
        AgentState::Failed => Style::new().fg(t.magenta).add_modifier(Modifier::BOLD),
    }
}

/// The sidebar's rows for an inner width of `width`, each with what a click
/// on it does.
pub fn lines(data: &Data, width: u16) -> Vec<(Line<'static>, Option<Target>)> {
    let width = usize::from(width);
    let t = theme::current();
    // Each section has a colour of its own, workspaces the colour they have
    // everywhere.
    let heading = |color| Style::new().fg(color).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.muted);
    let mut out = Vec::new();

    out.push((Line::styled("Windows", heading(t.blue)), None));
    for ws in &data.workspaces {
        let marker = if ws.active { "▸ " } else { "  " };
        let mut right = String::new();
        if let Some(state) = ws.state {
            right = format!("{} {}", state.symbol(), ws.agents);
        }
        let left = format!("{marker}{} {}", ws.number, ws.name);
        let room = width.saturating_sub(right.chars().count() + 1);
        let left = fit(&left, room);
        let pad = width.saturating_sub(left.chars().count() + right.chars().count());
        let style = if ws.active {
            Style::new()
                .fg(t.workspace(ws.number))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.workspace(ws.number))
        };
        let mut spans = vec![Span::styled(left, style), Span::raw(" ".repeat(pad))];
        if let Some(state) = ws.state {
            spans.push(Span::styled(right, state_style(state)));
        }
        out.push((Line::from(spans), Some(Target::Workspace(ws.number))));
    }

    if !data.popups.is_empty() {
        let plugins = data.popups.iter().any(|p| p.plugin);
        out.push((Line::default(), None));
        out.push((
            Line::styled(
                if plugins { "Plugins" } else { "Popups" },
                heading(t.accent),
            ),
            None,
        ));
        for (i, popup) in data.popups.iter().enumerate() {
            // ◆ runs, ◇ starts on a click.
            let symbol = if popup.running { "◆" } else { "◇" };
            let marker = if popup.shown { "▸" } else { " " };
            let right = popup.key.clone().unwrap_or_default();
            let left = fit(
                &format!("{marker} {symbol} {}", popup.title),
                width.saturating_sub(right.chars().count() + 1),
            );
            let pad = width.saturating_sub(left.chars().count() + right.chars().count());
            let style = if popup.shown {
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.accent)
            };
            let line = Line::from(vec![
                Span::styled(left, style),
                Span::raw(" ".repeat(pad)),
                Span::styled(right, dim),
            ]);
            out.push((line, Some(Target::Popup(i))));
            for status in &popup.status {
                out.push((
                    Line::styled(fit(&format!("    {status}"), width), dim),
                    Some(Target::Popup(i)),
                ));
            }
        }
    }

    if data.sessions.len() > 1 || data.sessions.iter().any(|s| s.host.is_some()) {
        out.push((Line::default(), None));
        out.push((Line::styled("Sessions", heading(t.cyan)), None));
        let mut machine: Option<&str> = None;
        for (i, session) in data.sessions.iter().enumerate() {
            if session.machine.as_deref() != machine {
                machine = session.machine.as_deref();
                if let Some(name) = machine {
                    out.push((
                        Line::styled(fit(name, width), Style::new().fg(t.cyan)),
                        None,
                    ));
                }
            }
            let indent = if session.machine.is_some() { "  " } else { "" };
            if let Some(problem) = &session.problem {
                out.push((
                    Line::styled(fit(&format!("{indent}  {problem}"), width), dim),
                    None,
                ));
                continue;
            }
            let marker = if session.current {
                "▸ "
            } else if session.dormant {
                "◌ "
            } else {
                "  "
            };
            let parts = agent_dots(&session.dots);
            let right: String = parts.iter().map(|(_, part)| part.as_str()).collect();
            let left = fit(
                &format!("{indent}{marker}{}", session.name),
                width.saturating_sub(right.chars().count() + 1),
            );
            let pad = width.saturating_sub(left.chars().count() + right.chars().count());
            let style = if session.offline || session.dormant {
                dim
            } else if session.current {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            let mut spans = vec![Span::styled(left, style), Span::raw(" ".repeat(pad))];
            for (style, part) in parts {
                spans.push(Span::styled(part, style));
            }
            let target = (!session.current && !session.offline).then_some(Target::Session(i));
            out.push((Line::from(spans), target));
        }
    }

    out.push((Line::default(), None));
    out.push((Line::styled("Agents", heading(t.magenta)), None));
    if data.agents.is_empty() && data.other_agents.is_empty() {
        out.push((Line::styled("  none running", dim), None));
    }
    // One line per agent: its state, its window (number and name, else
    // directory) before its program, as agents are mostly all `claude`,
    // and how long it has been so.
    let agent_line = |indent: &str,
                      state: AgentState,
                      program: &str,
                      workspace: u8,
                      dir: &str,
                      since: Duration,
                      focused: bool| {
        let age = age(since);
        let symbol = format!("{indent}{} ", state.symbol());
        let number = format!("{workspace} ");
        let used = symbol.chars().count()
            + number.chars().count()
            + program.chars().count()
            + age.chars().count()
            + 1;
        // The window's name gives way first, then the program.
        let label = if dir.is_empty() {
            String::new()
        } else {
            fit(&format!("{dir} › "), width.saturating_sub(used))
        };
        let program = fit(
            program,
            width.saturating_sub(used - program.chars().count() + label.chars().count()),
        );
        let used = symbol.chars().count()
            + number.chars().count()
            + label.chars().count()
            + program.chars().count()
            + age.chars().count();
        let pad = width.saturating_sub(used);
        let name_style = if focused {
            state_style(state).add_modifier(Modifier::UNDERLINED)
        } else {
            state_style(state)
        };
        Line::from(vec![
            Span::styled(symbol, state_style(state)),
            Span::styled(
                format!("{number}{label}"),
                Style::new().fg(t.workspace(workspace)),
            ),
            Span::styled(program, name_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(age, dim),
        ])
    };
    if data.other_agents.is_empty() {
        // Just this session: no headings needed.
        for agent in &data.agents {
            let line = agent_line(
                "",
                agent.state,
                &agent.program,
                agent.workspace,
                &agent.dir,
                agent.since,
                agent.focused,
            );
            out.push((line, Some(Target::Pane(agent.pane))));
            if !agent.title.is_empty() {
                let title = Line::styled(fit(&format!("  {}", agent.title), width), dim);
                out.push((title, Some(Target::Pane(agent.pane))));
            }
            if agent.focused && !agent.meta.is_empty() {
                let meta = Line::styled(fit(&format!("  {}", agent.meta), width), dim);
                out.push((meta, Some(Target::Pane(agent.pane))));
            }
        }
    } else {
        // The same on every machine: machine, then session, then its
        // agents; the user's own machine first, the rest by name, and this
        // session marked rather than moved to the top.
        struct Group<'a> {
            machine: Option<&'a str>,
            session: &'a str,
            current: bool,
            lines: Vec<(Line<'static>, Target)>,
        }
        let mut groups: Vec<Group> = Vec::new();
        if !data.agents.is_empty() {
            groups.push(Group {
                machine: data.here.as_deref(),
                session: &data.session,
                current: true,
                lines: data
                    .agents
                    .iter()
                    .flat_map(|a| {
                        let line = agent_line(
                            "    ",
                            a.state,
                            &a.program,
                            a.workspace,
                            &a.dir,
                            a.since,
                            a.focused,
                        );
                        let title = (!a.title.is_empty()).then(|| {
                            let text = fit(&format!("      {}", a.title), width);
                            (Line::styled(text, dim), Target::Pane(a.pane))
                        });
                        let meta = (a.focused && !a.meta.is_empty()).then(|| {
                            let text = fit(&format!("      {}", a.meta), width);
                            (Line::styled(text, dim), Target::Pane(a.pane))
                        });
                        std::iter::once((line, Target::Pane(a.pane)))
                            .chain(title)
                            .chain(meta)
                    })
                    .collect(),
            });
        }
        for agent in &data.other_agents {
            let line = agent_line(
                "    ",
                agent.state,
                &agent.program,
                agent.workspace,
                &agent.dir,
                agent.since,
                false,
            );
            let mut lines = vec![(line, Target::OtherAgent(agent.index))];
            if !agent.title.is_empty() {
                let text = fit(&format!("      {}", agent.title), width);
                lines.push((Line::styled(text, dim), Target::OtherAgent(agent.index)));
            }
            let found = groups.iter_mut().find(|g| {
                !g.current && g.machine == agent.machine.as_deref() && g.session == agent.session
            });
            match found {
                Some(group) => group.lines.extend(lines),
                None => groups.push(Group {
                    machine: agent.machine.as_deref(),
                    session: &agent.session,
                    current: false,
                    lines,
                }),
            }
        }
        let rank = |m: Option<&str>| !(m.is_none() || m.is_some_and(|m| m.starts_with('⌂')));
        groups.sort_by(|a, b| {
            (rank(a.machine), a.machine, a.session).cmp(&(rank(b.machine), b.machine, b.session))
        });
        let mut machine: Option<Option<&str>> = None;
        for group in groups {
            if machine != Some(group.machine) {
                machine = Some(group.machine);
                if let Some(name) = group.machine {
                    out.push((
                        Line::styled(fit(name, width), Style::new().fg(t.cyan)),
                        None,
                    ));
                }
            }
            let marker = if group.current { "▸ " } else { "  " };
            let style = if group.current {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            out.push((
                Line::styled(fit(&format!("  {marker}{}", group.session), width), style),
                None,
            ));
            for (line, target) in group.lines {
                out.push((line, Some(target)));
            }
        }
    }

    if !data.phones.is_empty() {
        out.push((Line::default(), None));
        out.push((Line::styled("Phones", heading(t.success)), None));
        for (device, viewing) in &data.phones {
            let mut spans = vec![Span::raw(fit(&format!("  {device}"), width))];
            if let Some(viewing) = viewing {
                let room = width.saturating_sub(device.chars().count() + 2);
                spans.push(Span::styled(fit(&format!(" · {viewing}"), room), dim));
            }
            out.push((Line::from(spans), None));
        }
    }

    if let Some(branch) = &data.branch {
        out.push((Line::default(), None));
        out.push((
            Line::from(vec![
                Span::styled("Git ", heading(t.orange)),
                Span::styled(
                    fit(&format!(" {branch}"), width.saturating_sub(4)),
                    Style::new().fg(t.accent),
                ),
            ]),
            None,
        ));
        for (i, change) in data.changes.iter().take(MAX_CHANGES).enumerate() {
            let color = match change.status.trim() {
                "??" => t.success,
                s if s.contains('D') => t.danger,
                _ => t.warning,
            };
            let status = format!("{:>2} ", change.status.trim());
            let path = fit_end(&change.path, width.saturating_sub(3));
            out.push((
                Line::from(vec![
                    Span::styled(status, Style::new().fg(color).add_modifier(Modifier::BOLD)),
                    Span::raw(path),
                ]),
                Some(Target::Change(i)),
            ));
        }
        if data.changes.len() > MAX_CHANGES {
            out.push((
                Line::styled(
                    format!("   +{} more", data.changes.len() - MAX_CHANGES),
                    dim,
                ),
                None,
            ));
        }
    }

    if let Some((root, entries)) = &data.files {
        out.push((Line::default(), None));
        out.push((
            Line::from(vec![
                Span::styled("Files ", heading(t.cyan)),
                Span::styled(fit(root, width.saturating_sub(6)), dim),
            ]),
            None,
        ));
        if entries.is_empty() {
            out.push((Line::styled("  empty", dim), None));
        }
        for (i, entry) in entries.iter().enumerate() {
            let indent = "  ".repeat(usize::from(entry.depth));
            let (marker, name, style) = if entry.dir {
                let marker = if entry.open { "▾ " } else { "▸ " };
                (marker, format!("{}/", entry.name), Style::new().fg(t.blue))
            } else if entry.changed {
                ("  ", entry.name.clone(), Style::new().fg(t.warning))
            } else {
                ("  ", entry.name.clone(), Style::new())
            };
            let room = width.saturating_sub(indent.chars().count() + 2);
            out.push((
                Line::from(vec![
                    Span::raw(indent),
                    Span::styled(marker, dim),
                    Span::styled(fit(&name, room), style),
                ]),
                Some(Target::File(i)),
            ));
        }
    }
    out
}

/// For each row, its place among the rows the keyboard selects. A row
/// under one with the same target (an agent's title, its model) belongs
/// to that one: it is selected along with it, not on its own.
fn selection_index(lines: &[(Line, Option<Target>)]) -> Vec<Option<usize>> {
    let mut next = 0;
    let mut previous: Option<Target> = None;
    lines
        .iter()
        .map(|(_, target)| {
            let target = (*target)?;
            if previous != Some(target) {
                next += 1;
            }
            previous = Some(target);
            Some(next - 1)
        })
        .collect()
}

/// How far to scroll a sidebar of `height` rows so the `selected` clickable
/// row shows, starting from `scroll`.
pub fn scroll_to_show(data: &Data, height: u16, selected: usize, scroll: usize) -> usize {
    let height = usize::from(height.max(1));
    let Some(row) = selection_index(&lines(data, WIDTH))
        .iter()
        .position(|index| *index == Some(selected))
    else {
        return scroll;
    };
    if row < scroll {
        // One more, so the heading of the first section shows.
        row.saturating_sub(1)
    } else if row >= scroll + height {
        row + 1 - height
    } else {
        scroll
    }
}

/// The furthest the sidebar can scroll with `height` rows.
pub fn max_scroll(data: &Data, height: u16) -> usize {
    lines(data, WIDTH).len().saturating_sub(usize::from(height))
}

/// `text` cut to `room` characters keeping its end, for paths.
fn fit_end(text: &str, room: usize) -> String {
    let len = text.chars().count();
    if len <= room {
        return text.to_owned();
    }
    if room == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(len - (room - 1)).collect();
    format!("…{tail}")
}

/// Parses `git status --porcelain` output.
pub fn parse_changes(porcelain: &str) -> Vec<Change> {
    porcelain
        .lines()
        .filter(|line| line.len() > 3)
        .map(|line| {
            let path = &line[3..];
            // A rename shows as `old -> new`, the new name is the file now.
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            Change {
                status: line[..2].to_owned(),
                path: path.trim_matches('"').to_owned(),
            }
        })
        .collect()
}

/// Draws the sidebar. `selected` is the index of the highlighted row among
/// the clickable ones, when the sidebar has the keyboard.
pub fn draw(frame: &mut Frame, area: Rect, data: &Data, selected: Option<usize>, scroll: usize) {
    let border = match selected {
        Some(_) => theme::current().accent,
        None => theme::current().subtle,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Span::styled(
            " ⬢ imkr ",
            Style::new()
                .fg(theme::current().accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    let all = lines(data, inner.width);
    let indices = selection_index(&all);
    let rows: Vec<Line> = all
        .into_iter()
        .zip(indices)
        .map(|((line, _), index)| {
            if index.is_some() && index == selected {
                let width = usize::from(inner.width).saturating_sub(line.width());
                let mut line = line;
                line.spans.push(Span::raw(" ".repeat(width)));
                line.patch_style(Style::new().bg(theme::current().subtle))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Clear, area);
    let scroll = u16::try_from(scroll).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(rows).block(block).scroll((scroll, 0)), area);
}

/// The clickable rows' targets, top to bottom.
pub fn targets(data: &Data) -> Vec<Target> {
    let mut targets: Vec<Target> = lines(data, WIDTH)
        .into_iter()
        .filter_map(|(_, target)| target)
        .collect();
    // Rows under an agent with the same target are part of it.
    targets.dedup();
    targets
}

/// What the row at `pos` does when clicked, for a sidebar drawn at `area`.
pub fn target_at(data: &Data, area: Rect, pos: Position, scroll: usize) -> Option<Target> {
    let inner = Block::bordered().inner(area);
    if !inner.contains(pos) {
        return None;
    }
    let row = usize::from(pos.y - inner.y) + scroll;
    lines(data, inner.width)
        .get(row)
        .and_then(|(_, target)| *target)
}

/// `text` cut to `room` characters with an ellipsis.
fn fit(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_owned();
    }
    if room == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(room - 1).collect();
    cut.push('…');
    cut
}

/// A short age like `12s`, `3m` or `2h`.
pub fn age(since: Duration) -> String {
    let secs = since.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The branch checked out in the repository containing `dir`, or the short
/// commit when detached. Reads `.git` directly instead of running git, since
/// this runs on every redraw.
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut current = Some(dir);
    while let Some(d) = current {
        let git = d.join(".git");
        let head = if git.is_dir() {
            fs::read_to_string(git.join("HEAD")).ok()
        } else if git.is_file() {
            // A worktree: `.git` names the real git directory.
            let pointer = fs::read_to_string(&git).ok()?;
            let gitdir = pointer.trim().strip_prefix("gitdir: ")?;
            fs::read_to_string(d.join(gitdir).join("HEAD")).ok()
        } else {
            None
        };
        if let Some(head) = head {
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_owned(),
                None => head.chars().take(7).collect(),
            });
        }
        current = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Data {
        Data {
            phones: Vec::new(),
            sessions: Vec::new(),
            popups: Vec::new(),
            other_agents: Vec::new(),
            here: None,
            session: "default".into(),
            workspaces: vec![
                WorkspaceRow {
                    number: 1,
                    name: "imkr".into(),
                    active: true,
                    state: Some(AgentState::Working),
                    agents: 1,
                },
                WorkspaceRow {
                    number: 2,
                    name: "a-very-long-project-name-indeed".into(),
                    active: false,
                    state: Some(AgentState::Blocked),
                    agents: 2,
                },
            ],
            agents: vec![AgentRow {
                pane: 7,
                program: "claude".into(),
                workspace: 2,
                dir: "api".into(),
                state: AgentState::Blocked,
                since: Duration::from_secs(185),
                focused: false,
                meta: "opus 5.5 · 300k".into(),
                title: "Fix the login".into(),
            }],
            branch: Some("main".into()),
            changes: vec![Change {
                status: " M".into(),
                path: "src/app.rs".into(),
            }],
            files: None,
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn rows_fit_and_click_to_their_target() {
        let rows = lines(&data(), 28);
        for (line, _) in &rows {
            assert!(line.width() <= 28, "{:?} too wide", text(line));
        }
        let texts: Vec<String> = rows.iter().map(|(l, _)| text(l)).collect();
        assert!(texts[1].starts_with("▸ 1 imkr") && texts[1].ends_with("● 1"));
        assert!(texts[2].contains('…') && texts[2].ends_with("◆ 2"));
        assert_eq!(rows[2].1, Some(Target::Workspace(2)));
        let agent = texts
            .iter()
            .position(|t| t.starts_with("◆ 2 api › claude"))
            .unwrap();
        assert!(texts[agent].ends_with("3m"));
        assert_eq!(rows[agent].1, Some(Target::Pane(7)));
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("Git") && t.contains("main"))
        );
        assert_eq!(texts.last().unwrap(), " M src/app.rs");
        assert_eq!(rows.last().unwrap().1, Some(Target::Change(0)));
    }

    #[test]
    fn click_positions_map_to_rows() {
        let area = Rect::new(70, 0, 30, 20);
        let data = data();
        // Row 0 is the heading, row 1 workspace 1, inside the border.
        assert_eq!(
            target_at(&data, area, Position::new(72, 2), 0),
            Some(Target::Workspace(1))
        );
        assert_eq!(target_at(&data, area, Position::new(72, 1), 0), None);
        assert_eq!(target_at(&data, area, Position::new(70, 2), 0), None);
        // Scrolled down one row, the same place is workspace 2.
        assert_eq!(
            target_at(&data, area, Position::new(72, 2), 1),
            Some(Target::Workspace(2))
        );
    }

    #[test]
    fn targets_list_workspaces_then_agents() {
        assert_eq!(
            targets(&data()),
            vec![
                Target::Workspace(1),
                Target::Workspace(2),
                Target::Pane(7),
                Target::Change(0)
            ]
        );
    }

    #[test]
    fn parses_git_status() {
        let changes =
            parse_changes(" M src/app.rs\n?? notes.md\nR  old.rs -> new.rs\nD  gone.rs\n");
        let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/app.rs", "notes.md", "new.rs", "gone.rs"]);
        assert_eq!(changes[1].status, "??");
    }

    #[test]
    fn ages() {
        assert_eq!(age(Duration::from_secs(12)), "12s");
        assert_eq!(age(Duration::from_secs(185)), "3m");
        assert_eq!(age(Duration::from_secs(7300)), "2h");
    }

    #[test]
    fn finds_the_branch_of_this_repository() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(git_branch(&here).is_some());
        assert_eq!(git_branch(Path::new("/")), None);
    }

    #[test]
    fn file_tree_rows_indent_and_scroll_into_view() {
        let entry = |name: &str, depth, dir, open| Entry {
            path: name.into(),
            name: name.into(),
            depth,
            dir,
            open,
            changed: false,
        };
        let mut data = data();
        data.files = Some((
            "imkr".into(),
            vec![
                entry("src", 0, true, true),
                entry("app.rs", 1, false, false),
            ],
        ));
        let rows = lines(&data, 28);
        let texts: Vec<String> = rows.iter().map(|(l, _)| text(l)).collect();
        assert!(texts.iter().any(|t| t == "Files imkr"));
        assert!(texts.iter().any(|t| t == "▾ src/"));
        assert_eq!(texts.last().unwrap(), "    app.rs");
        assert_eq!(rows.last().unwrap().1, Some(Target::File(1)));
        // The last clickable row, in a sidebar of 5 rows, needs scrolling.
        let last = targets(&data).len() - 1;
        let scroll = scroll_to_show(&data, 5, last, 0);
        assert_eq!(scroll, rows.len() - 5);
        assert_eq!(scroll_to_show(&data, 5, 0, scroll), 0);
    }

    #[test]
    fn sessions_are_grouped_by_machine_and_clickable() {
        let session = |host: Option<&str>, name: &str, current| SessionRow {
            host: host.map(str::to_owned),
            machine: host.map(|h| format!("⇄ {h}")),
            name: name.into(),
            current,
            dots: Vec::new(),
            problem: None,
            offline: false,
            dormant: false,
        };
        let mut data = data();
        data.sessions = vec![
            // One agent done, one still working: both show, working first.
            SessionRow {
                dots: vec![(AgentState::Working, Some(1)), (AgentState::Done, Some(2))],
                ..session(None, "default", true)
            },
            session(Some("codingmachine"), "brain", false),
            SessionRow {
                problem: Some("not reachable".into()),
                ..session(Some("kiserver"), "", false)
            },
        ];
        let rows: Vec<(String, Option<Target>)> = lines(&data, 28)
            .into_iter()
            .map(|(line, target)| (line.to_string(), target))
            .collect();
        let find = |text: &str| rows.iter().find(|(line, _)| line.contains(text)).cloned();
        assert!(find("Sessions").is_some());
        // This session is not a target, the other machine's is.
        assert_eq!(find("▸ default").unwrap().1, None);
        // A dot per agent with its window inside: 1 working, 2 done.
        assert!(find("▸ default").unwrap().0.ends_with("❶ ❷ "));
        let dots = [
            (AgentState::Working, Some(3)),
            (AgentState::Working, Some(3)),
            (AgentState::Done, Some(12)),
            (AgentState::Idle, None),
        ];
        let dots: Vec<String> = agent_dots(&dots).into_iter().map(|(_, d)| d).collect();
        assert_eq!(dots, ["❸ ", "❸ ", "●12 ", "● "]);
        // With a Nerd Font, round circles: 1 is U+F0CA0, 3 two each later.
        set_nerd_font(true);
        let round: Vec<String> = agent_dots(&[(AgentState::Working, Some(3))])
            .into_iter()
            .map(|(_, d)| d)
            .collect();
        set_nerd_font(false);
        assert_eq!(round, ["\u{F0CA4} "]);
        assert!(find("⇄ codingmachine").is_some());
        assert_eq!(find("brain").unwrap().1, Some(Target::Session(1)));
        assert_eq!(find("not reachable").unwrap().1, None);
    }

    #[test]
    fn agents_are_grouped_the_same_on_every_machine() {
        let other = |machine: &str, session: &str, index| OtherAgentRow {
            machine: Some(machine.into()),
            session: session.into(),
            index,
            program: "claude".into(),
            workspace: 1,
            dir: "d".into(),
            state: AgentState::Idle,
            since: Duration::from_secs(5),
            title: String::new(),
        };
        let mut data = data();
        // On codingmachine, in peppermint-tui, the user sitting at omarchy.
        data.here = Some("⇄ codingmachine".into());
        data.session = "peppermint-tui".into();
        data.other_agents = vec![
            other("⇄ codingmachine", "peppermint-verwaltung", 0),
            other("⌂ omarchy", "imkr", 1),
        ];
        let text: Vec<String> = lines(&data, 30)
            .into_iter()
            .map(|(line, _)| line.to_string())
            .collect();
        let at = |needle: &str| text.iter().position(|l| l.contains(needle)).unwrap();
        // The user's machine first, then the others; this session marked in
        // its place, not on top.
        assert!(at("⌂ omarchy") < at("⇄ codingmachine"));
        assert!(at("  imkr") < at("▸ peppermint-tui"));
        assert!(at("▸ peppermint-tui") < at("peppermint-verwaltung"));
    }
}
