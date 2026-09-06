//! Wire-level snapshot types shared by the session server and the viewer: pane cells, per-viewer
//! frames and private history views. Frames are derived data; the ECS World is authoritative.

use crate::ids::{PaneId, TabId};
use crate::layout::Rect;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

pub const MAX_DIM: u16 = crate::terminal::MAX_DIM;
pub const MAX_CELL_TEXT_BYTES: usize = 22;
pub const MAX_TITLE_BYTES: usize = 1024;

/// `text` without control characters, at most `max_chars` characters: the one rule for titles,
/// labels and notices that cross the wire or reach the screen.
#[must_use]
pub fn printable(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}
pub const MAX_LABEL_BYTES: usize = 128;
pub const MAX_PANES: usize = 128;
pub const MAX_TABS: usize = 32;
/// One maximum-size pane across a frame (`MAX_DIM * MAX_DIM`).
pub const MAX_TOTAL_CELLS: usize = 262_144;
pub const MAX_MESSAGE_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellKind {
    #[default]
    Blank,
    Text,
    WideLeading,
    WideContinuation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Color {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(index) => Self::Indexed(index),
            vt100::Color::Rgb(red, green, blue) => Self::Rgb(red, green, blue),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellStyle {
    pub foreground: Color,
    pub background: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl CellStyle {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    #[must_use]
    pub fn from_vt100(cell: &vt100::Cell) -> Self {
        Self {
            foreground: cell.fgcolor().into(),
            background: cell.bgcolor().into(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }
}

/// The kind a vt100 cell has on the wire and in a grid.
#[must_use]
pub fn kind_of(cell: &vt100::Cell) -> CellKind {
    if cell.is_wide_continuation() {
        CellKind::WideContinuation
    } else if cell.is_wide() {
        CellKind::WideLeading
    } else if cell.has_contents() {
        CellKind::Text
    } else {
        CellKind::Blank
    }
}

/// The text and kind a vt100 cell contributes to a frame. Content a cell cannot carry (a
/// zero-width or multi-grapheme sequence, a control character, a width that disagrees with the
/// emulator's) shows as a blank of the same style instead of invalidating the frame.
#[must_use]
pub fn classify(cell: &vt100::Cell) -> (&str, CellKind) {
    let text = cell.contents();
    let kind = kind_of(cell);
    let carried = match kind {
        CellKind::Text => one_grapheme_of_width(text, 1),
        CellKind::WideLeading => one_grapheme_of_width(text, 2),
        CellKind::Blank | CellKind::WideContinuation => text.is_empty(),
    };
    match (carried, kind) {
        (true, kind) => (text, kind),
        (false, CellKind::WideContinuation) => ("", CellKind::WideContinuation),
        (false, _) => ("", CellKind::Blank),
    }
}

fn one_grapheme_of_width(text: &str, width: usize) -> bool {
    if let [byte] = text.as_bytes() {
        // Printable ASCII: the common case needs no segmentation.
        return width == 1 && (0x20..0x7f).contains(byte);
    }
    text.len() <= MAX_CELL_TEXT_BYTES
        && !text.chars().any(char::is_control)
        && text.graphemes(true).count() == 1
        && text.width() == width
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub kind: CellKind,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: String::new(),
            kind: CellKind::Blank,
            style: CellStyle::default(),
        }
    }
}

impl Cell {
    #[must_use]
    pub fn from_vt100(cell: &vt100::Cell) -> Self {
        let (text, kind) = classify(cell);
        Self {
            text: text.to_owned(),
            kind,
            style: CellStyle::from_vt100(cell),
        }
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.text.len() <= MAX_CELL_TEXT_BYTES
            && !self.text.chars().any(char::is_control)
            && match self.kind {
                CellKind::Blank | CellKind::WideContinuation => self.text.is_empty(),
                CellKind::Text => self.text.graphemes(true).count() == 1 && self.text.width() == 1,
                CellKind::WideLeading => {
                    self.text.graphemes(true).count() == 1 && self.text.width() == 2
                }
            }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub hidden: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneModes {
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
}

impl PaneModes {
    #[must_use]
    pub fn from_vt100(screen: &vt100::Screen) -> Self {
        let mouse_mode = match screen.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => MouseMode::None,
            vt100::MouseProtocolMode::Press => MouseMode::Press,
            vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
            vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        };
        let mouse_encoding = match screen.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
            vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
            vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
        };
        Self {
            alternate_screen: screen.alternate_screen(),
            application_keypad: screen.application_keypad(),
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_mode,
            mouse_encoding,
        }
    }
}

/// One rendered pane surface, either the live screen or a private history viewport.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneView {
    pub rows: u16,
    pub columns: u16,
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    pub modes: PaneModes,
    pub title: String,
    /// One flag per row; true means the row continues into the next without a newline.
    pub wrapped_rows: Vec<bool>,
    /// History rows above the live screen that this view starts at (0 = live).
    pub offset: u32,
    /// Final process status once the pane's process has exited.
    pub exit: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("pane view exceeds frame bounds")]
pub struct PaneViewError;

impl PaneView {
    pub fn from_screen(
        screen: &vt100::Screen,
        title: &str,
        offset: u32,
        exit: Option<u32>,
    ) -> Result<Self, PaneViewError> {
        let (rows, columns) = screen.size();
        if rows > MAX_DIM || columns > MAX_DIM || title.len() > MAX_TITLE_BYTES {
            return Err(PaneViewError);
        }
        let capacity = usize::from(rows)
            .checked_mul(usize::from(columns))
            .ok_or(PaneViewError)?;
        let mut cells = Vec::with_capacity(capacity);
        for row in 0..rows {
            for column in 0..columns {
                cells.push(
                    screen
                        .cell(row, column)
                        .map_or_else(Cell::default, Cell::from_vt100),
                );
            }
        }
        let (cursor_row, cursor_column) = screen.cursor_position();
        let view = Self {
            rows,
            columns,
            cells,
            cursor: Cursor {
                row: cursor_row,
                column: cursor_column,
                hidden: screen.hide_cursor(),
            },
            modes: PaneModes::from_vt100(screen),
            title: title.to_owned(),
            wrapped_rows: (0..rows).map(|row| screen.row_wrapped(row)).collect(),
            offset,
            exit,
        };
        view.valid().then_some(view).ok_or(PaneViewError)
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.shape_valid() && self.cells.iter().all(Cell::valid)
    }

    /// The bounds and counts alone; cells are validated when they are produced.
    #[must_use]
    pub fn shape_valid(&self) -> bool {
        self.rows <= MAX_DIM
            && self.columns <= MAX_DIM
            && self.cells.len() == usize::from(self.rows) * usize::from(self.columns)
            && self.wrapped_rows.len() == usize::from(self.rows)
            && self.title.len() <= MAX_TITLE_BYTES
    }

    /// A view from a full update; every row must be carried exactly once.
    pub fn from_update(update: &PaneUpdate) -> Result<Self, PaneViewError> {
        if !update.full {
            return Err(PaneViewError);
        }
        let mut view = Self {
            rows: update.rows,
            columns: update.columns,
            cells: vec![Cell::default(); usize::from(update.rows) * usize::from(update.columns)],
            wrapped_rows: vec![false; usize::from(update.rows)],
            ..Self::default()
        };
        if update.rows > MAX_DIM || update.columns > MAX_DIM {
            return Err(PaneViewError);
        }
        view.apply_rows(update)?;
        view.apply_meta(update);
        if update.lines.len() != usize::from(update.rows) {
            return Err(PaneViewError);
        }
        Ok(view)
    }

    /// Applies an update: a full one replaces the view, a delta replaces the carried rows.
    pub fn apply(&mut self, update: &PaneUpdate) -> Result<(), PaneViewError> {
        if update.full {
            *self = Self::from_update(update)?;
            return Ok(());
        }
        if (update.rows, update.columns) != (self.rows, self.columns) {
            return Err(PaneViewError);
        }
        self.apply_rows(update)?;
        self.apply_meta(update);
        Ok(())
    }

    fn apply_meta(&mut self, update: &PaneUpdate) {
        self.cursor = update.cursor;
        self.modes = update.modes;
        self.title.clone_from(&update.title);
        self.offset = update.offset;
        self.exit = update.exit;
    }

    fn apply_rows(&mut self, update: &PaneUpdate) -> Result<(), PaneViewError> {
        if update.title.len() > MAX_TITLE_BYTES {
            return Err(PaneViewError);
        }
        let columns = usize::from(self.columns);
        let mut cells = update.cells.as_slice();
        let mut seen = vec![false; usize::from(self.rows)];
        for line in &update.lines {
            let row = usize::from(line.row);
            let flag = seen.get_mut(row).ok_or(PaneViewError)?;
            if *flag {
                return Err(PaneViewError);
            }
            *flag = true;
            let (carried, rest) = cells
                .split_at_checked(usize::from(line.len))
                .ok_or(PaneViewError)?;
            cells = rest;
            let target = self
                .cells
                .get_mut(row * columns..(row + 1) * columns)
                .ok_or(PaneViewError)?;
            expand(carried, target)?;
            if let Some(wrapped) = self.wrapped_rows.get_mut(row) {
                *wrapped = line.wrapped;
            }
        }
        if !cells.is_empty() {
            return Err(PaneViewError);
        }
        Ok(())
    }

    #[must_use]
    pub fn cell(&self, row: u16, column: u16) -> Option<&Cell> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        let index = usize::from(row)
            .checked_mul(usize::from(self.columns))?
            .checked_add(usize::from(column))?;
        self.cells.get(index)
    }

    /// Text of the rectangle from `start` to `end` (inclusive, row-major), joining wrapped rows
    /// and skipping wide-cell continuations so copied text matches what is displayed.
    #[must_use]
    pub fn text_between(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut output = String::new();
        let last_row = end.0.min(self.rows.saturating_sub(1));
        for row in start.0..=last_row {
            let first = if row == start.0 { start.1 } else { 0 };
            let last = if row == end.0 {
                end.1
            } else {
                self.columns.saturating_sub(1)
            };
            let mut line = String::new();
            for column in first..=last.min(self.columns.saturating_sub(1)) {
                if let Some(cell) = self.cell(row, column)
                    && cell.kind != CellKind::WideContinuation
                {
                    if cell.kind == CellKind::Blank {
                        line.push(' ');
                    } else {
                        line.push_str(&cell.text);
                    }
                }
            }
            output.push_str(line.trim_end_matches(' '));
            let wrapped = self
                .wrapped_rows
                .get(usize::from(row))
                .copied()
                .unwrap_or(false);
            if row != last_row && !wrapped {
                output.push('\n');
            }
        }
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabEntry {
    pub id: TabId,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneRect {
    pub pane: PaneId,
    pub rect: Rect,
}

/// Everything one viewer needs to paint: its own tab, focus and layout over the shared panes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub workspace: String,
    /// Increases with every published frame for this viewer; mouse events echo it.
    pub generation: u64,
    pub tabs: Vec<TabEntry>,
    pub active_tab: Option<TabId>,
    pub focused: Option<PaneId>,
    /// Content rectangles of the panes visible in the active tab; the viewer's last row is the
    /// bar and the one-cell gaps between siblings carry the separators.
    pub layout: Vec<PaneRect>,
    pub panes: BTreeMap<PaneId, PaneView>,
    /// Set once the workspace has retired; viewers exit with this code.
    pub exit_code: Option<u32>,
    /// A short, sanitized notice for this viewer (for example a rejected command).
    pub message: Option<String>,
}

impl Frame {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.tabs.len() <= MAX_TABS
            && self.panes.len() <= MAX_PANES
            && self.layout.len() <= MAX_PANES
            && self.workspace.len() <= 64
            && self
                .tabs
                .iter()
                .all(|tab| tab.label.len() <= MAX_LABEL_BYTES)
            && self.panes.values().all(PaneView::shape_valid)
            && self
                .panes
                .values()
                .map(|pane| pane.cells.len())
                .sum::<usize>()
                <= MAX_TOTAL_CELLS
            && self
                .layout
                .iter()
                .all(|entry| self.panes.contains_key(&entry.pane))
            && self
                .active_tab
                .is_none_or(|active| self.tabs.iter().any(|tab| tab.id == active))
            && self
                .focused
                .is_none_or(|focused| self.layout.iter().any(|entry| entry.pane == focused))
            && self
                .message
                .as_ref()
                .is_none_or(|message| message.len() <= MAX_MESSAGE_BYTES)
    }

    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&PaneView> {
        self.panes.get(&id)
    }

    /// Applies one update from the server. A full update replaces every pane; a delta replaces
    /// the carried rows of the panes it names. Panes that left the layout are dropped.
    pub fn apply(&mut self, update: FrameUpdate) -> Result<(), FrameError> {
        if update.full {
            self.panes.clear();
        }
        for (id, pane) in update.panes {
            match self.panes.get_mut(&id) {
                Some(view) if !pane.full => view.apply(&pane)?,
                _ => {
                    self.panes.insert(id, PaneView::from_update(&pane)?);
                }
            }
        }
        self.workspace = update.workspace;
        self.generation = update.generation;
        self.tabs = update.tabs;
        self.active_tab = update.active_tab;
        self.focused = update.focused;
        self.layout = update.layout;
        self.exit_code = update.exit_code;
        self.message = update.message;
        let shown: Vec<PaneId> = self.layout.iter().map(|entry| entry.pane).collect();
        self.panes.retain(|id, _| shown.contains(id));
        if self.valid() {
            Ok(())
        } else {
            Err(FrameError::Invalid)
        }
    }

    #[must_use]
    pub fn focused_pane(&self) -> Option<&PaneView> {
        self.panes.get(&self.focused?)
    }

    #[must_use]
    pub fn rect(&self, id: PaneId) -> Option<Rect> {
        self.layout
            .iter()
            .find(|entry| entry.pane == id)
            .map(|entry| entry.rect)
    }

    /// The pane whose outer rectangle contains the zero-based screen position.
    #[must_use]
    pub fn pane_at(&self, x: u16, y: u16) -> Option<PaneRect> {
        self.layout
            .iter()
            .copied()
            .find(|entry| entry.rect.contains(x, y))
    }
}

/// A frame or update that violates the documented bounds.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error(transparent)]
    Pane(#[from] PaneViewError),
    #[error("frame violates its bounds")]
    Invalid,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_u16(value: &u16) -> bool {
    *value == 0
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// One carried row of a pane update: the row, whether it wraps into the next, and how many wire
/// cells encode it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    pub row: u16,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrapped: bool,
    pub len: u16,
}

/// A cell on the wire: text with its kind and style, or a run of blank cells sharing a style.
/// Absent fields take their defaults, so a blank default cell is `{}` and a run `{"run":40}`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `text` implies `Text`, no text implies `Blank`; only the wide kinds are spelled out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CellKind>,
    #[serde(default, skip_serializing_if = "CellStyle::is_default")]
    pub style: CellStyle,
    /// Number of cells a blank stands for; 0 and 1 mean one.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub run: u16,
}

impl WireCell {
    fn matches_blank(&self, kind: CellKind, style: CellStyle) -> bool {
        self.text.is_none() && self.kind.unwrap_or_default() == kind && self.style == style
    }
}

/// Appends one cell to the wire row that starts at `row_start`, extending a run of equal blanks
/// within that row instead of adding a cell.
pub fn push_wire(
    cells: &mut Vec<WireCell>,
    row_start: usize,
    text: &str,
    kind: CellKind,
    style: CellStyle,
) {
    if text.is_empty() {
        if let Some(last) = cells.get_mut(row_start..).and_then(<[WireCell]>::last_mut)
            && last.matches_blank(kind, style)
            && last.run < u16::MAX
        {
            last.run = last.run.max(1).saturating_add(1);
            return;
        }
        cells.push(WireCell {
            text: None,
            kind: (kind != CellKind::Blank).then_some(kind),
            style,
            run: 0,
        });
        return;
    }
    cells.push(WireCell {
        text: Some(text.to_owned()),
        kind: (kind != CellKind::Text).then_some(kind),
        style,
        run: 0,
    });
}

/// Expands one carried row into exactly `target.len()` validated cells.
fn expand(carried: &[WireCell], target: &mut [Cell]) -> Result<(), PaneViewError> {
    let mut column = 0_usize;
    for wire in carried {
        let cell = match &wire.text {
            Some(text) => Cell {
                text: text.clone(),
                kind: wire.kind.unwrap_or(CellKind::Text),
                style: wire.style,
            },
            None => Cell {
                text: String::new(),
                kind: wire.kind.unwrap_or(CellKind::Blank),
                style: wire.style,
            },
        };
        if !cell.valid() {
            return Err(PaneViewError);
        }
        let count = if cell.text.is_empty() {
            usize::from(wire.run.max(1))
        } else if wire.run > 1 {
            return Err(PaneViewError);
        } else {
            1
        };
        let slots = target
            .get_mut(column..column.saturating_add(count))
            .ok_or(PaneViewError)?;
        if slots.len() != count {
            return Err(PaneViewError);
        }
        for slot in slots {
            slot.clone_from(&cell);
        }
        column = column.saturating_add(count);
    }
    if column != target.len() {
        return Err(PaneViewError);
    }
    Ok(())
}

/// One pane on the wire: its metadata and either every row (`full`) or the rows that changed
/// since the viewer's previous update.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneUpdate {
    pub rows: u16,
    pub columns: u16,
    pub cursor: Cursor,
    pub modes: PaneModes,
    pub title: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub full: bool,
    /// The carried rows, each followed in `cells` by `len` wire cells.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<WireCell>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub offset: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<u32>,
}

impl PaneUpdate {
    /// A full update of a vt100 screen (history views and tests).
    pub fn full_from_screen(
        screen: &vt100::Screen,
        title: &str,
        offset: u32,
        exit: Option<u32>,
    ) -> Result<Self, PaneViewError> {
        let (rows, columns) = screen.size();
        if rows > MAX_DIM || columns > MAX_DIM || title.len() > MAX_TITLE_BYTES {
            return Err(PaneViewError);
        }
        let (cursor_row, cursor_column) = screen.cursor_position();
        let mut update = Self {
            rows,
            columns,
            cursor: Cursor {
                row: cursor_row,
                column: cursor_column,
                hidden: screen.hide_cursor(),
            },
            modes: PaneModes::from_vt100(screen),
            title: title.to_owned(),
            full: true,
            lines: Vec::with_capacity(usize::from(rows)),
            cells: Vec::new(),
            offset,
            exit,
        };
        for row in 0..rows {
            let start = update.cells.len();
            for column in 0..columns {
                match screen.cell(row, column) {
                    Some(cell) => {
                        let (text, kind) = classify(cell);
                        push_wire(
                            &mut update.cells,
                            start,
                            text,
                            kind,
                            CellStyle::from_vt100(cell),
                        );
                    }
                    None => push_wire(
                        &mut update.cells,
                        start,
                        "",
                        CellKind::Blank,
                        CellStyle::default(),
                    ),
                }
            }
            update.lines.push(Line {
                row,
                wrapped: screen.row_wrapped(row),
                len: u16::try_from(update.cells.len() - start).unwrap_or(u16::MAX),
            });
        }
        Ok(update)
    }

    /// Folds a later update for the same pane into this one: rows the later one carries replace
    /// the rows carried here, everything else stays.
    pub fn merge(&mut self, newer: Self) {
        if newer.full || (self.rows, self.columns) != (newer.rows, newer.columns) {
            *self = newer;
            return;
        }
        let mut rows: BTreeMap<u16, (bool, Vec<WireCell>)> = BTreeMap::new();
        let mut cells = self.cells.iter();
        for line in &self.lines {
            let carried: Vec<WireCell> = cells
                .by_ref()
                .take(usize::from(line.len))
                .cloned()
                .collect();
            rows.insert(line.row, (line.wrapped, carried));
        }
        let mut cells = newer.cells.into_iter();
        for line in newer.lines {
            let carried: Vec<WireCell> = cells.by_ref().take(usize::from(line.len)).collect();
            rows.insert(line.row, (line.wrapped, carried));
        }
        self.lines.clear();
        self.cells.clear();
        for (row, (wrapped, carried)) in rows {
            self.lines.push(Line {
                row,
                wrapped,
                len: u16::try_from(carried.len()).unwrap_or(u16::MAX),
            });
            self.cells.extend(carried);
        }
        self.cursor = newer.cursor;
        self.modes = newer.modes;
        self.title = newer.title;
        self.offset = newer.offset;
        self.exit = newer.exit;
    }
}

/// One frame on the wire: the viewer's metadata plus the panes that changed (or, when `full`,
/// every visible pane in full).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameUpdate {
    pub workspace: String,
    pub generation: u64,
    pub tabs: Vec<TabEntry>,
    pub active_tab: Option<TabId>,
    pub focused: Option<PaneId>,
    pub layout: Vec<PaneRect>,
    pub panes: BTreeMap<PaneId, PaneUpdate>,
    pub exit_code: Option<u32>,
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub full: bool,
}

