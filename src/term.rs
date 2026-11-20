//! The terminal emulator: bytes in, a grid of cells out. It answers the
//! queries programs wait on, keeps the lines that scroll off the top, and
//! takes sixel pictures whole. It knows nothing of files or processes.

use crate::width::width;
use std::fmt::Write as _;

/// The most bytes one picture may have; a longer one is dropped and noted.
pub const PICTURE_MAX: usize = 4 << 20;
/// The most pictures a screen keeps; a new one past it drops the oldest.
pub const PICTURES_MAX: usize = 8;
const PARAMS_MAX: usize = 32;
/// The most columns and the most rows a screen may have.
pub const SIZE_MAX: usize = 500;
/// The most history kept between two looks by the holder; lines past it are dropped and noted.
const HISTORY_PENDING_MAX: usize = 4 << 20;
const REPLY_MAX: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u16 = 1;
pub const DIM: u16 = 2;
pub const ITALIC: u16 = 4;
pub const UNDERLINE: u16 = 8;
pub const BLINK: u16 = 16;
pub const REVERSE: u16 = 32;
pub const HIDDEN: u16 = 64;
pub const STRIKE: u16 = 128;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Style {
    /// The escape code that sets exactly this style.
    pub fn sgr(self, out: &mut String) {
        out.push_str("\x1b[0");
        for (bit, code) in
            [BOLD, DIM, ITALIC, UNDERLINE, BLINK, REVERSE, HIDDEN, STRIKE].iter().zip([1, 2, 3, 4, 5, 7, 8, 9])
        {
            if self.flags & bit != 0 {
                let _ = write!(out, ";{code}");
            }
        }
        for (color, base) in [(self.fg, 30), (self.bg, 40)] {
            let _ = match color {
                Color::Default => Ok(()),
                Color::Indexed(n @ 0..8) => write!(out, ";{}", base + u16::from(n)),
                Color::Indexed(n @ 8..16) => write!(out, ";{}", base + 52 + u16::from(n)),
                Color::Indexed(n) => write!(out, ";{};5;{n}", base + 8),
                Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
            };
        }
        out.push('m');
    }
}

/// One character cell. `ch` is NUL in the second half of a wide character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    /// Up to two combining marks; more are dropped.
    pub marks: [char; 2],
    pub style: Style,
}

impl Cell {
    pub fn blank(style: Style) -> Cell {
        Cell { ch: ' ', marks: ['\0'; 2], style: Style { bg: style.bg, ..Style::default() } }
    }
    pub fn push_text(&self, out: &mut String) {
        if self.ch != '\0' {
            out.push(self.ch);
            out.extend(self.marks.iter().filter(|&&m| m != '\0'));
        }
    }
}

