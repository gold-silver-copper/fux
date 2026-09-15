//! Typed constructors of the entity graph: the only way model entities are created. Each one
//! validates id syntax and uniqueness (invariant 1, 14), the owner's kind (invariant 2) and the
//! record bound (invariant 11) before spawning, so hooks only ever record.

use bevy_ecs::prelude::*;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// Not `1..=64` ASCII `[A-Za-z0-9_-]`.
    InvalidId(String),
    /// Already mapped, in this or the shared operation namespace.
    DuplicateId(String),
    /// The owner is not an entity of the expected kind.
    WrongKind {
        expected: &'static str,
        entity: Entity,
    },
    /// The record bound of `Limits` is reached.
    Limit(&'static str),
    Invalid(String),
}

impl core::fmt::Display for ModelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid id {id:?}"),
            Self::DuplicateId(id) => write!(f, "id {id:?} already exists"),
            Self::WrongKind { expected, entity } => {
                write!(f, "{entity} is not a {expected}")
            }
            Self::Limit(what) => write!(f, "{what} limit reached"),
            Self::Invalid(reason) => f.write_str(reason),
        }
    }
}

impl core::error::Error for ModelError {}

fn kind<M: Component>(world: &World, entity: Entity, name: &'static str) -> Result<(), ModelError> {
    if world.get::<M>(entity).is_some() {
        Ok(())
    } else {
        Err(ModelError::WrongKind {
            expected: name,
            entity,
        })
    }
}

fn count<M: Component>(world: &mut World) -> usize {
    world.query_filtered::<(), With<M>>().iter(world).count()
}

fn bound<M: Component>(
    world: &mut World,
    limit: impl Fn(&Limits) -> usize,
    what: &'static str,
) -> Result<(), ModelError> {
    let limit = limit(world.resource::<Limits>());
    if count::<M>(world) >= limit {
        return Err(ModelError::Limit(what));
    }
    Ok(())
}

fn string_id<T: Indexed + for<'a> From<&'a str>>(
    world: &mut World,
    id: &str,
) -> Result<T, ModelError> {
    if !valid_id(id) {
        return Err(ModelError::InvalidId(id.to_owned()));
    }
    let key = T::from(id);
    if world.resource_mut::<Ids>().get(&key).is_some() {
        return Err(ModelError::DuplicateId(id.to_owned()));
    }
    Ok(key)
}

macro_rules! from_str {
    ($($name:ident),*) => {
        $(impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(id.to_owned())
            }
        })*
    };
}
from_str!(
    TaskId,
    PromptId,
    OperationId,
    CheckId,
    SourceId,
    ArtifactId,
    GroupId,
    WorktreeId,
    MachineId,
    ServiceId,
    PluginId,
    PluginActionId
);

pub struct TaskSpec<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub location: Location,
    pub created_ms: u64,
}

pub fn spawn_task(world: &mut World, spec: TaskSpec<'_>) -> Result<Entity, ModelError> {
    bound::<Task>(world, |l| l.tasks, "task")?;
    let id: TaskId = string_id(world, spec.id)?;
    Ok(world
        .spawn((
            Task,
            id,
            Title(spec.title.to_owned()),
            spec.location,
            CreatedMs(spec.created_ms),
            TaskState::Open,
        ))
        .id())
}

pub struct AttemptSpec {
    pub task: Entity,
    pub ownership: Ownership,
    pub handle: PaneHandle,
}

/// Closes a task with `outcome` (TASKS.md:394-399): `Verified` needs the seal already in
/// place (invariant 4); a closed task is never reopened or re-outcomed.
pub fn close_task(
    world: &mut World,
    task: Entity,
    outcome: TaskOutcome,
    now_ms: u64,
) -> Result<(), ModelError> {
    kind::<Task>(world, task, "task")?;
    let current = world.get::<TaskState>(task).copied().unwrap_or_default();
    let next = TaskState::Closed { outcome };
    if !current.may_become(next) {
        return Err(ModelError::Invalid(format!(
            "task {task} is {current:?} and cannot become {next:?}"
        )));
    }
    if outcome == TaskOutcome::Verified && world.get::<Seal>(task).is_none() {
        return Err(ModelError::Invalid(
            "a task is verified only through its seal".into(),
        ));
    }
    world.entity_mut(task).insert((next, ClosedMs(now_ms)));
    Ok(())
}

pub fn spawn_attempt(world: &mut World, spec: AttemptSpec) -> Result<Entity, ModelError> {
    kind::<Task>(world, spec.task, "task")?;
    bound::<Attempt>(world, |l| l.attempts, "attempt")?;
    let id = world.resource_mut::<Ids>().allocate_attempt();
    Ok(world
        .spawn((
            Attempt,
            id,
            AttemptOf(spec.task),
            spec.ownership,
            spec.handle,
            AttemptState::Pending,
        ))
        .id())
}

