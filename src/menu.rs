//! The key menus drawn from the binding table: the which-key popup shown
//! while the prefix is active, and the full help overlay.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::bindings::{Binding, Group, Menu, PREFIX_LABEL, in_group};
use crate::config::{Config, SETTINGS, Side};
use crate::theme;

/// Space between the key column and the description, and between groups.
const GAP: usize = 2;
const GROUP_GAP: usize = 4;

/// The which-key popup in the bottom corner of `area` on `side`.
/// `commands` are the user's own, as (key, name), listed in their menu.
pub fn draw_which_key(
    frame: &mut Frame,
    area: Rect,
    side: Side,
    menu: Menu,
    commands: &[(char, String)],
    selected: Option<usize>,
) {
    let area = inset(area);
    // Border and one column of padding on each side.
    let max_width = usize::from(area.width).saturating_sub(4);
    let mut lines = match menu {
        Menu::Commands => command_lines(
            Group::Commands,
            commands,
            "none yet, add [[commands]] to config.toml",
            selected,
        ),
        Menu::Plugins => command_lines(
            Group::Plugins,
            commands,
            "none yet, add plugins/NAME.toml next to config.toml",
            selected,
        ),
        menu => groups(menu.groups(), max_width, selected),
    };
    // Rare keys of the main menu in one quiet line at the bottom.
    // It wraps at the width of the groups above, so it never widens the box.
    if menu == Menu::Root {
        let grid = lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(0)
            .min(max_width);
        lines.push(Line::default());
        lines.extend(footer_lines(grid));
    }

    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let height = lines.len() as u16 + 2;
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = match side {
        Side::Left => area.x,
        Side::Right => area.right() - width,
    };
    let popup = Rect::new(x, area.bottom() - height, width, height);

    let title = match menu {
        Menu::Root => format!(" ⬢ {PREFIX_LABEL} "),
        menu => format!(" ⬢ {PREFIX_LABEL} {} ", menu.path()),
    };
    draw_box(frame, popup, title, 1, lines);
}

/// The copy-mode keys in the bottom corner of `area` on `side`, as a
/// which-key board: the groups side by side when they fit, else stacked.
/// It goes to the top corner while the cursor row `cursor` would be under
/// it.
pub fn draw_copy_keys(frame: &mut Frame, area: Rect, side: Side, cursor: Option<u16>) {
    let area = inset(area);
    let max_width = usize::from(area.width).saturating_sub(4);
    let lines = copy_lines(max_width);
    let width = (lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let x = match side {
        Side::Left => area.x,
        Side::Right => area.right() - width,
    };
    let mut y = area.bottom() - height;
    if cursor.is_some_and(|row| row >= y) {
        y = area.y;
    }
    let popup = Rect::new(x, y, width, height);
    draw_box(frame, popup, " ⬢ copy mode ".into(), 1, lines);
}

/// The groups of `COPY_KEYS`, as many side by side as fit `max_width`.
fn copy_lines(max_width: usize) -> Vec<Line<'static>> {
    let columns: Vec<Vec<Line>> = crate::bindings::COPY_KEYS
        .iter()
        .map(|(title, keys)| {
            let key_width = keys
                .iter()
                .map(|(k, _)| k.chars().count())
                .max()
                .unwrap_or(0);
            let mut lines = vec![Line::styled(
                (*title).to_owned(),
                Style::new()
                    .fg(theme::current().magenta)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )];
            lines.extend(keys.iter().map(|(key, what)| {
                Line::from(vec![
                    Span::styled(*key, key_style()),
                    Span::raw(" ".repeat(key_width - key.chars().count() + GAP)),
                    Span::raw(*what),
                ])
            }));
            lines
        })
        .collect();
    grid(columns, max_width)
}

