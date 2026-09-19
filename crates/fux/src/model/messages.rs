//! The World never holds an OS handle (prompt 3.6): the runner writes a bounded batch of
//! [`Inbound`] messages before each `update` and drains [`Effect`]s after it. Both are Bevy
//! `Messages` used only within one update.

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use super::ids::{PaneId, ViewerId};

/// Who asked for something, so completions and errors can be routed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Requester {
    Viewer(Entity),
    /// A BRP request; the reply travels through `bevy_remote`'s own sender, so only the
    /// token-checked client entity is remembered for authority audits.
    Control(Entity),
    /// Restoration or the startup template.
    Server,
}

/// Bytes or actions originating from a viewer, after the viewer App resolved its own
/// presentation focus (prompt 3.11): the server only sees pane-directed input and layout
/// intents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewerRequest {
    /// Raw bytes for the targeted pane.
    Input(Vec<u8>),
    /// Retarget to a pane by id (refused for exact attachments).
    Target(PaneId),
    /// Show a template root by node id.
    Show(super::ids::NodeId),
    Resize {
        rows: u16,
        cols: u16,
    },
    /// Pointer event in viewport cells, forwarded to picking.
    Pointer(PointerEvent),
    Scroll {
        node: super::ids::NodeId,
        rows: i32,
    },
    Zoom(super::ids::NodeId),
    Unzoom,
    /// Convenience compositions of template edits, addressed relative to the targeted pane.
    Split {
        direction: SplitDirection,
        template: Option<super::components::PaneTemplate>,
    },
    ClosePane,
    Swap {
        direction: SplitDirection,
    },
    /// Keys typed while the viewer's focus is on a surface leaf; routed to the surface's
    /// provider as a `SurfaceInput { kind: Key }` event, never to a PTY.
    SurfaceKey {
        /// Scene revision actually painted when these bytes were read.
        revision: u64,
        node: super::ids::NodeId,
        bytes: Vec<u8>,
    },
    /// Create a new template root in the viewer's workspace with one pane and show it.
    NewRoot {
        template: Option<super::components::PaneTemplate>,
    },
    Detach,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirection {
    Right,
    Below,
}

/// A viewer mouse event in viewport cells. `modifiers` is a bit set of
/// [`PointerEvent::SHIFT`], [`PointerEvent::ALT`] and [`PointerEvent::CTRL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerEvent {
    /// Scene revision actually painted when the event was read.
    pub revision: u64,
    pub col: u16,
    pub row: u16,
    pub kind: PointerKind,
    pub button: PointerButton,
    pub modifiers: u8,
}

impl PointerEvent {
    /// Modifier bits in xterm parameter order (the same order the keyboard `1 + bits` parameter
    /// uses): shift, alt (meta), control.
    pub const SHIFT: u8 = 1;
    /// Alt held: with a primary press on a pane's content this starts a pane move
    /// (`crate::pointer`) instead of reaching the pane.
    pub const ALT: u8 = 2;
    pub const CTRL: u8 = 4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerKind {
    Move,
    Press,
    Release,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerButton {
    None,
    Left,
    Middle,
    Right,
}

/// Runner → World.
#[derive(Message, Debug)]
pub enum Inbound {
    /// PTY bytes for a pane. The buffer is owned by the adapter's pool of reusable buffers and
    /// returned through `Effect::RecycleBuffer`.
    PaneOutput {
        pane: Entity,
        bytes: Vec<u8>,
    },
    PaneEof {
        pane: Entity,
    },
    PaneSpawned {
        pane: Entity,
        pid: u32,
    },
    PaneSpawnFailed {
        pane: Entity,
        error: String,
    },
    PaneExited {
        pane: Entity,
        code: i32,
    },
    ViewerAttached {
        viewer: Entity,
        id: ViewerId,
    },
    ViewerRequest {
        viewer: Entity,
        request: ViewerRequest,
    },
    ViewerGone {
        viewer: Entity,
    },
    Signal(Signal),
    /// An adapter queued work for an in-World drain (e.g. an accepted attachment whose viewer
    /// entity does not exist yet); carries nothing and is ignored by every consumer.
    Wake,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
}

/// World → runner → OS adapters.
#[derive(Message, Debug)]
pub enum Effect {
    SpawnPane {
        pane: Entity,
        argv: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        rows: u16,
        cols: u16,
    },
    WritePty {
        pane: Entity,
        bytes: Vec<u8>,
    },
    ResizePty {
        pane: Entity,
        rows: u16,
        cols: u16,
    },
    /// SIGHUP to the process group now, SIGKILL after one second.
    Terminate {
        pane: Entity,
    },
    /// Drop the PTY and reader for an exited pane.
    ReleasePty {
        pane: Entity,
    },
    SendFrame {
        viewer: Entity,
        frame: crate::wire::ServerFrame,
    },
    CloseViewer {
        viewer: Entity,
    },
    RecycleBuffer(Vec<u8>),
    /// The World has nothing live left; the runner returns `AppExit`.
    Exit {
        code: u8,
    },
}
