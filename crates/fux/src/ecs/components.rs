//! Components of the authoritative multiplexer model. Entities: workspaces, tabs, panes and
//! attached viewers. Everything else (cells, history, bytes, configuration) is data inside them.

use crate::ids::{PaneId, TabId, ViewerId};
use crate::layout::{LayoutTree, Rect};
use crate::proto::attach::ServerMessage;
use crate::terminal::ServerTerminal;
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use super::messages::{Requester, ViewerRequest};

/// Which tab a viewer (or the workspace default) shows, and the focused pane per tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub tab: Option<Entity>,
    pub focus: BTreeMap<Entity, Entity>,
    pub history: FocusHistory,
}

/// At most two generational entity handles. History never authorizes substituting a pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusHistory {
    pub current: Option<Entity>,
    pub previous: Option<Entity>,
}

impl FocusHistory {
    pub fn observe(&mut self, focused: Option<Entity>) {
        if focused != self.current {
            if self.current.is_some() {
                self.previous = self.current;
            }
            self.current = focused;
        }
    }
}

impl Selection {
    /// Initial selection is inherited, but another viewer's focus history is not.
    #[must_use]
    pub fn for_viewer(&self) -> Self {
        let mut selection = self.clone();
        selection.history = FocusHistory {
            current: self.history.current.or(self.focused()),
            previous: None,
        };
        selection
    }
    #[must_use]
    pub fn focused_in(&self, tab: Entity, component: &Tab) -> Option<Entity> {
        component
            .zoomed
            .filter(|pane| component.layout.contains(*pane))
            .or_else(|| {
                self.focus
                    .get(&tab)
                    .copied()
                    .filter(|pane| component.layout.contains(*pane))
            })
            .or_else(|| component.layout.leaves().first().copied())
    }

    #[must_use]
    pub fn focused(&self) -> Option<Entity> {
        self.focus.get(&self.tab?).copied()
    }
    pub fn set_focus(&mut self, tab: Entity, pane: Entity) {
        self.focus.insert(tab, pane);
    }
    pub fn forget_tab(&mut self, tab: Entity) {
        self.focus.remove(&tab);
        if self.tab == Some(tab) {
            self.tab = None;
        }
    }
    /// Shows `tab`, focusing `pane` in it when one is given.
    pub fn select(&mut self, tab: Entity, pane: Option<Entity>) {
        self.tab = Some(tab);
        if let Some(pane) = pane {
            self.set_focus(tab, pane);
        }
    }
    /// Points the focus of `tab` at `next`, or forgets it when there is no successor.
    pub fn retarget(&mut self, tab: Entity, next: Option<Entity>) {
        match next {
            Some(next) => self.set_focus(tab, next),
            None => {
                self.focus.remove(&tab);
            }
        }
    }
}

/// A workspace groups tabs and is the unit koh gateways and zor observers address by name. Its
/// member tabs are the [`Tabs`] relationship target, kept by the ECS from each tab's [`TabOf`].
#[derive(Component, Debug)]
#[component(on_remove = release_workspace_name)]
pub struct Workspace {
    pub name: String,
    /// User-facing name, independent of the immutable routing identity.
    pub label: Option<String>,
    /// Default selection for new attachments and control-socket clients.
    pub selection: Selection,
    /// Step counter of the most recent attachment; the deterministic no-name attach rule.
    pub last_attached: u64,
    /// Consecutive automatic tab labels.
    pub tab_counter: u32,
}

/// The workspace is usable by viewers: its initial pane went live. Absent while reserved.
#[derive(Component, Debug)]
pub struct Open;

/// A workspace whose retirement has begun; finalized after the grace period.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retiring {
    pub since_ms: u64,
    pub exit_code: Option<u32>,
}

/// Query filter for workspaces that accept viewers and requests: open and not retiring.
pub type Accepting = (With<Open>, Without<Retiring>);

/// Membership of a tab in a workspace. Reserved tabs (a new tab or workspace whose first pane is
/// still starting) carry no `TabOf` until their completion; the ECS keeps [`Tabs`] in sync.
#[derive(Component, Debug)]
#[relationship(relationship_target = Tabs)]
pub struct TabOf(pub Entity);

/// A workspace's member tabs in order. Absent while a workspace has none. Despawning a workspace
/// does not cascade: panes are released through explicit effects first, then tabs are despawned.
#[derive(Component, Debug, Default)]
#[relationship_target(relationship = TabOf)]
pub struct Tabs(Vec<Entity>);

impl Tabs {
    /// Reorders existing relationship members without adding or removing membership.
    pub fn place_before(&mut self, tab: Entity, before: Option<Entity>) -> bool {
        let Some(index) = self.0.iter().position(|entry| *entry == tab) else {
            return false;
        };
        if before == Some(tab) {
            return true;
        }
        if before.is_some_and(|before| !self.0.contains(&before)) {
            return false;
        }
        self.0.remove(index);
        let position = before
            .and_then(|before| self.0.iter().position(|entry| *entry == before))
            .unwrap_or(self.0.len());
        self.0.insert(position, tab);
        true
    }
}

