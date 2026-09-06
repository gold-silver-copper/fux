//! The authoritative multiplexer model: a `bevy_ecs` World of workspaces, tabs, panes and
//! viewers, advanced by one explicitly ordered schedule per step.
//!
//! The owner loop (`server`) feeds a bounded batch of [`Inbound`] events, runs one step and
//! applies the resulting [`Effect`]s through operating-system adapters. Tests drive the same
//! `Session` with injected events and time.

pub mod components;
pub mod messages;
pub mod resources;
pub mod support;
pub mod systems;

pub use messages::{Effect, Inbound, ManagerAction, ManagerOutcome, Requester, ViewerRequest};
pub use resources::{Clock, Deadlines, Ids, Limits, Registry};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{ScheduleLabel, SingleThreadedExecutor};

#[derive(ScheduleLabel, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Step;

/// Chained phases; see docs/design.md for what each one owns.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Ingest,
    Output,
    Requests,
    Completions,
    Lifecycle,
    Layout,
    Snapshot,
    Publish,
}

pub struct Session {
    world: World,
    schedule: Schedule,
}

impl Session {
    pub fn new(config: &crate::config::Config) -> anyhow::Result<Self> {
        let bindings = crate::commands::configured_bindings(config)?;
        let mut world = World::new();
        world.insert_resource(Limits::from_config(config));
        world.insert_resource(Registry {
            bindings,
            default_command: config.default_command.argv.clone(),
        });
        world.init_resource::<Ids>();
        world.init_resource::<Clock>();
        world.init_resource::<Deadlines>();
        world.init_resource::<resources::ShuttingDown>();
        world.init_resource::<resources::WorkspaceCounter>();
        world.init_resource::<Messages<Inbound>>();
        world.init_resource::<Messages<Effect>>();
        let mut schedule = Schedule::new(Step);
        schedule.set_executor(SingleThreadedExecutor::new());
        schedule.configure_sets(
            (
                Phase::Ingest,
                Phase::Output,
                Phase::Requests,
                Phase::Completions,
                Phase::Lifecycle,
                Phase::Layout,
                Phase::Snapshot,
                Phase::Publish,
            )
                .chain(),
        );
        // Queued viewer requests are drained after the requests phase and again after
        // completions release creation barriers, so input that followed a split in the same read
        // reaches the newly focused pane before this step publishes frames.
        schedule.add_systems((
            systems::requests::apply_attachments.in_set(Phase::Ingest),
            systems::output::apply_pane_output.in_set(Phase::Output),
            (
                systems::requests::apply_requests,
                systems::requests::drain_viewer_queues,
            )
                .chain()
                .in_set(Phase::Requests),
            (
                systems::creation::apply_spawn_completions,
                systems::requests::drain_viewer_queues,
            )
                .chain()
                .in_set(Phase::Completions),
            systems::lifecycle::resolve_lifecycle.in_set(Phase::Lifecycle),
            systems::layout::resolve_layout.in_set(Phase::Layout),
            systems::snapshot::publish_frames.in_set(Phase::Snapshot),
            systems::snapshot::finish_step.in_set(Phase::Publish),
        ));
        Ok(Self { world, schedule })
    }

    /// Runs one step at `now_ms` over the given inbound batch and returns the ordered effects.
    pub fn step(&mut self, now_ms: u64, inbound: impl IntoIterator<Item = Inbound>) -> Vec<Effect> {
        {
            let mut clock = self.world.resource_mut::<Clock>();
            clock.now_ms = now_ms;
            clock.step = clock.step.wrapping_add(1);
        }
        self.world.resource_mut::<Deadlines>().next_ms = None;
        self.world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(inbound);
        self.schedule.run(&mut self.world);
        self.world.clear_trackers();
        self.world
            .resource_mut::<Messages<Effect>>()
            .drain()
            .collect()
    }

