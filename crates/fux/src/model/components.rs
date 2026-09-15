//! Components, grouped by mutation owner (prompt 3.2). Layout and appearance components are
//! `bevy_ui`'s own (`Node`, `ComputedNode`, `ZIndex`, `BackgroundColor`, `BorderColor`,
//! `ScrollPosition`, `UiTargetCamera`) and are not redeclared here.

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use bevy_time::Timer;
use serde::{Deserialize, Serialize};

use super::ids::{NodeId, PaneId, ViewerId, WorkspaceName};

// ---------------------------------------------------------------------------------------------
// Entity kind markers
// ---------------------------------------------------------------------------------------------

/// The single server entity.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Server;

#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Workspace;

/// A pane: the shared process entity (`Terminal`, PTY identity, receipts).
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Pane;

/// A template node: part of an inert `Node` subgraph per root, never laid out (no camera).
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct TemplateNode;

/// A template root: a "tab". Carries `RootOf`, `LayoutGeneration`, `Name`.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct TemplateRoot;

/// An instance node: one clone of a template subtree per viewer showing it, laid out against
/// that viewer's camera.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct InstanceNode;

/// A template leaf whose subtree is a scene streamed by another app (prompt 3.13).
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Surface;

#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Viewer;

#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct ControlClient;

// ---------------------------------------------------------------------------------------------
// Immutable identity (beyond the ids module)
// ---------------------------------------------------------------------------------------------

/// Named stream inside a workspace a pane or viewer belongs to (zor's per-agent streams).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct WorkspaceStream(pub String);

/// What launched a pane; immutable for the pane's life and persisted for restoration.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct LaunchAttribution {
    pub workspace_name: String,
    pub stream: String,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Process (written by the lifecycle set)
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub enum Process {
    Starting,
    Live {
        pid: u32,
    },
    /// The PTY reached EOF but the exit status is not known yet.
    Eof {
        pid: u32,
    },
    Terminating {
        pid: u32,
        since_ms: u64,
    },
    Exited {
        code: i32,
    },
}

impl Process {
    pub fn pid(self) -> Option<u32> {
        match self {
            Self::Live { pid } | Self::Eof { pid } | Self::Terminating { pid, .. } => Some(pid),
            Self::Starting | Self::Exited { .. } => None,
        }
    }
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Live { .. } | Self::Eof { .. } | Self::Terminating { .. }
        )
    }
}

/// How long a `FinalRecord` is retained after the pane exits.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct FinalRetention(pub u64);

/// Intent to launch a process into the leaf that carries it; materialised by the `Requests`
/// phase through the same validated transition as a client request (prompt 3.7). Never spawns
/// anything itself.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct PaneTemplate {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub stream: String,
}

// ---------------------------------------------------------------------------------------------
// Terminal (written by the ingest set)
// ---------------------------------------------------------------------------------------------

/// Pane title from OSC 0/2, bounded.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Title(pub String);

/// The pane's last accepted OSC 52 write (base64, bounded): present only once the pane has
/// written one. Carries content, so it is deliberately not reflected.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct Clipboard(pub String);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
pub enum RightClickPolicy {
    #[default]
    Auto,
    Paste,
    Forward,
}

/// Pacing state for the public `PaneOutput { seq }` event: at most one per interval per pane.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct OutputPacing {
    pub last_event_ms: u64,
    pub last_event_seq: u64,
}

// ---------------------------------------------------------------------------------------------
// Layout metadata
// ---------------------------------------------------------------------------------------------

/// Monotonic revision of a template root's shape; every mutating request names the generation
/// it saw and is rejected when stale.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
pub struct LayoutGeneration(pub u64);

/// The pane's emulator size: written by the layout set as the minimum over its `ShownBy`
/// instances' `ComputedNode` content sizes (`set_if_neq`); the terminal set reacts to
/// `Changed<PaneSize>` by resizing the emulator and emitting `Effect::ResizePty`.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct PaneSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PaneSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

/// Per-viewer zoom on an instance root: the named instance node fills the root; siblings off
/// its path get `Display::None`.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct Zoomed(pub Entity);

// ---------------------------------------------------------------------------------------------
// Viewer
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Viewport {
    pub rows: u16,
    pub cols: u16,
}

/// The viewer's layout camera entity (`Camera` + `RenderTarget::None`).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct ViewerCamera(pub Entity);

/// The picking pointer entity (`PointerId::Custom`) for this viewer.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct ViewerPointer(pub Entity);

/// An exact attachment: `Targets` cannot be retargeted and loss of the pane detaches the viewer.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct ExactTarget;

/// Viewer requests queued behind a creation barrier so input that followed a split in the same
/// read reaches the newly targeted pane.
#[derive(Component, Default, Debug)]
pub struct RequestQueue(pub std::collections::VecDeque<super::messages::ViewerRequest>);

#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct CreationBarrier(pub Option<Entity>);

