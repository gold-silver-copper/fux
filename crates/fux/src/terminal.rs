//! Pane terminal emulation and bounded history: a long-lived `vt100::Parser` fed by PTY output,
//! the retained per-row change sequence a frame is diffed against, and the terminal queries
//! answered on the application's behalf. This is the `Terminal` component's inner object
//! (prompt section 1); nothing here touches the World.
//!
//! Adapted from koh (MIT); the upstream notice is retained in LICENSES/koh.txt.

use bevy_ecs::prelude::*;
use vt100::Screen;

use crate::model::{PaneBaseline, clamp_dims};
use crate::wire::{Cell, Color, Cursor, Line, Modes, MouseMode, Style, TerminalDelta};

/// Titles longer than this are truncated (characters, after control characters are dropped).
pub const MAX_TITLE_CHARS: usize = 256;
/// Bound on one OSC/DCS/APC control string before it is dropped.
const MAX_CONTROL_STRING_BYTES: usize = 64 * 1024;

/// `text` without control characters, at most `max_chars` characters: the one rule for titles
/// and other application-supplied strings that reach a screen.
pub fn printable(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

/// A ConEmu / Windows Terminal progress report (`OSC 9;4;<state>;<percent>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub state: u8,
    pub percent: u8,
}

fn parse_progress(params: &[&[u8]]) -> Option<Progress> {
    if params.len() < 3
        || params.first().copied() != Some(b"9".as_slice())
        || params.get(1).copied() != Some(b"4".as_slice())
    {
        return None;
    }
    let number = |index: usize| -> Option<u8> {
        std::str::from_utf8(params.get(index).copied()?)
            .ok()?
            .parse::<u8>()
            .ok()
    };
    let state = number(2)?;
    if state > 4 {
        return None;
    }
    let percent = if state == 0 { 0 } else { number(3)? };
    (percent <= 100).then_some(Progress { state, percent })
}