    /// The next time the session needs a step without new input, if any.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.world.resource::<Deadlines>().next_ms
    }

    /// Retained messages after a step: zero by construction.
    pub fn retained_messages(&self) -> usize {
        self.world.resource::<Messages<Inbound>>().len()
            + self.world.resource::<Messages<Effect>>().len()
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// Names of open workspaces, sorted.
    pub fn workspace_names(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self
            .world
            .query::<&components::Workspace>()
            .iter(&self.world)
            .filter(|workspace| workspace.open && workspace.retiring.is_none())
            .map(|workspace| workspace.name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn entity_counts(&mut self) -> EntityCounts {
        EntityCounts {
            workspaces: self
                .world
                .query::<&components::Workspace>()
                .iter(&self.world)
                .count(),
            tabs: self
                .world
                .query::<&components::Tab>()
                .iter(&self.world)
                .count(),
            panes: self
                .world
                .query::<&components::Pane>()
                .iter(&self.world)
                .count(),
            viewers: self
                .world
                .query::<&components::Viewer>()
                .iter(&self.world)
                .count(),
        }
    }

    /// Structural invariants that must hold after every step.
    pub fn check_invariants(&mut self) -> Result<(), String> {
        let ids = self.world.resource::<Ids>().clone();
        let mut layout_panes = std::collections::HashSet::new();
        for (entity, tab) in self
            .world
            .query::<(Entity, &components::Tab)>()
            .iter(&self.world)
        {
            tab.layout
                .validate()
                .map_err(|error| format!("tab {} layout: {error}", tab.id))?;
            let workspace = self
                .world
                .get::<components::Workspace>(tab.workspace)
                .ok_or_else(|| format!("tab {} has no workspace", tab.id))?;
            if ids.tab(tab.id) != Some(entity) {
                return Err(format!("tab {} is not registered", tab.id));
            }
            let member = self
                .world
                .get::<components::TabOf>(entity)
                .is_some_and(|member| member.0 == tab.workspace);
            for pane in tab.layout.leaves() {
                if pane == Entity::PLACEHOLDER {
                    if member {
                        return Err(format!("tab {} exposes a placeholder pane", tab.id));
                    }
                    continue;
                }
                if !layout_panes.insert(pane) {
                    return Err(format!("pane {pane:?} appears in two layouts"));
                }
                let component = self
                    .world
                    .get::<components::Pane>(pane)
                    .ok_or_else(|| format!("tab {} references a missing pane", tab.id))?;
                if component.tab != entity {
                    return Err(format!("pane {} is in a foreign layout", component.id));
                }
                if matches!(component.state, components::PaneState::Starting) {
                    return Err(format!("starting pane {} is visible", component.id));
                }
            }
            if member && tab.layout.is_empty() && workspace.retiring.is_none() {
                return Err(format!("tab {} is empty but still a member", tab.id));
            }
        }
        for (entity, pane) in self
            .world
            .query::<(Entity, &components::Pane)>()
            .iter(&self.world)
        {
            if ids.pane(pane.id) != Some(entity) {
                return Err(format!("pane {} is not registered", pane.id));
            }
            // A tab may close ahead of its panes' exit reports; only a terminating pane may
            // outlive its tab, and it leaves as soon as the adapter reports the exit.
            if self.world.get::<components::Tab>(pane.tab).is_none()
                && !matches!(pane.state, components::PaneState::Terminating { .. })
            {
                return Err(format!("pane {} has no tab", pane.id));
            }
        }
        for (entity, workspace) in self
            .world
            .query::<(Entity, &components::Workspace)>()
            .iter(&self.world)
        {
            if ids.workspace(&workspace.name) != Some(entity) {
                return Err(format!("workspace {} is not registered", workspace.name));
            }
            let members = support::member_tabs(&self.world, entity);
            for tab in &members {
                if self
                    .world
                    .get::<components::Tab>(*tab)
                    .is_none_or(|tab| tab.workspace != entity)
                {
                    return Err(format!("workspace {} lists a foreign tab", workspace.name));
                }
            }
            if let Some(tab) = workspace.selection.tab
                && !members.contains(&tab)
                && workspace.retiring.is_none()
                && workspace.open
            {
                return Err(format!(
                    "workspace {} selects a foreign tab",
                    workspace.name
                ));
            }
        }
        for (entity, viewer) in self
            .world
            .query::<(Entity, &components::Viewer)>()
            .iter(&self.world)
        {
            if ids.viewer(viewer.id) != Some(entity) {
                return Err(format!("viewer {} is not registered", viewer.id));
            }
            let workspace = self
                .world
                .get::<components::Workspace>(viewer.workspace)
                .ok_or_else(|| format!("viewer {} has no workspace", viewer.id))?;
            if let Some(tab) = viewer.selection.tab
                && !support::is_member(&self.world, viewer.workspace, tab)
                && workspace.retiring.is_none()
            {
                return Err(format!("viewer {} shows a foreign tab", viewer.id));
            }
            if let Some(barrier) = viewer.barrier
                && self.world.get::<components::Creation>(barrier).is_none()
            {
                return Err(format!("viewer {} waits on a finished creation", viewer.id));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntityCounts {
    pub workspaces: usize,
    pub tabs: usize,
    pub panes: usize,
    pub viewers: usize,
}
