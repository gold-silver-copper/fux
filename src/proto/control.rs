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
pub const MAX_EVENT_FILTERS: usize = 32;
pub const MAX_SUBSCRIBER_QUEUE: usize = 1024;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_CONTROL_CONNECTIONS: usize = 64;
pub const MAX_WAIT_MS: u64 = 300_000;
pub const MAX_WAIT_PATTERN_BYTES: usize = 512;
/// A server holds at most this many pending waits across every connection.
pub const MAX_PENDING_WAITS: usize = 1024;
/// And at most this many on one pane, so one client cannot fill the table against a pane.
pub const MAX_WAITS_PER_PANE: usize = 64;

pub type RequestId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// Split the focused pane (or `target`) and start `argv` (default command when empty).
    Split {
        id: RequestId,
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
        target: FocusTarget,
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
        /// `escapes` (default) reads `\n \e \xHH`; `keys` reads space-separated key names
        /// (`Enter`, `Up`, `C-c`, `M-x`, literal characters).
        #[serde(default)]
        notation: KeyNotation,
    },
    /// The pane's text. `format: "rows"` returns the visible rows one by one with the cursor and
    /// the output sequence; with `since` only the rows changed after that sequence.
    Capture {
        id: RequestId,
        pane: PaneId,
        #[serde(default)]
        attrs: bool,
        #[serde(default)]
        scrollback: u32,
        max_bytes: usize,
        #[serde(default)]
        format: CaptureFormat,
        #[serde(default)]
        since: Option<u64>,
    },
    List {
        id: RequestId,
    },
    /// The server's identity, version, runtime directory and limits.
    Info {
        id: RequestId,
    },
    /// Block until `pane` meets `until` or `timeout_ms` elapses; the reply says which fired.
    Wait {
        id: RequestId,
        pane: PaneId,
        until: WaitUntil,
        timeout_ms: u64,
    },
    Tab {
        id: RequestId,
        action: TabAction,
    },
    Workspace {
        id: RequestId,
        action: WorkspaceAction,
    },
    Subscribe {
        id: RequestId,
        #[serde(default)]
        events: Vec<EventKind>,
    },
}

impl Request {
    pub fn id(&self) -> RequestId {
        match self {
            Self::Split { id, .. }
            | Self::Focus { id, .. }
            | Self::Kill { id, .. }
            | Self::Resize { id, .. }
            | Self::SendKeys { id, .. }
            | Self::Capture { id, .. }
            | Self::List { id }
            | Self::Info { id }
            | Self::Wait { id, .. }
            | Self::Tab { id, .. }
            | Self::Workspace { id, .. }
            | Self::Subscribe { id, .. } => *id,
        }
    }

