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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellKind {
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
        let kind = if cell.is_wide_continuation() {
            CellKind::WideContinuation
        } else if cell.is_wide() {
            CellKind::WideLeading
        } else if cell.has_contents() {
            CellKind::Text
        } else {
            CellKind::Blank
        };
        Self {
            text: cell.contents().to_owned(),
            kind,
            style: CellStyle {
                foreground: cell.fgcolor().into(),
                background: cell.bgcolor().into(),
                bold: cell.bold(),
                dim: cell.dim(),
                italic: cell.italic(),
                underline: cell.underline(),
                inverse: cell.inverse(),
            },
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
        self.rows <= MAX_DIM
            && self.columns <= MAX_DIM
            && self.cells.len() == usize::from(self.rows) * usize::from(self.columns)
            && self.cells.iter().all(Cell::valid)
            && self.wrapped_rows.len() == usize::from(self.rows)
            && self.title.len() <= MAX_TITLE_BYTES
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
    pub bindings: crate::commands::ClientBindings,
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
            && self.panes.values().all(PaneView::valid)
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

#[cfg(test)]
mod tests {
    use super::*;

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
