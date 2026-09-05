use super::{
    MAX_ARG_BYTES, MAX_ARGV_BYTES, MAX_ARGV_ENTRIES, MAX_CAPTURE_BYTES, MAX_ENV_BYTES,
    MAX_ENV_ENTRIES, MAX_EVENT_FILTERS, MAX_FRAME_BYTES, MAX_KEY_BYTES, MAX_SCROLLBACK_LINES,
    MAX_STATUS_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;

pub type RequestId = u64;
pub type PaneId = u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    New {
        id: RequestId,
        cwd: Option<PathBuf>,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Split {
        id: RequestId,
        axis: Axis,
        target: Option<PaneId>,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Focus {
        id: RequestId,
        target: FocusTarget,
    },
    Zoom {
        id: RequestId,
        pane: Option<PaneId>,
    },
    Kill {
        id: RequestId,
        pane: PaneId,
    },
    Resize {
        id: RequestId,
        pane: PaneId,
        delta: i16,
    },
    SendKeys {
        id: RequestId,
        pane: PaneId,
        keys: String,
    },
    Capture {
        id: RequestId,
        pane: PaneId,
        attrs: bool,
        scrollback: u32,
        max_bytes: usize,
    },
    List {
        id: RequestId,
    },
    Tab {
        id: RequestId,
        action: TabAction,
    },
    Workspace {
        id: RequestId,
        action: WorkspaceAction,
    },
    SetStatus {
        id: RequestId,
        segment: String,
        text: String,
    },
    Popup {
        id: RequestId,
        rows: Option<u16>,
        cols: Option<u16>,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Subscribe {
        id: RequestId,
        events: Vec<EventKind>,
    },
}

impl Request {
    pub fn id(&self) -> RequestId {
        match self {
            Self::New { id, .. }
            | Self::Split { id, .. }
            | Self::Focus { id, .. }
            | Self::Zoom { id, .. }
            | Self::Kill { id, .. }
            | Self::Resize { id, .. }
            | Self::SendKeys { id, .. }
            | Self::Capture { id, .. }
            | Self::List { id }
            | Self::Tab { id, .. }
            | Self::Workspace { id, .. }
            | Self::SetStatus { id, .. }
            | Self::Popup { id, .. }
            | Self::Subscribe { id, .. } => *id,
        }
    }

    pub fn validate(&self) -> Result<(), ControlError> {
        match self {
            Self::New { argv, env, .. }
            | Self::Split { argv, env, .. }
            | Self::Popup { argv, env, .. } => {
                validate_argv(argv)?;
                validate_env(env)?;
            }
            _ => {}
        }
        match self {
            Self::Resize { delta: 0, .. } => {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    "resize delta must not be zero",
                ));
            }
            Self::SendKeys { keys, .. } if keys.len() > MAX_KEY_BYTES => {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    format!("send-keys payload must be at most {MAX_KEY_BYTES} bytes"),
                ));
            }
            Self::Capture { max_bytes, .. }
                if *max_bytes == 0 || *max_bytes > MAX_CAPTURE_BYTES =>
            {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    format!("capture max-bytes must be 1-{MAX_CAPTURE_BYTES}"),
                ));
            }
            Self::Capture { scrollback, .. } if *scrollback > MAX_SCROLLBACK_LINES => {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    format!("scrollback must be at most {MAX_SCROLLBACK_LINES} lines"),
                ));
            }
            Self::SetStatus { segment, text, .. } => {
                if segment.is_empty() || segment.len() > 64 || !safe_name(segment) {
                    return Err(ControlError::invalid(
                        Some(self.id()),
                        "status segment must use 1-64 ASCII letters, digits, `.`, `_`, or `-`",
                    ));
                }
                if text.len() > MAX_STATUS_BYTES || text.contains('\0') {
                    return Err(ControlError::invalid(
                        Some(self.id()),
                        format!("status text must be at most {MAX_STATUS_BYTES} bytes without NUL"),
                    ));
                }
            }
            Self::Popup { rows, cols, .. }
                if matches!(rows, Some(0 | 513..)) || matches!(cols, Some(0 | 513..)) =>
            {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    "popup dimensions must be in 1-512",
                ));
            }
            Self::Tab {
                action: TabAction::New { name: Some(name) } | TabAction::Rename { name, .. },
                ..
            } if name.len() > 128 || name.contains('\0') => {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    "tab name must be at most 128 bytes without NUL",
                ));
            }
            Self::Workspace {
                action: WorkspaceAction::New { name } | WorkspaceAction::Kill { name },
                ..
            } if name.is_empty()
                || name.len() > 64
                || !safe_name(name)
                || name == "."
                || name == ".." =>
            {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    "workspace name is unsafe",
                ));
            }
            Self::Subscribe { events, .. } if events.len() > MAX_EVENT_FILTERS => {
                return Err(ControlError::invalid(
                    Some(self.id()),
                    format!("at most {MAX_EVENT_FILTERS} event filters are allowed"),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum FocusTarget {
    Pane(PaneId),
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum TabAction {
    SelectId { tab: u32 },
    Rename { tab: u32, name: String },
    New { name: Option<String> },
    Next,
    Previous,
    Select { index: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum WorkspaceAction {
    List,
    New { name: String },
    Kill { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reply {
    Accepted {
        id: RequestId,
    },
    Completed {
        id: RequestId,
        result: CommandResult,
    },
    Failed {
        id: RequestId,
        error: ReplyError,
    },
}

impl Reply {
    pub fn id(&self) -> RequestId {
        match self {
            Self::Accepted { id } | Self::Completed { id, .. } | Self::Failed { id, .. } => *id,
        }
    }

    pub fn state(&self) -> ReplyState {
        match self {
            Self::Accepted { .. } => ReplyState::Accepted,
            Self::Completed { .. } => ReplyState::Completed,
            Self::Failed { .. } => ReplyState::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyState {
    Accepted,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum CommandResult {
    Unit,
    Pane { pane: PaneId },
    Capture { text: String },
    Listing { workspaces: Vec<WorkspaceSummary> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub name: String,
    pub focused: bool,
    pub status: BTreeMap<String, String>,
    pub tabs: Vec<TabSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSummary {
    pub index: u32,
    pub name: String,
    pub focused: bool,
    pub panes: Vec<PaneSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneSummary {
    pub id: PaneId,
    pub command: Vec<String>,
    pub pid: Option<u32>,
    pub cwd: PathBuf,
    pub title: String,
    #[serde(default)]
    pub progress: Option<(u8, u8)>,
    pub agent: Option<String>,
    pub state: AgentStatus,
    pub geometry: PaneGeometry,
    pub focused: bool,
    pub cursor: PaneCursorSummary,
    pub modes: PaneModesSummary,
    pub copy: PaneCopySummary,
    pub viewport_offset: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneCursorSummary {
    pub row: u16,
    pub column: u16,
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneModesSummary {
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: String,
    pub mouse_encoding: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneCopySummary {
    pub active: bool,
    pub cursor_row: u16,
    pub cursor_column: u16,
    pub anchor: Option<(u16, u16)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneGeometry {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplyError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    InvalidJson,
    UnknownCommand,
    InvalidRequest,
    FrameTooLarge,
    Unauthorized,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Event {
    #[serde(rename = "pane.opened")]
    PaneOpened {
        id: RequestId,
        pane: PaneId,
        command: Vec<String>,
    },
    #[serde(rename = "pane.closed")]
    PaneClosed {
        id: RequestId,
        pane: PaneId,
        exit_status: Option<i32>,
    },
    #[serde(rename = "pane.focused")]
    PaneFocused { id: RequestId, pane: PaneId },
    #[serde(rename = "pane.title")]
    PaneTitle {
        id: RequestId,
        pane: PaneId,
        title: String,
    },
    #[serde(rename = "agent.state")]
    AgentState {
        id: RequestId,
        pane: PaneId,
        agent: Option<String>,
        old_state: AgentStatus,
        new_state: AgentStatus,
        timestamp_ms: u64,
    },
    #[serde(rename = "pane.output")]
    PaneOutput { id: RequestId, pane: PaneId },
    #[serde(rename = "workspace.resized")]
    WorkspaceResized { id: RequestId, rows: u16, cols: u16 },
    #[serde(rename = "client.attached")]
    ClientAttached {
        id: RequestId,
        client: ClientIdentity,
    },
    #[serde(rename = "client.detached")]
    ClientDetached {
        id: RequestId,
        client: ClientIdentity,
    },
}

impl Event {
    pub fn id(&self) -> RequestId {
        match self {
            Self::PaneOpened { id, .. }
            | Self::PaneClosed { id, .. }
            | Self::PaneFocused { id, .. }
            | Self::PaneTitle { id, .. }
            | Self::AgentState { id, .. }
            | Self::PaneOutput { id, .. }
            | Self::WorkspaceResized { id, .. }
            | Self::ClientAttached { id, .. }
            | Self::ClientDetached { id, .. } => *id,
        }
    }

    pub fn kind(&self) -> EventKind {
        match self {
            Self::PaneOpened { .. } => EventKind::PaneOpened,
            Self::PaneClosed { .. } => EventKind::PaneClosed,
            Self::PaneFocused { .. } => EventKind::PaneFocused,
            Self::PaneTitle { .. } => EventKind::PaneTitle,
            Self::AgentState { .. } => EventKind::AgentState,
            Self::PaneOutput { .. } => EventKind::PaneOutput,
            Self::WorkspaceResized { .. } => EventKind::WorkspaceResized,
            Self::ClientAttached { .. } => EventKind::ClientAttached,
            Self::ClientDetached { .. } => EventKind::ClientDetached,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    #[serde(rename = "pane.opened")]
    PaneOpened,
    #[serde(rename = "pane.closed")]
    PaneClosed,
    #[serde(rename = "pane.focused")]
    PaneFocused,
    #[serde(rename = "pane.title")]
    PaneTitle,
    #[serde(rename = "agent.state")]
    AgentState,
    #[serde(rename = "pane.output")]
    PaneOutput,
    #[serde(rename = "workspace.resized")]
    WorkspaceResized,
    #[serde(rename = "client.attached")]
    ClientAttached,
    #[serde(rename = "client.detached")]
    ClientDetached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentStatus {
    Working,
    Blocked,
    Idle,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientIdentity {
    Local,
    Viewer(u64),
}

#[derive(Debug)]
pub struct ControlError {
    pub id: Option<RequestId>,
    pub code: ErrorCode,
    pub message: String,
}

impl ControlError {
    fn invalid(id: Option<RequestId>, message: impl Into<String>) -> Self {
        Self {
            id,
            code: ErrorCode::InvalidRequest,
            message: message.into(),
        }
    }
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ControlError {}

pub fn decode_request_frame(frame: &[u8]) -> Result<Request, ControlError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ControlError {
            id: extract_id(frame),
            code: ErrorCode::FrameTooLarge,
            message: format!("control frame exceeds {MAX_FRAME_BYTES} bytes"),
        });
    }
    let text = std::str::from_utf8(frame).map_err(|_| ControlError {
        id: None,
        code: ErrorCode::InvalidJson,
        message: "control frame must be UTF-8".to_owned(),
    })?;
    let request = serde_json::from_str::<Request>(text).map_err(|error| {
        let code = if error.to_string().contains("unknown variant") {
            ErrorCode::UnknownCommand
        } else {
            ErrorCode::InvalidJson
        };
        ControlError {
            id: extract_id(frame),
            code,
            message: error.to_string(),
        }
    })?;
    if let Err(mut error) = request.validate() {
        error.id = Some(request.id());
        return Err(error);
    }
    Ok(request)
}

pub fn read_request<R: Read>(reader: &mut R) -> Result<Option<Request>, ControlError> {
    let mut frame = Vec::new();
    loop {
        let mut slot = [0_u8; 1];
        let count = reader.read(&mut slot).map_err(|error| ControlError {
            id: None,
            code: ErrorCode::Internal,
            message: error.to_string(),
        })?;
        if count == 0 {
            return if frame.is_empty() {
                Ok(None)
            } else {
                decode_request_frame(&frame).map(Some)
            };
        }
        let [byte] = slot;
        if byte == b'\n' {
            return decode_request_frame(&frame).map(Some);
        }
        if frame.len() == MAX_FRAME_BYTES {
            drain_line(reader)?;
            return Err(ControlError {
                id: extract_id(&frame),
                code: ErrorCode::FrameTooLarge,
                message: format!("control frame exceeds {MAX_FRAME_BYTES} bytes"),
            });
        }
        frame.push(byte);
    }
}

fn drain_line<R: Read>(reader: &mut R) -> Result<(), ControlError> {
    loop {
        let mut slot = [0_u8; 1];
        let count = reader.read(&mut slot).map_err(|error| ControlError {
            id: None,
            code: ErrorCode::Internal,
            message: error.to_string(),
        })?;
        let [byte] = slot;
        if count == 0 || byte == b'\n' {
            return Ok(());
        }
    }
}

pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "serialized control frame exceeds limit",
        ));
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn error_reply(error: &ControlError) -> Reply {
    Reply::Failed {
        id: error.id.unwrap_or(0),
        error: ReplyError {
            code: error.code,
            message: error.message.clone(),
        },
    }
}

/// Decodes CLI/config key text once for both socket clients and in-process bindings.
pub fn decode_key_bytes(input: &str) -> Result<Vec<u8>, ControlError> {
    let mut output = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            push_char(&mut output, character);
            continue;
        }
        match chars.next() {
            Some('n') => output.push(b'\n'),
            Some('r') => output.push(b'\r'),
            Some('t') => output.push(b'\t'),
            Some('\\') => output.push(b'\\'),
            Some('0') => output.push(0),
            Some('x') => {
                let high = chars.next().and_then(|value| value.to_digit(16));
                let low = chars.next().and_then(|value| value.to_digit(16));
                let value = high.zip(low).ok_or_else(|| {
                    ControlError::invalid(None, "`\\x` requires exactly two hexadecimal digits")
                })?;
                output.push(((value.0 << 4) | value.1) as u8);
            }
            Some(other) => {
                return Err(ControlError::invalid(
                    None,
                    format!("unknown escape `\\{other}`"),
                ));
            }
            None => return Err(ControlError::invalid(None, "trailing backslash")),
        }
    }
    Ok(output)
}

fn push_char(output: &mut Vec<u8>, character: char) {
    let mut encoded = [0_u8; 4];
    output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
}

fn validate_argv(argv: &[String]) -> Result<(), ControlError> {
    if argv.len() > MAX_ARGV_ENTRIES {
        return Err(ControlError::invalid(
            None,
            format!("argv must contain at most {MAX_ARGV_ENTRIES} entries"),
        ));
    }
    let mut total = 0usize;
    for argument in argv {
        if argument.is_empty() || argument.len() > MAX_ARG_BYTES || argument.contains('\0') {
            return Err(ControlError::invalid(
                None,
                "argv entries must be non-empty, bounded, and contain no NUL",
            ));
        }
        total = total.saturating_add(argument.len());
    }
    if total > MAX_ARGV_BYTES {
        return Err(ControlError::invalid(
            None,
            format!("argv exceeds {MAX_ARGV_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn validate_env(env: &BTreeMap<String, String>) -> Result<(), ControlError> {
    if env.len() > MAX_ENV_ENTRIES {
        return Err(ControlError::invalid(
            None,
            format!("env exceeds {MAX_ENV_ENTRIES} entries"),
        ));
    }
    let mut total = 0usize;
    for (key, value) in env {
        let mut bytes = key.bytes();
        let portable_name = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if !portable_name || value.contains('\0') {
            return Err(ControlError::invalid(
                None,
                "environment names must be portable identifiers ([A-Za-z_][A-Za-z0-9_]*), and values must not contain NUL",
            ));
        }
        total = total.saturating_add(key.len()).saturating_add(value.len());
    }
    if total > MAX_ENV_BYTES {
        return Err(ControlError::invalid(
            None,
            format!("env exceeds {MAX_ENV_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn safe_name(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn extract_id(frame: &[u8]) -> Option<RequestId> {
    serde_json::from_slice::<serde_json::Value>(frame)
        .ok()?
        .get("id")?
        .as_u64()
}
