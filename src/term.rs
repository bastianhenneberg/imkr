//! The terminal each pane draws into: `alacritty_terminal`, behind the
//! methods imkr used from `vt100` before (`process`, `screen()`, `cell`,
//! `contents_between`, `scrollback` …), so the rest reads the same.
//!
//! Rows are view rows, as on the screen; `scrollback` is how far the view
//! is moved into the history. What the terminal answers programs (cursor
//! position, colours, size) is collected and taken with `answers`.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{self, NamedColor, Processor};

/// A colour as programs set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// Which mouse events a program asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseProtocolMode {
    None,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// How mouse events are encoded for the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    Default,
    Utf8,
    Sgr,
}

/// What the terminal tells imkr while reading: answers for the program,
/// and the size and colours to answer with.
#[derive(Clone)]
struct Listener {
    answers: Arc<Mutex<Vec<u8>>>,
    size: Arc<Mutex<WindowSize>>,
    /// The title the program set (OSC 0/2), `None` after a reset.
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let answer = match event {
            Event::Title(title) => {
                *self.title.lock().unwrap() = Some(title);
                return;
            }
            Event::ResetTitle => {
                *self.title.lock().unwrap() = None;
                return;
            }
            Event::PtyWrite(text) => text,
            Event::TextAreaSizeRequest(format) => format(*self.size.lock().unwrap()),
            // Colours nobody set: a plain dark terminal's, near enough for
            // programs choosing light or dark.
            Event::ColorRequest(index, format) => {
                let rgb = match index {
                    i if i == NamedColor::Background as usize => ansi::Rgb {
                        r: 0x1e,
                        g: 0x1e,
                        b: 0x2e,
                    },
                    _ => ansi::Rgb {
                        r: 0xcd,
                        g: 0xd6,
                        b: 0xf4,
                    },
                };
                format(rgb)
            }
            _ => return,
        };
        self.answers
            .lock()
            .unwrap()
            .extend_from_slice(answer.as_bytes());
    }
}

struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal: what a program wrote, as cells, with its history.
pub struct Parser {
    term: alacritty_terminal::Term<Listener>,
    processor: Processor,
    listener: Listener,
}

/// A cell of the view.
pub struct Cell<'a>(&'a alacritty_terminal::term::cell::Cell);

impl Parser {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let listener = Listener {
            answers: Arc::default(),
            title: Arc::default(),
            size: Arc::new(Mutex::new(WindowSize {
                num_lines: rows,
                num_cols: cols,
                cell_width: 0,
                cell_height: 0,
            })),
        };
        let config = Config {
            scrolling_history: scrollback,
            // Copies to the clipboard are passed on by `clipboard`, which
            // sees the sequence first.
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        let size = Size {
            rows: usize::from(rows),
            cols: usize::from(cols),
        };
        Self {
            term: alacritty_terminal::Term::new(config, &size, listener.clone()),
            processor: Processor::new(),
            listener,
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
    }

    /// What the terminal answered the program since the last call, to
    /// write back to it.
    pub fn answers(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.listener.answers.lock().unwrap())
    }

    /// The title the program gave its terminal, as it set it.
    pub fn title(&self) -> Option<String> {
        self.listener.title.lock().unwrap().clone()
    }

    /// Sets the title, for a terminal taken over by `imkr update`.
    pub fn set_title(&mut self, title: Option<String>) {
        *self.listener.title.lock().unwrap() = title;
    }

    /// The same terminal, as vt100 split it into parser and screen.
    pub fn screen(&self) -> &Self {
        self
    }

    pub fn screen_mut(&mut self) -> &mut Self {
        self
    }