/// The full key reference in the middle of `area`, scrolled down by
/// `scroll` lines when it is taller than the screen.
pub fn draw_help(frame: &mut Frame, area: Rect, scroll: u16) {
    let area = inset(area);
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut lines = vec![
        Line::from(vec![
            Span::raw("Press "),
            Span::styled(PREFIX_LABEL, key_style()),
            Span::raw(", then one of these keys:"),
        ]),
        Line::default(),
    ];
    // Border and two columns of padding on each side.
    let max_width = usize::from(area.width).saturating_sub(6);
    let mut root = Menu::Root.groups().to_vec();
    root.push(Group::More);
    lines.extend(groups(&root, max_width, None));
    let submenus: Vec<Group> = Menu::ALL
        .iter()
        .filter(|m| **m != Menu::Root)
        .flat_map(|m| m.groups().iter().copied())
        .collect();
    lines.push(Line::default());
    lines.extend(groups(&submenus, max_width, None));
    // The lists take keys of their own once they are open.
    for (title, key, keys) in crate::bindings::LISTS {
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                (*title).to_owned(),
                Style::new()
                    .fg(Group::Sessions.color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {PREFIX_LABEL} {key}, then in the list"), dim),
        ]));
        lines.extend(key_lines(keys, max_width));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            "Copy mode",
            Style::new()
                .fg(theme::current().magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {PREFIX_LABEL} y, then"), dim),
    ]));
    lines.extend(copy_lines(max_width));
    lines.extend([
        Line::default(),
        Line::styled("Focus and resize repeat: for a moment afterwards,", dim),
        Line::styled("arrow keys work without the prefix.", dim),
        Line::styled("`imkr keys` prints this list in a shell.", dim),
        Line::default(),
        Line::from(vec![
            Span::styled("j k", key_style()),
            Span::raw(" scroll · "),
            Span::styled("any other key", key_style()),
            Span::raw(" close"),
        ]),
    ]);

    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6;
    let height = lines.len() as u16 + 2;
    let width = width.min(area.width);
    let height = height.min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );

    // Scroll no further than the last line reaching the bottom.
    let overflow = (lines.len() as u16 + 2).saturating_sub(height);
    draw_box_scrolled(
        frame,
        popup,
        " ⬢ imkr keys ".into(),
        2,
        lines,
        scroll.min(overflow),
    );
}

/// `area` minus one cell on each side, so popups leave the pane borders
/// around them visible.
fn inset(area: Rect) -> Rect {
    area.inner(Margin::new(1, 1))
}

fn draw_box(
    frame: &mut Frame,
    popup: Rect,
    title: String,
    padding: u16,
    lines: Vec<Line<'static>>,
) {
    draw_box_scrolled(frame, popup, title, padding, lines, 0);
}

fn draw_box_scrolled(
    frame: &mut Frame,
    popup: Rect,
    title: String,
    padding: u16,
    lines: Vec<Line<'static>>,
    scroll: u16,
) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::current().accent))
        .title(Span::styled(title, key_style()))
        .padding(Padding::horizontal(padding));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        popup,
    );
}

/// All groups, side by side when they fit into `max_width`, stacked
/// otherwise.
/// The groups in a grid: as many side by side as fit into `max_width`,
/// the rest in further rows.
/// `selected`: the entry picked with the arrows, counted over the groups.
fn groups(groups: &[Group], max_width: usize, selected: Option<usize>) -> Vec<Line<'static>> {
    let mut before = 0;
    let columns: Vec<Vec<Line>> = groups
        .iter()
        .map(|&g| {
            let here = selected.and_then(|s| s.checked_sub(before));
            before += in_group(g).filter(|b| crate::bindings::pickable(b)).count();
            group_lines(g, here)
        })
        .collect();
    grid(columns, max_width)
}

/// `columns` in a grid: as many side by side as fit into `max_width`,
/// the rest in further rows.
fn grid(columns: Vec<Vec<Line<'static>>>, max_width: usize) -> Vec<Line<'static>> {
    let widths: Vec<usize> = columns
        .iter()
        .map(|lines| lines.iter().map(Line::width).max().unwrap_or(0))
        .collect();
    let fits = |per_row: usize| {
        widths
            .chunks(per_row)
            .all(|row| row.iter().sum::<usize>() + GROUP_GAP * (row.len() - 1) <= max_width)
    };
    let per_row = (1..=columns.len()).rev().find(|&n| fits(n)).unwrap_or(1);
    let mut lines = Vec::new();
    for (i, (row, row_widths)) in columns
        .chunks(per_row)
        .zip(widths.chunks(per_row))
        .enumerate()
    {
        if i > 0 {
            lines.push(Line::default());
        }
        lines.extend(beside(row.to_vec(), row_widths));
    }
    lines
}

/// The main menu's rare keys as `key what · key what`, wrapped to
/// `max_width`.
fn footer_lines(max_width: usize) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme::current().muted);
    let items: Vec<(String, String)> = in_group(Group::More)
        .map(|b| (b.label.to_owned(), b.description.to_owned()))
        .collect();
    let mut lines = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut width = 0;
    for (label, description) in items {
        let item_width = label.chars().count() + 1 + description.chars().count();
        if width > 0 && width + 3 + item_width > max_width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            width = 0;
        }
        if width > 0 {
            spans.push(Span::styled(" · ", muted));
            width += 3;
        }
        spans.push(Span::styled(label, key_style()));
        spans.push(Span::styled(format!(" {description}"), muted));
        width += item_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// An entry of the quit menu.