#[derive(Default)]
struct Callbacks {
    title: String,
    /// Set when a title arrived that differs from the previous one; cleared by
    /// [`Terminal::take_title_change`].
    title_changed: bool,
    bell_count: u64,
    /// Query answers the application expects back on its input.
    host_replies: Vec<u8>,
    progress: Option<Progress>,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        let title = printable(&String::from_utf8_lossy(title), MAX_TITLE_CHARS);
        if title != self.title {
            self.title = title;
            self.title_changed = true;
        }
    }
    fn set_window_icon_name(&mut self, _: &mut Screen, _: &[u8]) {}
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bell_count = self.bell_count.saturating_add(1);
    }
    fn unhandled_osc(&mut self, _: &mut Screen, params: &[&[u8]]) {
        match parse_progress(params) {
            Some(progress) if progress.state == 0 => self.progress = None,
            Some(progress) => self.progress = Some(progress),
            None => {}
        }
    }
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        intermediate: Option<u8>,
        second: Option<u8>,
        params: &[&[u16]],
        action: char,
    ) {
        let first = params
            .first()
            .and_then(|values| values.first())
            .copied()
            .unwrap_or(0);
        match (intermediate, second, action) {
            (None, _, 'n') => match first {
                6 => {
                    let (row, column) = screen.cursor_position();
                    self.host_replies.extend_from_slice(
                        format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(column) + 1)
                            .as_bytes(),
                    );
                }
                5 => self.host_replies.extend_from_slice(b"\x1b[0n"),
                _ => {}
            },
            (Some(b'?'), _, 'n') if first == 6 => {
                let (row, column) = screen.cursor_position();
                self.host_replies.extend_from_slice(
                    format!("\x1b[?{};{}R", u32::from(row) + 1, u32::from(column) + 1).as_bytes(),
                );
            }
            (None, _, 'c') => self.host_replies.extend_from_slice(b"\x1b[?62;1;6c"),
            (Some(b'>'), _, 'c') => self.host_replies.extend_from_slice(b"\x1b[>1;10;0c"),
            (Some(b'?'), Some(b'$'), 'p') => {
                let status: u16 = match first {
                    2004 => {
                        if screen.bracketed_paste() {
                            1
                        } else {
                            2
                        }
                    }
                    _ => 0,
                };
                self.host_replies
                    .extend_from_slice(format!("\x1b[?{first};{status}$y").as_bytes());
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlStringKind {
    Osc,
    Other,
}

/// Bounds control strings independently of pane history and keeps UTF-8 continuation bytes from
/// being mistaken for C1 string introducers or terminators.
#[derive(Default)]
struct ControlStringFilter {
    utf8_remaining: u8,
    pending_escape: bool,
    string: Option<ControlStringKind>,
    string_escape: bool,
    dropping: bool,
    buffered: Vec<u8>,
}

/// Upper bound on continuation bytes still needed by the current UTF-8 prefix.
fn utf8_remaining(remaining: u8, byte: u8) -> u8 {
    if remaining > 0 && (0x80..=0xbf).contains(&byte) {
        remaining - 1
    } else {
        match byte {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        }
    }
}

/// `utf8_remaining` folded over `bytes` from a zero state, without scanning them all: a
/// non-continuation byte resets the state regardless of history, and any run of four or more
/// continuation bytes ends at zero, so only the final four bytes can matter.
fn utf8_remaining_after(bytes: &[u8]) -> u8 {
    let tail = bytes.get(bytes.len().saturating_sub(4)..).unwrap_or(bytes);
    tail.iter()
        .fold(0, |remaining, &byte| utf8_remaining(remaining, byte))
}

impl ControlStringFilter {
    /// Filters `input` into `output` (cleared first); the buffer is the pane's scratch space so a
    /// chunk costs no allocation once it has grown to the chunk size.
    fn process_into(&mut self, input: &[u8], output: &mut Vec<u8>) {
        output.clear();
        for &byte in input {
            let continuation = self.utf8_remaining > 0 && (0x80..=0xbf).contains(&byte);
            self.utf8_remaining = utf8_remaining(self.utf8_remaining, byte);
            if let Some(kind) = self.string {
                let terminated = (byte == 0x9c && !continuation)
                    || (kind == ControlStringKind::Osc && byte == 0x07)
                    || (self.string_escape && byte == b'\\');
                if !self.dropping {
                    if self.buffered.len() < MAX_CONTROL_STRING_BYTES {
                        self.buffered.push(byte);
                    } else {
                        self.buffered.clear();
                        self.dropping = true;
                    }
                }
                self.string_escape = byte == 0x1b;
                if terminated || matches!(byte, 0x18 | 0x1a) {
                    if !self.dropping {
                        output.extend_from_slice(&self.buffered);
                    }
                    self.buffered.clear();
                    self.string = None;
                    self.string_escape = false;
                    self.dropping = false;
                }
                continue;
            }
            if self.pending_escape {
                self.pending_escape = false;
                if let Some(kind) = control_string_introducer(byte) {
                    self.string = Some(kind);
                    self.buffered.extend_from_slice(&[0x1b, byte]);
                    continue;
                }
                output.push(0x1b);
            }
            if byte == 0x1b {
                self.pending_escape = true;
            } else if let Some(kind) = c1_control_string_introducer(byte).filter(|_| !continuation)
            {
                self.string = Some(kind);
                self.buffered.push(byte);
            } else {
                output.push(byte);
            }
        }
    }
}

fn control_string_introducer(byte: u8) -> Option<ControlStringKind> {
    match byte {
        b']' => Some(ControlStringKind::Osc),
        b'P' | b'X' | b'_' | b'^' => Some(ControlStringKind::Other),
        _ => None,
    }
}

fn c1_control_string_introducer(byte: u8) -> Option<ControlStringKind> {
    match byte {
        0x9d => Some(ControlStringKind::Osc),
        0x90 | 0x98 | 0x9e | 0x9f => Some(ControlStringKind::Other),
        _ => None,
    }
}

fn color_of(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn style_of(cell: &vt100::Cell) -> Style {
    let mut attrs = 0;
    if cell.bold() {
        attrs |= Style::BOLD;
    }
    if cell.italic() {
        attrs |= Style::ITALIC;
    }
    if cell.underline() {
        attrs |= Style::UNDERLINE;
    }
    if cell.inverse() {
        attrs |= Style::INVERSE;
    }
    if cell.dim() {
        attrs |= Style::DIM;
    }
    Style {
        fg: color_of(cell.fgcolor()),
        bg: color_of(cell.bgcolor()),
        attrs,
    }
}

fn modes_of(screen: &Screen) -> Modes {
    let mouse = match screen.mouse_protocol_mode() {
        vt100::MouseProtocolMode::None => MouseMode::None,
        vt100::MouseProtocolMode::Press | vt100::MouseProtocolMode::PressRelease => {
            MouseMode::Press
        }
        vt100::MouseProtocolMode::ButtonMotion => MouseMode::Drag,
        vt100::MouseProtocolMode::AnyMotion => MouseMode::Motion,
    };
    Modes {
        application_cursor: screen.application_cursor(),
        bracketed_paste: screen.bracketed_paste(),
        mouse,
        mouse_sgr: screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr,
        alternate_screen: screen.alternate_screen(),
    }
}

fn cursor_of(screen: &Screen) -> Cursor {
    let (row, col) = screen.cursor_position();
    Cursor {
        row,
        col,
        visible: !screen.hide_cursor(),
    }
}

/// A copy of what an observer last saw of the pane: the visible screen with the sequence each
/// row last changed at, plus cursor, modes and title. The sequence advances once per refresh
/// that changed anything, so a frame carries only the rows a viewer has not seen.
#[derive(Default)]
struct Grid {
    rows: u16,
    cols: u16,
    /// Retained vt100 cells (`None` only for a position the screen did not report).
    cells: Vec<Option<vt100::Cell>>,
    wrapped: Vec<bool>,
    changed: Vec<u64>,
    seq: u64,
    /// Sequence at which the title last changed.
    title_seq: u64,
    cursor: Cursor,
    modes: Modes,
    title: String,
}

impl Grid {
    /// Brings the grid up to date with `screen` and `title`; when anything changed the sequence
    /// advances, the rows that differ are stamped with it, and `true` is returned.
    fn refresh(&mut self, screen: &Screen, title: &str) -> bool {
        let next = self.seq.saturating_add(1);
        let (rows, cols) = screen.size();
        let width = usize::from(cols);
        let mut changed = false;
        if (rows, cols) != (self.rows, self.cols) {
            self.rows = rows;
            self.cols = cols;
            self.cells.clear();
            self.cells.resize(usize::from(rows) * width, None);
            self.wrapped.clear();
            self.wrapped.resize(usize::from(rows), false);
            self.changed.clear();
            self.changed.resize(usize::from(rows), next);
            for row in 0..rows {
                self.copy_row(screen, row, width);
            }
            changed = true;
        } else {
            for row in 0..rows {
                let wrapped = screen.row_wrapped(row);
                let start = usize::from(row) * width;
                let differs = self.wrapped.get(usize::from(row)) != Some(&wrapped)
                    || (0..cols).any(|col| {
                        self.cells
                            .get(start + usize::from(col))
                            .is_none_or(|current| current.as_ref() != screen.cell(row, col))
                    });
                if differs {
                    self.copy_row(screen, row, width);
                    if let Some(stamp) = self.changed.get_mut(usize::from(row)) {
                        *stamp = next;
                    }
                    changed = true;
                }
            }
        }
        let cursor = cursor_of(screen);
        let modes = modes_of(screen);
        if cursor != self.cursor || modes != self.modes || title != self.title {
            self.cursor = cursor;
            self.modes = modes;
            if title != self.title {
                self.title.clear();
                self.title.push_str(title);
                self.title_seq = next;
            }
            changed = true;
        }
        if changed {
            self.seq = next;
        }
        changed
    }

    fn copy_row(&mut self, screen: &Screen, row: u16, width: usize) {
        let start = usize::from(row) * width;
        for col in 0..self.cols {
            if let Some(slot) = self.cells.get_mut(start + usize::from(col)) {
                *slot = screen.cell(row, col).cloned();
            }
        }
        if let Some(flag) = self.wrapped.get_mut(usize::from(row)) {
            *flag = screen.row_wrapped(row);
        }
    }

    /// The retained cells of `row` (missing positions read as blanks).
    fn row_cells(&self, row: u16) -> impl Iterator<Item = Option<&vt100::Cell>> {
        let width = usize::from(self.cols);
        self.cells
            .iter()
            .skip(usize::from(row) * width)
            .take(width)
            .map(Option::as_ref)
    }

    /// Encodes `row` into `line`, reusing its cell vector (resized to the number of emitted
    /// cells, so no allocation once it has grown to the row width). Wide glyphs get `width: 2`
    /// and their spacer cell is skipped; content a cell cannot carry (control characters) shows
    /// as a blank of the same style.
    fn encode_row(&self, row: u16, line: &mut Line) {
        line.row = row;
        let emitted = self
            .row_cells(row)
            .filter(|cell| !cell.is_some_and(vt100::Cell::is_wide_continuation))
            .count();
        line.cells.resize_with(emitted, Cell::default);
        let cells = self
            .row_cells(row)
            .filter(|cell| !cell.is_some_and(vt100::Cell::is_wide_continuation));
        for (out, cell) in line.cells.iter_mut().zip(cells) {
            out.text.clear();
            let Some(cell) = cell else {
                out.width = 1;
                out.style = Style::default();
                continue;
            };
            out.style = style_of(cell);
            out.width = if cell.is_wide() { 2 } else { 1 };
            let text = cell.contents();
            if !text.chars().any(char::is_control) {
                out.text.push_str(text);
            }
        }
    }
}

/// The authoritative emulator and bounded history for one pane.
#[derive(Component)]
pub struct Terminal {
    parser: vt100::Parser<Callbacks>,
    filter: ControlStringFilter,
    parser_utf8_remaining: u8,
    grid: Grid,
    /// Filtered output of the chunk being fed; reused across chunks.
    scratch: Vec<u8>,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16, scrollback_lines: usize) -> Self {
        let (rows, cols) = clamp_dims(rows, cols);
        let mut terminal = Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                scrollback_lines,
                Callbacks::default(),
            ),
            filter: ControlStringFilter::default(),
            parser_utf8_remaining: 0,
            grid: Grid::default(),
            scratch: Vec::new(),
        };
        terminal.refresh();
        terminal
    }

    /// Feeds application output; the sequence advances only if something observable changed.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.process(bytes);
        self.refresh();
    }

    /// Feeds several chunks in order and refreshes the retained grid once.
    pub fn feed_all<'a>(&mut self, chunks: impl IntoIterator<Item = &'a [u8]>) {
        for chunk in chunks {
            self.process(chunk);
        }
        self.refresh();
    }

    fn refresh(&mut self) {
        self.grid
            .refresh(self.parser.screen(), &self.parser.callbacks().title);
    }

    /// A `vt100` panic on hostile output is contained: the chunk is dropped and later output
    /// repaints.
    fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.filter.process_into(bytes, &mut self.scratch);
        let contained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Complete split UTF-8 characters separately: vte lookahead can otherwise
            // consume bytes following the completed character. Keep the reusable buffer.
            let mut rest = self.scratch.as_slice();
            while self.parser_utf8_remaining > 0 && !rest.is_empty() {
                let (next, suffix) = rest.split_at(1);
                self.parser.process(next);
                if let Some(&byte) = next.first() {
                    self.parser_utf8_remaining = utf8_remaining(self.parser_utf8_remaining, byte);
                }
                rest = suffix;
            }
            if !rest.is_empty() {
                self.parser.process(rest);
                self.parser_utf8_remaining = utf8_remaining_after(rest);
            }
        }));
        if contained.is_err() {
            bevy_log::error!("terminal emulator rejected application output; chunk dropped");
        }
    }

    /// Resizes the emulator (clamped by [`clamp_dims`]); every row is stamped with a new
    /// sequence so the next delta is full.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = clamp_dims(rows, cols);
        if self.parser.screen().size() != (rows, cols) {
            self.parser.screen_mut().set_size(rows, cols);
            self.refresh();
        }
    }

    /// The output sequence: advances when a refresh found anything an observer can see changed.
    pub fn seq(&self) -> u64 {
        self.grid.seq
    }

    pub fn rows(&self) -> u16 {
        self.grid.rows
    }

    pub fn cols(&self) -> u16 {
        self.grid.cols
    }

    /// The latest OSC 0/2 title, bounded and printable; `None` until one was set.
    pub fn title(&self) -> Option<&str> {
        let title = self.parser.callbacks().title.as_str();
        (!title.is_empty()).then_some(title)
    }

    /// The new title, once, after it changed.
    pub fn take_title_change(&mut self) -> Option<String> {
        let callbacks = self.parser.callbacks_mut();
        std::mem::take(&mut callbacks.title_changed).then(|| callbacks.title.clone())
    }

    pub fn bell_count(&self) -> u64 {
        self.parser.callbacks().bell_count
    }

    pub fn progress(&self) -> Option<Progress> {
        self.parser.callbacks().progress
    }

    /// Moves the bytes the application must receive in reply to its queries (DSR, DA, DECRQM)
    /// into `out`; `false` when there are none.
    pub fn take_host_replies(&mut self, out: &mut Vec<u8>) -> bool {
        let replies = &mut self.parser.callbacks_mut().host_replies;
        if replies.is_empty() {
            return false;
        }
        out.clear();
        out.append(replies);
        true
    }

    pub fn modes(&self) -> Modes {
        self.grid.modes
    }

    pub fn cursor(&self) -> Cursor {
        self.grid.cursor
    }

    /// Writes the rows changed since `baseline` (`None` = everything) into `out`, reusing its
    /// line and cell vectors; `pane` and `process` are left for the caller. Returns `false` when
    /// the baseline already saw this sequence at this size. The title is carried only when the
    /// delta is full or the title changed after the baseline.
    pub fn write_delta(&self, baseline: Option<PaneBaseline>, out: &mut TerminalDelta) -> bool {
        let grid = &self.grid;
        let full = baseline.is_none_or(|b| b.rows != grid.rows || b.cols != grid.cols);
        let since = if full { None } else { baseline.map(|b| b.seq) };
        if since.is_some_and(|seq| seq >= grid.seq) {
            return false;
        }
        out.seq = grid.seq;
        out.rows = grid.rows;
        out.cols = grid.cols;
        out.full = full;
        out.cursor = grid.cursor;
        out.modes = grid.modes;
        out.title = (since.is_none_or(|seq| grid.title_seq > seq) && !grid.title.is_empty())
            .then(|| grid.title.clone());
        // Rows whose stamp is newer than the baseline; every row when the delta is full.
        let changed_rows = || {
            grid.changed
                .iter()
                .enumerate()
                .filter(move |(_, stamp)| since.is_none_or(|seq| **stamp > seq))
                .filter_map(|(row, _)| u16::try_from(row).ok())
        };
        out.lines.resize_with(changed_rows().count(), Line::default);
        for (line, row) in out.lines.iter_mut().zip(changed_rows()) {
            grid.encode_row(row, line);
        }
        true
    }

    /// Every visible row as plain text (for final records and captures).
    pub fn screen_lines(&self) -> Vec<String> {
        let screen = self.parser.screen();
        screen.rows(0, self.grid.cols).collect()
    }

    /// The plain text of the row `offset` rows above the live screen (`0` is the top visible
    /// row); `None` when the retained history is shorter. The live viewport is restored before
    /// returning, which is why this needs `&mut self`.
    pub fn scrollback_line(&mut self, offset: usize) -> Option<String> {
        let screen = self.parser.screen_mut();
        let previous = screen.scrollback();
        screen.set_scrollback(offset);
        let line = (screen.scrollback() == offset)
            .then(|| screen.rows(0, self.grid.cols).next())
            .flatten();
        screen.set_scrollback(previous);
        line
    }

    /// Raw screen access for tests and captures.
    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line) -> String {
        line.cells.iter().map(|cell| cell.text.as_str()).collect()
    }

    fn baseline(terminal: &Terminal) -> PaneBaseline {
        PaneBaseline {
            seq: terminal.seq(),
            rows: terminal.rows(),
            cols: terminal.cols(),
        }
    }

    #[test]
    fn utf8_remaining_after_equals_full_fold() {
        let full = |bytes: &[u8]| bytes.iter().fold(0, |r, &b| utf8_remaining(r, b));
        let classes = [
            0x41u8, 0x80, 0xbf, 0xc2, 0xdf, 0xe0, 0xef, 0xf0, 0xf4, 0xf5, 0xff,
        ];
        for &a in &classes {
            for &b in &classes {
                for &c in &classes {
                    for &d in &classes {
                        for &e in &classes {
                            let bytes = [a, b, c, d, e];
                            assert_eq!(utf8_remaining_after(&bytes), full(&bytes), "{bytes:x?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn delta_carries_only_rows_changed_since_the_baseline() {
        let mut terminal = Terminal::new(4, 10, 0);
        terminal.feed(b"one\r\ntwo\r\nthree");
        let mut delta = TerminalDelta {
            pane: crate::model::PaneId(1),
            seq: 0,
            rows: 0,
            cols: 0,
            full: false,
            lines: Vec::new(),
            cursor: Cursor::default(),
            modes: Modes::default(),
            title: None,
            process: crate::wire::ProcessSummary::Live,
        };
        assert!(terminal.write_delta(None, &mut delta));
        assert!(delta.full);
        assert_eq!(delta.lines.len(), 4);
        assert_eq!(text_of(&delta.lines[0]).trim_end(), "one");
        assert_eq!(
            delta.cursor,
            Cursor {
                row: 2,
                col: 5,
                visible: true
            }
        );
        let seen = baseline(&terminal);
        assert!(!terminal.write_delta(Some(seen), &mut delta), "nothing new");
        terminal.feed(b"\x1b[2;1HTWO");
        assert!(terminal.write_delta(Some(seen), &mut delta));
        assert!(!delta.full);
        let rows: Vec<(u16, String)> = delta
            .lines
            .iter()
            .map(|line| (line.row, text_of(line).trim_end().to_owned()))
            .collect();
        assert_eq!(rows, vec![(1, "TWO".to_owned())]);
        assert!(delta.title.is_none());
        // Cursor-only movement advances the sequence with no rows.
        let seen = baseline(&terminal);
        terminal.feed(b"\x1b[4;1H");
        assert!(terminal.write_delta(Some(seen), &mut delta));
        assert!(delta.lines.is_empty());
        assert_eq!(delta.cursor.row, 3);
    }

    #[test]
    fn resize_forces_a_full_delta_and_clamps() {
        let mut terminal = Terminal::new(4, 10, 0);
        terminal.feed(b"hello");
        let seen = baseline(&terminal);
        let mut delta = TerminalDelta {
            pane: crate::model::PaneId(1),
            seq: 0,
            rows: 0,
            cols: 0,
            full: false,
            lines: Vec::new(),
            cursor: Cursor::default(),
            modes: Modes::default(),
            title: None,
            process: crate::wire::ProcessSummary::Live,
        };
        terminal.resize(5, 12);
        assert!(terminal.write_delta(Some(seen), &mut delta));
        assert!(delta.full);
        assert_eq!((delta.rows, delta.cols), (5, 12));
        assert_eq!(delta.lines.len(), 5);
        // Same size again: the baseline at the new size sees no change.
        let seen = baseline(&terminal);
        terminal.resize(5, 12);
        assert!(!terminal.write_delta(Some(seen), &mut delta));
        terminal.resize(65000, 1);
        assert_eq!(
            (terminal.rows(), terminal.cols()),
            (crate::model::MAX_DIM, crate::model::MIN_DIM)
        );
    }

    #[test]
    fn title_is_bounded_printable_and_reported_once() {
        let mut terminal = Terminal::new(24, 80, 0);
        assert!(terminal.title().is_none());
        assert!(terminal.take_title_change().is_none());
        let long: String = "t\u{85}x".repeat(MAX_TITLE_CHARS);
        terminal.feed(format!("\x1b]2;{long}\x1b\\").as_bytes());
        let title = terminal.title().expect("title set").to_owned();
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.chars().all(|c| !c.is_control()));
        assert_eq!(terminal.take_title_change(), Some(title));
        assert!(terminal.take_title_change().is_none());
        terminal.feed(b"\x1b]2;same\x07\x1b]2;same\x07");
        assert_eq!(terminal.take_title_change().as_deref(), Some("same"));
        assert!(terminal.take_title_change().is_none());
    }

    #[test]
    fn bell_and_host_replies() {
        let mut terminal = Terminal::new(24, 80, 0);
        terminal.feed(b"\x07\x07\x1b[5;3H\x1b[6n");
        assert_eq!(terminal.bell_count(), 2);
        let mut replies = Vec::new();
        assert!(terminal.take_host_replies(&mut replies));
        assert_eq!(replies, b"\x1b[5;3R");
        assert!(!terminal.take_host_replies(&mut replies));
    }

    #[test]
    fn wide_glyphs_are_two_columns_wide_with_no_spacer_cell() {
        let mut terminal = Terminal::new(2, 6, 0);
        terminal.feed("a\u{1F41D}b".as_bytes());
        let mut delta = TerminalDelta {
            pane: crate::model::PaneId(1),
            seq: 0,
            rows: 0,
            cols: 0,
            full: false,
            lines: Vec::new(),
            cursor: Cursor::default(),
            modes: Modes::default(),
            title: None,
            process: crate::wire::ProcessSummary::Live,
        };
        assert!(terminal.write_delta(None, &mut delta));
        let cells = &delta.lines[0].cells;
        let widths: Vec<u8> = cells.iter().map(|c| c.width).collect();
        assert_eq!(widths, vec![1, 2, 1, 1, 1]);
        assert_eq!(cells[1].text, "\u{1F41D}");
        assert_eq!(cells[2].text, "b");
        assert_eq!(widths.iter().map(|w| usize::from(*w)).sum::<usize>(), 6);
    }

    #[test]
    fn styles_map_to_wire_attributes() {
        let mut terminal = Terminal::new(1, 4, 0);
        terminal.feed(b"\x1b[1;3;4;7;31;48;2;1;2;3mx\x1b[0;2my");
        let mut delta = TerminalDelta {
            pane: crate::model::PaneId(1),
            seq: 0,
            rows: 0,
            cols: 0,
            full: false,
            lines: Vec::new(),
            cursor: Cursor::default(),
            modes: Modes::default(),
            title: None,
            process: crate::wire::ProcessSummary::Live,
        };
        assert!(terminal.write_delta(None, &mut delta));
        let cell = &delta.lines[0].cells[0];
        assert_eq!(cell.text, "x");
        assert_eq!(
            cell.style,
            Style {
                fg: Color::Indexed(1),
                bg: Color::Rgb(1, 2, 3),
                attrs: Style::BOLD | Style::ITALIC | Style::UNDERLINE | Style::INVERSE,
            }
        );
        let dim = &delta.lines[0].cells[1];
        assert_eq!(dim.text, "y");
        assert_eq!(
            dim.style,
            Style {
                attrs: Style::DIM,
                ..Style::default()
            }
        );
        assert_eq!(delta.lines[0].cells[2].style, Style::default());
    }

    #[test]
    fn modes_follow_the_screen() {
        let mut terminal = Terminal::new(4, 10, 0);
        terminal.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1h");
        assert_eq!(
            terminal.modes(),
            Modes {
                application_cursor: true,
                bracketed_paste: true,
                mouse: MouseMode::Drag,
                mouse_sgr: true,
                alternate_screen: true,
            }
        );
    }

    #[test]
    fn unterminated_control_strings_are_bounded_and_reset() {
        let mut terminal = Terminal::new(24, 80, 0);
        terminal.feed(b"before\x1b]");
        for _ in 0..32 {
            terminal.feed(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            assert!(terminal.filter.buffered.len() <= MAX_CONTROL_STRING_BYTES);
        }
        assert!(terminal.filter.dropping);
        terminal.feed(b"\x1b\\after");
        assert!(!terminal.filter.dropping);
        assert!(terminal.screen().contents().contains("beforeafter"));
        for introducer in [&b"\x1bX"[..], &b"\x98"[..]] {
            let mut terminal = Terminal::new(24, 80, 0);
            terminal.feed(introducer);
            for _ in 0..5 {
                terminal.feed(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            }
            assert!(terminal.filter.dropping);
            terminal.feed(b"\x1b\\ok");
            assert!(terminal.screen().contents().contains("ok"));
        }
    }

    #[test]
    fn utf8_continuations_do_not_open_or_terminate_control_strings() {
        let text = "beforeАИМНОП😀 prefix Ü 🐝 end";
        for split in 0..=text.len() {
            let mut terminal = Terminal::new(24, 80, 0);
            let (first, second) = text.as_bytes().split_at(split);
            terminal.feed(first);
            terminal.feed(second);
            assert_eq!(terminal.screen().contents(), text, "split {split}");
        }
        let mut terminal = Terminal::new(24, 80, 0);
        for byte in "\x1b]2;М title\x1b\\visible".as_bytes() {
            terminal.feed(&[*byte]);
        }
        assert_eq!(terminal.title(), Some("М title"));
        assert_eq!(terminal.screen().contents(), "visible");
        terminal.feed(b"\x1b]2;");
        terminal.feed(&vec![b'x'; MAX_CONTROL_STRING_BYTES]);
        for byte in "Мstill hidden".as_bytes() {
            terminal.feed(&[*byte]);
        }
        assert!(terminal.filter.dropping);
        terminal.feed(b"\x1b\\after");
        assert_eq!(terminal.title(), Some("М title"));
        assert_eq!(terminal.screen().contents(), "visibleafter");
    }

    #[test]
    fn split_sequences_keep_their_meaning_across_feeds() {
        let mut terminal = Terminal::new(24, 80, 0);
        terminal.feed_all([
            &b"\x1b"[..],
            &b"]2;split"[..],
            &b" title\x1b"[..],
            &b"\\\x1b"[..],
            &b"[5;3Hok"[..],
        ]);
        assert_eq!(terminal.title(), Some("split title"));
        assert!(terminal.screen().contents().contains("ok"));
        assert_eq!(
            terminal.cursor(),
            Cursor {
                row: 4,
                col: 4,
                visible: true
            }
        );
    }

    #[test]
    fn scrollback_is_bounded_and_history_reads_restore_the_live_viewport() {
        let mut terminal = Terminal::new(4, 10, 5);
        for line in 0..20 {
            terminal.feed(format!("line{line}\r\n").as_bytes());
        }
        // The live screen shows line16..line19 plus the empty prompt row.
        assert_eq!(terminal.scrollback_line(0).as_deref(), Some("line17"));
        assert_eq!(terminal.scrollback_line(1).as_deref(), Some("line16"));
        assert_eq!(terminal.scrollback_line(5).as_deref(), Some("line12"));
        assert!(terminal.scrollback_line(6).is_none(), "history is 5 rows");
        assert_eq!(terminal.screen().scrollback(), 0);
        assert_eq!(terminal.screen_lines()[0], "line17");
        assert_eq!(terminal.screen_lines().len(), 4);
    }

    #[test]
    fn progress_reports_are_parsed_and_bounded() {
        let mut terminal = Terminal::new(2, 10, 0);
        terminal.feed(b"\x1b]9;4;1;50\x1b\\");
        assert_eq!(
            terminal.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        terminal.feed(b"\x1b]9;4;1;150\x1b\\");
        assert_eq!(
            terminal.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        terminal.feed(b"\x1b]9;4;0\x1b\\");
        assert_eq!(terminal.progress(), None);
    }
}