impl FrameUpdate {
    /// Folds a later update into this one so that applying the result equals applying both in
    /// order: later metadata wins, later rows replace earlier rows, untouched panes stay.
    pub fn merge(&mut self, newer: Self) {
        if newer.full {
            *self = newer;
            return;
        }
        for (id, pane) in newer.panes {
            match self.panes.get_mut(&id) {
                Some(existing) => existing.merge(pane),
                None => {
                    self.panes.insert(id, pane);
                }
            }
        }
        self.workspace = newer.workspace;
        self.generation = newer.generation;
        self.tabs = newer.tabs;
        self.active_tab = newer.active_tab;
        self.focused = newer.focused;
        self.layout = newer.layout;
        self.exit_code = newer.exit_code;
        if newer.message.is_some() {
            self.message = newer.message;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_updates_round_trip_to_the_screen_view() {
        let mut parser = vt100::Parser::new(3, 8, 0);
        parser.process("e\u{301}界x\x1b[1;31mred\r\n  wrap".as_bytes());
        let direct = PaneView::from_screen(parser.screen(), "t", 2, Some(1)).unwrap_or_default();
        let update =
            PaneUpdate::full_from_screen(parser.screen(), "t", 2, Some(1)).unwrap_or_default();
        let rebuilt = PaneView::from_update(&update);
        assert_eq!(rebuilt.as_ref().ok(), Some(&direct), "{update:?}");
        let json = serde_json::to_string(&update).unwrap_or_default();
        let parsed: PaneUpdate = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(parsed, update);
        assert!(json.contains("\"run\":"), "blank runs are compact: {json}");
    }

    #[test]
    fn vt100_conversion_keeps_combining_text_and_wide_cells() {
        let mut parser = vt100::Parser::new(2, 6, 0);
        parser.process("e\u{301}界x".as_bytes());
        let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
        assert_eq!(
            view.cell(0, 0).map(|cell| cell.text.as_str()),
            Some("e\u{301}")
        );
        assert_eq!(
            view.cell(0, 1).map(|cell| cell.kind),
            Some(CellKind::WideLeading)
        );
        assert_eq!(
            view.cell(0, 2).map(|cell| cell.kind),
            Some(CellKind::WideContinuation)
        );
        assert_eq!(view.text_between((0, 0), (0, 5)), "e\u{301}界x");
    }

    #[test]
    fn cells_reject_terminal_controls_and_multiple_clusters() {
        for text in ["\u{1b}[2J", "\n", "ab", "a\u{85}", "界"] {
            let cell = Cell {
                text: text.to_owned(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            };
            assert!(!cell.valid(), "accepted unsafe cell {text:?}");
        }
        for text in ["🇰🇷", "한"] {
            assert!(
                Cell {
                    text: text.to_owned(),
                    kind: CellKind::WideLeading,
                    style: CellStyle::default(),
                }
                .valid()
            );
        }
    }

    #[test]
    fn wrapped_rows_join_and_blank_tails_are_trimmed() {
        let mut parser = vt100::Parser::new(3, 4, 0);
        parser.process(b"abcdef\r\nxy");
        let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
        assert_eq!(view.text_between((0, 0), (2, 3)), "abcdef\nxy");
        assert_eq!(view.text_between((2, 3), (1, 0)), "ef\nxy");
    }

    #[test]
    fn frame_validation_rejects_dangling_references() {
        let mut frame = Frame::default();
        frame.layout.push(PaneRect {
            pane: PaneId(7),
            rect: Rect::default(),
        });
        assert!(!frame.valid());
        frame.layout.clear();
        frame.focused = Some(PaneId(1));
        assert!(!frame.valid());
        frame.focused = None;
        frame.active_tab = Some(TabId(3));
        assert!(!frame.valid());
        frame.active_tab = None;
        assert!(frame.valid());
        let oversized = PaneView {
            rows: u16::MAX,
            columns: u16::MAX,
            ..PaneView::default()
        };
        frame.panes.insert(PaneId(1), oversized);
        assert!(!frame.valid());
    }
}