#[derive(Clone, Debug)]
pub struct MenuItem {
    pub key: char,
    pub title: String,
    pub hint: String,
    /// Destroys something, shown in red.
    pub danger: bool,
    /// Set apart from the items before it, e.g. starting something new
    /// after a list of what runs: a line above, quieter, and out of the
    /// arrow keys' way.
    pub apart: bool,
    /// A colour of its own, e.g. an agent's state as the sidebar shows it.
    pub color: Option<ratatui::style::Color>,
    /// The group it belongs to, e.g. its machine: a heading goes above the
    /// first item of each group. Groups follow each other in the list.
    pub group: Option<String>,
    /// Coloured marks before the hint, e.g. a dot per agent of a session in
    /// its state's colour.
    pub marks: Vec<(ratatui::style::Color, String)>,
}

/// The quit menu in the middle of `area`, `selected` highlighted.
pub fn draw_quit_menu(frame: &mut Frame, area: Rect, items: &[MenuItem], selected: usize) {
    draw_choices(
        frame,
        area,
        " ⬢ Quit imkr? ",
        items,
        selected,
        &[
            ("↑↓", "select"),
            ("Enter", "or letter choose"),
            ("Esc", "cancel"),
        ],
    );
}

/// A menu of `items` in the middle of `area`, `selected` highlighted, with
/// every key it takes besides the items' own listed below as `keys`.
pub fn draw_choices(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: &[MenuItem],
    selected: usize,
    keys: &[(&str, &str)],
) {
    let area = inset(area);
    let title_width = items
        .iter()
        .map(|i| i.title.chars().count())
        .max()
        .unwrap_or(0);
    let marks_width = |i: &MenuItem| {
        let n: usize = i.marks.iter().map(|(_, m)| m.chars().count()).sum();
        let space = i.marks.last().is_some_and(|(_, m)| !m.ends_with(' '));
        n + usize::from(space)
    };
    let hint_width = items
        .iter()
        .map(|i| marks_width(i) + i.hint.chars().count())
        .max()
        .unwrap_or(0);
    let row_width = 3 + GAP + title_width + GAP + hint_width;

    let mut lines = Vec::new();
    let mut group: Option<&str> = None;
    for (i, item) in items.iter().enumerate() {
        if let Some(name) = item.group.as_deref()
            && group != Some(name)
        {
            group = Some(name);
            if i > 0 {
                lines.push(Line::default());
            }
            lines.push(Line::styled(
                format!(" {name}"),
                Style::new()
                    .fg(theme::current().cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if item.apart && i > 0 {
            lines.push(Line::styled(
                "─".repeat(row_width),
                Style::new().fg(theme::current().subtle),
            ));
        }
        let color = if let Some(color) = item.color {
            color
        } else if item.danger {
            theme::current().danger
        } else if item.apart {
            theme::current().muted
        } else {
            theme::current().accent
        };
        let title_pad = title_width - item.title.chars().count() + GAP;
        let mut spans = vec![
            Span::styled(
                format!(" {} ", item.key),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(GAP)),
            Span::styled(
                item.title.clone(),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(title_pad)),
        ];
        for (color, mark) in &item.marks {
            spans.push(Span::styled(mark.clone(), Style::new().fg(*color)));
        }
        if item.marks.last().is_some_and(|(_, m)| !m.ends_with(' ')) {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            item.hint.clone(),
            Style::new().add_modifier(Modifier::DIM),
        ));
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(Span::raw(" ".repeat(row_width.saturating_sub(used))));
        let mut line = Line::from(spans);
        if i == selected {
            line = line.patch_style(Style::new().bg(theme::current().subtle));
        }
        lines.push(line);
    }
    lines.push(Line::default());
    // Border and two columns of padding on each side.
    lines.extend(key_lines(keys, usize::from(area.width).saturating_sub(6)));

    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6;
    let height = lines.len() as u16 + 2;
    let width = width.min(area.width);
    let height = height.min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    draw_box(frame, popup, title.into(), 2, lines);
}

/// The settings menu in the middle of `area`. `note` is shown below the
/// settings, e.g. where they were saved.
pub fn draw_settings(
    frame: &mut Frame,
    area: Rect,
    config: &Config,
    selected: usize,
    note: Option<&str>,
) {
    let area = inset(area);
    let name_width = SETTINGS
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0);
    let value_width = SETTINGS
        .iter()
        .map(|s| (s.value)(config).chars().count())
        .max()
        .unwrap_or(0)
        .max(6);

    let mut lines = Vec::new();
    for (i, setting) in SETTINGS.iter().enumerate() {
        let pad = name_width - setting.name.chars().count() + GROUP_GAP;
        let value = format!("‹ {:^value_width$} ›", (setting.value)(config));
        let mut line = Line::from(vec![
            Span::raw(" "),
            Span::raw(setting.name),
            Span::raw(" ".repeat(pad)),
            Span::styled(value, key_style()),
            Span::raw(" "),
        ]);
        if i == selected {
            line = line.patch_style(Style::new().bg(theme::current().subtle));
        }
        lines.push(line);
    }
    // The colours of the chosen theme, to see what it looks like.
    let t = theme::current();
    let mut swatch = vec![Span::raw(" ")];
    for color in [t.accent, t.danger, t.success, t.warning]
        .into_iter()
        .chain(t.palette())
    {
        swatch.push(Span::styled("■ ", Style::new().fg(color)));
    }
    lines.push(Line::default());
    lines.push(Line::from(swatch));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("↑↓", key_style()),
        Span::raw(" select   "),
        Span::styled("←→ Enter", key_style()),
        Span::raw(" change   "),
        Span::styled("Esc", key_style()),
        Span::raw(" close"),
    ]));
    // The note does not widen the box, a long path is cut off instead.
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6;
    if let Some(note) = note {
        let hint = lines.len() - 1;
        lines.insert(
            hint,
            Line::styled(note.to_owned(), Style::new().add_modifier(Modifier::DIM)),
        );
    }
    let height = lines.len() as u16 + 2;
    let width = width.min(area.width);
    let height = height.min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    // On a short screen the list scrolls so the selected setting shows.
    let rows = height.saturating_sub(2);
    let overflow = (lines.len() as u16).saturating_sub(rows);
    let scroll = (selected as u16 + 2).saturating_sub(rows).min(overflow);
    draw_box_scrolled(frame, popup, " ⬢ Settings ".into(), 2, lines, scroll);
}

