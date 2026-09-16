//! Read-only projections of the model for BRP (`world.query`, `world.get_components`): one
//! reflected view component per public entity kind on projection entities of their own
//! (`Mirrors` → model, `linked_spawn` so a projection dies with its model). No authoritative
//! component is reachable through the reflected read path. Refreshed every update in
//! `PostUpdate`/`Phase::Projection` with in-place writes: steady state moves change ticks only
//! when a value changed.

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Has;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_reflect::prelude::*;

use crate::model::{
    AttemptId, AttemptState, Attempts, Check, CheckId, CheckOf, CheckState, Machine, MachineId,
    MachineName, Mirrors, NeedsInput, ProjectionEntity, Projections, Required, Requirement, Task,
    TaskId, TaskState, Title, Uncertain,
};

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct TaskView {
    pub id: String,
    pub title: String,
    pub state: String,
    pub attempts: usize,
    pub current_attempt: Option<u64>,
    pub uncertain: bool,
    /// An attempt of the task asked for input (`NeedsInput`).
    pub needs_input: bool,
}

/// One observed pane: identity, the merged observation and its evidence
/// (`providers::AgentRecord`, DASHBOARD.md:38-40).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct AgentView {
    pub id: u64,
    pub instance: String,
    pub workspace: String,
    pub pane: u64,
    pub pid: Option<u32>,
    pub state: String,
    /// `native`, `passive` or `none`.
    pub source: String,
    pub rule: Option<String>,
    pub passive: String,
    pub since_ms: u64,
    pub age_upper_bound_ms: u64,
    pub attempt: Option<u64>,
    pub task: Option<String>,
    pub provider: Option<String>,
    pub producer: Option<String>,
    pub last_event: Option<String>,
    pub last_event_ms: u64,
    pub problem: Option<String>,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct MachineView {
    pub id: String,
    pub name: String,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct CheckView {
    pub id: String,
    pub task: String,
    pub state: String,
    pub requirement: Option<String>,
    pub required: bool,
}

/// Fully qualified type paths of everything the wrapped `world.*` reads may name.
pub const ALLOWED_TYPE_PATHS: [&str; 5] = [
    "zor::model::components::ProjectionEntity",
    "zor::remote::projection::TaskView",
    "zor::remote::projection::AgentView",
    "zor::remote::projection::MachineView",
    "zor::remote::projection::CheckView",
];

pub fn is_allowed_type_path(path: &str) -> bool {
    ALLOWED_TYPE_PATHS.contains(&path)
}

pub fn register_types(app: &mut App) {
    app.register_type::<TaskView>()
        .register_type::<AgentView>()
        .register_type::<MachineView>()
        .register_type::<CheckView>();
}

fn state_name<T: core::fmt::Debug>(state: &T) -> String {
    format!("{state:?}")
}

/// Reused rows: phase one fills them from the model, phase two writes them through change
/// detection.
#[derive(Default)]
pub struct Scratch {
    tasks: Vec<(Entity, TaskView)>,
    agents: Vec<(Entity, AgentView)>,
    machines: Vec<(Entity, MachineView)>,
    checks: Vec<(Entity, CheckView)>,
}

fn slot<T: Default>(rows: &mut Vec<(Entity, T)>, i: usize, entity: Entity) -> Option<&mut T> {
    if rows.len() <= i {
        rows.push((entity, T::default()));
    }
    let (slot_entity, view) = rows.get_mut(i)?;
    *slot_entity = entity;
    Some(view)
}

fn set_str(dst: &mut String, src: &str) {
    if dst != src {
        dst.clear();
        dst.push_str(src);
    }
}

pub(super) fn sync(world: &mut World, mut scratch: Local<Scratch>) {
    let scratch = &mut *scratch;
    // Tasks.
    let mut n = 0;
    let rows: Vec<(Entity, String, String, String, Vec<Entity>)> = world
        .query_filtered::<(Entity, &TaskId, &Title, &TaskState, Option<&Attempts>), With<Task>>()
        .iter(world)
        .map(|(e, id, title, state, attempts)| {
            (
                e,
                id.0.clone(),
                title.0.clone(),
                state_name(state),
                attempts.map(|a| a.iter().collect()).unwrap_or_default(),
            )
        })
        .collect();
    for (entity, id, title, state, attempts) in rows {
        let current = attempts
            .iter()
            .find(|a| {
                world
                    .get::<AttemptState>(**a)
                    .is_some_and(|s| *s != AttemptState::Finished)
            })
            .and_then(|a| world.get::<AttemptId>(*a))
            .map(|a| a.0);
        let uncertain = attempts
            .iter()
            .any(|a| world.get::<Uncertain>(*a).is_some());
        let needs_input = attempts
            .iter()
            .any(|a| world.get::<NeedsInput>(*a).is_some());
        let Some(view) = slot(&mut scratch.tasks, n, entity) else {
            continue;
        };
        set_str(&mut view.id, &id);
        set_str(&mut view.title, &title);
        set_str(&mut view.state, &state);
        view.attempts = attempts.len();
        view.current_attempt = current;
        view.uncertain = uncertain;
        view.needs_input = needs_input;
        n += 1;
    }
    scratch.tasks.truncate(n);

    // Agents.
    n = 0;
    for (entity, record) in crate::providers::agent_records(world) {
        let Some(view) = slot(&mut scratch.agents, n, entity) else {
            continue;
        };
        view.id = record.id;
        set_str(&mut view.instance, &record.instance);
        set_str(&mut view.workspace, &record.workspace);
        view.pane = record.pane;
        view.pid = record.pid;
        set_str(&mut view.state, &record.state);
        set_str(&mut view.source, &record.source);
        view.rule = record.rule;
        set_str(&mut view.passive, &record.passive);
        view.since_ms = record.since_ms;
        view.age_upper_bound_ms = record.age_upper_bound_ms;
        view.attempt = record.attempt;
        view.task = record.task;
        view.provider = record.provider;
        view.producer = record.producer;
        view.last_event = record.last_event;
        view.last_event_ms = record.last_event_ms;
        view.problem = record.problem;
        n += 1;
    }
    scratch.agents.truncate(n);

    // Machines.
    n = 0;
    let rows: Vec<(Entity, String, String)> = world
        .query_filtered::<(Entity, &MachineId, &MachineName), With<Machine>>()
        .iter(world)
        .map(|(e, id, name)| (e, id.0.clone(), name.0.clone()))
        .collect();
    for (entity, id, name) in rows {
        let Some(view) = slot(&mut scratch.machines, n, entity) else {
            continue;
        };
        set_str(&mut view.id, &id);
        set_str(&mut view.name, &name);
        n += 1;
    }
    scratch.machines.truncate(n);

    // Checks.
    n = 0;
    let rows: Vec<(Entity, String, Entity, String, Option<String>, bool)> = world
        .query_filtered::<(
            Entity,
            &CheckId,
            &CheckOf,
            &CheckState,
            Option<&Requirement>,
            Has<Required>,
        ), With<Check>>()
        .iter(world)
        .map(|(e, id, of, state, req, required)| {
            (
                e,
                id.0.clone(),
                of.0,
                state_name(state),
                req.map(|r| r.0.clone()),
                required,
            )
        })
        .collect();
    for (entity, id, task, state, requirement, required) in rows {
        let task_id = world
            .get::<TaskId>(task)
            .map(|t| t.0.clone())
            .unwrap_or_default();
        let Some(view) = slot(&mut scratch.checks, n, entity) else {
            continue;
        };
        set_str(&mut view.id, &id);
        set_str(&mut view.task, &task_id);
        set_str(&mut view.state, &state);
        view.requirement = requirement;
        view.required = required;
        n += 1;
    }
    scratch.checks.truncate(n);

    upsert(world, &scratch.tasks);
    upsert(world, &scratch.agents);
    upsert(world, &scratch.machines);
    upsert(world, &scratch.checks);
}

/// Writes each row into the model's projection carrying `T`, spawning one linked to the model
/// when there is none yet.
fn upsert<T: Component<Mutability = bevy_ecs::component::Mutable> + Clone + PartialEq + Default>(
    world: &mut World,
    rows: &[(Entity, T)],
) {
    for (model, view) in rows {
        let projection = world
            .get::<Projections>(*model)
            .and_then(|p| p.iter().find(|&p| world.get::<T>(p).is_some()));
        match projection {
            Some(projection) => write_view(world, projection, view),
            None => {
                world.spawn((ProjectionEntity, view.clone(), Mirrors(*model)));
            }
        }
    }
}

/// `set_if_neq` without a by-value argument: compares, then clones into existing capacity.
fn write_view<T: Component<Mutability = bevy_ecs::component::Mutable> + Clone + PartialEq>(
    world: &mut World,
    entity: Entity,
    view: &T,
) {
    if let Some(mut current) = world.get_mut::<T>(entity) {
        let inner = current.bypass_change_detection();
        if inner != view {
            inner.clone_from(view);
            current.set_changed();
        }
    }
}
