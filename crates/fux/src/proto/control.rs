//! Control protocol: newline-delimited JSON commands, replies, subscriptions and
//! lifecycle events over a per-workspace Unix socket. Zor's observer and the fux CLI are wire
//! consumers; nothing here references ECS types.

use crate::ids::{PaneId, TabId};
use crate::layout::Rect;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;

pub const CONTROL_PREFACE: &[u8; 4] = b"FUX\n";
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_ARGV_ENTRIES: usize = 128;
pub const MAX_ARG_BYTES: usize = 4096;
pub const MAX_ARGV_BYTES: usize = 16 * 1024;
/// Maximum captured text before JSON encoding; worst-case escaping stays under the frame limit.
pub const MAX_CAPTURE_BYTES: usize = 128 * 1024;
pub const MAX_KEY_BYTES: usize = 64 * 1024;
pub const MAX_ENV_ENTRIES: usize = 64;
pub const MAX_ENV_BYTES: usize = 16 * 1024;
pub const MAX_SCROLLBACK_LINES: u32 = 100_000;
pub const MAX_SUBSCRIBER_QUEUE: usize = 1024;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_CONTROL_CONNECTIONS: usize = 64;

pub type RequestId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// Split the focused pane (or `target`) and start `argv` (default command when empty).
    Split {
        #[serde(default)]
        stream: Option<u64>,
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        axis: crate::layout::Axis,
        #[serde(default)]
        target: Option<PaneId>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        argv: Vec<String>,
        /// Extra environment for the pane command, on top of the sanitized inherited set.
        #[serde(default)]
        env: Vec<(String, String)>,
        /// Initial pane size when no viewer sizes the tab (a headless workspace).
        #[serde(default)]
        rows: Option<u16>,
        #[serde(default)]
        columns: Option<u16>,
    },
    Focus {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        target: FocusTarget,
    },
    Kill {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        pane: PaneId,
    },
    Resize {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        pane: PaneId,
        delta: i16,
    },
    SendKeys {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        pane: PaneId,
        keys: String,
        /// `escapes` (default) reads `\n \e \xHH`; `keys` reads space-separated key names
        /// (`Enter`, `Up`, `C-c`, `M-x`, literal characters).
        #[serde(default)]
        notation: KeyNotation,
    },
    InputReserve {
        id: RequestId,
        instance: Option<String>,
        pane: PaneId,
    },
    InputSubmit {
        id: RequestId,
        instance: Option<String>,
        operation: u64,
        keys: String,
    },
    InputStatus {
        id: RequestId,
        instance: Option<String>,
        operation: u64,
    },
    /// The pane's screen. `format: "text"` returns the screen (and requested history) as one
    /// text; `format: "cells"` returns the visible grid cell by cell with the same coherent
    /// metadata as the text form.
    Capture {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        pane: PaneId,
        #[serde(default)]
        attrs: bool,
        #[serde(default)]
        scrollback: u32,
        max_bytes: usize,
        #[serde(default)]
        format: CaptureFormat,
        #[serde(default)]
        if_revision: Option<u64>,
    },
    List {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
    },
    /// The server's identity, version, runtime directory and limits.
    Info {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
    },
    Tab {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        action: TabAction,
    },
    Workspace {
        #[serde(default)]
        stream: Option<u64>,
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        action: WorkspaceAction,
    },
    Events {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        after: EventCursor,
    },
    Subscribe {
        id: RequestId,
        #[serde(default)]
        instance: Option<String>,
        #[serde(default)]
        after: Option<EventCursor>,
    },
}

impl Request {
    pub fn id(&self) -> RequestId {
        match self {
            Self::InputReserve { id, .. }
            | Self::InputSubmit { id, .. }
            | Self::InputStatus { id, .. }
            | Self::Split { id, .. }
            | Self::Focus { id, .. }
            | Self::Kill { id, .. }
            | Self::Resize { id, .. }
            | Self::SendKeys { id, .. }
            | Self::Capture { id, .. }
            | Self::List { id, .. }
            | Self::Info { id, .. }
            | Self::Tab { id, .. }
            | Self::Workspace { id, .. }
            | Self::Events { id, .. }
            | Self::Subscribe { id, .. } => *id,
        }
    }