impl std::ops::Deref for Tabs {
    type Target = [Entity];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A tab owns one recursive split layout whose leaves are pane entities.
#[derive(Component, Debug)]
#[component(on_remove = release_tab_id)]
pub struct Tab {
    pub id: TabId,
    pub workspace: Entity,
    pub label: String,
    pub layout: LayoutTree<Entity>,
    /// Outer rectangles from the last layout resolution.
    pub geometry: Vec<(Entity, Rect)>,
    /// The area the geometry was computed for.
    pub area: Rect,
    pub layout_changed: bool,
    /// Monotonic layout/area revision for optimistic edits.
    pub layout_generation: u64,
    /// Shared tab zoom; the underlying tree and hidden PTYs remain intact.
    pub zoomed: Option<Entity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneState {
    /// Reserved; the adapter has not reported the spawn yet. Never in a layout or a frame.
    Starting,
    Live {
        pid: u32,
    },
    /// The PTY reached EOF but the exit status has not been observed yet.
    Eof {
        pid: u32,
    },
    /// Termination was requested; waiting for the exit report.
    Terminating {
        pid: u32,
        since_ms: u64,
    },
    Exited {
        code: u32,
    },
}

impl PaneState {
    #[must_use]
    pub fn pid(self) -> Option<u32> {
        match self {
            Self::Live { pid } | Self::Eof { pid } | Self::Terminating { pid, .. } => Some(pid),
            Self::Starting | Self::Exited { .. } => None,
        }
    }
    #[must_use]
    pub fn exit_code(self) -> Option<u32> {
        match self {
            Self::Exited { code } => Some(code),
            _ => None,
        }
    }
    #[must_use]
    pub fn accepts_input(self) -> bool {
        matches!(self, Self::Live { .. })
    }
}

/// Automatic spawn pins can be released after a consumer records exact process identity.
/// An explicit fixed route is a separate intent and cannot be released by that operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspacePin {
    None,
    Creation,
    Explicit,
}
impl WorkspacePin {
    pub fn is_fixed(self) -> bool {
        self != Self::None
    }
}

/// A pane is one terminal: its emulator/history, process lifecycle and geometry.
#[derive(Component)]
#[component(on_remove = release_pane_id)]
pub struct Pane {
    /// Current ownership, retained even after its tab closes. Launch attribution stays below.
    pub routing_workspace: Entity,
    /// A consumer requested stable workspace routing for this pane's lifetime.
    pub workspace_pin: WorkspacePin,
    pub id: PaneId,
    /// Immutable attribution survives tab/workspace retirement and name reuse.
    pub workspace_name: String,
    pub workspace_stream: u64,
    pub tab: Entity,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub state: PaneState,
    pub terminal: ServerTerminal,
    /// Outer rectangle within its tab, including the border cells.
    pub rect: Rect,
    /// The emulator, title or exit status changed since the retained grid was last refreshed.
    pub dirty: bool,
    /// Nonempty application bytes arrived since the last paced invalidation. Publish pane.output
    /// when the grid sequence changed, otherwise workspace.changed for capture-only changes.
    pub event_pending: bool,
    /// The grid sequence observed at the last paced invalidation. Capture-only changes do not
    /// advance this counter or produce a pane.output event.
    pub last_event_seq: u64,
    pub published_title: String,
    pub right_click: crate::view::RightClickPolicy,
    pub label: Option<String>,
    /// Nonempty controller/viewer writes; terminal query replies are excluded.
    pub input_sequence: u64,
    pub last_output_event_ms: Option<u64>,
    /// The launcher's retention for this pane's final record, already clamped to
    /// `MAX_FINAL_RETENTION_MS`; applied when the record is created at close.
    pub final_retain_ms: u64,
}

impl Pane {
    /// Brings the retained grid up to date when the pane is dirty; returns whether the output
    /// sequence advanced (a change an observer can see: rows, cursor, modes, title or exit).
    pub fn refresh(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        self.terminal
            .refresh_grid(&self.published_title, self.state.exit_code())
    }

    /// Whether this pane is the live, input-accepting process an exact attachment named.
    #[must_use]
    pub fn is_required_process(&self, want: &crate::proto::attach::InitialTarget) -> bool {
        self.workspace_stream == want.stream
            && self.state.pid() == Some(want.pid)
            && self.state.accepts_input()
    }

