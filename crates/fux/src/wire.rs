//! Attachment stream frames (prompt 3.10). A viewer connects to the loopback listener named in
//! `brp.json`, sends [`Hello`] as its first frame, and then exchanges length-prefixed
//! serde-encoded frames. Every message derives `deny_unknown_fields` and is pinned by a fixture
//! (prompt section 5). The frame is the viewer's instance scene: the changed allowlisted
//! components of this viewer's instance roots as a `DynamicWorld` RON delta with server-stable
//! entity ids, plus terminal deltas keyed by pane id.

use serde::{Deserialize, Serialize};

use crate::model::{NodeId, PaneId, ViewerRequest, Viewport};

/// Frames longer than this are a protocol error and close the connection.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Length prefix: big-endian u32.
pub const FRAME_PREFIX_BYTES: usize = 4;

/// First frame from the viewer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub token: String,
    /// Server instance nonce the viewer read from `brp.json`; a mismatch is refused.
    pub instance: String,
    pub workspace: String,
    #[serde(default)]
    pub stream: String,
    pub viewport: Viewport,
    /// Exact attachment: target this pane, refuse retargeting, detach on its loss.
    #[serde(default)]
    pub exact_target: Option<ExactTargetSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactTargetSpec {
    pub pane: PaneId,
    pub pid: Option<u32>,
}

/// Viewer → server after `Hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Request {
        request: ViewerRequest,
    },
    /// Acknowledges frames up to `revision` so the server can bound its outstanding window.
    Ack {
        revision: u64,
    },
}

/// Server → viewer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Reply to `Hello`.
    Welcome(Welcome),
    /// One update's worth of changes.
    Scene(SceneFrame),
    /// The server is closing this attachment; the viewer restores its terminal and exits.
    Bye { reason: ByeReason, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub viewer: crate::model::ViewerId,
    pub instance: String,
    pub workspace: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    Refused,
    Detached,
    WorkspaceRetired,
    ExactTargetLost,
    ServerShutdown,
    Protocol,
}

/// The viewer's instance scene delta for one server update.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneFrame {
    /// Monotonic per-viewer scene revision.
    pub revision: u64,
    /// `true` when `scene` is a full snapshot (first frame, resync); otherwise a delta.
    pub full: bool,
    /// RON `DynamicWorld` of allowlisted instance components with server-stable entity ids:
    /// `Node`, `ComputedNode`, `ZIndex`, `BackgroundColor`, `BorderColor`, `Name`, `PaneId`,
    /// `NodeId`, `Zoomed`, `ChildOf`, `Shows`, `InstanceNode`, plus surface subtrees. Empty when
    /// nothing changed.
    pub scene: String,
    /// Server-stable ids of instance entities that no longer exist.
    pub despawned: Vec<u64>,
    /// The template roots of the workspace in `RootOrder`, for the tab strip.
    pub roots: Option<Vec<RootEntry>>,
    /// The pane the viewer's input currently targets.
    pub target: Option<PaneId>,
    /// Which instance root is shown (server-stable entity id) and its `NodeId`.
    pub showing: Option<NodeId>,
    pub terminals: Vec<TerminalDelta>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootEntry {
    pub node: NodeId,
    pub name: String,
}

/// Rows of one pane's screen that changed since the viewer's baseline. Row content is the
/// old wire cell encoding: per row a run-length list of styled cells.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDelta {
    pub pane: PaneId,
    pub seq: u64,
    pub rows: u16,
    pub cols: u16,
    /// `true` when every row is present (size change or first sight).
    pub full: bool,
    pub lines: Vec<Line>,
    pub cursor: Cursor,
    pub modes: Modes,
    pub title: Option<String>,
    pub process: ProcessSummary,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Line {
    pub row: u16,
    pub cells: Vec<Cell>,
}

/// One cell; wide glyphs occupy `width` columns and are followed by no spacer cell.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub text: String,
    pub width: u8,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: u8,
}

impl Style {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 2;
    pub const UNDERLINE: u8 = 4;
    pub const INVERSE: u8 = 8;
    pub const DIM: u8 = 16;
    pub const STRIKE: u8 = 32;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Modes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub alternate_screen: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    #[default]
    None,
    Press,
    Drag,
    Motion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSummary {
    #[default]
    Starting,
    Live,
    Exited {
        code: i32,
    },
}

/// Encodes one frame as a length-prefixed JSON payload into `out` (reused buffer).
pub fn encode<T: Serialize>(frame: &T, out: &mut Vec<u8>) -> Result<(), serde_json::Error> {
    out.clear();
    out.extend_from_slice(&[0, 0, 0, 0]);
    serde_json::to_writer(&mut *out, frame)?;
    let len = u32::try_from(out.len() - FRAME_PREFIX_BYTES)
        .map_err(|_| serde_json::Error::io(std::io::Error::other("frame exceeds u32")))?;
    out[..FRAME_PREFIX_BYTES].copy_from_slice(&len.to_be_bytes());
    Ok(())
}

/// Reads a prefixed payload length; `None` until four bytes are available. Errors on oversize.
pub fn payload_len(prefix: &[u8]) -> Result<Option<usize>, std::io::Error> {
    let Some(head) = prefix.get(..FRAME_PREFIX_BYTES) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::other("frame too large"));
    }
    Ok(Some(len))
}
