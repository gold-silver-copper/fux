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
pub const MAX_SCROLLBACK_LINES: u32 = 100_000;
pub const MAX_EVENT_FILTERS: usize = 32;
pub const MAX_SUBSCRIBER_QUEUE: usize = 1024;
pub const MAX_NAME_BYTES: usize = 128;

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
    },
    /// Alias for a side-by-side split of the focused pane.
    New {
        id: RequestId,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        argv: Vec<String>,
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
    },
    Capture {
        id: RequestId,
        pane: PaneId,
        #[serde(default)]
        attrs: bool,
        #[serde(default)]
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
            | Self::New { id, .. }
            | Self::Focus { id, .. }
            | Self::Kill { id, .. }
            | Self::Resize { id, .. }
            | Self::SendKeys { id, .. }
            | Self::Capture { id, .. }
            | Self::List { id }
            | Self::Tab { id, .. }
            | Self::Workspace { id, .. }
            | Self::Subscribe { id, .. } => *id,
        }
    }

    pub fn validate(&self) -> Result<(), ControlError> {
        let id = Some(self.id());
        match self {
            Self::Split { argv, cwd, .. } | Self::New { argv, cwd, .. } => {
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
            }
            Self::Resize { delta: 0, .. } => {
                return Err(ControlError::invalid(id, "resize delta must not be zero"));
            }
            Self::SendKeys { keys, .. } => {
                if keys.len() > MAX_KEY_BYTES {
                    return Err(ControlError::invalid(
                        id,
                        format!("send-keys payload must be at most {MAX_KEY_BYTES} bytes"),
                    ));
                }
                decode_key_bytes(keys).map_err(|mut error| {
                    error.id = id;
                    error
                })?;
            }
            Self::Capture {
                max_bytes,
                scrollback,
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
    Select {
        index: u32,
    },
    SelectId {
        tab: TabId,
    },
    Rename {
        tab: TabId,
        name: String,
    },
    Close {
        tab: TabId,
    },
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
    Pane { pane: PaneId },
    Tab { tab: TabId },
    Workspace { name: String },
    Capture { text: String },
    Listing { workspaces: Vec<WorkspaceSummary> },
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
    PaneOutput { id: RequestId, pane: PaneId },
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
    fn key_text_decodes_escapes_exactly() {
        assert_eq!(
            decode_key_bytes("a\\n\\x1b\\e\\\\\\0é").ok(),
            Some(vec![b'a', b'\n', 0x1b, 0x1b, b'\\', 0, 0xc3, 0xa9])
        );
        assert!(decode_key_bytes("\\x1").is_err());
        assert!(decode_key_bytes("\\").is_err());
    }
}
