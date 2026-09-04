//! Host-independent control protocol and local Unix transport.

mod protocol;
mod queue;
#[cfg(unix)]
mod socket;

pub use protocol::{
    AgentStatus, Axis, ClientIdentity, CommandResult, ControlError, ErrorCode, Event, EventKind,
    FocusTarget, PaneCopySummary, PaneCursorSummary, PaneGeometry, PaneModesSummary, PaneSummary,
    Reply, ReplyError, ReplyState, Request, TabAction, TabSummary, WorkspaceAction,
    WorkspaceSummary, decode_key_bytes, decode_request_frame, error_reply, read_request,
    write_frame,
};
pub use queue::{EventQueue, EventReceiver, PublishOutcome};
#[cfg(unix)]
pub use socket::{BoundControlSocket, PeerAuthorization, bind_control_socket, control_socket_path};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_ARGV_ENTRIES: usize = 128;
pub const MAX_ARG_BYTES: usize = 4096;
pub const MAX_ARGV_BYTES: usize = 16 * 1024;
pub const MAX_ENV_ENTRIES: usize = 128;
pub const MAX_ENV_BYTES: usize = 32 * 1024;
/// Maximum captured text before JSON encoding. At 128 KiB even worst-case `\u00xx` escaping plus
/// the reply envelope remains below [`MAX_FRAME_BYTES`].
pub const MAX_CAPTURE_BYTES: usize = 128 * 1024;
pub const MAX_KEY_BYTES: usize = 64 * 1024;
pub const MAX_SCROLLBACK_LINES: u32 = 100_000;
pub const MAX_STATUS_BYTES: usize = 4096;
pub const MAX_EVENT_FILTERS: usize = 32;
pub const MAX_SUBSCRIBER_QUEUE: usize = 1024;