    /// Inner terminal size for an outer rectangle; never below the emulator minimum.
    #[must_use]
    pub fn terminal_size(rect: Rect) -> (u16, u16) {
        crate::terminal::clamp_dims(rect.height, rect.width)
    }
}

/// A reserved pane awaiting its spawn report, with everything needed to finish or roll back.
#[derive(Component, Debug)]
pub struct Creation {
    pub requesters: Vec<(Requester, u64)>,
    pub kind: CreationKind,
}

#[derive(Debug)]
pub enum CreationKind {
    /// Insert next to `target` in `tab`.
    Split {
        tab: Entity,
        target: Entity,
        axis: crate::layout::Axis,
        ratio: std::num::NonZeroU16,
        focus: bool,
    },
    /// The first pane of a new tab; the tab entity already exists but is not in the workspace.
    NewTab { tab: Entity },
    /// The first pane of a new workspace; the workspace is not open yet.
    Workspace { tab: Entity },
}

/// A pane as last sent to a viewer: its size and the output sequence the viewer holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub right_click: crate::view::RightClickPolicy,
    pub label: Option<String>,
    pub rows: u16,
    pub columns: u16,
    pub seq: u64,
}

/// An attached viewer: private tab/focus selection, bounded request queue and publication state.
#[derive(Component)]
#[component(on_remove = release_viewer_id)]
pub struct Viewer {
    /// Exact-attachment viewers close if their required process exits or changes workspace route.
    pub required_process: Option<crate::proto::attach::InitialTarget>,
    pub id: ViewerId,
    pub workspace: Entity,
    pub rows: u16,
    pub cols: u16,
    pub selection: Selection,
    pub queue: VecDeque<ViewerRequest>,
    /// A creation this viewer requested; later requests wait until it completes.
    pub barrier: Option<Entity>,
    pub generation: u64,
    /// Rectangles published in the last frame, for mouse hit tests.
    pub layout: Vec<(Entity, Rect)>,
    /// What this viewer holds of each visible pane: its size and the output sequence of its last
    /// update, so the next frame carries only the rows changed since.
    pub sent: BTreeMap<PaneId, Sent>,
    /// Last published tab catalog; ordinary terminal output need not repeat it.
    pub sent_tabs: Vec<crate::view::TabEntry>,
    pub sent_workspace_label: Option<String>,
    /// Metadata or selection changed: a frame goes out this step.
    pub dirty: bool,
    /// Output changed under this viewer since its last frame; paced by the frame interval.
    pub pending: bool,
    /// Set by the grid refresh when this step's frame may go out.
    pub publish_now: bool,
    /// When this viewer last sent input (keys, mouse, a request): output that follows within
    /// the frame interval is its echo and is never delayed.
    pub input_ms: u64,
    /// When the last frame went out, for pacing output-driven frames.
    pub last_frame_ms: u64,
    pub notice: Option<String>,
    /// Ordered messages that must follow the next frame.
    pub after_frame: Vec<ServerMessage>,
    pub detaching: bool,
    /// The retirement exit status was already sent; the detach path must not send another.
    pub exit_sent: bool,
}

impl Viewer {
    #[must_use]
    pub fn focused(&self) -> Option<Entity> {
        self.selection.focused()
    }

    /// Whether this viewer currently counts toward `workspace`'s viewer limit: attached to it
    /// and not already detaching.
    #[must_use]
    pub fn attached_to(&self, workspace: Entity) -> bool {
        self.workspace == workspace && !self.detaching
    }
}

// Index maintenance only: the public-id maps in `Ids` follow the entities they name, so no
// despawn path can forget to release an id. These hooks drive no command and emit no effect.
fn release_pane_id(mut world: DeferredWorld, context: HookContext) {
    let id = world.get_mut::<Pane>(context.entity).map(|pane| pane.id);
    if let (Some(id), Some(mut ids)) = (id, world.get_resource_mut::<super::resources::Ids>()) {
        ids.panes.remove(&id);
    }
}

fn release_tab_id(mut world: DeferredWorld, context: HookContext) {
    let id = world.get_mut::<Tab>(context.entity).map(|tab| tab.id);
    if let (Some(id), Some(mut ids)) = (id, world.get_resource_mut::<super::resources::Ids>()) {
        ids.tabs.remove(&id);
    }
}

fn release_viewer_id(mut world: DeferredWorld, context: HookContext) {
    let id = world
        .get_mut::<Viewer>(context.entity)
        .map(|viewer| viewer.id);
    if let (Some(id), Some(mut ids)) = (id, world.get_resource_mut::<super::resources::Ids>()) {
        ids.viewers.remove(&id);
    }
}

fn release_workspace_name(mut world: DeferredWorld, context: HookContext) {
    let name = world
        .get_mut::<Workspace>(context.entity)
        .map(|workspace| workspace.name.clone());
    if let (Some(name), Some(mut ids)) = (name, world.get_resource_mut::<super::resources::Ids>()) {
        ids.workspaces.remove(&name);
    }
}
