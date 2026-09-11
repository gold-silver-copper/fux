//! Shared resources: limits, identity registry, clock and deadlines.

use crate::ids::{PaneId, TabId, ViewerId};
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, VecDeque};

/// Resource budgets. Fixed ceilings live in `view`/`config`; these are the configured values.
#[derive(Resource, Clone, Debug)]
pub struct Limits {
    pub max_workspaces: usize,
    pub max_tabs: usize,
    pub max_panes: usize,
    pub max_viewers: usize,
    pub scrollback_lines: usize,
    /// Viewer requests buffered while a creation barrier is pending.
    pub viewer_queue: usize,
    /// Retirement waits this long for viewers to observe the final frame.
    pub retire_grace_ms: u64,
    /// A terminating pane whose exit report never arrives is forcibly dropped after this.
    pub terminate_deadline_ms: u64,
    /// Minimum spacing of `pane.output` events per pane.
    pub output_event_interval_ms: u64,
    /// Minimum spacing of output-driven frames per viewer; replies, selection changes and
    /// retirement are never delayed.
    pub frame_interval_ms: u64,
    /// Retention of the final record of a pane fux creates itself (the initial pane of a
    /// workspace, a new tab's pane): the configured `[final] retain-ms`.
    pub final_retain_ms: u64,
}

impl Limits {
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            max_workspaces: config.limits.max_workspaces,
            max_tabs: config.limits.max_tabs,
            max_panes: config.limits.max_panes,
            max_viewers: crate::proto::attach::MAX_VIEWERS_PER_WORKSPACE,
            scrollback_lines: usize::try_from(config.history.scrollback_lines)
                .unwrap_or(usize::MAX),
            viewer_queue: 256,
            retire_grace_ms: 5_000,
            terminate_deadline_ms: 10_000,
            output_event_interval_ms: 250,
            frame_interval_ms: 8,
            final_retain_ms: config.final_records.retain_ms,
        }
    }
}

/// Public identities and their entities. Ids never repeat within a server lifetime.
#[derive(Resource, Clone, Debug, Default)]
pub struct Ids {
    next_pane: u32,
    next_stream: u64,
    next_tab: u32,
    pub panes: BTreeMap<PaneId, Entity>,
    pub tabs: BTreeMap<TabId, Entity>,
    pub viewers: BTreeMap<ViewerId, Entity>,
    pub workspaces: BTreeMap<String, Entity>,
}

impl Ids {
    pub fn next_stream(&mut self) -> Option<u64> {
        self.next_stream = self.next_stream.checked_add(1)?;
        Some(self.next_stream)
    }
    pub fn next_pane(&mut self) -> Option<PaneId> {
        self.next_pane = self.next_pane.checked_add(1)?;
        Some(PaneId(self.next_pane))
    }
    pub fn next_tab(&mut self) -> Option<TabId> {
        self.next_tab = self.next_tab.checked_add(1)?;
        Some(TabId(self.next_tab))
    }
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<Entity> {
        self.panes.get(&id).copied()
    }
    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<Entity> {
        self.tabs.get(&id).copied()
    }
    #[must_use]
    pub fn viewer(&self, id: ViewerId) -> Option<Entity> {
        self.viewers.get(&id).copied()
    }
    #[must_use]
    pub fn workspace(&self, name: &str) -> Option<Entity> {
        self.workspaces.get(name).copied()
    }
}

/// Step time in milliseconds, injected by the owner loop (or a test) before each step.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct Clock {
    pub now_ms: u64,
    pub step: u64,
}

/// The earliest future time at which a system needs to run without new input.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct Deadlines {
    pub next_ms: Option<u64>,
}

impl Deadlines {
    pub fn propose(&mut self, at_ms: u64) {
        self.next_ms = Some(self.next_ms.map_or(at_ms, |current| current.min(at_ms)));
    }
}

/// The configured registry published with every frame plus the default pane command.
#[derive(Resource, Clone, Debug)]
pub struct Registry {
    pub bindings: crate::commands::ClientBindings,
    pub default_command: Vec<String>,
}

/// What the server answers to `info`: installed by the owner before the first step. Tests keep
/// the default.
#[derive(Resource, Clone, Debug, Default)]
pub struct ServerIdentity {
    pub pid: u32,
    pub instance_nonce: String,
    pub runtime_dir: std::path::PathBuf,
}

/// Set when the owner loop asked for shutdown; lifecycle drains everything and reports idle.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ShuttingDown(pub bool);

/// Automatic workspace label counter.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct WorkspaceCounter(pub u32);

/// Fixed, global receipt budget. Unexpired operations are never evicted to admit new ones.
/// The retention duration is the caller's (`input-reserve.retain_ms`), under
/// [`crate::proto::control::MAX_INPUT_RETENTION_MS`].
pub const MAX_INPUT_OPERATIONS: usize = 128;

pub struct InputRecord {
    pub workspace: Entity,
    pub receipt: crate::proto::control::InputReceipt,
    /// Exact decoded payload, retained only after submission to detect conflicting retries.
    pub bytes: Option<Vec<u8>>,
}

#[derive(Resource, Default)]
pub struct InputOperations {
    pub next: u64,
    pub records: BTreeMap<u64, InputRecord>,
}

/// Fixed, global final-record budget. The retention duration is the launcher's
/// (`split.final_retain_ms`, stored on the pane), under
/// [`crate::proto::control::MAX_FINAL_RETENTION_MS`].
pub const MAX_FINAL_RECORDS: usize = 128;

#[derive(Resource, Default)]
pub struct FinalRecords(pub BTreeMap<PaneId, RetainedFinal>);

/// A retained final record with the server-side bookkeeping that never crosses the wire.
pub struct RetainedFinal {
    pub record: crate::proto::control::FinalRecord,
    pub closed_ms: u64,
    pub expires_ms: u64,
}

/// How many pane ids each ring of [`ForgottenFinals`] remembers. 1024 ids (4 KiB per ring) is
/// eight times the record cap: even a burst that turns the whole record set over several times
/// keeps the recently forgotten ids distinguishable, while nothing here grows with the load.
pub const MAX_FORGOTTEN_FINAL_IDS: usize = 1024;

/// Pane ids whose final records are gone, so `final` can say why instead of guessing. Two
/// bounded rings, per server instance, oldest id dropped first: `evicted` holds ids the record
/// cap forced out before their `expires_ms` (under load), `expired` holds ids whose retention
/// elapsed. An id that fell off its ring, or that never had a record, is `unknown`.
#[derive(Resource, Default)]
pub struct ForgottenFinals {
    pub evicted: VecDeque<PaneId>,
    pub expired: VecDeque<PaneId>,
}

impl ForgottenFinals {
    fn push(ring: &mut VecDeque<PaneId>, pane: PaneId) {
        if ring.len() >= MAX_FORGOTTEN_FINAL_IDS {
            ring.pop_front();
        }
        ring.push_back(pane);
    }

    pub fn evicted(&mut self, pane: PaneId) {
        Self::push(&mut self.evicted, pane);
    }

    pub fn expired(&mut self, pane: PaneId) {
        Self::push(&mut self.expired, pane);
    }
}
