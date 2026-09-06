//! Components of the authoritative multiplexer model. Entities: workspaces, tabs, panes and
//! attached viewers. Everything else (cells, history, bytes, configuration) is data inside them.

use crate::ids::{PaneId, TabId, ViewerId};
use crate::layout::{LayoutTree, Rect};
use crate::proto::attach::ServerMessage;
use crate::terminal::ServerTerminal;
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use super::messages::{Requester, ViewerRequest};

/// Which tab a viewer (or the workspace default) shows, and the focused pane per tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub tab: Option<Entity>,
    pub focus: BTreeMap<Entity, Entity>,
}

impl Selection {
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
pub struct Workspace {
    pub name: String,
    /// Default selection for new attachments and control-socket clients.
    pub selection: Selection,
    /// Step counter of the most recent attachment; the deterministic no-name attach rule.
    pub last_attached: u64,
    /// The workspace is usable by viewers once its initial pane is live.
    pub open: bool,
    pub retiring: Option<Retiring>,
    /// Consecutive automatic tab labels.
    pub tab_counter: u32,
}

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

impl std::ops::Deref for Tabs {
    type Target = [Entity];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retiring {
    pub since_ms: u64,
    pub exit_code: Option<u32>,
}

/// A tab owns one recursive split layout whose leaves are pane entities.
#[derive(Component, Debug)]
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

/// A pane is one terminal: its emulator/history, process lifecycle and geometry.
#[derive(Component)]
pub struct Pane {
    pub id: PaneId,
    pub tab: Entity,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub state: PaneState,
    pub terminal: ServerTerminal,
    /// Outer rectangle within its tab, including the border cells.
    pub rect: Rect,
    /// The emulator, title or exit status changed since the retained grid was last refreshed.
    pub dirty: bool,
    /// Application bytes arrived since the last `pane.output` event; one is due when the interval
    /// allows. Set by the output phase, not by a refresh, so a resize or cursor move advances the
    /// sequence without counting as output.
    pub event_pending: bool,
    /// The output sequence carried by the last `pane.output` event, so an event fires only when
    /// the sequence has actually advanced (bytes that changed nothing produce none).
    pub last_event_seq: u64,
    pub published_title: String,
    /// The agent state last announced in a `pane.agent` event, to detect changes.
    pub published_agent: Option<crate::view::AgentReport>,
    pub last_output_event_ms: Option<u64>,
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
    },
    /// The first pane of a new tab; the tab entity already exists but is not in the workspace.
    NewTab { tab: Entity },
    /// The first pane of a new workspace; the workspace is not open yet.
    Workspace { tab: Entity },
}

/// A pane as last sent to a viewer: its size and the output sequence the viewer holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    pub rows: u16,
    pub columns: u16,
    pub seq: u64,
}

/// An attached viewer: private tab/focus selection, bounded request queue and publication state.
#[derive(Component)]
pub struct Viewer {
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
}