/// Keys with what they do, like the bottom of a menu, in rows no wider
/// than `max_width`.
fn key_lines(keys: &[(&str, &str)], max_width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut width = 0;
    for (key, what) in keys {
        let entry = key.chars().count() + 1 + what.chars().count();
        if width > 0 && width + 3 + entry > max_width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            width = 0;
        }
        if width > 0 {
            spans.push(Span::raw("   "));
            width += 3;
        }
        spans.push(Span::styled(key.to_string(), key_style()));
        spans.push(Span::raw(format!(" {what}")));
        width += entry;
    }
    lines.push(Line::from(spans));
    lines
}

fn key_style() -> Style {
    Style::new()
        .fg(theme::current().accent)
        .add_modifier(Modifier::BOLD)
}

/// The commands menu: the user's commands from the config, then back and
/// cancel.
fn command_lines(
    group: Group,
    commands: &[(char, String)],
    none: &str,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    let mut lines = group_lines(group, selected.and_then(|s| s.checked_sub(commands.len())));
    let fixed = lines.split_off(1);
    if commands.is_empty() {
        lines.push(Line::styled(
            none.to_owned(),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    for (i, (key, name)) in commands.iter().enumerate() {
        let line = Line::from(vec![
            Span::styled(key.to_string(), key_style()),
            Span::raw(" ".repeat(2 + GAP)),
            Span::raw(name.clone()),
        ]);
        lines.push(if selected == Some(i) {
            picked(line)
        } else {
            line
        });
    }
    lines.extend(fixed);
    lines
}

/// `line` as the entry picked with the arrows.
fn picked(line: Line<'static>) -> Line<'static> {
    // On each span: columns are joined from spans, the line's own style
    // would get lost.
    let reversed = Style::new().add_modifier(Modifier::REVERSED);
    Line::from(
        line.spans
            .into_iter()
            .map(|span| span.patch_style(reversed))
            .collect::<Vec<_>>(),
    )
}

/// A group's title followed by one line per binding, keys aligned; the
/// `selected` one of those to pick (see `bindings::pickable`) stands out.
fn group_lines(group: Group, selected: Option<usize>) -> Vec<Line<'static>> {
    let bindings: Vec<&Binding> = in_group(group).collect();
    let key_width = bindings
        .iter()
        .map(|b| b.label.chars().count())
        .max()
        .unwrap_or(0);

    let title = match group.menu() {
        Menu::Root => group.title().to_owned(),
        menu => format!("{} ({})", group.title(), menu.path()),
    };
    let mut lines = vec![Line::styled(
        title,
        Style::new()
            .fg(group.color())
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )];
    // Entries that open a submenu stand out in one colour everywhere.
    let submenu = Style::new().fg(theme::current().magenta);
    let mut pickable = 0;
    for binding in bindings {
        let pad = key_width - binding.label.chars().count() + GAP;
        let opens_menu = binding.description.ends_with('…');
        let this = crate::bindings::pickable(binding).then(|| {
            pickable += 1;
            pickable - 1
        });
        let line = Line::from(vec![
            Span::styled(
                binding.label,
                if opens_menu {
                    submenu.add_modifier(Modifier::BOLD)
                } else {
                    key_style()
                },
            ),
            Span::raw(" ".repeat(pad)),
            if opens_menu {
                Span::styled(binding.description, submenu)
            } else {
                Span::raw(binding.description)
            },
        ]);
        lines.push(if this.is_some() && this == selected {
            picked(line)
        } else {
            line
        });
    }
    lines
}

/// Joins columns of lines side by side, padding each column to its width.
fn beside(columns: Vec<Vec<Line<'static>>>, widths: &[usize]) -> Vec<Line<'static>> {
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    (0..rows)
        .map(|row| {
            let mut spans = Vec::new();
            for (i, column) in columns.iter().enumerate() {
                let line = column.get(row).cloned().unwrap_or_default();
                let pad = widths[i] - line.width();
                spans.extend(line.spans);
                if i + 1 < columns.len() {
                    spans.push(Span::raw(" ".repeat(pad + GROUP_GAP)));
                }
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(width: u16, height: u16, draw: impl Fn(&mut Frame, Rect)) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text of the cells drawn reversed: the entry picked with arrows.
    fn reversed(width: u16, height: u16, draw: impl Fn(&mut Frame, Rect)) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buffer[(x, y)];
                if cell.modifier.contains(Modifier::REVERSED) {
                    text.push_str(cell.symbol());
                }
            }
        }
        text.trim().to_owned()
    }

    #[test]
    fn arrows_pick_what_the_keys_would_run() {
        // The first entry of the session menu, then the next, as
        // `pick_keys` orders them.
        let keys = crate::bindings::pick_keys(Menu::Session, &[]);
        assert!(keys.len() > 2);
        let first = reversed(120, 40, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Session, &[], Some(0))
        });
        assert!(first.contains("switch"), "{first}");
        let second = reversed(120, 40, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Session, &[], Some(1))
        });
        assert!(second.contains("new session"), "{second}");
        // The user's commands come first in their menu.
        let commands = [('g', "lazygit".to_owned())];
        let own = reversed(120, 40, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Commands, &commands, Some(0))
        });
        assert!(own.contains("lazygit"), "{own}");
        // `←↑↓→ pane there` is no one thing: left out of the main menu.
        let root = crate::bindings::pick_keys(Menu::Root, &[]);
        assert!(!root.contains(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Left,
            crossterm::event::KeyModifiers::NONE
        )));
        // ←/→ to the column beside at the same height, round, skipping
        // columns with nothing to pick; a shorter one takes its last entry.
        use crate::bindings::pick_beside;
        let columns = [3, 0, 2, 4];
        assert_eq!(pick_beside(&columns, 1, 1), 4);
        assert_eq!(pick_beside(&columns, 2, 1), 4);
        assert_eq!(pick_beside(&columns, 4, 1), 6);
        assert_eq!(pick_beside(&columns, 6, 1), 1);
        assert_eq!(pick_beside(&columns, 0, -1), 5);
        assert_eq!(pick_beside(&[5], 3, 1), 3);
        let none = reversed(120, 40, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Root, &[], None)
        });
        assert!(none.is_empty(), "{none}");
    }

    #[test]
    fn choices_list_the_keys_they_take() {
        let items = [MenuItem {
            key: '1',
            title: "default".into(),
            hint: String::new(),
            danger: false,
            apart: false,
            color: None,
            group: None,
            marks: Vec::new(),
        }];
        let keys = [("Enter", "switch"), ("r", "rename"), ("Esc", "cancel")];
        let screen = render(80, 20, |f, a| {
            draw_choices(f, a, " Sessions ", &items, 0, &keys)
        });
        let footer = screen.split_once("default").expect("item").1;
        assert!(footer.contains("r rename") && footer.contains("Esc cancel"));
    }

    #[test]
    fn menu_keys_wrap_instead_of_being_cut_off() {
        let keys = [("Enter", "bring back"), ("x", "delete"), ("Esc", "cancel")];
        let lines = key_lines(&keys, 24);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.width() <= 24));
        assert_eq!(key_lines(&keys, 80).len(), 1);
    }

    #[test]
    fn which_key_fits_side_by_side_on_a_wide_screen() {
        let screen = render(120, 30, |f, a| {
            draw_which_key(f, a, Side::Right, Menu::Root, &[], None)
        });
        let row = screen
            .lines()
            .find(|l| l.contains("Panes"))
            .expect("Panes header");
        assert!(row.contains("Go to") && row.contains("View") && row.contains("Session"));
        assert!(screen.contains("split side by side"));
        // Rare keys sit in the footer, below the groups.
        let footer = screen.split_once("resize").expect("footer").1;
        assert!(footer.contains("move float") && footer.contains("cancel"));
    }

    #[test]
    fn which_key_stacks_on_a_narrow_screen() {
        let screen = render(40, 60, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Root, &[], None)
        });
        let row = screen
            .lines()
            .find(|l| l.contains("Panes"))
            .expect("Panes header");
        assert!(!row.contains("Go to"));
        assert!(screen.contains("Go to") && screen.contains("View") && screen.contains("Session"));
    }

    #[test]
    fn which_key_opens_on_the_chosen_side() {
        let left = render(120, 30, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Root, &[], None)
        });
        let right = render(120, 30, |f, a| {
            draw_which_key(f, a, Side::Right, Menu::Root, &[], None)
        });
        let corner = |screen: &str| {
            let row = screen.lines().find(|l| l.contains("⬢ Ctrl+B")).unwrap();
            row.chars().position(|c| c == '╭').unwrap()
        };
        assert_eq!(corner(&left), 1);
        assert!(corner(&right) > corner(&left));
    }

    #[test]
    fn which_key_shows_the_open_submenu() {
        let screen = render(120, 30, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Floating, &[], None)
        });
        assert!(screen.contains("next floating pane"));
        assert!(!screen.contains("split side by side"));
    }

    #[test]
    fn commands_menu_lists_the_users_commands() {
        let commands = [('g', "lazygit".to_owned())];
        let screen = render(80, 20, |f, a| {
            draw_which_key(f, a, Side::Left, Menu::Commands, &commands, None)
        });
        assert!(screen.contains("lazygit") && screen.contains("cancel"));
    }

    #[test]
    fn settings_show_every_setting_with_its_value() {
        let config = Config::default();
        let screen = render(80, 50, |f, a| {
            draw_settings(f, a, &config, 0, Some("saved"))
        });
        for setting in SETTINGS {
            assert!(screen.contains(setting.name), "{} missing", setting.name);
            assert!(
                screen.contains(&(setting.value)(&config)),
                "the value of {:?} does not fit in 80 columns: {:?}",
                setting.name,
                (setting.value)(&config)
            );
        }
        assert!(screen.contains("saved"));
    }

    #[test]
    fn settings_scroll_to_the_selected_one_on_a_short_screen() {
        let config = Config::default();
        let last = SETTINGS.len() - 1;
        let screen = render(80, 12, |f, a| draw_settings(f, a, &config, last, None));
        assert!(screen.contains(SETTINGS[last].name));
    }

    #[test]
    fn help_lists_the_keys_inside_the_lists() {
        let screen = render(120, 80, |f, a| draw_help(f, a, 0));
        assert!(screen.contains("Session list") && screen.contains("Hives"));
        assert!(screen.contains("save all as hive") && screen.contains("bring back"));
        assert!(screen.contains("rename this session"));
    }

    #[test]
    fn help_scrolls_when_the_screen_is_short() {
        let top = render(80, 20, |f, a| draw_help(f, a, 0));
        let scrolled = render(80, 20, |f, a| draw_help(f, a, 500));
        assert!(top.contains("Press"));
        assert!(!scrolled.contains("Press"));
        assert!(scrolled.contains("any other key"));
    }

    #[test]
    fn help_lists_every_binding() {
        let screen = render(160, 60, |f, a| draw_help(f, a, 0));
        for binding in crate::bindings::BINDINGS {
            assert!(
                screen.contains(binding.description),
                "{} missing",
                binding.description
            );
        }
    }
}