    /// New size; long lines wrap anew instead of being cut.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        self.term.resize(Size {
            rows: usize::from(rows),
            cols: usize::from(cols),
        });
        let mut size = self.listener.size.lock().unwrap();
        size.num_lines = rows;
        size.num_cols = cols;
    }

    /// (rows, columns)
    pub fn size(&self) -> (u16, u16) {
        (self.term.screen_lines() as u16, self.term.columns() as u16)
    }

    /// How many lines the history holds.
    pub fn history_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// How far the view is moved into the history.
    pub fn scrollback(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        let now = self.scrollback() as i64;
        let wanted = rows.min(self.history_len()) as i64;
        if wanted != now {
            self.term
                .scroll_display(Scroll::Delta((wanted - now) as i32));
        }
    }

    /// The grid line view row `row` shows.
    fn line(&self, row: u16) -> Line {
        Line(i32::from(row) - self.scrollback() as i32)
    }

    /// The grid line of absolute row `abs`, 0 the oldest of the history.
    fn abs_line(&self, abs: usize) -> Line {
        Line(abs as i32 - self.history_len() as i32)
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<Cell<'_>> {
        let (rows, cols) = self.size();
        if row >= rows || col >= cols {
            return None;
        }
        Some(Cell(
            &self.term.grid()[self.line(row)][Column(usize::from(col))],
        ))
    }

    /// The cursor as (row, column) on the screen, not the view.
    pub fn cursor_position(&self) -> (u16, u16) {
        let point = self.term.grid().cursor.point;
        (point.line.0.max(0) as u16, point.column.0 as u16)
    }

    pub fn hide_cursor(&self) -> bool {
        !self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        let mode = self.term.mode();
        if mode.contains(TermMode::MOUSE_MOTION) {
            MouseProtocolMode::AnyMotion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseProtocolMode::ButtonMotion
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseProtocolMode::PressRelease
        } else {
            MouseProtocolMode::None
        }
    }

    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        let mode = self.term.mode();
        if mode.contains(TermMode::SGR_MOUSE) {
            MouseProtocolEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseProtocolEncoding::Utf8
        } else {
            MouseProtocolEncoding::Default
        }
    }

    /// The text of grid line `line` from `from` to `to` (exclusive).
    fn line_text(&self, line: Line, from: u16, to: u16) -> String {
        let row = &self.term.grid()[line];
        let to = usize::from(to).min(self.term.columns());
        let mut out = String::new();
        for col in usize::from(from)..to {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            out.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                out.extend(extra);
            }
        }
        out
    }

    fn line_wrapped(&self, line: Line) -> bool {
        let last = self.term.columns() - 1;
        self.term.grid()[line][Column(last)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    /// The text of the view from (`start_row`, `start_col`) to
    /// (`end_row`, `end_col`) exclusive; rows that wrap are joined.
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        let (rows, cols) = self.size();
        let end_row = end_row.min(rows.saturating_sub(1));
        let mut out = String::new();
        for row in start_row..=end_row {
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row { end_col } else { cols };
            let line = self.line(row);
            let text = self.line_text(line, from, to);
            if row != end_row && self.line_wrapped(line) {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                if row != end_row {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// The whole view as text.
    pub fn contents(&self) -> String {
        let (rows, cols) = self.size();
        let text = self.contents_between(0, 0, rows.saturating_sub(1), cols);
        text.trim_end().to_owned()
    }

    /// Each view row from `start` for `width` columns, as text.
    pub fn rows(&self, start: u16, width: u16) -> impl Iterator<Item = String> + '_ {
        let (rows, _) = self.size();
        (0..rows).map(move |row| {
            self.line_text(self.line(row), start, start.saturating_add(width))
                .trim_end()
                .to_owned()
        })
    }

    /// The text of absolute row `abs` (0 the oldest of the history) from
    /// `from` to `to` exclusive, and whether it goes on in the next.
    pub fn abs_row(&self, abs: usize, from: u16, to: u16) -> Option<(String, bool)> {
        let (rows, _) = self.size();
        if abs >= self.history_len() + usize::from(rows) {
            return None;
        }
        let line = self.abs_line(abs);
        Some((self.line_text(line, from, to), self.line_wrapped(line)))
    }

    /// The screen (not the history) as escape codes that draw it again in a
    /// fresh terminal of the same size, with the cursor and the modes
    /// programs set: what `imkr update` hands over.
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mode = *self.term.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(b"\x1b[H\x1b[2J\x1b[0m");
        let (rows, cols) = self.size();
        for row in 0..rows {
            out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
            let line = Line(i32::from(row));
            let mut last = String::new();
            for col in 0..cols {
                let cell = &self.term.grid()[line][Column(usize::from(col))];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let sgr = sgr(cell);
                if sgr != last {
                    out.extend_from_slice(sgr.as_bytes());
                    last = sgr;
                }
                let mut text = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
                out.extend_from_slice(text.as_bytes());
            }
            out.extend_from_slice(b"\x1b[0m");
        }
        let (row, col) = self.cursor_position();
        out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        for (flag, code) in [
            (TermMode::BRACKETED_PASTE, "2004"),
            (TermMode::APP_CURSOR, "1"),
            (TermMode::MOUSE_REPORT_CLICK, "1000"),
            (TermMode::MOUSE_DRAG, "1002"),
            (TermMode::MOUSE_MOTION, "1003"),
            (TermMode::SGR_MOUSE, "1006"),
            (TermMode::UTF8_MOUSE, "1005"),
            (TermMode::FOCUS_IN_OUT, "1004"),
        ] {
            if mode.contains(flag) {
                out.extend_from_slice(format!("\x1b[?{code}h").as_bytes());
            }
        }
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }
}

/// The SGR sequence that sets `cell`'s look from scratch.
fn sgr(cell: &alacritty_terminal::term::cell::Cell) -> String {
    let mut codes = vec!["0".to_owned()];
    let flags = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if flags.contains(flag) {
            codes.push(code.to_owned());
        }
    }
    for (color, base) in [(convert(cell.fg), 30), (convert(cell.bg), 40)] {
        match color {
            Color::Default => {}
            Color::Idx(i) if i < 8 => codes.push((base + u16::from(i)).to_string()),
            Color::Idx(i) if i < 16 => codes.push((base + 60 + u16::from(i) - 8).to_string()),
            Color::Idx(i) => codes.push(format!("{};5;{i}", base + 8)),
            Color::Rgb(r, g, b) => codes.push(format!("{};2;{r};{g};{b}", base + 8)),
        }
    }
    format!("\x1b[{}m", codes.join(";"))
}

fn convert(color: ansi::Color) -> Color {
    match color {
        ansi::Color::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        ansi::Color::Indexed(i) => Color::Idx(i),
        ansi::Color::Named(named) => {
            let n = named as usize;
            match named {
                NamedColor::Foreground
                | NamedColor::Background
                | NamedColor::Cursor
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => Color::Default,
                _ if n < 16 => Color::Idx(n as u8),
                // The dim ones: their plain colour.
                _ => Color::Idx((n.saturating_sub(259)).min(7) as u8),
            }
        }
    }
}

impl Cell<'_> {
    /// The right half of a wide character, drawn by its left half.
    pub fn is_wide_continuation(&self) -> bool {
        self.0
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
    }

    pub fn has_contents(&self) -> bool {
        self.0.c != ' ' || self.0.zerowidth().is_some()
    }

    pub fn contents(&self) -> String {
        let mut text = self.0.c.to_string();
        if let Some(extra) = self.0.zerowidth() {
            text.extend(extra);
        }
        text
    }

    pub fn fgcolor(&self) -> Color {
        convert(self.0.fg)
    }

    pub fn bgcolor(&self) -> Color {
        convert(self.0.bg)
    }

    pub fn bold(&self) -> bool {
        self.0.flags.contains(Flags::BOLD)
    }

    pub fn dim(&self) -> bool {
        self.0.flags.contains(Flags::DIM)
    }

    pub fn italic(&self) -> bool {
        self.0.flags.contains(Flags::ITALIC)
    }

    pub fn underline(&self) -> bool {
        self.0.flags.intersects(Flags::ALL_UNDERLINES)
    }

    pub fn inverse(&self) -> bool {
        self.0.flags.contains(Flags::INVERSE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_colours_and_history() {
        let mut p = Parser::new(3, 20, 100);
        p.process(b"hi \x1b[31mred\x1b[0m");
        assert_eq!(p.cell(0, 3).unwrap().contents(), "r");
        assert_eq!(p.cell(0, 3).unwrap().fgcolor(), Color::Idx(1));
        assert_eq!(p.cell(0, 0).unwrap().fgcolor(), Color::Default);
        for i in 0..10 {
            p.process(format!("\r\nline {i}").as_bytes());
        }
        assert_eq!(p.history_len(), 8);
        assert_eq!(p.contents(), "line 7\nline 8\nline 9");
        p.set_scrollback(2);
        assert_eq!(p.scrollback(), 2);
        assert_eq!(p.contents_between(0, 0, 0, 20), "line 5");
        assert_eq!(p.abs_row(0, 0, 20).unwrap().0.trim_end(), "hi red");
    }

    #[test]
    fn long_lines_wrap_again_when_wider() {
        let mut p = Parser::new(3, 10, 100);
        p.process(b"0123456789abcdef");
        assert!(p.abs_row(0, 0, 10).unwrap().1);
        p.set_size(3, 20);
        assert_eq!(p.contents(), "0123456789abcdef");
    }

    #[test]
    fn answers_the_program() {
        let mut p = Parser::new(5, 10, 0);
        p.process(b"ab\x1b[6n");
        assert_eq!(p.answers(), b"\x1b[1;3R");
        assert!(p.answers().is_empty());
    }

    #[test]
    fn the_screen_draws_itself_again() {
        let mut p = Parser::new(3, 12, 0);
        p.process(b"\x1b[1;32mgreen\x1b[0m x\r\n\xe4\xbd\xa0\xe5\xa5\xbd\x1b[?2004h");
        let mut again = Parser::new(3, 12, 0);
        again.process(&p.state_formatted());
        assert_eq!(again.contents(), p.contents());
        assert_eq!(again.cursor_position(), p.cursor_position());
        assert!(again.cell(0, 0).unwrap().bold());
        assert_eq!(again.cell(0, 0).unwrap().fgcolor(), Color::Idx(2));
        assert!(again.bracketed_paste());
    }
}
