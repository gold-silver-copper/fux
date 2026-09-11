//! Pane terminal emulation and bounded history: a long-lived `vt100::Parser` fed by PTY output
//! plus the terminal queries answered on the application's behalf.
//!
//! Adapted from koh (MIT); the upstream notice is retained in LICENSES/koh.txt.

use crate::proto::control::{CaptureLine, CaptureRow};
use crate::view::{
    CellKind, CellStyle, Cursor, Line, MAX_CELL_TEXT_BYTES, PaneModes, PaneUpdate, classify,
    push_wire,
};
use vt100::Screen;

pub const MIN_DIM: u16 = 2;
/// Peer-controlled dimensions are clamped here so vt100 never allocates an unbounded grid.
pub const MAX_DIM: u16 = 512;
pub const MAX_CLIPBOARD_BASE64: usize = 16 * 1024;
const MAX_TITLE_CHARS: usize = 256;
/// Bound on one OSC/DCS/APC control string before it is dropped.
const MAX_CONTROL_STRING_BYTES: usize = 64 * 1024;

pub fn clamp_dims(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(MIN_DIM, MAX_DIM), cols.clamp(MIN_DIM, MAX_DIM))
}

/// A ConEmu / Windows Terminal progress report (`OSC 9;4;<state>;<percent>`), exposed through the
/// control listing for external observers.
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

fn title_from(bytes: &[u8]) -> String {
    crate::view::printable(&String::from_utf8_lossy(bytes), MAX_TITLE_CHARS)
}

#[derive(Default)]
struct Callbacks {
    title: String,
    clipboard: String,
    bell_count: u64,
    /// Query answers the application expects back on its input.
    host_replies: Vec<u8>,
    progress: Option<Progress>,
}

impl vt100::Callbacks for Callbacks {
    fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
        self.title = title_from(title);
    }
    fn set_window_icon_name(&mut self, _: &mut Screen, _: &[u8]) {}
    fn audible_bell(&mut self, _: &mut Screen) {
        self.bell_count = self.bell_count.saturating_add(1);
    }
    fn copy_to_clipboard(&mut self, _: &mut Screen, _selection: &[u8], data: &[u8]) {
        if data.len() <= MAX_CLIPBOARD_BASE64 {
            self.clipboard = String::from_utf8_lossy(data).into_owned();
        }
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

/// One cell of the retained grid: the vt100 cell's text and attributes without its allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridCell {
    text: [u8; MAX_CELL_TEXT_BYTES],
    len: u8,
    kind: CellKind,
    style: CellStyle,
}

impl Default for GridCell {
    fn default() -> Self {
        Self {
            text: [0; MAX_CELL_TEXT_BYTES],
            len: 0,
            kind: CellKind::Blank,
            style: CellStyle::default(),
        }
    }
}

impl GridCell {
    fn from_vt100(cell: &vt100::Cell) -> Self {
        let mut text = [0; MAX_CELL_TEXT_BYTES];
        let (contents, kind) = classify(cell);
        let contents = contents.as_bytes();
        let len = contents.len().min(MAX_CELL_TEXT_BYTES);
        if let (Some(target), Some(source)) = (text.get_mut(..len), contents.get(..len)) {
            target.copy_from_slice(source);
        }
        Self {
            text,
            len: u8::try_from(len).unwrap_or(0),
            kind,
            style: CellStyle::from_vt100(cell),
        }
    }

    fn text(&self) -> &str {
        self.text
            .get(..usize::from(self.len))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }

    /// Exactly `*self == Self::from_vt100(cell)` for every retained cell (which is canonical:
    /// zero padding past `len`) without building the copy: the style is compared first because
    /// it is cheapest, then the classified kind and retained text.
    fn matches_vt100(&self, cell: &vt100::Cell) -> bool {
        if self.style != CellStyle::from_vt100(cell) {
            return false;
        }
        let (contents, kind) = classify(cell);
        let contents = contents.as_bytes();
        let len = contents.len().min(MAX_CELL_TEXT_BYTES);
        kind == self.kind
            && usize::from(self.len) == len
            && self.text.get(..len) == contents.get(..len)
    }
}