/// A picture the program drew: where it is, in cells, and its bytes until
/// the holder takes them to write the file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub id: u32,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub data: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Default)]
struct Saved {
    row: usize,
    col: usize,
    style: Style,
    origin: bool,
    graphics: [bool; 2],
    shift: bool,
    wrap: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeInter,
    Csi,
    /// A string to skip (OSC, SOS, PM, APC), up to BEL or ST.
    Skip,
    /// A device control string, kept in `dcs`: a sixel picture if it says so.
    Dcs,
    /// ESC seen inside a string: `\` ends it, anything else starts anew.
    StringEscape,
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    grid: Vec<Vec<Cell>>,
    other: Vec<Vec<Cell>>,
    pub alt: bool,
    pub row: usize,
    pub col: usize,
    style: Style,
    wrap_pending: bool,
    top: usize,
    bottom: usize,
    origin: bool,
    autowrap: bool,
    insert: bool,
    pub cursor_shown: bool,
    pub cursor_shape: u8,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub paste: bool,
    /// The mouse reports the program asked for: 0 for none, else 9, 1000, 1002 or 1003; and whether as SGR (1006).
    pub mouse: u16,
    pub mouse_sgr: bool,
    tabs: Vec<bool>,
    graphics: [bool; 2],
    shift: bool,
    saved: [Saved; 2],
    last: char,
    state: State,
    params: Vec<(u16, bool)>,
    param: Option<u16>,
    sub: bool,
    private: u8,
    inter: u8,
    utf8: u32,
    utf8_left: u8,
    /// The smallest character this many bytes may encode: a longer spelling of a smaller one is refused.
    utf8_min: u32,
    /// The sequence being read is malformed or has more parameters than are kept: it will be ignored.
    too_long: bool,
    dcs: Vec<u8>,
    dcs_was: State,
    /// Answers to the program's queries, for the holder to write to it.
    pub reply: Vec<u8>,
    /// Lines that scrolled off the top of the main screen, as plain text.
    pub history: String,
    pub pictures: Vec<Picture>,
    /// The pictures of the screen not shown (the main one while the alternate is).
    pub other_pictures: Vec<Picture>,
    next_picture: u32,
    /// The size of a cell in pixels, to know how many cells a picture covers.
    pub cell: (usize, usize),
    /// What was dropped for being over a bound, for the holder's log (the last few of each kind).
    pub refused: Vec<&'static str>,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let (cols, rows) = (cols.clamp(1, SIZE_MAX), rows.clamp(1, SIZE_MAX));
        let grid = vec![vec![Cell::blank(Style::default()); cols]; rows];
        Term {
            cols,
            rows,
            other: grid.clone(),
            grid,
            alt: false,
            row: 0,
            col: 0,
            style: Style::default(),
            wrap_pending: false,
            top: 0,
            bottom: rows - 1,
            origin: false,
            autowrap: true,
            insert: false,
            cursor_shown: true,
            cursor_shape: 0,
            app_cursor: false,
            app_keypad: false,
            paste: false,
            mouse: 0,
            mouse_sgr: false,
            tabs: (0..SIZE_MAX).map(|c| c % 8 == 0).collect(),
            graphics: [false; 2],
            shift: false,
            saved: [Saved::default(); 2],
            last: ' ',
            state: State::Ground,
            params: Vec::new(),
            param: None,
            sub: false,
            private: 0,
            inter: 0,
            utf8: 0,
            utf8_left: 0,
            utf8_min: 0,
            too_long: false,
            dcs: Vec::new(),
            dcs_was: State::Ground,
            reply: Vec::new(),
            history: String::new(),
            pictures: Vec::new(),
            other_pictures: Vec::new(),
            next_picture: 1,
            cell: (10, 20),
            refused: Vec::new(),
        }
    }

    pub fn cell_at(&self, row: usize, col: usize) -> Option<&Cell> {
        self.grid.get(row)?.get(col)
    }

    // ---- files -----------------------------------------------------------

    fn row_text(row: &[Cell], out: &mut String) {
        let start = out.len();
        row.iter().for_each(|cell| cell.push_text(out));
        out.truncate(start + out.get(start..).map_or(0, |text| text.trim_end_matches(' ').len()));
    }

    /// The screen as plain text, one line per row, blanks at the end cut.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        for row in &self.grid {
            Term::row_text(row, &mut out);
            out.push('\n');
        }
        out
    }

    /// The screen with colours: text and SGR codes only, each row on a line
    /// that starts and ends in the default style.
    pub fn styled(&self) -> String {
        let mut out = String::new();
        let plain = Style::default();
        for row in &self.grid {
            let used = row.iter().rposition(|c| *c != Cell::blank(plain)).map_or(0, |last| last + 1);
            let mut style = plain;
            for cell in row.iter().take(used) {
                if cell.style != style && cell.ch != '\0' {
                    style = cell.style;
                    style.sgr(&mut out);
                }
                cell.push_text(&mut out);
            }
            if style != plain {
                out.push_str("\x1b[m");
            }
            out.push('\n');
        }
        out
    }

    // ---- the grid --------------------------------------------------------

    fn blank(&self) -> Cell {
        Cell::blank(self.style)
    }

    /// Repairs a row after cells moved: half of a wide character becomes a blank.
    fn tidy(&mut self, row: usize) {
        let Some(cells) = self.grid.get_mut(row) else {
            return;
        };
        let mut lead = false;
        for i in 0..cells.len() {
            let next_is_half = cells.get(i + 1).is_some_and(|c| c.ch == '\0');
            let Some(cell) = cells.get_mut(i) else { return };
            let wide = width(cell.ch) == 2;
            if (cell.ch == '\0' && !lead) || (wide && !next_is_half) {
                cell.ch = ' ';
                cell.marks = ['\0'; 2];
            }
            lead = wide && next_is_half;
        }
    }

    /// Blanks a wide character that a boundary before column `at` would cut in two.
    fn split(&mut self, row: usize, at: usize) {
        let blank = self.blank();
        if let Some(cells) = self.grid.get_mut(row).filter(|cells| cells.get(at).is_some_and(|c| c.ch == '\0'))
            && let Some(pair) = cells.get_mut(at.saturating_sub(1)..=at)
        {
            pair.fill(blank);
        }
    }

    /// Blanks columns `from..to` of a row.
    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let (blank, to) = (self.blank(), to.min(self.cols));
        self.split(row, from);
        self.split(row, to);
        if let Some(cells) = self.grid.get_mut(row).and_then(|r| r.get_mut(from..to)) {
            cells.fill(blank);
        }
        self.wrap_pending = false;
        self.pictures.retain(|p| !((p.row..p.row + p.rows).contains(&row) && p.col < to && from < p.col + p.cols));
    }

    /// Pictures move with rows `from..=bottom` when those scroll by `n`; one
    /// that would leave those rows, or lies across their edge, goes.
    fn scroll_pictures(&mut self, from: usize, n: usize, up: bool) {
        let bottom = self.bottom;
        self.pictures.retain_mut(|p| {
            let end = p.row + p.rows;
            if end <= from || p.row > bottom {
                return true;
            }
            let stays =
                p.row >= from && end <= bottom + 1 && if up { p.row >= from + n } else { end + n <= bottom + 1 };
            p.row = if up { p.row.saturating_sub(n) } else { p.row + n };
            stays
        });
    }

    /// Moves rows `from..=bottom` up by `n`, blank rows coming in below.
    fn scroll_up(&mut self, from: usize, n: usize) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = vec![self.blank(); self.cols];
        let Some(region) = self.grid.get_mut(from..=self.bottom) else {
            return;
        };
        let mut full = false;
        for row in region.iter_mut().take(n) {
            if from == 0 && !self.alt {
                if self.history.len() < HISTORY_PENDING_MAX {
                    Term::row_text(row, &mut self.history);
                    self.history.push('\n');
                } else {
                    full = true;
                }
            }
            row.clone_from(&blank);
        }
        region.rotate_left(n);
        self.scroll_pictures(from, n, true);
        if full {
            self.refuse("lines were not kept in history: 4 MiB of it were waiting to be written");
        }
    }

    /// Moves rows `from..=bottom` down by `n`, blank rows coming in at `from`.
    fn scroll_down(&mut self, from: usize, n: usize) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = vec![self.blank(); self.cols];
        let Some(region) = self.grid.get_mut(from..=self.bottom) else {
            return;
        };
        region.rotate_right(n);
        region.iter_mut().take(n).for_each(|row| row.clone_from(&blank));
        self.scroll_pictures(from, n, false);
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.bottom {
            self.scroll_up(self.top, 1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn reverse_linefeed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.top {
            self.scroll_down(self.top, 1);
        } else {
            self.row = self.row.saturating_sub(1);
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        let (low, high) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.row = (low + row).min(high);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn print(&mut self, mut c: char) {
        if self.graphics.get(usize::from(self.shift)) == Some(&true) {
            c = line_drawing(c);
        }
        let wide = match width(c) {
            0 => return self.add_mark(c),
            w => w == 2,
        };
        if wide && self.cols < 2 {
            return;
        }
        if self.wrap_pending || (wide && self.col + 2 > self.cols) {
            if self.autowrap {
                self.col = 0;
                self.linefeed();
            } else if wide {
                self.col = self.cols - 2;
            }
            self.wrap_pending = false;
        }
        let (row, col) = (self.row, self.col);
        let cells = 1 + usize::from(wide);
        if self.insert
            && let Some(rest) = self.grid.get_mut(row).and_then(|r| r.get_mut(col..))
        {
            rest.rotate_right(cells.min(rest.len()));
        }
        let style = self.style;
        let blank = self.blank();
        if let Some(line) = self.grid.get_mut(row) {
            // What this overwrites may be half of a wide character: blank the other half.
            if line.get(col).is_some_and(|c| c.ch == '\0')
                && let Some(lead) = col.checked_sub(1).and_then(|i| line.get_mut(i))
            {
                *lead = blank;
            }
            if let Some(half) = line.get_mut(col + cells).filter(|c| c.ch == '\0') {
                *half = blank;
            }
            if let Some(cell) = line.get_mut(col) {
                *cell = Cell { ch: c, marks: ['\0'; 2], style };
            }
            if let Some(cell) = line.get_mut(col + 1).filter(|_| wide) {
                *cell = Cell { ch: '\0', marks: ['\0'; 2], style };
            }
        }
        if self.insert {
            self.tidy(row);
        }
        if !self.pictures.is_empty() {
            self.pictures
                .retain(|p| !((p.row..p.row + p.rows).contains(&row) && (p.col..p.col + p.cols).contains(&col)));
        }
        self.last = c;
        self.col += cells;
        if self.col >= self.cols {
            self.col = self.cols - 1;
            self.wrap_pending = self.autowrap;
        }
    }

    fn add_mark(&mut self, mark: char) {
        let mut col = if self.wrap_pending { self.col } else { self.col.saturating_sub(1) };
        if self.cell_at(self.row, col).is_some_and(|c| c.ch == '\0') {
            col = col.saturating_sub(1);
        }
        let row = self.row;
        if let Some(slot) =
            self.grid.get_mut(row).and_then(|r| r.get_mut(col)).and_then(|c| c.marks.iter_mut().find(|m| **m == '\0'))
        {
            *slot = mark;
        }
    }

    fn save_cursor(&mut self) {
        let saved = Saved {
            row: self.row,
            col: self.col,
            style: self.style,
            origin: self.origin,
            graphics: self.graphics,
            shift: self.shift,
            wrap: self.wrap_pending,
        };
        if let Some(slot) = self.saved.get_mut(usize::from(self.alt)) {
            *slot = saved;
        }
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved.get(usize::from(self.alt)).copied().unwrap_or_default();
        self.row = saved.row.min(self.rows - 1);
        self.col = saved.col.min(self.cols - 1);
        self.style = saved.style;
        self.origin = saved.origin;
        self.graphics = saved.graphics;
        self.shift = saved.shift;
        self.wrap_pending = saved.wrap && saved.col + 1 == self.cols;
    }

    fn set_alt(&mut self, alt: bool, clear: bool) {
        if alt != self.alt {
            std::mem::swap(&mut self.grid, &mut self.other);
            std::mem::swap(&mut self.pictures, &mut self.other_pictures);
            self.alt = alt;
        }
        if alt && clear {
            self.clear();
        }
    }

    fn clear(&mut self) {
        let blank = vec![Cell::blank(Style::default()); self.cols];
        self.grid.iter_mut().for_each(|row| row.clone_from(&blank));
        self.pictures.clear();
    }

    /// A new size. Rows keep their text (no reflow): what does not fit to the
    /// right is cut, and rows that do not fit above the cursor go to history.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.clamp(1, SIZE_MAX), rows.clamp(1, SIZE_MAX));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let blank = Cell::blank(Style::default());
        if self.alt {
            std::mem::swap(&mut self.grid, &mut self.other);
        }
        // `grid` is the main screen here. Blank rows below the cursor go first.
        let cursor = if self.alt { self.saved.first().map_or(0, |s| s.row) } else { self.row };
        while self.grid.len() > rows && self.grid.len() > cursor + 1 {
            self.grid.pop();
        }
        let extra = self.grid.len().saturating_sub(rows);
        for row in self.grid.drain(..extra) {
            Term::row_text(&row, &mut self.history);
            self.history.push('\n');
        }
        if self.alt {
            if let Some(saved) = self.saved.first_mut() {
                saved.row = saved.row.saturating_sub(extra);
            }
            std::mem::swap(&mut self.grid, &mut self.other);
        } else {
            self.row = self.row.saturating_sub(extra);
        }
        for grid in [&mut self.grid, &mut self.other] {
            grid.resize(rows, Vec::new());
            grid.iter_mut().for_each(|row| row.resize(cols, blank));
        }
        (self.cols, self.rows) = (cols, rows);
        (self.top, self.bottom) = (0, rows - 1);
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.wrap_pending = false;
        // A picture stays if it still fits whole where it is; the main screen's moved up with its rows.
        let (shown, other) = if self.alt { (0, extra) } else { (extra, 0) };
        for (pictures, up) in [(&mut self.pictures, shown), (&mut self.other_pictures, other)] {
            pictures.retain_mut(|p| {
                let fits = p.row >= up && p.row - up + p.rows <= rows && p.col + p.cols <= cols;
                p.row = p.row.saturating_sub(up);
                fits
            });
        }
        (0..rows).for_each(|row| self.tidy(row));
    }

    /// Notes a refusal for the log, once until the holder has taken it.
    fn refuse(&mut self, why: &'static str) {
        if !self.refused.contains(&why) {
            self.refused.push(why);
        }
    }

    fn respond(&mut self, text: &str) {
        if self.reply.len() + text.len() > REPLY_MAX {
            self.refuse("an answer to the program was dropped: 4096 bytes of answers were waiting");
        } else {
            self.reply.extend_from_slice(text.as_bytes());
        }
    }

    // ---- the parser ------------------------------------------------------

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        match self.state {
            State::Ground => self.ground(byte),
            State::Skip | State::Dcs => match byte {
                0x1b => {
                    self.dcs_was = self.state;
                    self.state = State::StringEscape;
                }
                0x07 if self.state == State::Skip => self.state = State::Ground,
                0x18 | 0x1a => self.end_string(false),
                0x20..=0x7e if self.state == State::Dcs => {
                    if self.dcs.len() < PICTURE_MAX {
                        self.dcs.push(byte);
                    } else {
                        self.refuse("a picture over 4 MiB was dropped");
                        self.dcs = Vec::new();
                        self.state = State::Skip;
                    }
                }
                _ => {}
            },
            State::StringEscape => {
                let was_dcs = self.dcs_was == State::Dcs;
                self.end_string(byte == b'\\' && was_dcs);
                if byte != b'\\' {
                    self.state = State::Escape;
                    self.byte(byte);
                }
            }
            _ => match byte {
                0x1b => self.state = State::Escape,
                0x18 | 0x1a => self.state = State::Ground,
                0x00..=0x1f => self.control(byte),
                0x7f => {}
                _ => match self.state {
                    State::Escape => self.escape(byte),
                    State::EscapeInter => self.escape_inter(byte),
                    _ => self.csi(byte),
                },
            },
        }
    }

    fn ground(&mut self, byte: u8) {
        if self.utf8_left > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8 = (self.utf8 << 6) | u32::from(byte & 0x3f);
                self.utf8_left -= 1;
                if self.utf8_left == 0 {
                    let whole = char::from_u32(self.utf8).filter(|c| *c >= '\u{a0}' && self.utf8 >= self.utf8_min);
                    self.print(whole.unwrap_or('\u{fffd}'));
                }
                return;
            }
            self.utf8_left = 0;
            self.print('\u{fffd}');
        }
        match byte {
            0x1b => self.state = State::Escape,
            0x00..=0x1f => self.control(byte),
            0x20..=0x7e => self.print(char::from(byte)),
            0xc2..=0xdf => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x1f), 1, 0x80),
            0xe0..=0xef => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x0f), 2, 0x800),
            0xf0..=0xf4 => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x07), 3, 0x10000),
            0x7f => {}
            _ => self.print('\u{fffd}'),
        }
    }

    fn control(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => self.tab(1),
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.col = 0;
                self.wrap_pending = false;
            }
            0x0e => self.shift = true,
            0x0f => self.shift = false,
            _ => {}
        }
    }

    fn tab(&mut self, n: usize) {
        for _ in 0..n {
            let next = (self.col + 1..self.cols).find(|&c| self.tabs.get(c) == Some(&true));
            self.col = next.unwrap_or(self.cols - 1);
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = State::Ground;
        match byte {
            b'[' => {
                self.params.clear();
                (self.param, self.sub, self.private, self.inter, self.too_long) = (None, false, 0, 0, false);
                self.state = State::Csi;
            }
            b']' | b'X' | b'^' | b'_' => self.state = State::Skip,
            b'P' => {
                self.dcs.clear();
                self.state = State::Dcs;
            }
            0x20..=0x2f => {
                self.inter = byte;
                self.state = State::EscapeInter;
            }
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(),
            b'E' => {
                self.col = 0;
                self.linefeed();
            }
            b'M' => self.reverse_linefeed(),
            b'H' => {
                if let Some(stop) = self.tabs.get_mut(self.col) {
                    *stop = true;
                }
            }
            b'=' => self.app_keypad = true,
            b'>' => self.app_keypad = false,
            b'c' => {
                // A full reset. History stays, and picture numbers go on, so no file's name is used twice.
                let (cell, history, refused) =
                    (self.cell, std::mem::take(&mut self.history), std::mem::take(&mut self.refused));
                *self =
                    Term { cell, history, refused, next_picture: self.next_picture, ..Term::new(self.cols, self.rows) };
            }
            _ => {}
        }
    }

    fn escape_inter(&mut self, byte: u8) {
        if (0x20..=0x2f).contains(&byte) {
            return;
        }
        self.state = State::Ground;
        match (self.inter, byte) {
            (b'(' | b')', _) => {
                if let Some(set) = self.graphics.get_mut(usize::from(self.inter == b')')) {
                    *set = byte == b'0';
                }
            }
            (b'#', b'8') => {
                let fill = Cell { ch: 'E', ..Cell::blank(Style::default()) };
                self.grid.iter_mut().for_each(|row| row.fill(fill));
            }
            _ => {}
        }
    }

    fn end_string(&mut self, picture: bool) {
        self.state = State::Ground;
        let data = std::mem::take(&mut self.dcs);
        let header = data.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
        if picture && data.get(header) == Some(&b'q') {
            self.picture(data);
        }
    }

    /// Places a sixel picture at the cursor and moves the cursor below it.
    fn picture(&mut self, data: Vec<u8>) {
        let (width, height) = sixel_size(&data);
        let cols = width.div_ceil(self.cell.0.max(1)).max(1);
        let rows = height.div_ceil(self.cell.1.max(1)).max(1);
        if cols > SIZE_MAX || rows > SIZE_MAX {
            self.refuse("a picture over 500 cells wide or high was dropped");
            return;
        }
        if self.pictures.len() >= PICTURES_MAX {
            self.pictures.remove(0);
            self.refuse("a screen keeps 8 pictures: the oldest was dropped for a new one");
        }
        let mut whole = Vec::with_capacity(data.len() + 4);
        whole.extend_from_slice(b"\x1bP");
        whole.extend_from_slice(&data);
        whole.extend_from_slice(b"\x1b\\");
        self.pictures.push(Picture {
            id: self.next_picture,
            row: self.row,
            col: self.col,
            cols,
            rows,
            data: Some(whole),
        });
        self.next_picture = self.next_picture.wrapping_add(1);
        // The picture scrolls with the text; one taller than the screen leaves it.
        for _ in 0..rows.min(self.rows + 1) {
            self.linefeed();
        }
        self.col = 0;
        self.wrap_pending = false;
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            // After an intermediate byte only the last byte may come: anything else spoils the sequence.
            b'0'..=b'?' if self.inter != 0 => self.too_long = true,
            b'0'..=b'9' => {
                let digit = u16::from(byte - b'0');
                self.param = Some(self.param.unwrap_or(0).saturating_mul(10).saturating_add(digit));
            }
            b';' | b':' | 0x40..=0x7e => {
                // More parameters than are kept: the whole sequence is ignored, and said so.
                self.too_long |= self.params.len() >= PARAMS_MAX;
                if !self.too_long {
                    self.params.push((self.param.unwrap_or(0), self.sub));
                }
                self.param = None;
                self.sub = byte == b':';
                if byte >= 0x40 {
                    self.state = State::Ground;
                    if self.too_long {
                        self.refuse("a control sequence that was malformed or had more than 32 parameters was ignored");
                    } else {
                        self.dispatch(byte);
                    }
                }
            }
            // `?` and its like count only before the parameters; elsewhere the sequence means nothing.
            b'<'..=b'?' if self.params.is_empty() && self.param.is_none() && self.private == 0 => self.private = byte,
            b'<'..=b'?' => self.too_long = true,
            0x20..=0x2f => self.inter = byte,
            _ => self.state = State::Ground,
        }
    }

    /// Parameter `i`, or `default` when it is missing or zero.
    fn arg(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&(value, _)) if value != 0 => usize::from(value),
            _ => default,
        }
    }

    fn dispatch(&mut self, last: u8) {
        let n = self.arg(0, 1);
        let (row, col) = (self.row, self.col);
        match (self.private, self.inter, last) {
            (0, 0, b'A') => self.move_rows(n, false),
            (0, 0, b'B') => self.move_rows(n, true),
            (0, 0, b'e') => {
                self.row = row.saturating_add(n).min(self.rows - 1);
                self.wrap_pending = false;
            }
            (0, 0, b'C' | b'a') => self.move_to_col(col + n),
            (0, 0, b'D') => self.move_to_col(col.saturating_sub(n)),
            (0, 0, b'E') => {
                self.move_rows(n, true);
                self.move_to_col(0);
            }
            (0, 0, b'F') => {
                self.move_rows(n, false);
                self.move_to_col(0);
            }
            (0, 0, b'G' | b'`') => self.move_to_col(n - 1),
            (0, 0, b'H' | b'f') => self.move_to(n - 1, self.arg(1, 1) - 1),
            (0, 0, b'd') => {
                self.move_to(n - 1, col);
            }
            (0, 0, b'I') => self.tab(n),
            (0, 0, b'Z') => {
                for _ in 0..n {
                    self.col = (0..self.col).rev().find(|&c| self.tabs.get(c) == Some(&true)).unwrap_or(0);
                }
                self.wrap_pending = false;
            }
            (0 | b'?', 0, b'J') => match self.arg(0, 0) {
                0 => {
                    self.erase(row, col, self.cols);
                    (row + 1..self.rows).for_each(|r| self.erase(r, 0, self.cols));
                }
                1 => {
                    (0..row).for_each(|r| self.erase(r, 0, self.cols));
                    self.erase(row, 0, col + 1);
                }
                2 => (0..self.rows).for_each(|r| self.erase(r, 0, self.cols)),
                _ => {}
            },
            (0 | b'?', 0, b'K') => match self.arg(0, 0) {
                0 => self.erase(row, col, self.cols),
                1 => self.erase(row, 0, col + 1),
                2 => self.erase(row, 0, self.cols),
                _ => {}
            },
            (0, 0, b'X') => self.erase(row, col, col.saturating_add(n)),
            (0, 0, b'@' | b'P') => {
                self.split(row, col);
                if last == b'P' {
                    self.split(row, col.saturating_add(n));
                }
                let blank = self.blank();
                if let Some(rest) = self.grid.get_mut(row).and_then(|r| r.get_mut(col..)) {
                    let n = n.min(rest.len());
                    let gap = if last == b'@' {
                        rest.rotate_right(n);
                        rest.get_mut(..n)
                    } else {
                        rest.rotate_left(n);
                        let keep = rest.len() - n;
                        rest.get_mut(keep..)
                    };
                    if let Some(gap) = gap {
                        gap.fill(blank);
                    }
                }
                self.tidy(row);
                self.wrap_pending = false;
            }
            (0, 0, b'L' | b'M') if (self.top..=self.bottom).contains(&row) => {
                if last == b'L' {
                    self.scroll_down(row, n);
                } else {
                    self.scroll_up(row, n);
                }
                self.col = 0;
                self.wrap_pending = false;
            }
            (0, 0, b'S') => self.scroll_up(self.top, n),
            (0, 0, b'T') => self.scroll_down(self.top, n),
            (0, 0, b'b') => {
                for _ in 0..n {
                    self.print(self.last);
                }
            }
            (0, 0, b'c') => self.respond("\x1b[?62;4;22c"),
            (b'>', 0, b'c') => self.respond("\x1b[>1;10;0c"),
            (0, 0, b'g') => match self.arg(0, 0) {
                0 => {
                    if let Some(stop) = self.tabs.get_mut(col) {
                        *stop = false;
                    }
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, 0, b'h' | b'l') => {
                if self.params.iter().any(|p| p.0 == 4) {
                    self.insert = last == b'h';
                }
            }
            (b'?', 0, b'h' | b'l') => {
                for i in 0..self.params.len() {
                    self.private_mode(self.arg(i, 0), last == b'h');
                }
            }
            (0, 0, b'm') => self.sgr(),
            (0, 0, b'n') => match self.arg(0, 0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let row = if self.origin { row.saturating_sub(self.top) } else { row };
                    self.respond(&format!("\x1b[{};{}R", row + 1, col + 1));
                }
                _ => {}
            },
            (b'?', b'$', b'p') => {
                let mode = self.arg(0, 0);
                let set = match mode {
                    1 => Some(self.app_cursor),
                    6 => Some(self.origin),
                    7 => Some(self.autowrap),
                    25 => Some(self.cursor_shown),
                    47 | 1047 | 1049 => Some(self.alt),
                    2004 => Some(self.paste),
                    _ => None,
                };
                self.respond(&format!("\x1b[?{mode};{}$y", set.map_or(0, |on| 2 - u8::from(on))));
            }
            (0, b'!', b'p') => {
                // A soft reset: modes and the scroll region back to how they start.
                (self.insert, self.origin, self.autowrap, self.cursor_shown) = (false, false, true, true);
                (self.app_cursor, self.app_keypad, self.graphics, self.shift) = (false, false, [false; 2], false);
                (self.mouse, self.mouse_sgr) = (0, false);
                (self.top, self.bottom, self.style) = (0, self.rows - 1, Style::default());
                self.saved = [Saved::default(); 2];
            }
            (0, b' ', b'q') => self.cursor_shape = u8::try_from(self.arg(0, 0)).unwrap_or(0),
            (0, 0, b'r') => {
                let (top, bottom) = (n - 1, self.arg(1, self.rows).min(self.rows) - 1);
                if top < bottom {
                    (self.top, self.bottom) = (top, bottom);
                    self.move_to(0, 0);
                }
            }
            (0, 0, b's') => self.save_cursor(),
            (0, 0, b'u') => self.restore_cursor(),
            (0, 0, b't') => {
                let (cols, rows, (w, h)) = (self.cols, self.rows, self.cell);
                match self.arg(0, 0) {
                    14 => self.respond(&format!("\x1b[4;{};{}t", rows * h, cols * w)),
                    16 => self.respond(&format!("\x1b[6;{h};{w}t")),
                    18 => self.respond(&format!("\x1b[8;{rows};{cols}t")),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn move_rows(&mut self, n: usize, down: bool) {
        // The cursor stops at the scroll region's edge, unless it is already past it.
        self.row = if down {
            let limit = if self.row <= self.bottom { self.bottom } else { self.rows - 1 };
            self.row.saturating_add(n).min(limit)
        } else {
            let limit = if self.row >= self.top { self.top } else { 0 };
            self.row.saturating_sub(n).max(limit)
        };
        self.wrap_pending = false;
    }

    fn move_to_col(&mut self, col: usize) {
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        match mode {
            1 => self.app_cursor = on,
            6 => {
                self.origin = on;
                self.move_to(0, 0);
            }
            7 => self.autowrap = on,
            25 => self.cursor_shown = on,
            47 => self.set_alt(on, false),
            1047 => {
                if !on && self.alt {
                    self.clear();
                }
                self.set_alt(on, on);
            }
            1048 if on => self.save_cursor(),
            1048 => self.restore_cursor(),
            1049 if on => {
                self.save_cursor();
                self.set_alt(true, true);
            }
            1049 => {
                self.set_alt(false, false);
                self.restore_cursor();
            }
            2004 => self.paste = on,
            9 | 1000 | 1002 | 1003 => self.mouse = if on { mode as u16 } else { 0 },
            1006 => self.mouse_sgr = on,
            _ => {}
        }
    }

    fn sgr(&mut self) {
        let mut i = 0;
        while let Some(&(code, _)) = self.params.get(i) {
            i += 1;
            let flag = |flags: u16, bit: u16, on: bool| if on { flags | bit } else { flags & !bit };
            let style = &mut self.style;
            match code {
                0 => *style = Style::default(),
                1 => style.flags |= BOLD,
                2 => style.flags |= DIM,
                3 => style.flags |= ITALIC,
                4 | 21 => {
                    // `4:0` is "no underline"; every other kind is drawn as one line.
                    let off = matches!(self.params.get(i), Some(&(0, true)));
                    style.flags = flag(style.flags, UNDERLINE, !off);
                }
                5 | 6 => style.flags |= BLINK,
                7 => style.flags |= REVERSE,
                8 => style.flags |= HIDDEN,
                9 => style.flags |= STRIKE,
                22 => style.flags &= !(BOLD | DIM),
                23 => style.flags &= !ITALIC,
                24 => style.flags &= !UNDERLINE,
                25 => style.flags &= !BLINK,
                27 => style.flags &= !REVERSE,
                28 => style.flags &= !HIDDEN,
                29 => style.flags &= !STRIKE,
                30..=37 => style.fg = Color::Indexed((code - 30) as u8),
                39 => style.fg = Color::Default,
                40..=47 => style.bg = Color::Indexed((code - 40) as u8),
                49 => style.bg = Color::Default,
                90..=97 => style.fg = Color::Indexed((code - 82) as u8),
                100..=107 => style.bg = Color::Indexed((code - 92) as u8),
                38 | 48 | 58 => {
                    let (color, used) = self.long_color(i);
                    i += used;
                    match (code, color) {
                        (38, Some(color)) => self.style.fg = color,
                        (48, Some(color)) => self.style.bg = color,
                        _ => {}
                    }
                }
                _ => {}
            }
            // Skip what is left of this code's own `:` parameters.
            while matches!(self.params.get(i), Some(&(_, true))) {
                i += 1;
            }
        }
    }

    /// The colour after a 38 or 48 at `i`, and how many parameters it used:
    /// `5;N`, `2;R;G;B`, or the same with `:` (where a colour space may come first).
    fn long_color(&self, i: usize) -> (Option<Color>, usize) {
        let value = |k: usize| self.params.get(i + k).and_then(|p| u8::try_from(p.0).ok());
        let rgb = |at: usize| Some(Color::Rgb(value(at)?, value(at + 1)?, value(at + 2)?));
        let colons = self.params.get(i).is_some_and(|p| p.1);
        let count = self.params.iter().skip(i).take_while(|p| p.1).count();
        let left = self.params.len().saturating_sub(i);
        // A colour with a part missing or over 255 sets nothing.
        match self.params.get(i).map(|p| p.0) {
            Some(5) if colons => (value(1).filter(|_| count >= 2).map(Color::Indexed), count),
            Some(5) => (value(1).map(Color::Indexed), left.min(2)),
            Some(2) if colons => {
                let at = if count >= 5 { 2 } else { 1 };
                (rgb(at).filter(|_| at + 2 < count), count)
            }
            Some(2) => (rgb(1), left.min(4)),
            _ => (None, if colons { count } else { 0 }),
        }
    }
}

/// The DEC line-drawing set, for programs that draw boxes with `ESC ( 0`.
fn line_drawing(c: char) -> char {
    const SET: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
    match c {
        '`'..='~' => SET.chars().nth(c as usize - '`' as usize).unwrap_or(c),
        _ => c,
    }
}

/// The size of a sixel picture in pixels: the larger of what its raster
/// attributes say and what its data paints.
fn sixel_size(data: &[u8]) -> (usize, usize) {
    let body = data.iter().copied().skip_while(|&b| b != b'q').skip(1);
    let (mut x, mut band, mut painted, mut declared) = (0usize, 0usize, (0usize, 0usize), (0usize, 0usize));
    // Whose numbers are being read: `!` a repeat count, `#` a colour, `"` the raster attributes.
    let (mut owner, mut numbers) = (b' ', vec![0usize]);
    for byte in body.chain(std::iter::once(b'$')) {
        if byte.is_ascii_digit() {
            if let Some(n) = numbers.last_mut() {
                *n = n.saturating_mul(10).saturating_add(usize::from(byte - b'0')).min(1 << 20);
            }
        } else if byte == b';' {
            if numbers.len() < 8 {
                numbers.push(0);
            }
        } else {
            if owner == b'"' {
                let size = |i: usize| numbers.get(i).copied().unwrap_or(0);
                declared = (declared.0.max(size(2)), declared.1.max(size(3)));
            }
            let repeat = if owner == b'!' { numbers.first().copied().unwrap_or(1).max(1) } else { 1 };
            (owner, numbers) = (b' ', vec![0]);
            match byte {
                b'!' | b'#' | b'"' => owner = byte,
                b'?'..=b'~' => {
                    x = x.saturating_add(repeat);
                    painted = (painted.0.max(x), (band + 1) * 6);
                }
                b'$' => x = 0,
                b'-' => (x, band) = (0, band + 1),
                _ => {}
            }
        }
    }
    (painted.0.max(declared.0), painted.1.max(declared.1))
}