    /// Optional precondition scoped to the discovered server incarnation.
    pub fn instance(&self) -> Option<&str> {
        match self {
            Self::InputReserve { instance, .. }
            | Self::InputSubmit { instance, .. }
            | Self::InputStatus { instance, .. }
            | Self::Split { instance, .. }
            | Self::Focus { instance, .. }
            | Self::Kill { instance, .. }
            | Self::Resize { instance, .. }
            | Self::SendKeys { instance, .. }
            | Self::Capture { instance, .. }
            | Self::List { instance, .. }
            | Self::Info { instance, .. }
            | Self::Tab { instance, .. }
            | Self::Workspace { instance, .. }
            | Self::Events { instance, .. }
            | Self::Subscribe { instance, .. } => instance.as_deref(),
        }
    }

    pub fn validate(&self) -> Result<(), ControlError> {
        let id = Some(self.id());
        if self.instance().is_some_and(|instance| {
            instance.is_empty()
                || instance.len() > 128
                || !instance
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        }) {
            return Err(ControlError::invalid(id, "invalid server instance"));
        }
        if matches!(
            self,
            Self::InputReserve { .. }
                | Self::InputSubmit { .. }
                | Self::InputStatus { .. }
                | Self::Events { .. }
                | Self::Split {
                    stream: Some(_),
                    ..
                }
                | Self::Workspace {
                    stream: Some(_),
                    ..
                }
                | Self::Subscribe { after: Some(_), .. }
        ) && self.instance().is_none()
        {
            return Err(ControlError::invalid(
                id,
                "tracked operations require a server instance",
            ));
        }
        match self {
            Self::Split { argv, cwd, env, .. } => {
                validate_argv(argv).map_err(|mut error| {
                    error.id = id;
                    error
                })?;
                if let Some(cwd) = cwd
                    && (cwd.as_os_str().is_empty()
                        || !cwd.is_absolute()
                        || cwd.to_string_lossy().contains('\0'))
                {
                    return Err(ControlError::invalid(id, "cwd must be an absolute path"));
                }
                validate_env(id, env)?;
            }
            Self::Resize { delta: 0, .. } => {
                return Err(ControlError::invalid(id, "resize delta must not be zero"));
            }
            Self::InputSubmit { keys, .. } => {
                if keys.len() > MAX_KEY_BYTES {
                    return Err(ControlError::invalid(
                        id,
                        "input payload exceeds byte limit",
                    ));
                }
                decode_key_bytes(keys).map_err(|mut error| {
                    error.id = id;
                    error
                })?;
            }
            Self::SendKeys { keys, notation, .. } => {
                if keys.len() > MAX_KEY_BYTES {
                    return Err(ControlError::invalid(
                        id,
                        format!("send-keys payload must be at most {MAX_KEY_BYTES} bytes"),
                    ));
                }
                decode_keys(keys, *notation).map_err(|mut error| {
                    error.id = id;
                    error
                })?;
            }
            Self::Capture {
                max_bytes,
                scrollback,
                format,
                if_revision,
                attrs,
                ..
            } => {
                if *max_bytes == 0 || *max_bytes > MAX_CAPTURE_BYTES {
                    return Err(ControlError::invalid(
                        id,
                        format!("capture max-bytes must be 1-{MAX_CAPTURE_BYTES}"),
                    ));
                }
                if *scrollback > MAX_SCROLLBACK_LINES {
                    return Err(ControlError::invalid(
                        id,
                        format!("scrollback must be at most {MAX_SCROLLBACK_LINES} lines"),
                    ));
                }
                if if_revision.is_some() && self.instance().is_none() {
                    return Err(ControlError::invalid(
                        id,
                        "conditional capture requires a server instance",
                    ));
                }
                if *format == CaptureFormat::Cells && *attrs {
                    return Err(ControlError::invalid(
                        id,
                        "capture format cells carries styles; attrs applies to the text format",
                    ));
                }
                if *format == CaptureFormat::Cells && *scrollback > 0 {
                    return Err(ControlError::invalid(
                        id,
                        "capture format cells reads the visible grid; scrollback applies to the text format",
                    ));
                }
            }
            Self::Tab {
                action: TabAction::New { name: Some(name) } | TabAction::Rename { name, .. },
                ..
            } => validate_label(id, name)?,
            Self::Workspace {
                action:
                    WorkspaceAction::New { name: Some(name) }
                    | WorkspaceAction::Kill { name }
                    | WorkspaceAction::Select { name },
                ..
            } => {
                crate::ids::validate_workspace_name(name)
                    .map_err(|error| ControlError::invalid(id, error.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_label(id: Option<RequestId>, name: &str) -> Result<(), ControlError> {
    if name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(ControlError::invalid(
            id,
            format!("labels use at most {MAX_NAME_BYTES} bytes without control characters"),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureFormat {
    /// The screen (and requested history) as one text, plain or with attributes.
    #[default]
    Text,
    /// The visible grid as wire cells per row with the text form's coherent metadata.
    Cells,
}

/// One visible row of a `cells` capture: its wrap flag and the row's cells in the viewer wire
/// encoding (`text` implies kind `text`, no text implies `blank`, default styles are omitted,
/// and `run` folds equal blanks), covering exactly `columns` cells when expanded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureLine {
    pub row: u16,
    pub wrapped: bool,
    pub cells: Vec<crate::view::WireCell>,
}

/// What `info` reports about the server answering the socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerInfo {
    pub pid: u32,
    pub instance_nonce: String,
    /// The fux crate version the server was built from.
    pub version: String,
    pub runtime_dir: PathBuf,
    /// The workspace the socket serves; `null` on the manager socket.
    pub workspace: Option<String>,
    pub limits: InfoLimits,
}

/// The bounds a client must honor when it sizes a request: the frame limit, the capture text
/// limit, the `send-keys`/`input-submit` payload limit and the configured scrollback depth.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoLimits {
    pub scrollback_lines: usize,
    pub frame_bytes: usize,
    pub capture_bytes: usize,
    pub key_bytes: usize,
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
    New {
        #[serde(default)]
        name: Option<String>,
    },
    Next,
    Previous,
    /// Show a tab by its position or its stable id.
    Select {
        target: TabTarget,
    },
    Rename {
        tab: TabId,
        name: String,
    },
    Close {
        tab: TabId,
    },
}

/// Which tab `tab select` shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum TabTarget {
    Index(u32),
    Id(TabId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum WorkspaceAction {
    List,
    New {
        #[serde(default)]
        name: Option<String>,
    },
    Kill {
        name: String,
    },
    /// Re-target the requesting viewer's attachment; invalid for control-socket clients.
    Select {
        name: String,
    },
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

    pub fn failed(id: RequestId, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Failed {
            id,
            error: ReplyError {
                code,
                message: message.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum CommandResult {
    Final {
        record: Box<FinalRecord>,
    },
    Events {
        cursor: EventCursor,
        events: Vec<SequencedEvent>,
    },
    Input {
        receipt: InputReceipt,
    },
    Unit,
    Pane {
        pane: PaneId,
    },
    Tab {
        tab: TabId,
    },
    Workspace {
        name: String,
    },
    /// Coherent text/metadata plus the separately refreshed grid sequence.
    Capture {
        seq: u64,
        input_sequence: u64,
        #[serde(flatten)]
        capture: Box<crate::terminal::CaptureSnapshot>,
    },
    /// The visible grid cell by cell from one borrow of the pane: the same `revision`, `seq` and
    /// `input_sequence` a text capture taken in the same step reports. `lines` is empty when
    /// `unchanged`; `truncated` means trailing lines were dropped whole to honor `max_bytes`.
    Cells {
        seq: u64,
        input_sequence: u64,
        revision: u64,
        rows: u16,
        columns: u16,
        cursor: crate::view::Cursor,
        title: String,
        progress: Option<(u8, u8)>,
        unchanged: bool,
        truncated: bool,
        lines: Vec<CaptureLine>,
    },
    Listing {
        instance: String,
        workspaces: Vec<WorkspaceSummary>,
    },
    Info {
        info: Box<ServerInfo>,
    },
}

/// Receipt retention is scoped to the server incarnation; expiry never cancels queued bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputReceipt {
    pub operation: u64,
    pub pane: PaneId,
    pub state: InputState,
    pub revision: u64,
    pub input_sequence: u64,
    pub expires_ms: u64,
    pub bytes_written: usize,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputState {
    Reserved,
    Queued,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub event_cursor: EventCursor,
    pub name: String,
    pub focused: bool,
    pub viewers: u32,
    pub tabs: Vec<TabSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSummary {
    pub id: TabId,
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
    /// The output sequence: advances whenever the visible screen, cursor, modes, title or exit
    /// status changed; `capture` and `pane.output` report the same counter.
    pub seq: u64,
    /// Terminal capture revision; independent of the grid sequence.
    pub revision: u64,
    pub input_sequence: u64,
    pub geometry: Rect,
    pub focused: bool,
    pub cursor: crate::view::Cursor,
    pub modes: crate::view::PaneModes,
    pub exit_status: Option<u32>,
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
    Pending,
    Gap,
    Expired,
    InvalidJson,
    UnknownCommand,
    InvalidRequest,
    FrameTooLarge,
    Unauthorized,
    NotFound,
    Conflict,
    Limit,
    Timeout,
    Internal,
}

/// Scoped to one workspace lifetime within the required server instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCursor {
    pub stream: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub cursor: EventCursor,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Event {
    #[serde(rename = "workspace.changed")]
    WorkspaceChanged { id: RequestId },
    #[serde(rename = "pane.opened")]
    PaneOpened {
        id: RequestId,
        pane: PaneId,
        tab: TabId,
        command: Vec<String>,
    },
    #[serde(rename = "pane.closed")]
    PaneClosed {
        id: RequestId,
        pane: PaneId,
        exit_status: Option<i32>,
    },
    #[serde(rename = "pane.output")]
    PaneOutput {
        id: RequestId,
        pane: PaneId,
        /// The output sequence after the change that produced the event.
        seq: u64,
    },
    #[serde(rename = "tab.opened")]
    TabOpened {
        id: RequestId,
        tab: TabId,
        name: String,
    },
    #[serde(rename = "tab.closed")]
    TabClosed { id: RequestId, tab: TabId },
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::WorkspaceChanged { .. } => EventKind::WorkspaceChanged,
            Self::PaneOpened { .. } => EventKind::PaneOpened,
            Self::PaneClosed { .. } => EventKind::PaneClosed,
            Self::PaneOutput { .. } => EventKind::PaneOutput,
            Self::TabOpened { .. } => EventKind::TabOpened,
            Self::TabClosed { .. } => EventKind::TabClosed,
        }
    }

    /// Stamps the subscriber's request id on a published copy.
    pub fn with_id(mut self, subscription: RequestId) -> Self {
        match &mut self {
            Self::WorkspaceChanged { id }
            | Self::PaneOpened { id, .. }
            | Self::PaneClosed { id, .. }
            | Self::PaneOutput { id, .. }
            | Self::TabOpened { id, .. }
            | Self::TabClosed { id, .. } => *id = subscription,
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    #[serde(rename = "workspace.changed")]
    WorkspaceChanged,
    #[serde(rename = "pane.opened")]
    PaneOpened,
    #[serde(rename = "pane.closed")]
    PaneClosed,
    #[serde(rename = "pane.output")]
    PaneOutput,
    #[serde(rename = "tab.opened")]
    TabOpened,
    #[serde(rename = "tab.closed")]
    TabClosed,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ControlError {
    pub id: Option<RequestId>,
    pub code: ErrorCode,
    pub message: String,
}

impl ControlError {
    pub fn invalid(id: Option<RequestId>, message: impl Into<String>) -> Self {
        Self {
            id,
            code: ErrorCode::InvalidRequest,
            message: message.into(),
        }
    }
}

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
    request.validate()?;
    Ok(request)
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

/// A control frame with one absolute deadline covering every partial socket write.
pub fn write_frame_until<T: Serialize>(
    writer: &mut std::os::unix::net::UnixStream,
    value: &T,
    deadline: std::time::Instant,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "serialized control frame exceeds limit",
        ));
    }
    bytes.push(b'\n');
    super::socket::write_all_until(writer, &bytes, deadline)
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

/// Decodes CLI/control key text once for socket clients and viewers.
pub fn decode_key_bytes(input: &str) -> Result<Vec<u8>, ControlError> {
    let mut output = Vec::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut encoded = [0_u8; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => output.push(b'\n'),
            Some('r') => output.push(b'\r'),
            Some('t') => output.push(b'\t'),
            Some('e') => output.push(0x1b),
            Some('\\') => output.push(b'\\'),
            Some('0') => output.push(0),
            Some('x') => {
                let high = chars.next().and_then(|value| value.to_digit(16));
                let low = chars.next().and_then(|value| value.to_digit(16));
                let value = high.zip(low).ok_or_else(|| {
                    ControlError::invalid(None, "`\\x` requires exactly two hexadecimal digits")
                })?;
                output.push(u8::try_from((value.0 << 4) | value.1).unwrap_or(0));
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

/// How `send-keys` reads its payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyNotation {
    /// Byte escapes: `\n \r \t \e \\ \0 \xHH`, everything else literal UTF-8.
    #[default]
    Escapes,
    /// Space-separated key names: `Enter`, `Tab`, `Escape`, `Space`, `Up`/`Down`/`Left`/`Right`,
    /// `Home`, `End`, `PageUp`, `PageDown`, `F1`-`F12`, `C-<key>`, `M-<key>`, or a literal char.
    Keys,
}

/// Decodes `send-keys` input in the requested notation into the exact bytes for the pane.
pub fn decode_keys(input: &str, notation: KeyNotation) -> Result<Vec<u8>, ControlError> {
    match notation {
        KeyNotation::Escapes => decode_key_bytes(input),
        KeyNotation::Keys => decode_key_notation(input),
    }
}

/// The xterm byte sequence for one named key (normal cursor mode).
fn named_key(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "Enter" | "Return" => b"\r",
        "Tab" => b"\t",
        "Escape" | "Esc" => b"\x1b",
        "Space" => b" ",
        "Backspace" | "BSpace" => b"\x7f",
        "Up" => b"\x1b[A",
        "Down" => b"\x1b[B",
        "Right" => b"\x1b[C",
        "Left" => b"\x1b[D",
        "Home" => b"\x1b[H",
        "End" => b"\x1b[F",
        "Insert" | "IC" => b"\x1b[2~",
        "Delete" | "DC" => b"\x1b[3~",
        "PageUp" | "PgUp" => b"\x1b[5~",
        "PageDown" | "PgDn" => b"\x1b[6~",
        "F1" => b"\x1bOP",
        "F2" => b"\x1bOQ",
        "F3" => b"\x1bOR",
        "F4" => b"\x1bOS",
        "F5" => b"\x1b[15~",
        "F6" => b"\x1b[17~",
        "F7" => b"\x1b[18~",
        "F8" => b"\x1b[19~",
        "F9" => b"\x1b[20~",
        "F10" => b"\x1b[21~",
        "F11" => b"\x1b[23~",
        "F12" => b"\x1b[24~",
        _ => return None,
    })
}

/// The most `C-`/`M-` modifiers one key token may stack, bounding recursion.
const MAX_KEY_MODIFIERS: usize = 8;

/// Decodes one token (a named key, `C-<x>`, `M-<x>`, or a literal character) into bytes.
fn decode_token(token: &str, depth: usize) -> Result<Vec<u8>, ControlError> {
    if depth > MAX_KEY_MODIFIERS {
        return Err(ControlError::invalid(None, "too many key modifiers"));
    }
    if let Some(rest) = token.strip_prefix("C-") {
        let inner = decode_token(rest, depth + 1)?;
        // Control applies to a single ASCII letter or `@`-`_`; otherwise it is undefined.
        let [byte] = inner.as_slice() else {
            return Err(ControlError::invalid(
                None,
                format!("C- needs one key: {token}"),
            ));
        };
        return Ok(vec![byte.to_ascii_uppercase().wrapping_sub(0x40) & 0x7f]);
    }
    if let Some(rest) = token.strip_prefix("M-") {
        let mut bytes = vec![0x1b];
        bytes.extend(decode_token(rest, depth + 1)?);
        return Ok(bytes);
    }
    if let Some(bytes) = named_key(token) {
        return Ok(bytes.to_vec());
    }
    let mut chars = token.chars();
    if let (Some(character), None) = (chars.next(), chars.clone().next()) {
        let mut encoded = [0_u8; 4];
        return Ok(character.encode_utf8(&mut encoded).as_bytes().to_vec());
    }
    Err(ControlError::invalid(
        None,
        format!("unknown key `{token}`"),
    ))
}

/// Decodes space-separated key tokens into the bytes a pane receives.
pub fn decode_key_notation(input: &str) -> Result<Vec<u8>, ControlError> {
    let mut output = Vec::with_capacity(input.len());
    for token in input.split_whitespace() {
        output.extend(decode_token(token, 0)?);
    }
    Ok(output)
}

fn validate_env(id: Option<RequestId>, env: &[(String, String)]) -> Result<(), ControlError> {
    if env.len() > MAX_ENV_ENTRIES {
        return Err(ControlError::invalid(
            id,
            format!("at most {MAX_ENV_ENTRIES} environment entries"),
        ));
    }
    let mut total = 0usize;
    for (name, value) in env {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(ControlError::invalid(
                id,
                "environment names are non-empty without `=` or NUL; values carry no NUL",
            ));
        }
        total = total.saturating_add(name.len()).saturating_add(value.len());
    }
    if total > MAX_ENV_BYTES {
        return Err(ControlError::invalid(
            id,
            format!("environment exceeds {MAX_ENV_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), ControlError> {
    if argv.len() > MAX_ARGV_ENTRIES {
        return Err(ControlError::invalid(
            None,
            format!("argv must contain at most {MAX_ARGV_ENTRIES} entries"),
        ));
    }
    let mut total = 0usize;
    for (index, argument) in argv.iter().enumerate() {
        if (index == 0 && argument.is_empty())
            || argument.len() > MAX_ARG_BYTES
            || argument.contains('\0')
        {
            return Err(ControlError::invalid(
                None,
                "executable must be non-empty; argv entries must be bounded and contain no NUL",
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

fn extract_id(frame: &[u8]) -> Option<RequestId> {
    serde_json::from_slice::<serde_json::Value>(frame)
        .ok()?
        .get("id")?
        .as_u64()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn hostile_frames_are_bounded_and_rejected() {
        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            decode_request_frame(&oversized),
            Err(ControlError {
                code: ErrorCode::FrameTooLarge,
                ..
            })
        ));
        assert_eq!(
            decode_request_frame(b"{\"command\":\"list\",\"id\":7}")
                .ok()
                .map(|request| request.id()),
            Some(7)
        );
        for frame in [
            b"{}".as_slice(),
            b"{\"command\":\"popup\",\"id\":9,\"argv\":[]}",
            b"{\"command\":\"zoom\",\"id\":9}",
            b"{\"command\":\"set-status\",\"id\":9,\"segment\":\"a\",\"text\":\"b\"}",
            b"{\"command\":\"list\",\"id\":9,\"extra\":[]}",
            b"{\"command\":\"resize\",\"id\":1,\"pane\":1,\"delta\":0}",
            b"{\"command\":\"capture\",\"id\":1,\"pane\":1,\"max_bytes\":0}",
            b"{\"command\":\"workspace\",\"id\":1,\"action\":{\"kill\":{\"name\":\"../x\"}}}",
            b"{\"command\":\"send-keys\",\"id\":1,\"pane\":1,\"keys\":\"\\\\q\"}",
            b"{\"command\":\"wait\",\"id\":1,\"pane\":1,\"until\":{\"kind\":\"exit\"},\"timeout_ms\":100}",
            b"{\"command\":\"subscribe\",\"id\":1,\"events\":[\"pane.output\"]}",
        ] {
            assert!(decode_request_frame(frame).is_err(), "{frame:?}");
        }
        let removed = decode_request_frame(b"{\"command\":\"popup\",\"id\":9,\"argv\":[]}")
            .err()
            .map(|error| error.code);
        assert_eq!(removed, Some(ErrorCode::UnknownCommand));
    }

    #[test]
    fn events_serialize_with_dotted_names_and_take_subscription_ids() {
        let event = Event::PaneClosed {
            id: 1,
            pane: PaneId(3),
            exit_status: Some(2),
        }
        .with_id(9);
        let json = serde_json::to_string(&event).unwrap_or_default();
        assert!(json.contains("\"event\":\"pane.closed\""));
        assert!(json.contains("\"id\":9"));
        assert_eq!(
            serde_json::from_str::<EventKind>("\"pane.output\"").ok(),
            Some(EventKind::PaneOutput)
        );
        for removed in ["pane.title", "client.attached", "client.detached"] {
            assert!(serde_json::from_str::<EventKind>(&format!("\"{removed}\"")).is_err());
        }
    }

    #[test]
    fn key_notation_decodes_named_keys_modifiers_and_literals() {
        assert_eq!(decode_key_notation("Enter").unwrap(), b"\r");
        assert_eq!(decode_key_notation("Up Down").unwrap(), b"\x1b[A\x1b[B");
        assert_eq!(decode_key_notation("C-c").unwrap(), vec![3]);
        assert_eq!(decode_key_notation("C-a b C-c").unwrap(), vec![1, b'b', 3]);
        assert_eq!(decode_key_notation("M-x").unwrap(), vec![0x1b, b'x']);
        assert_eq!(decode_key_notation("F5").unwrap(), b"\x1b[15~");
        assert_eq!(decode_key_notation("h i").unwrap(), b"hi");
        assert!(decode_key_notation("Nope").is_err());
        assert!(decode_key_notation("C-ab").is_err());
        // Deeply stacked modifiers are rejected rather than recursing without bound.
        assert!(decode_key_notation(&"M-".repeat(64)).is_err());
        // The default escapes notation is unchanged.
        assert_eq!(decode_keys("a\\n", KeyNotation::Escapes).unwrap(), b"a\n");
    }

    #[test]
    fn env_and_send_keys_notation_are_validated() {
        let ok = decode_request_frame(
            br#"{"command":"split","id":1,"axis":"horizontal","env":[["FOO","bar"]],"rows":40,"columns":100}"#,
        );
        assert!(matches!(
            ok,
            Ok(Request::Split {
                rows: Some(40),
                columns: Some(100),
                ..
            })
        ));
        assert!(ok.unwrap().validate().is_ok());
        let bad_name = decode_request_frame(
            br#"{"command":"split","id":1,"axis":"horizontal","env":[["A=B","c"]]}"#,
        )
        .ok()
        .filter(|request| request.validate().is_ok());
        assert!(bad_name.is_none(), "an `=` in an env name is rejected");
        let keys = decode_request_frame(
            br#"{"command":"send-keys","id":2,"pane":1,"keys":"C-c Enter","notation":"keys"}"#,
        );
        assert!(matches!(
            keys,
            Ok(Request::SendKeys {
                notation: KeyNotation::Keys,
                ..
            })
        ));
        assert!(keys.unwrap().validate().is_ok());
    }

    #[test]
    fn cells_capture_rejects_attrs_and_history_and_defaults_stay_valid() {
        let cells = |attrs: bool, scrollback: u32| Request::Capture {
            if_revision: None,
            instance: None,
            id: 1,
            pane: PaneId(1),
            attrs,
            scrollback,
            max_bytes: 100,
            format: CaptureFormat::Cells,
        };
        assert!(cells(false, 0).validate().is_ok());
        assert!(cells(true, 0).validate().is_err());
        assert!(cells(false, 5).validate().is_err());
        // The defaults keep the plain request shape valid.
        assert!(matches!(
            decode_request_frame(br#"{"command":"capture","id":2,"pane":1,"max_bytes":10}"#),
            Ok(Request::Capture {
                format: CaptureFormat::Text,
                ..
            })
        ));
        assert!(matches!(
            decode_request_frame(br#"{"command":"info","id":3}"#),
            Ok(Request::Info { id: 3, .. })
        ));
    }

    #[test]
    fn key_text_decodes_escapes_exactly() {
        assert_eq!(
            decode_key_bytes("a\\n\\x1b\\e\\\\\\0é").ok(),
            Some(vec![b'a', b'\n', 0x1b, 0x1b, b'\\', 0, 0xc3, 0xa9])
        );
        assert!(decode_key_bytes("\\x1").is_err());
        assert!(decode_key_bytes("\\").is_err());
    }

    #[test]
    fn capture_rejects_removed_forms_and_conditional_capture_needs_an_identity() {
        for input in [
            r#"{"command":"capture","id":1,"pane":1,"max_bytes":4096,"if_revision":1}"#,
            r#"{"command":"capture","id":1,"pane":1,"max_bytes":4096,"instance":"","if_revision":1}"#,
            r#"{"command":"capture","id":1,"pane":1,"max_bytes":4096,"instance":"valid","if_revision":1,"format":"rows"}"#,
            r#"{"command":"capture","id":1,"pane":1,"max_bytes":4096,"since":3}"#,
        ] {
            assert!(decode_request_frame(input.as_bytes()).is_err());
        }
        assert!(decode_request_frame(br#"{"command":"capture","id":1,"pane":1,"max_bytes":4096,"instance":"valid","if_revision":1}"#).is_ok());
    }

    #[test]
    fn coherent_capture_reply_roundtrips_and_rejects_malformed_metadata() {
        let mut terminal = crate::terminal::ServerTerminal::new(4, 8, 10);
        terminal.process(b"text\x1b]2;title\x07");
        let reply = Reply::Completed {
            id: 7,
            result: CommandResult::Capture {
                seq: 42,
                input_sequence: 3,
                capture: Box::new(terminal.capture_snapshot(0, false, 4096, None)),
            },
        };
        let value = serde_json::to_value(&reply).unwrap();
        assert_eq!(value["result"]["value"]["text"], "text");
        assert_eq!(value["result"]["value"]["seq"], 42);
        assert_eq!(value["result"]["value"]["input_sequence"], 3);
        assert_eq!(
            serde_json::from_value::<Reply>(value.clone()).unwrap(),
            reply
        );
        let mut missing = value.clone();
        missing["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove("revision");
        assert!(serde_json::from_value::<Reply>(missing).is_err());
        let mut unknown = value.clone();
        unknown["result"]["value"]["unknown"] = true.into();
        assert!(serde_json::from_value::<Reply>(unknown).is_err());
        let encoded = serde_json::to_string(&value).unwrap();
        let duplicate = encoded.replace("\"revision\":1", "\"revision\":1,\"revision\":2");
        assert_ne!(duplicate, encoded);
        assert!(serde_json::from_str::<Reply>(&duplicate).is_err());
    }
}

/// Bounded final screen evidence; a missing exit status means teardown preceded exit observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalRecord {
    pub pane: PaneId,
    pub workspace: String,
    pub stream: u64,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub exit_status: Option<u32>,
    pub input_sequence: u64,
    pub capture: crate::terminal::CaptureSnapshot,
}
