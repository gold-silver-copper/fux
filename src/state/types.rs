use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

pub const MAX_DIM: u16 = 512;
pub const MAX_CELL_TEXT_BYTES: usize = 22;
pub const MAX_PANES: usize = 256;
/// One maximum-size pane across the entire workspace (`MAX_DIM * MAX_DIM`).
pub const MAX_TOTAL_CELLS: usize = 262_144;
pub const MAX_TABS: usize = 64;
pub const MAX_POPUPS: usize = 32;
pub const MAX_STATUS_SEGMENTS: usize = 64;
pub const MAX_NAME_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 4096;
pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PaneId(pub u32);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TabId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

impl Cursor {
    #[must_use]
    pub fn from_vt100(screen: &vt100::Screen) -> Self {
        let (row, column) = screen.cursor_position();
        Self {
            row,
            column,
            hidden: screen.hide_cursor(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    Working,
    Blocked,
    Idle,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentFlags {
    pub idle: bool,
    pub blocker: bool,
    pub working: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub id: Option<String>,
    pub state: AgentState,
    pub flags: AgentFlags,
    pub sequence: u64,
    pub exited: bool,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CopyState {
    pub active: bool,
    pub cursor_row: u16,
    pub cursor_column: u16,
    pub anchor: Option<(u16, u16)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneView {
    pub rows: u16,
    pub columns: u16,
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    pub modes: PaneModes,
    pub title: String,
    pub agent: AgentStatus,
    pub viewport_offset: u32,
    pub copy: CopyState,
    /// Final process status for a durable, non-focusable completed-pane snapshot.
    pub exit_status: Option<u32>,
    /// One flag per visible row; true means the row continues into the next without a newline.
    pub wrapped_rows: Vec<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneViewError;

impl PaneView {
    pub fn from_vt100(
        screen: &vt100::Screen,
        title: String,
        agent: AgentStatus,
        viewport_offset: u32,
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
        let value = Self {
            rows,
            columns,
            cells,
            cursor: Cursor::from_vt100(screen),
            modes: PaneModes::from_vt100(screen),
            title,
            agent,
            viewport_offset,
            copy: CopyState::default(),
            exit_status: None,
            wrapped_rows: (0..rows).map(|row| screen.row_wrapped(row)).collect(),
        };
        value.valid().then_some(value).ok_or(PaneViewError)
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.rows <= MAX_DIM
            && self.columns <= MAX_DIM
            && self.cells.len() == usize::from(self.rows) * usize::from(self.columns)
            && self.cells.iter().all(Cell::valid)
            && (self.wrapped_rows.is_empty() || self.wrapped_rows.len() == usize::from(self.rows))
            && self.title.len() <= MAX_TITLE_BYTES
            && self.copy.cursor_row < self.rows.max(1)
            && self.copy.cursor_column < self.columns.max(1)
            && self
                .copy
                .anchor
                .is_none_or(|(row, column)| row < self.rows.max(1) && column < self.columns.max(1))
            && self
                .agent
                .id
                .as_ref()
                .is_none_or(|value| value.len() <= MAX_NAME_BYTES)
            && self
                .agent
                .message
                .as_ref()
                .is_none_or(|value| value.len() <= MAX_NAME_BYTES)
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
}

impl From<&zor::osc::Report> for AgentStatus {
    fn from(report: &zor::osc::Report) -> Self {
        let state = match report.state() {
            zor::osc::State::Working => AgentState::Working,
            zor::osc::State::Blocked => AgentState::Blocked,
            zor::osc::State::Idle => AgentState::Idle,
            zor::osc::State::None => AgentState::None,
        };
        let flags = report.visible();
        Self {
            id: report.agent().map(|agent| agent.as_str().to_owned()),
            state,
            flags: AgentFlags {
                idle: flags.idle,
                blocker: flags.blocker,
                working: flags.working,
            },
            sequence: report.seq(),
            exited: report.exited(),
            message: report.message().map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Popup {
    pub pane: PaneId,
    pub width: u16,
    pub height: u16,
    pub z_index: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    pub status: BTreeMap<String, String>,
    pub window_title: String,
    pub clipboard_base64: String,
    pub bell_count: u64,
    /// Per-connection acknowledgement stamped onto a cloned outbound snapshot.
    pub echo_ack: u64,
    pub exit_code: Option<u32>,
    pub generation: u64,
}