/// A copy of what an observer last saw of the pane: the visible screen with the sequence each
/// row last changed at, plus cursor, modes, title and exit status. The sequence advances once per
/// refresh that changed anything, so a frame carries only the rows a viewer has not seen, a
/// `capture` can return the rows changed since a sequence, and `list` reports the sequence.
#[derive(Default)]
pub struct Grid {
    rows: u16,
    columns: u16,
    cells: Vec<GridCell>,
    wrapped: Vec<bool>,
    changed: Vec<u64>,
    seq: u64,
    cursor: Cursor,
    modes: PaneModes,
    title: String,
    exit: Option<u32>,
}

impl Grid {
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }

    /// The output sequence: advances when a refresh found anything an observer can see changed.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Brings the grid up to date with `screen`, `title` and `exit`; when anything changed the
    /// sequence advances, the rows that differ are stamped with it, and `true` is returned.
    fn refresh(&mut self, screen: &vt100::Screen, title: &str, exit: Option<u32>) -> bool {
        let next = self.seq.saturating_add(1);
        let (rows, columns) = screen.size();
        let width = usize::from(columns);
        let mut changed = false;
        if (rows, columns) != (self.rows, self.columns) {
            self.rows = rows;
            self.columns = columns;
            self.cells = vec![GridCell::default(); usize::from(rows) * width];
            self.wrapped = vec![false; usize::from(rows)];
            self.changed = vec![next; usize::from(rows)];
            for row in 0..rows {
                self.copy_row(screen, row, width);
            }
            changed = true;
        } else {
            for row in 0..rows {
                let wrapped = screen.row_wrapped(row);
                let start = usize::from(row) * width;
                let differs = self.wrapped.get(usize::from(row)) != Some(&wrapped)
                    || (0..columns).any(|column| {
                        match (
                            self.cells.get(start + usize::from(column)),
                            screen.cell(row, column),
                        ) {
                            (Some(current), Some(fresh)) => !current.matches_vt100(fresh),
                            // Unreachable while the retained size matches the screen; kept
                            // defensive: a missing screen cell would be copied as default.
                            (Some(current), None) => *current != GridCell::default(),
                            (None, _) => true,
                        }
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
        let (row, column) = screen.cursor_position();
        let cursor = Cursor {
            row,
            column,
            hidden: screen.hide_cursor(),
        };
        let modes = PaneModes::from_vt100(screen);
        if cursor != self.cursor || modes != self.modes || title != self.title || exit != self.exit
        {
            self.cursor = cursor;
            self.modes = modes;
            if title != self.title {
                self.title.clear();
                self.title.push_str(title);
            }
            self.exit = exit;
            changed = true;
        }
        if changed {
            self.seq = next;
        }
        changed
    }

    /// The rows changed after `since` (every row when `since` is `None`) as plain text with the
    /// row's wrap flag: wide continuations are skipped and trailing blanks trimmed.
    #[must_use]
    pub fn rows_since(&self, since: Option<u64>) -> Vec<CaptureRow> {
        let width = usize::from(self.columns);
        let mut rows = Vec::new();
        for row in 0..self.rows {
            let index = usize::from(row);
            if since
                .is_some_and(|since| self.changed.get(index).is_none_or(|stamp| *stamp <= since))
            {
                continue;
            }
            let mut text = String::with_capacity(width);
            for cell in self.cells.iter().skip(index * width).take(width) {
                match cell.kind {
                    CellKind::WideContinuation => {}
                    CellKind::Blank => text.push(' '),
                    CellKind::Text | CellKind::WideLeading => text.push_str(cell.text()),
                }
            }
            let trimmed = text.trim_end_matches(' ').len();
            text.truncate(trimmed);
            rows.push(CaptureRow {
                row,
                text,
                wrapped: self.wrapped.get(index).copied().unwrap_or(false),
            });
        }
        rows
    }

    /// Every visible row as wire cells, top to bottom, keeping whole rows while the JSON encoding
    /// of the kept rows stays within `max_bytes`; returns the rows and whether any were dropped.
    #[must_use]
    pub fn capture_lines(&self, max_bytes: usize) -> (Vec<CaptureLine>, bool) {
        let width = usize::from(self.columns);
        let mut lines = Vec::with_capacity(usize::from(self.rows));
        let mut bytes = 0_usize;
        for row in 0..self.rows {
            let index = usize::from(row);
            let mut cells = Vec::new();
            for cell in self.cells.iter().skip(index * width).take(width) {
                push_wire(&mut cells, 0, cell.text(), cell.kind, cell.style);
            }
            let line = CaptureLine {
                row,
                wrapped: self.wrapped.get(index).copied().unwrap_or(false),
                cells,
            };
            // Each row is one array element; the separating comma counts toward the bound.
            let encoded = serde_json::to_vec(&line).map_or(usize::MAX, |json| json.len());
            bytes = bytes
                .saturating_add(encoded)
                .saturating_add(usize::from(row > 0));
            if bytes > max_bytes {
                return (lines, true);
            }
            lines.push(line);
        }
        (lines, false)
    }

    fn copy_row(&mut self, screen: &vt100::Screen, row: u16, width: usize) {
        let start = usize::from(row) * width;
        for column in 0..self.columns {
            if let Some(slot) = self.cells.get_mut(start + usize::from(column)) {
                *slot = screen
                    .cell(row, column)
                    .map(GridCell::from_vt100)
                    .unwrap_or_default();
            }
        }
        if let Some(flag) = self.wrapped.get_mut(usize::from(row)) {
            *flag = screen.row_wrapped(row);
        }
    }

    /// The rows changed after `since` (every row when `since` is `None`) as a pane update carrying
    /// the retained cursor, modes, title and exit status.
    #[must_use]
    pub fn update(&self, since: Option<u64>) -> PaneUpdate {
        let mut update = PaneUpdate {
            rows: self.rows,
            columns: self.columns,
            full: since.is_none(),
            cursor: self.cursor,
            modes: self.modes,
            title: self.title.clone(),
            exit: self.exit,
            ..PaneUpdate::default()
        };
        let width = usize::from(self.columns);
        for row in 0..self.rows {
            let index = usize::from(row);
            if since
                .is_some_and(|since| self.changed.get(index).is_none_or(|stamp| *stamp <= since))
            {
                continue;
            }
            let start = update.cells.len();
            for cell in self.cells.iter().skip(index * width).take(width) {
                push_wire(&mut update.cells, start, cell.text(), cell.kind, cell.style);
            }
            update.lines.push(Line {
                row,
                wrapped: self.wrapped.get(index).copied().unwrap_or(false),
                len: u16::try_from(update.cells.len() - start).unwrap_or(u16::MAX),
            });
        }
        update
    }
}

/// A bounded history viewport, not an append-only output transcript. Its first row is
/// `scrollback_offset` rows above the live screen and its height is `rows`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSnapshot {
    pub text: String,
    pub revision: u64,
    pub rows: u16,
    pub columns: u16,
    pub scrollback_offset: u32,
    pub title: String,
    pub progress: Option<(u8, u8)>,
    pub unchanged: bool,
    /// Whether the returned text hit the byte limit. For unchanged responses consult the cache.
    pub truncated: bool,
}

/// The authoritative emulator and bounded history for one pane.
pub struct ServerTerminal {
    parser: vt100::Parser<Callbacks>,
    filter: ControlStringFilter,
    parser_utf8_remaining: u8,
    revision: u64,
    history_limit: usize,
    grid: Grid,
    /// Filtered output of the chunk being fed; reused across chunks.
    scratch: Vec<u8>,
}

impl ServerTerminal {
    #[must_use]
    pub fn new(rows: u16, cols: u16, history_limit: usize) -> Self {
        let (rows, cols) = clamp_dims(rows, cols);
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                history_limit,
                Callbacks::default(),
            ),
            filter: ControlStringFilter::default(),
            parser_utf8_remaining: 0,
            revision: 0,
            history_limit,
            grid: Grid::default(),
            scratch: Vec::new(),
        }
    }

    /// Brings the retained grid up to date with the live screen, `title` and `exit`; returns
    /// whether the output sequence advanced.
    pub fn refresh_grid(&mut self, title: &str, exit: Option<u32>) -> bool {
        self.grid.refresh(self.parser.screen(), title, exit)
    }

    #[must_use]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Feeds application output. A `vt100` panic on hostile output is contained: the chunk is
    /// dropped and later output repaints.
    pub fn process(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.revision = self.revision.wrapping_add(1);
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
            tracing::error!("terminal emulator rejected application output; chunk dropped");
        }
    }

    /// Bytes the application must receive in reply to its queries (DSR, DA, DECRQM).
    pub fn take_host_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.parser.callbacks_mut().host_replies)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = clamp_dims(rows, cols);
        if self.size() != (rows, cols) {
            self.revision = self.revision.wrapping_add(1);
            self.parser.screen_mut().set_size(rows, cols);
        }
    }

    /// A change token scoped to this terminal lifetime; not a timestamp or byte count.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    #[must_use]
    pub fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.parser.callbacks().title
    }

    #[must_use]
    pub fn clipboard(&self) -> &str {
        &self.parser.callbacks().clipboard
    }

    #[must_use]
    pub fn bell_count(&self) -> u64 {
        self.parser.callbacks().bell_count
    }

    #[must_use]
    pub fn progress(&self) -> Option<Progress> {
        self.parser.callbacks().progress
    }

    #[must_use]
    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Temporarily selects a bounded history viewport and reads it, restoring the live viewport
    /// before returning. The offset is clamped to the retained history.
    pub fn with_history_screen<R>(&mut self, offset: usize, read: impl FnOnce(&Screen) -> R) -> R {
        let previous = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(offset.min(self.history_limit));
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(self.parser.screen())));
        self.parser.screen_mut().set_scrollback(previous);
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Plain or attribute-preserving text of a viewport shifted up by at most `scrollback`
    /// retained history rows, truncated to `max_bytes` on a character boundary.
    pub fn capture(&mut self, scrollback: usize, attrs: bool, max_bytes: usize) -> String {
        self.capture_snapshot(scrollback, attrs, max_bytes, None)
            .text
    }

    /// Text and metadata from one terminal revision. Conditional callers must reuse capture
    /// options and invalidate cached revisions when the pane/server identity changes.
    pub fn capture_snapshot(
        &mut self,
        scrollback: usize,
        attrs: bool,
        max_bytes: usize,
        if_revision: Option<u64>,
    ) -> CaptureSnapshot {
        let (rows, columns) = self.size();
        let progress = self.progress().map(|p| (p.state, p.percent));
        let mut snapshot = CaptureSnapshot {
            text: String::new(),
            revision: self.revision,
            rows,
            columns,
            scrollback_offset: 0,
            title: self.title().to_owned(),
            progress,
            unchanged: if_revision == Some(self.revision),
            truncated: false,
        };
        let columns = usize::from(columns).max(1);
        let bytes_per_row = if attrs {
            columns.saturating_mul(128)
        } else {
            columns.saturating_mul(4).saturating_add(1)
        };
        let bounded_rows = max_bytes.saturating_div(bytes_per_row).saturating_add(1);
        let offset = scrollback.min(self.history_limit).min(bounded_rows);
        self.with_history_screen(offset, |screen| {
            snapshot.scrollback_offset = u32::try_from(screen.scrollback()).unwrap_or(u32::MAX);
            if !snapshot.unchanged {
                snapshot.text = if attrs {
                    String::from_utf8_lossy(&screen.contents_formatted()).into_owned()
                } else {
                    screen.contents()
                };
                snapshot.truncated = snapshot.text.len() > max_bytes;
                if snapshot.truncated {
                    snapshot
                        .text
                        .truncate(snapshot.text.floor_char_boundary(max_bytes));
                }
            }
        });
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::indexing_slicing)]
    fn grid_cell_match_equals_constructed_comparison() {
        let mut terminal = ServerTerminal::new(6, 24, 100);
        terminal.process(
            "plain \u{1b}[1;31mbold red\u{1b}[0m \u{1b}[4;7mul inv\u{1b}[0m 日本語 e\u{301} \u{1b}[2;3mdim\r\n"
                .as_bytes(),
        );
        terminal.process(
            "\u{1b}[44mbg\u{1b}[0m x\u{1f600}y \u{1b}[38;5;200m256\u{1b}[0m\r\n".as_bytes(),
        );
        terminal.refresh_grid("", None);
        let screen = terminal.parser.screen();
        let width = usize::from(terminal.grid.columns);
        let mut cells = 0;
        for row in 0..terminal.grid.rows {
            for column in 0..terminal.grid.columns {
                let (Some(fresh), Some(current)) = (
                    screen.cell(row, column),
                    terminal
                        .grid
                        .cells
                        .get(usize::from(row) * width + usize::from(column))
                        .copied(),
                ) else {
                    // The final count proves every visible cell was actually compared.
                    continue;
                };
                assert!(current.matches_vt100(fresh));
                assert!(current == GridCell::from_vt100(fresh));
                // Every field perturbation must be detected exactly as the full comparison.
                for perturb in 0..4 {
                    let mut altered = current;
                    match perturb {
                        0 => altered.style.bold = !altered.style.bold,
                        1 => {
                            altered.kind = if altered.kind == CellKind::Blank {
                                CellKind::Text
                            } else {
                                CellKind::Blank
                            }
                        }
                        2 => altered.len = altered.len.wrapping_add(1),
                        // Retained cells are canonical (zero padding past `len`), so only
                        // bytes inside `len` are reachable perturbations.
                        _ if altered.len > 0 => altered.text[0] = altered.text[0].wrapping_add(1),
                        _ => altered.style.inverse = !altered.style.inverse,
                    }
                    assert_eq!(
                        altered.matches_vt100(fresh),
                        altered == GridCell::from_vt100(fresh),
                        "row {row} column {column} perturb {perturb}"
                    );
                }
                cells += 1;
            }
        }
        assert_eq!(cells, 6 * 24);
    }

    #[test]
    fn utf8_remaining_after_equals_full_fold() {
        let full = |bytes: &[u8]| bytes.iter().fold(0, |r, &b| utf8_remaining(r, b));
        let classes = [
            0x41u8, 0x80, 0xbf, 0xc2, 0xdf, 0xe0, 0xef, 0xf0, 0xf4, 0xf5, 0xff,
        ];
        let mut sequence = Vec::new();
        assert_eq!(utf8_remaining_after(&sequence), full(&sequence));
        // Every sequence of up to six class representatives, including ones whose
        // history a naive last-byte check would get wrong.
        for length in 1..=6 {
            let total = classes.len().pow(length);
            for index in 0..total {
                sequence.clear();
                let mut rest = index;
                for _ in 0..length {
                    sequence.push(classes.get(rest % classes.len()).copied().unwrap_or(0));
                    rest /= classes.len();
                }
                assert_eq!(
                    utf8_remaining_after(&sequence),
                    full(&sequence),
                    "{sequence:x?}"
                );
            }
        }
    }
    use crate::view::PaneView;
    use proptest::prelude::*;

    #[test]
    fn capture_revision_is_distinct_from_grid_sequence() {
        let mut terminal = ServerTerminal::new(4, 8, 20);
        terminal.process(b"hello");
        terminal.refresh_grid("", None);
        let grid_sequence = terminal.grid().seq();
        let first = terminal.capture_snapshot(0, false, 4096, None);

        // Progress belongs to coherent capture metadata, but does not change grid cells.
        terminal.process(b"\x1b]9;4;1;50\x07");
        assert!(!terminal.refresh_grid("", None));
        assert_eq!(terminal.grid().seq(), grid_sequence);
        let changed = terminal.capture_snapshot(0, false, 4096, Some(first.revision));
        assert!(!changed.unchanged);
        assert_eq!(changed.progress, Some((1, 50)));
        assert_eq!(changed.text, first.text);

        // Grid refresh and temporary history selection must not themselves dirty captures.
        terminal.with_history_screen(3, |_| ());
        assert_eq!(terminal.revision(), changed.revision);
        assert_eq!(terminal.screen().scrollback(), 0);
        assert!(
            terminal
                .capture_snapshot(0, false, 4096, Some(changed.revision))
                .unchanged
        );
    }

    #[test]
    fn history_changes_invalidate_capture_even_when_live_grid_is_identical() {
        let mut terminal = ServerTerminal::new(2, 8, 20);
        terminal.process(b"old\r\nscreen");
        terminal.process(b"\x1b[2J\x1b[H");
        terminal.refresh_grid("", None);
        let before = terminal.capture_snapshot(1, false, 4096, None);
        let seq = terminal.grid().seq();
        terminal.process(b"new\r\nline\r\n\x1b[2J\x1b[H");
        assert!(!terminal.refresh_grid("", None));
        assert_eq!(terminal.grid().seq(), seq);
        let after = terminal.capture_snapshot(1, false, 4096, Some(before.revision));
        assert!(!after.unchanged);
        assert_ne!(after.text, before.text);
        assert_eq!(terminal.screen().scrollback(), 0);
    }

    #[test]
    fn conditional_capture_tracks_output_resize_and_metadata_without_moving_history() {
        let mut terminal = ServerTerminal::new(4, 8, 20);
        terminal.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\x1b]2;title\x07");
        let first = terminal.capture_snapshot(1, false, 4096, None);
        assert_eq!((first.rows, first.columns), (4, 8));
        assert_eq!(first.scrollback_offset, 1);
        assert_eq!(first.title, "title");
        assert!(!first.unchanged);
        assert_eq!(terminal.screen().scrollback(), 0);
        let cached = terminal.capture_snapshot(1, false, 4096, Some(first.revision));
        assert!(cached.unchanged);
        assert!(cached.text.is_empty());
        assert_eq!(cached.scrollback_offset, first.scrollback_offset);
        terminal.resize(4, 8);
        assert_eq!(terminal.revision(), first.revision);
        terminal.resize(6, 10);
        let resized = terminal.capture_snapshot(1, false, 4096, Some(first.revision));
        assert!(!resized.unchanged);
        assert_eq!((resized.rows, resized.columns), (6, 10));
        terminal.process(b"\x1b]9;4;1;50\x07");
        let progress = terminal.capture_snapshot(0, true, 4096, Some(resized.revision));
        assert!(!progress.unchanged);
        assert_eq!(progress.progress, Some((1, 50)));
        assert!(terminal.capture_snapshot(0, true, 1, None).truncated);
    }

    /// The rows of a view, as text plus wrap flags, for comparing deltas with the screen.
    fn rows_of(view: &PaneView) -> Vec<(String, bool)> {
        (0..view.rows)
            .map(|row| {
                let text: String = (0..view.columns)
                    .filter_map(|column| view.cell(row, column))
                    .map(|cell| format!("{}:{:?}:{:?}|", cell.text, cell.kind, cell.style))
                    .collect();
                (
                    text,
                    view.wrapped_rows
                        .get(usize::from(row))
                        .copied()
                        .unwrap_or(false),
                )
            })
            .collect()
    }

    fn edit(terminal: &mut ServerTerminal, op: u8, byte: u8) {
        match op % 6 {
            0 => terminal.process(&[b'a' + byte % 26]),
            1 => terminal.process(b"\r\n"),
            2 => terminal.process("\u{65e5}".as_bytes()),
            3 => terminal.process(format!("\x1b[{};{}H", byte % 5 + 1, byte % 11 + 1).as_bytes()),
            4 => terminal.process(b"\x1b[1;31mX\x1b[m"),
            _ => terminal.process(b"\x1b[2J"),
        }
    }

    proptest! {
        #[test]
        fn deltas_and_merged_deltas_reproduce_the_screen(
            ops in proptest::collection::vec((0_u8..6, any::<u8>(), any::<bool>()), 1..60)
        ) {
            let mut terminal = ServerTerminal::new(4, 10, 0);
            // `held` is what a viewer applied; `pending` is the update queued for it (merged).
            let mut held: Option<PaneView> = None;
            let mut held_seq: Option<u64> = None;
            let mut pending: Option<PaneUpdate> = None;
            for (op, byte, deliver) in ops {
                edit(&mut terminal, op, byte);
                terminal.refresh_grid("", None);
                let update = terminal.grid().update(held_seq);
                held_seq = Some(terminal.grid().seq());
                match &mut pending {
                    Some(queued) => queued.merge(update),
                    None => pending = Some(update),
                }
                if deliver {
                    let Some(update) = pending.take() else { continue };
                    let mut view = held.take().unwrap_or_default();
                    prop_assert!(view.apply(&update).is_ok(), "{update:?}");
                    let direct = PaneView::from_screen(terminal.screen(), "", 0, None)
                        .unwrap_or_default();
                    prop_assert_eq!(rows_of(&view), rows_of(&direct));
                    held = Some(view);
                }
            }
        }
    }

    #[test]
    fn the_sequence_advances_only_for_observable_changes() {
        let mut terminal = ServerTerminal::new(4, 10, 0);
        assert!(
            terminal.refresh_grid("", None),
            "first refresh sizes the grid"
        );
        let first = terminal.grid().seq();
        assert!(!terminal.refresh_grid("", None), "nothing changed");
        assert_eq!(terminal.grid().seq(), first);
        terminal.process(b"\x07");
        assert!(!terminal.refresh_grid("", None), "a bell is not visible");
        terminal.process(b"hi");
        assert!(terminal.refresh_grid("", None));
        assert_eq!(terminal.grid().seq(), first + 1);
        assert_eq!(
            terminal.grid().rows_since(Some(first)),
            vec![CaptureRow {
                row: 0,
                text: "hi".into(),
                wrapped: false
            }]
        );
        assert!(terminal.grid().rows_since(Some(first + 1)).is_empty());
        // Cursor moves, titles and the exit status are observable too.
        terminal.process(b"\x1b[3;1H");
        assert!(terminal.refresh_grid("", None));
        assert_eq!(terminal.grid().cursor().row, 2);
        assert!(terminal.grid().rows_since(Some(first + 1)).is_empty());
        assert!(terminal.refresh_grid("title", None));
        assert!(terminal.refresh_grid("title", Some(3)));
        assert!(!terminal.refresh_grid("title", Some(3)));
        assert_eq!(terminal.grid().seq(), first + 4);
        // A resize stamps every row.
        terminal.resize(5, 10);
        assert!(terminal.refresh_grid("title", Some(3)));
        assert_eq!(terminal.grid().rows_since(Some(first + 4)).len(), 5);
        // Wide characters take one entry in the row text and blanks are trimmed.
        terminal.process("\x1b[H\u{65e5}x  ".as_bytes());
        terminal.refresh_grid("title", Some(3));
        assert_eq!(
            terminal
                .grid()
                .rows_since(None)
                .first()
                .map(|row| row.text.as_str()),
            Some("\u{65e5}x")
        );
    }

    proptest! {
        /// Folding `capture {since}` over any split of an output stream reproduces the rows a
        /// full capture returns.
        #[test]
        fn row_captures_since_fold_to_the_full_capture(
            ops in proptest::collection::vec((0_u8..6, any::<u8>(), any::<bool>()), 1..60)
        ) {
            let mut terminal = ServerTerminal::new(4, 10, 0);
            let mut folded: std::collections::BTreeMap<u16, CaptureRow> =
                std::collections::BTreeMap::new();
            let mut seen: Option<u64> = None;
            for (op, byte, read) in ops {
                edit(&mut terminal, op, byte);
                if !read {
                    continue;
                }
                terminal.refresh_grid("", None);
                for row in terminal.grid().rows_since(seen) {
                    folded.insert(row.row, row);
                }
                seen = Some(terminal.grid().seq());
            }
            terminal.refresh_grid("", None);
            for row in terminal.grid().rows_since(seen) {
                folded.insert(row.row, row);
            }
            let full: Vec<CaptureRow> = terminal.grid().rows_since(None);
            let folded: Vec<CaptureRow> = folded.into_values().collect();
            prop_assert_eq!(folded, full);
        }
    }

    #[test]
    fn answers_cursor_position_and_device_attributes() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"\x1b[5;3H\x1b[6n");
        assert_eq!(terminal.take_host_replies(), b"\x1b[5;3R");
        assert!(terminal.take_host_replies().is_empty());
        terminal.process(b"\x1b[c\x1b[>c\x1b[?2004$p");
        assert_eq!(
            terminal.take_host_replies(),
            b"\x1b[?62;1;6c\x1b[>1;10;0c\x1b[?2004;2$y"
        );
    }

    #[test]
    fn title_bell_clipboard_and_progress_are_captured_and_bounded() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"\x1b]2;my-title\x07\x07\x1b]52;c;aGVsbG8=\x07\x1b]9;4;1;50\x1b\\");
        assert_eq!(terminal.title(), "my-title");
        assert_eq!(terminal.bell_count(), 1);
        assert_eq!(terminal.clipboard(), "aGVsbG8=");
        assert_eq!(
            terminal.progress(),
            Some(Progress {
                state: 1,
                percent: 50
            })
        );
        terminal.process(b"\x1b]9;4;0\x07");
        assert_eq!(terminal.progress(), None);
        let huge = "x".repeat(MAX_TITLE_CHARS + 500);
        terminal.process(format!("\x1b]2;{huge}\x07").as_bytes());
        assert_eq!(terminal.title().chars().count(), MAX_TITLE_CHARS);
        let big = "A".repeat(MAX_CLIPBOARD_BASE64 + 1);
        terminal.process(format!("\x1b]52;c;{big}\x07").as_bytes());
        assert_eq!(terminal.clipboard(), "aGVsbG8=");
    }

    #[test]
    fn resize_clamps_and_wide_output_never_panics() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.resize(65000, 65000);
        assert_eq!(terminal.size(), (MAX_DIM, MAX_DIM));
        terminal.resize(0, 0);
        assert_eq!(terminal.size(), (MIN_DIM, MIN_DIM));
        terminal.process("AAAA日本🦀\r\nBBBB\r\n".repeat(8).as_bytes());
        terminal.resize(40, 120);
        assert_eq!(terminal.size(), (40, 120));
    }

    #[test]
    fn unterminated_control_strings_are_bounded_and_reset() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        terminal.process(b"before\x1b]");
        for _ in 0..32 {
            terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            assert!(terminal.filter.buffered.len() <= MAX_CONTROL_STRING_BYTES);
        }
        assert!(terminal.filter.dropping);
        terminal.process(b"\x1b\\after");
        assert!(!terminal.filter.dropping);
        assert!(terminal.screen().contents().contains("beforeafter"));
    }

    #[test]
    fn utf8_continuations_do_not_open_or_terminate_control_strings() {
        let text = "beforeАИМНОП😀 prefix Ü 🐝 end";
        for split in 0..=text.len() {
            let mut terminal = ServerTerminal::new(24, 80, 0);
            let (first, second) = text.as_bytes().split_at(split);
            terminal.process(first);
            terminal.process(second);
            assert_eq!(terminal.screen().contents(), text, "split {split}");
        }
        let mut terminal = ServerTerminal::new(24, 80, 0);
        for byte in "\x1b]2;М title\x1b\\visible".as_bytes() {
            terminal.process(&[*byte]);
        }
        assert_eq!(terminal.title(), "М title");
        assert_eq!(terminal.screen().contents(), "visible");
        terminal.process(b"\x1b]2;");
        terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES]);
        for byte in "Мstill hidden".as_bytes() {
            terminal.process(&[*byte]);
        }
        assert!(terminal.filter.dropping);
        terminal.process(b"\x1b\\after");
        assert_eq!(terminal.title(), "М title");
        assert_eq!(terminal.screen().contents(), "visibleafter");
    }

    #[test]
    fn split_osc_and_csi_sequences_keep_their_meaning() {
        let mut terminal = ServerTerminal::new(24, 80, 0);
        for chunk in [
            &b"\x1b"[..],
            &b"]2;split"[..],
            &b" title\x1b"[..],
            &b"\\\x1b"[..],
            &b"[5;3Hok"[..],
        ] {
            terminal.process(chunk);
        }
        assert_eq!(terminal.title(), "split title");
        assert!(terminal.screen().contents().contains("ok"));
        for introducer in [&b"\x1bX"[..], &b"\x98"[..]] {
            let mut terminal = ServerTerminal::new(24, 80, 0);
            terminal.process(introducer);
            for _ in 0..5 {
                terminal.process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4]);
            }
            assert!(terminal.filter.dropping);
            terminal.process(b"\x1b\\ok");
            assert!(terminal.screen().contents().contains("ok"));
        }
    }

    #[test]
    fn history_reads_restore_the_live_viewport_and_capture_is_bounded() {
        let mut terminal = ServerTerminal::new(4, 10, 100);
        for line in 0..20 {
            terminal.process(format!("line{line}\r\n").as_bytes());
        }
        let live = terminal.screen().contents();
        let older = terminal.with_history_screen(10, |screen| screen.contents());
        assert!(older.contains("line8"), "{older}");
        assert_eq!(terminal.screen().contents(), live);
        assert_eq!(terminal.screen().scrollback(), 0);
        let capture = terminal.capture(50, false, 40);
        assert!(capture.len() <= 40);
        let full = terminal.capture(50, false, 100_000);
        assert!(full.contains("line0"));
        assert!(terminal.with_history_screen(usize::MAX, |screen| screen.scrollback()) <= 100);
    }
}
