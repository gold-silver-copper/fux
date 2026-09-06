//! Shared resources: limits, identity registry, clock and deadlines.

use crate::ids::{PaneId, TabId, ViewerId};
use bevy_ecs::prelude::*;
use std::collections::BTreeMap;

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
        }
    }
}

/// One public identity space mapped to entities.
#[derive(Clone, Debug)]
pub struct IdIndex<K: Ord>(BTreeMap<K, Entity>);

impl<K: Ord> Default for IdIndex<K> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: Ord> IdIndex<K> {
    #[must_use]
    pub fn entity<Q>(&self, key: &Q) -> Option<Entity>
    where
        K: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.get(key).copied()
    }
}

impl<K: Ord> std::ops::Deref for IdIndex<K> {
    type Target = BTreeMap<K, Entity>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Ord> std::ops::DerefMut for IdIndex<K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Public identities and their entities. Ids never repeat within a server lifetime.
#[derive(Resource, Clone, Debug, Default)]
pub struct Ids {
    next_pane: u32,
    next_tab: u32,
    pub panes: IdIndex<PaneId>,
    pub tabs: IdIndex<TabId>,
    pub viewers: IdIndex<ViewerId>,
    pub workspaces: IdIndex<String>,
}

impl Ids {
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
        self.panes.entity(&id)
    }
    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<Entity> {
        self.tabs.entity(&id)
    }
    #[must_use]
    pub fn viewer(&self, id: ViewerId) -> Option<Entity> {
        self.viewers.entity(&id)
    }
    #[must_use]
    pub fn workspace(&self, name: &str) -> Option<Entity> {
        self.workspaces.entity(name)
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

/// Set when the owner loop asked for shutdown; lifecycle drains everything and reports idle.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ShuttingDown(pub bool);

/// Automatic workspace label counter.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct WorkspaceCounter(pub u32);