pub struct PromptSpec<'a> {
    pub id: &'a str,
    pub attempt: Entity,
    pub text: &'a str,
    pub deadline_ms: u64,
    pub report_token: &'a str,
}

pub fn spawn_prompt(world: &mut World, spec: PromptSpec<'_>) -> Result<Entity, ModelError> {
    kind::<Attempt>(world, spec.attempt, "attempt")?;
    bound::<Prompt>(world, |l| l.prompts, "prompt")?;
    if world.resource::<Ids>().operation_taken(spec.id) {
        return Err(ModelError::DuplicateId(spec.id.to_owned()));
    }
    let id: PromptId = string_id(world, spec.id)?;
    Ok(world
        .spawn((
            Prompt,
            id,
            PromptOf(spec.attempt),
            PromptText(spec.text.to_owned()),
            Deadline(spec.deadline_ms),
            ReportToken(spec.report_token.to_owned()),
            Delivery::Prepared,
            WaitState::Pending,
        ))
        .id())
}

pub fn spawn_operation(
    world: &mut World,
    id: &str,
    attempt: Entity,
    kind_: OperationKind,
) -> Result<Entity, ModelError> {
    kind::<Attempt>(world, attempt, "attempt")?;
    bound::<Operation>(world, |l| l.operations, "operation")?;
    if world.resource::<Ids>().operation_taken(id) {
        return Err(ModelError::DuplicateId(id.to_owned()));
    }
    let id: OperationId = string_id(world, id)?;
    Ok(world
        .spawn((
            Operation,
            id,
            OperationOf(attempt),
            kind_,
            OperationPhase::Prepared,
        ))
        .id())
}

pub struct CheckSpec<'a> {
    pub id: &'a str,
    pub task: Entity,
    pub source: Option<Entity>,
    pub command: CheckCommand,
    pub requirement: Option<&'a str>,
    pub generation: u64,
}

pub fn spawn_check(world: &mut World, spec: CheckSpec<'_>) -> Result<Entity, ModelError> {
    kind::<Task>(world, spec.task, "task")?;
    if let Some(source) = spec.source {
        kind::<Source>(world, source, "source")?;
        // A source-bound check must use a source of its own task (SOURCES.md:45).
        if world.get::<SourceOf>(source).map(|s| s.0) != Some(spec.task) {
            return Err(ModelError::Invalid("source belongs to another task".into()));
        }
    }
    bound::<Check>(world, |l| l.checks, "check")?;
    let id: CheckId = string_id(world, spec.id)?;
    let mut entity = world.spawn((
        Check,
        id,
        CheckOf(spec.task),
        spec.command,
        CreatedGeneration(spec.generation),
        CheckState::Queued,
    ));
    if let Some(source) = spec.source {
        entity.insert(CheckOn(source));
    }
    if let Some(name) = spec.requirement {
        entity.insert((Required, Requirement(name.to_owned())));
    }
    Ok(entity.id())
}

pub fn spawn_result(
    world: &mut World,
    check: Entity,
    verdict: Verdict,
    output: OutputTail,
) -> Result<Entity, ModelError> {
    kind::<Check>(world, check, "check")?;
    let id = world.resource_mut::<Ids>().allocate_result();
    Ok(world
        .spawn((CheckResult, id, ResultOf(check), verdict, output))
        .id())
}

pub fn spawn_source(
    world: &mut World,
    id: &str,
    task: Entity,
    revision: SourceRevision,
) -> Result<Entity, ModelError> {
    kind::<Task>(world, task, "task")?;
    bound::<Source>(world, |l| l.sources, "source")?;
    let id: SourceId = string_id(world, id)?;
    Ok(world.spawn((Source, id, SourceOf(task), revision)).id())
}

pub struct ArtifactSpec<'a> {
    pub id: &'a str,
    pub attempt: Entity,
    pub path: &'a str,
    pub requirement: Option<&'a str>,
}

pub fn spawn_artifact(world: &mut World, spec: ArtifactSpec<'_>) -> Result<Entity, ModelError> {
    kind::<Attempt>(world, spec.attempt, "attempt")?;
    bound::<Artifact>(world, |l| l.artifacts, "artifact")?;
    let id: ArtifactId = string_id(world, spec.id)?;
    let mut entity = world.spawn((
        Artifact,
        id,
        ArtifactOf(spec.attempt),
        ArtifactPath(spec.path.to_owned()),
        ArtifactState::Pending,
    ));
    if let Some(name) = spec.requirement {
        entity.insert((Required, Requirement(name.to_owned())));
    }
    Ok(entity.id())
}