    pub fn validate(&self) -> Result<(), ControlError> {
        let id = Some(self.id());
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
                since,
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
                if since.is_some() && (*format != CaptureFormat::Rows || *scrollback > 0) {
                    return Err(ControlError::invalid(
                        id,
                        "capture since needs format rows and no scrollback (history rows carry no sequence)",
                    ));
                }
                if *format == CaptureFormat::Rows && *attrs {
                    return Err(ControlError::invalid(
                        id,
                        "capture format rows carries plain text; attrs applies to the text format",
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
            Self::Wait {
                until, timeout_ms, ..
            } => {
                if *timeout_ms == 0 || *timeout_ms > MAX_WAIT_MS {
                    return Err(ControlError::invalid(
                        id,
                        format!("wait timeout must be 1-{MAX_WAIT_MS} ms"),
                    ));
                }
                match until {
                    WaitUntil::Quiet { ms } if *ms == 0 || *ms > MAX_WAIT_MS => {
                        return Err(ControlError::invalid(
                            id,
                            format!("wait quiet must be 1-{MAX_WAIT_MS} ms"),
                        ));
                    }
                    WaitUntil::Pattern { regex } => {
                        if regex.len() > MAX_WAIT_PATTERN_BYTES {
                            return Err(ControlError::invalid(
                                id,
                                format!(
                                    "wait pattern must be at most {MAX_WAIT_PATTERN_BYTES} bytes"
                                ),
                            ));
                        }
                        if regex_lite::Regex::new(regex).is_err() {
                            return Err(ControlError::invalid(
                                id,
                                "wait pattern is not a valid regex",
                            ));
                        }
                    }
                    _ => {}
                }
            }
            Self::Subscribe { events, .. } if events.len() > MAX_EVENT_FILTERS => {
                return Err(ControlError::invalid(
                    id,
                    format!("at most {MAX_EVENT_FILTERS} event filters are allowed"),
                ));
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
    /// Visible rows as `{row, text, wrapped}` entries with the cursor and the output sequence.
    Rows,
}

/// One visible row of a `rows` capture: plain text without trailing blanks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureRow {
    pub row: u16,
    pub text: String,
    pub wrapped: bool,
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

/// The configured and fixed limits a client may plan against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoLimits {
    pub workspaces: usize,
    pub tabs: usize,
    pub panes: usize,
    pub viewers: usize,
    pub scrollback_lines: usize,
    pub control_connections: usize,
    pub frame_bytes: usize,
    pub capture_bytes: usize,
    pub key_bytes: usize,
    pub event_filters: usize,
    pub subscriber_queue: usize,
    pub viewer_queue: usize,
    pub retire_grace_ms: u64,
    pub terminate_deadline_ms: u64,
    pub output_event_interval_ms: u64,
    pub frame_interval_ms: u64,
}

/// What a `wait` blocks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WaitUntil {
    /// No observable change for `ms`.
    Quiet { ms: u64 },
    /// The visible screen's plain text matches `regex`.
    Pattern { regex: String },
    /// The pane's process exits.
    Exit,
    /// The pane's output sequence reaches `value`.
    Seq { value: u64 },
}

/// Which `wait` condition fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitFired {
    Quiet,
    Pattern,
    Exit,
    Seq,
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
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum CommandResult {
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
    /// `text` plus the output sequence the text reflects.
    Capture {
        text: String,
        seq: u64,
    },
    /// The visible rows (only the changed ones when `since_applied`), the cursor and the output
    /// sequence they reflect.
    Rows {
        seq: u64,
        cursor: crate::view::Cursor,
        rows: Vec<CaptureRow>,
        since_applied: bool,
    },
    Listing {
        workspaces: Vec<WorkspaceSummary>,
    },
    Info {
        info: Box<ServerInfo>,
    },
    /// A `wait` fired: which condition, the pane's current sequence and its exit status.
    Waited {
        fired: WaitFired,
        seq: u64,
        exit_status: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
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
    #[serde(default)]
    pub progress: Option<(u8, u8)>,
    /// The output sequence: advances whenever the visible screen, cursor, modes, title or exit
    /// status changed; `capture` and `pane.output` report the same counter.
    pub seq: u64,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Event {
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
    #[serde(rename = "pane.title")]
    PaneTitle {
        id: RequestId,
        pane: PaneId,
        title: String,
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
    #[serde(rename = "client.attached")]
    ClientAttached { id: RequestId, client: u64 },
    #[serde(rename = "client.detached")]
    ClientDetached { id: RequestId, client: u64 },
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::PaneOpened { .. } => EventKind::PaneOpened,
            Self::PaneClosed { .. } => EventKind::PaneClosed,
            Self::PaneTitle { .. } => EventKind::PaneTitle,
            Self::PaneOutput { .. } => EventKind::PaneOutput,
            Self::TabOpened { .. } => EventKind::TabOpened,
            Self::TabClosed { .. } => EventKind::TabClosed,
            Self::ClientAttached { .. } => EventKind::ClientAttached,
            Self::ClientDetached { .. } => EventKind::ClientDetached,
        }
    }

    /// Stamps the subscriber's request id on a published copy.
    pub fn with_id(mut self, subscription: RequestId) -> Self {
        match &mut self {
            Self::PaneOpened { id, .. }
            | Self::PaneClosed { id, .. }
            | Self::PaneTitle { id, .. }
            | Self::PaneOutput { id, .. }
            | Self::TabOpened { id, .. }
            | Self::TabClosed { id, .. }
            | Self::ClientAttached { id, .. }
            | Self::ClientDetached { id, .. } => *id = subscription,
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    #[serde(rename = "pane.opened")]
    PaneOpened,
    #[serde(rename = "pane.closed")]
    PaneClosed,
    #[serde(rename = "pane.title")]
    PaneTitle,
    #[serde(rename = "pane.output")]
    PaneOutput,
    #[serde(rename = "tab.opened")]
    TabOpened,
    #[serde(rename = "tab.closed")]
    TabClosed,
    #[serde(rename = "client.attached")]
    ClientAttached,
    #[serde(rename = "client.detached")]
    ClientDetached,
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

/// Decodes one token (a named key, `C-<x>`, `M-<x>`, or a literal character) into bytes.
fn decode_token(token: &str) -> Result<Vec<u8>, ControlError> {
    if let Some(rest) = token.strip_prefix("C-") {
        let inner = decode_token(rest)?;
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
        bytes.extend(decode_token(rest)?);
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
        output.extend(decode_token(token)?);
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
    fn capture_since_needs_rows_without_history_and_rows_carry_no_attrs() {
        let base = Request::Capture {
            id: 1,
            pane: PaneId(1),
            attrs: false,
            scrollback: 0,
            max_bytes: 100,
            format: CaptureFormat::Rows,
            since: Some(3),
        };
        assert!(base.validate().is_ok());
        let text_since = match base.clone() {
            Request::Capture {
                id,
                pane,
                attrs,
                scrollback,
                max_bytes,
                since,
                ..
            } => Request::Capture {
                id,
                pane,
                attrs,
                scrollback,
                max_bytes,
                since,
                format: CaptureFormat::Text,
            },
            other => other,
        };
        assert!(text_since.validate().is_err());
        let history_since = match base.clone() {
            Request::Capture {
                id,
                pane,
                attrs,
                max_bytes,
                format,
                since,
                ..
            } => Request::Capture {
                id,
                pane,
                attrs,
                max_bytes,
                format,
                since,
                scrollback: 5,
            },
            other => other,
        };
        assert!(history_since.validate().is_err());
        let rows_attrs = match base {
            Request::Capture {
                id,
                pane,
                scrollback,
                max_bytes,
                format,
                since,
                ..
            } => Request::Capture {
                id,
                pane,
                scrollback,
                max_bytes,
                format,
                since,
                attrs: true,
            },
            other => other,
        };
        assert!(rows_attrs.validate().is_err());
        // The defaults keep yesterday's request shape valid.
        assert!(matches!(
            decode_request_frame(br#"{"command":"capture","id":2,"pane":1,"max_bytes":10}"#),
            Ok(Request::Capture {
                format: CaptureFormat::Text,
                since: None,
                ..
            })
        ));
        assert!(matches!(
            decode_request_frame(br#"{"command":"info","id":3}"#),
            Ok(Request::Info { id: 3 })
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
}