/// What the viewer has been sent: the scene revision and per-pane terminal sequence and size,
/// so a frame carries only what changed. Rows are reused; no per-frame allocation.
#[derive(Component, Default, Debug)]
pub struct ProjectionBaseline {
    pub scene_revision: u64,
    pub panes: bevy_platform::collections::HashMap<Entity, PaneBaseline>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneBaseline {
    pub seq: u64,
    pub rows: u16,
    pub cols: u16,
}

/// A transient notice shown by the viewer, cleared when its timer finishes.
#[derive(Component, Debug)]
pub struct Notice {
    pub text: String,
    pub timer: Timer,
}

#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Detaching;

// ---------------------------------------------------------------------------------------------
// Presence markers (prompt 3.2: presence instead of flags)
// ---------------------------------------------------------------------------------------------

/// A workspace accepting new panes and viewers.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct Open;

/// A workspace winding down: its panes are terminating and no new work is accepted.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct Retiring {
    pub since_ms: u64,
}

/// A pane being created on behalf of the named requesters.
#[derive(Component, Debug, Default)]
pub struct Creation {
    pub requesters: Vec<super::messages::Requester>,
    pub kind: CreationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CreationKind {
    #[default]
    Spawn,
    Restore,
}

// ---------------------------------------------------------------------------------------------
// Receipts and records
// ---------------------------------------------------------------------------------------------

/// A reserved input operation on a pane (`fux/input.{reserve,submit,status}`).
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct InputOperation {
    pub id: u64,
    pub state: InputState,
    pub reserved_ms: u64,
}

#[derive(Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputState {
    Reserved,
    Submitted { seq: u64, bytes: usize },
    Uncertain,
    Expired,
}

/// Retained evidence of an exited pane (prompt: receipts, retained evidence).
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct FinalRecord {
    pub pane: PaneId,
    pub workspace: String,
    pub stream: String,
    pub exit_code: i32,
    pub title: String,
    pub last_seq: u64,
    pub exited_ms: u64,
    pub expires_ms: u64,
    pub screen: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

/// Server instance identity: nonce that every mutating request carries, plus the paths clients
/// need. Never persisted.
#[derive(Resource, Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerInstance {
    pub name: String,
    pub nonce: String,
    pub pid: u32,
    pub started_ms: u64,
}

pub(super) fn register_types(app: &mut App) {
    app.register_type::<PaneId>()
        .register_type::<NodeId>()
        .register_type::<ViewerId>()
        .register_type::<WorkspaceName>()
        .register_type::<WorkspaceStream>()
        .register_type::<LaunchAttribution>()
        .register_type::<Process>()
        .register_type::<FinalRetention>()
        .register_type::<PaneTemplate>()
        .register_type::<Title>()
        .register_type::<RightClickPolicy>()
        .register_type::<OutputPacing>()
        .register_type::<LayoutGeneration>()
        .register_type::<PaneSize>()
        .register_type::<Zoomed>()
        .register_type::<Viewport>()
        .register_type::<ViewerCamera>()
        .register_type::<ViewerPointer>()
        .register_type::<ExactTarget>()
        .register_type::<CreationBarrier>()
        .register_type::<Detaching>()
        .register_type::<Open>()
        .register_type::<Retiring>()
        .register_type::<InputOperation>()
        .register_type::<FinalRecord>()
        .register_type::<Server>()
        .register_type::<Workspace>()
        .register_type::<Pane>()
        .register_type::<TemplateNode>()
        .register_type::<TemplateRoot>()
        .register_type::<InstanceNode>()
        .register_type::<Surface>()
        .register_type::<Viewer>()
        .register_type::<ControlClient>()
        .register_type::<super::relations::RootOf>()
        .register_type::<super::relations::Roots>()
        .register_type::<super::relations::InstanceOf>()
        .register_type::<super::relations::Instances>()
        .register_type::<super::relations::Shows>()
        .register_type::<super::relations::ShownBy>()
        .register_type::<super::relations::Places>()
        .register_type::<super::relations::PlacedIn>()
        .register_type::<super::relations::PaneIn>()
        .register_type::<super::relations::WorkspacePanes>()
        .register_type::<super::relations::Viewing>()
        .register_type::<super::relations::ViewedBy>()
        .register_type::<super::relations::Targets>()
        .register_type::<super::relations::TargetedBy>()
        .register_type::<super::relations::OperationOn>()
        .register_type::<super::relations::PaneOperations>()
        .register_type::<super::relations::RootOrder>()
        .register_type::<super::relations::Showing>()
        .register_type::<bevy_ui::Node>()
        .register_type::<bevy_ui::ComputedNode>()
        .register_type::<bevy_ui::ZIndex>()
        .register_type::<bevy_ui::GlobalZIndex>()
        .register_type::<bevy_ui::BackgroundColor>()
        .register_type::<bevy_ui::BorderColor>()
        .register_type::<bevy_ui::ScrollPosition>()
        .register_type::<bevy_ui::UiTargetCamera>()
        .register_type::<Name>()
        .register_type::<ChildOf>()
        .register_type::<Children>();
}