/// A group over already-spawned operations; `after` pairs `(member index, prerequisite index)`.
pub fn spawn_group(
    world: &mut World,
    id: &str,
    members: &[Entity],
    concurrency: u32,
    after: &[(usize, usize)],
) -> Result<Entity, ModelError> {
    bound::<Group>(world, |l| l.groups, "group")?;
    if members.is_empty() || members.len() > MAX_GROUP_MEMBERS {
        return Err(ModelError::Invalid(format!(
            "a group has 1..={MAX_GROUP_MEMBERS} members"
        )));
    }
    if concurrency == 0 || concurrency as usize > members.len() {
        return Err(ModelError::Invalid(
            "concurrency must be 1..=members".into(),
        ));
    }
    for member in members {
        kind::<Operation>(world, *member, "operation")?;
        if world.get::<MemberOf>(*member).is_some() {
            return Err(ModelError::Invalid(format!(
                "operation {member} is already a group member"
            )));
        }
    }
    if members
        .iter()
        .enumerate()
        .any(|(i, m)| members.get(..i).is_some_and(|prior| prior.contains(m)))
    {
        return Err(ModelError::Invalid("members must be distinct".into()));
    }
    let id: GroupId = string_id(world, id)?;
    let group = world
        .spawn((Group, id, Concurrency(concurrency), GroupIntent::Manual))
        .id();
    for (i, member) in members.iter().enumerate() {
        let prerequisites: Vec<Entity> = after
            .iter()
            .filter(|(m, _)| *m == i)
            .filter_map(|(_, p)| members.get(*p).copied())
            .collect();
        world
            .entity_mut(*member)
            .insert((MemberOf(group), After(prerequisites)));
    }
    Ok(group)
}

pub fn spawn_worktree(
    world: &mut World,
    id: &str,
    task: Entity,
    spec: WorktreeSpec,
) -> Result<Entity, ModelError> {
    kind::<Task>(world, task, "task")?;
    bound::<Worktree>(world, |l| l.worktrees, "worktree")?;
    let id: WorktreeId = string_id(world, id)?;
    Ok(world
        .spawn((
            Worktree,
            id,
            OwnedWorktree(task),
            spec,
            WorktreeState::Allocating,
        ))
        .id())
}

pub fn spawn_machine(world: &mut World, id: &str, name: &str) -> Result<Entity, ModelError> {
    bound::<Machine>(world, |l| l.machines, "machine")?;
    let id: MachineId = string_id(world, id)?;
    Ok(world
        .spawn((Machine, id, MachineName(name.to_owned())))
        .id())
}

pub fn spawn_service(
    world: &mut World,
    id: &str,
    machine: Option<Entity>,
) -> Result<Entity, ModelError> {
    if let Some(machine) = machine {
        kind::<Machine>(world, machine, "machine")?;
    }
    let id: ServiceId = string_id(world, id)?;
    let mut entity = world.spawn((Service, id));
    if let Some(machine) = machine {
        entity.insert(Bound(machine));
    }
    Ok(entity.id())
}

/// Retires a group: members keep their operations and lose their membership and `After`
/// edges (GROUPS.md:118-119: a group goes quiet, its operations' history stays).
pub fn retire_group(world: &mut World, group: Entity) -> Result<(), ModelError> {
    kind::<Group>(world, group, "group")?;
    let members: Vec<Entity> = world
        .get::<Members>(group)
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    for member in members {
        world.entity_mut(member).remove::<(MemberOf, After)>();
    }
    world.despawn(group);
    Ok(())
}

pub fn spawn_observed_agent(
    world: &mut World,
    handle: PaneHandle,
    machine: Option<Entity>,
) -> Result<Entity, ModelError> {
    if let Some(machine) = machine {
        kind::<Machine>(world, machine, "machine")?;
    }
    let id = world.resource_mut::<Ids>().allocate_agent();
    let mut entity = world.spawn((ObservedAgent, id, handle, Observation::default()));
    if let Some(machine) = machine {
        entity.insert(Bound(machine));
    }
    Ok(entity.id())
}

pub fn spawn_plugin(
    world: &mut World,
    id: &str,
    manifest: PluginManifest,
) -> Result<Entity, ModelError> {
    let id: PluginId = string_id(world, id)?;
    Ok(world.spawn((HostedPlugin, id, manifest)).id())
}

pub fn spawn_plugin_action(
    world: &mut World,
    id: &str,
    plugin: Entity,
) -> Result<Entity, ModelError> {
    kind::<HostedPlugin>(world, plugin, "plugin")?;
    let id: PluginActionId = string_id(world, id)?;
    Ok(world.spawn((PluginAction, id, ActionOf(plugin))).id())
}
