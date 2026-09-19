//! Durable prompt groups (GROUPS.md; `docs/model.md` invariants 10, 18, 24): bounded admission
//! of already-prepared prompts on managed tasks. A member is an `Operation { GroupStep }` on
//! the prompt's attempt carrying [`MemberPrompt`]; admission writes the member's
//! `OperationPhase::Submitting` and the group's rotation [`Cursor`] before calling
//! `lifecycle::submit_prompt`, whose `Effect::FuxCall` drains only after the journal phase.
//! A slot is released (`OperationPhase::Closed`) only when the task carries a `Seal` *and* the
//! original prompt is `Delivered`. Cancellation flips the intent, releases still-prepared
//! prompts and nothing else.

use std::collections::HashSet;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lifecycle::{self, LifecycleError};
use crate::model::*;

/// Rotation cursor (GROUPS.md:43-44, 68): the member index the next admission scan starts at.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
pub struct Cursor(pub u32);

/// The prepared prompt a `GroupStep` member submits when admitted (GROUPS.md:8-16, 68-71).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[component(immutable)]
pub struct MemberPrompt(#[entities] pub Entity);

/// Bound on a member's prerequisites (GROUPS.md:103).
pub const MAX_AFTER: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupError {
    Model(ModelError),
    NotFound(String),
    Conflict(String),
    Refused(String),
    Lifecycle(LifecycleError),
}

impl core::fmt::Display for GroupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Conflict(why) => write!(f, "conflict: {why}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Lifecycle(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for GroupError {}

impl From<ModelError> for GroupError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

impl From<LifecycleError> for GroupError {
    fn from(e: LifecycleError) -> Self {
        Self::Lifecycle(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSpec {
    /// The member's operation id (shared operation namespace).
    pub operation: String,
    /// An already-prepared prompt id on a managed attempt.
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterSpec {
    pub operation: String,
    /// A member operation of the same group whose task must be verified first.
    pub prerequisite: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub id: String,
    pub concurrency: u32,
    pub members: Vec<MemberSpec>,
    pub after: Vec<AfterSpec>,
}

/// Retained member state for inspection (GROUPS.md:109-111).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberStatus {
    Queued,
    DependenciesPending,
    Active,
    VerificationRequired,
    NeedsAttention,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub operation: String,
    pub task: String,
    pub prompt: String,
    pub phase: OperationPhase,
    pub delivery: Delivery,
    pub status: MemberStatus,
    pub after: Vec<String>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupStatus {
    Active,
    NeedsAttention,
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRecord {
    pub id: String,
    pub intent: GroupIntent,
    pub status: GroupStatus,
    pub concurrency: u32,
    pub cursor: u32,
    /// Admitted-but-unreleased members; retained through cancellation (GROUPS.md:111-112).
    pub active_count: u32,
    pub members: Vec<MemberRecord>,
    pub problem: Option<String>,
}

pub struct GroupsPlugin;

impl Plugin for GroupsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Cursor>()
            .register_type::<MemberPrompt>()
            .add_systems(
                Update,
                (
                    settle,
                    schedule.run_if(bevy_state::condition::in_state(ServerMode::Serving)),
                )
                    .chain()
                    .in_set(Phase::Lifecycle),
            );
    }
}

// ---------------------------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------------------------

/// Creates a group over prepared prompts (GROUPS.md:14-16, 102-107). Every rule is checked
/// before anything is spawned; an identical retry returns the record, a changed plan fails.
pub fn create(world: &mut World, spec: GroupSpec) -> Result<Entity, GroupError> {
    if !valid_id(&spec.id) {
        return Err(ModelError::InvalidId(spec.id).into());
    }
    let n = spec.members.len();
    if n == 0 || n > MAX_GROUP_MEMBERS {
        return Err(GroupError::Refused(format!(
            "a group has 1..={MAX_GROUP_MEMBERS} members"
        )));
    }
    if spec.concurrency == 0 || spec.concurrency as usize > n {
        return Err(GroupError::Refused(
            "concurrency must be 1..=members".into(),
        ));
    }
    let index_of = |operation: &str| spec.members.iter().position(|m| m.operation == operation);
    let mut edges: Vec<(usize, usize)> = Vec::with_capacity(spec.after.len());
    for after in &spec.after {
        let (Some(member), Some(prerequisite)) =
            (index_of(&after.operation), index_of(&after.prerequisite))
        else {
            return Err(GroupError::Refused(format!(
                "--after {}={} names an operation outside the group",
                after.operation, after.prerequisite
            )));
        };
        if member == prerequisite {
            return Err(GroupError::Refused(format!(
                "{} depends on itself",
                after.operation
            )));
        }
        if !edges.contains(&(member, prerequisite)) {
            edges.push((member, prerequisite));
        }
    }
    for (i, member) in spec.members.iter().enumerate() {
        if edges.iter().filter(|(m, _)| *m == i).count() > MAX_AFTER {
            return Err(GroupError::Refused(format!(
                "{} has more than {MAX_AFTER} prerequisites",
                member.operation
            )));
        }
    }
    if has_cycle(n, &edges) {
        return Err(GroupError::Refused("dependencies form a cycle".into()));
    }

    if let Some(existing) = world.resource::<Ids>().group(&spec.id) {
        return if same_plan(world, existing, &spec, &edges) {
            Ok(existing)
        } else {
            Err(GroupError::Conflict(format!(
                "group {} exists with a different plan",
                spec.id
            )))
        };
    }

    // Members: distinct operations, distinct prompts, distinct tasks, prompts prepared on
    // managed attempts of open tasks, no attempt already pinned by an active group.
    let mut operations = HashSet::new();
    let mut prompts = Vec::with_capacity(n);
    let mut tasks = HashSet::new();
    for member in &spec.members {
        if !valid_id(&member.operation) {
            return Err(ModelError::InvalidId(member.operation.clone()).into());
        }
        if !operations.insert(member.operation.as_str()) {
            return Err(GroupError::Refused(format!(
                "operation {} is listed twice",
                member.operation
            )));
        }
        if world.resource::<Ids>().operation_taken(&member.operation) {
            return Err(ModelError::DuplicateId(member.operation.clone()).into());
        }
        let prompt = world
            .resource::<Ids>()
            .prompt(&member.prompt)
            .ok_or_else(|| GroupError::NotFound(format!("prompt {}", member.prompt)))?;
        if prompts.contains(&prompt) {
            return Err(GroupError::Refused(format!(
                "prompt {} is listed twice",
                member.prompt
            )));
        }
        if world.get::<Delivery>(prompt).copied() != Some(Delivery::Prepared) {
            return Err(GroupError::Refused(format!(
                "prompt {} is not prepared",
                member.prompt
            )));
        }
        let attempt = world
            .get::<PromptOf>(prompt)
            .map(|p| p.0)
            .ok_or_else(|| GroupError::NotFound("prompt attempt".into()))?;
        if world.get::<Ownership>(attempt).copied() != Some(Ownership::Managed) {
            return Err(GroupError::Refused(format!(
                "prompt {} targets an adopted attempt",
                member.prompt
            )));
        }
        let task = world
            .get::<AttemptOf>(attempt)
            .map(|a| a.0)
            .ok_or_else(|| GroupError::NotFound("attempt task".into()))?;
        if world.get::<TaskState>(task).is_some_and(|s| s.is_closed()) {
            return Err(GroupError::Refused(format!(
                "task of prompt {} is closed",
                member.prompt
            )));
        }
        if !tasks.insert(task) {
            return Err(GroupError::Refused(
                "members must target distinct tasks".into(),
            ));
        }
        if pinned_by_active_group(world, attempt) {
            return Err(GroupError::Refused(format!(
                "prompt {}'s attempt is already pinned by an active group",
                member.prompt
            )));
        }
        prompts.push(prompt);
    }

    let mut members = Vec::with_capacity(n);
    for (member, prompt) in spec.members.iter().zip(&prompts) {
        let attempt = world
            .get::<PromptOf>(*prompt)
            .map(|p| p.0)
            .ok_or_else(|| GroupError::NotFound("prompt attempt".into()))?;
        match spawn_operation(world, &member.operation, attempt, OperationKind::GroupStep) {
            Ok(operation) => {
                world.entity_mut(operation).insert(MemberPrompt(*prompt));
                members.push(operation);
            }
            Err(e) => {
                for spawned in members {
                    world.despawn(spawned);
                }
                return Err(e.into());
            }
        }
    }
    match spawn_group(world, &spec.id, &members, spec.concurrency, &edges) {
        Ok(group) => {
            world.entity_mut(group).insert(Cursor(0));
            Ok(group)
        }
        Err(e) => {
            for spawned in members {
                world.despawn(spawned);
            }
            Err(e.into())
        }
    }
}

fn same_plan(world: &World, group: Entity, spec: &GroupSpec, edges: &[(usize, usize)]) -> bool {
    let members: Vec<Entity> = world
        .get::<Members>(group)
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    if members.len() != spec.members.len()
        || world.get::<Concurrency>(group).map(|c| c.0) != Some(spec.concurrency)
    {
        return false;
    }
    for (entity, member) in members.iter().zip(&spec.members) {
        let operation = world.get::<OperationId>(*entity).map(|o| o.0.as_str());
        let prompt = world
            .get::<MemberPrompt>(*entity)
            .and_then(|p| world.get::<PromptId>(p.0))
            .map(|p| p.0.as_str());
        if operation != Some(member.operation.as_str()) || prompt != Some(member.prompt.as_str()) {
            return false;
        }
    }
    let mut retained: Vec<(usize, usize)> = Vec::new();
    for (i, entity) in members.iter().enumerate() {
        if let Some(after) = world.get::<After>(*entity) {
            for prerequisite in &after.0 {
                if let Some(j) = members.iter().position(|m| m == prerequisite) {
                    retained.push((i, j));
                }
            }
        }
    }
    let mut wanted = edges.to_vec();
    retained.sort_unstable();
    wanted.sort_unstable();
    retained == wanted
}

fn pinned_by_active_group(world: &World, attempt: Entity) -> bool {
    let Some(operations) = world.get::<Operations>(attempt) else {
        return false;
    };
    operations.iter().any(|operation| {
        world
            .get::<MemberOf>(operation)
            .and_then(|m| world.get::<GroupIntent>(m.0))
            .is_some_and(|intent| !matches!(intent, GroupIntent::Cancelled | GroupIntent::Complete))
    })
}

fn has_cycle(n: usize, edges: &[(usize, usize)]) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit(node: usize, edges: &[(usize, usize)], marks: &mut [Mark]) -> bool {
        match marks.get(node).copied() {
            Some(Mark::Open) => return true,
            Some(Mark::Done) | None => return false,
            Some(Mark::New) => {}
        }
        if let Some(mark) = marks.get_mut(node) {
            *mark = Mark::Open;
        }
        for (_, next) in edges.iter().filter(|(from, _)| *from == node) {
            if visit(*next, edges, marks) {
                return true;
            }
        }
        if let Some(mark) = marks.get_mut(node) {
            *mark = Mark::Done;
        }
        false
    }
    let mut marks = vec![Mark::New; n];
    (0..n).any(|node| visit(node, edges, &mut marks))
}

// ---------------------------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------------------------

fn members_of(world: &World, group: Entity) -> Vec<Entity> {
    world
        .get::<Members>(group)
        .map(|m| m.iter().collect())
        .unwrap_or_default()
}

fn prompt_of(world: &World, member: Entity) -> Option<Entity> {
    world.get::<MemberPrompt>(member).map(|p| p.0)
}

fn task_of(world: &World, member: Entity) -> Option<Entity> {
    let attempt = world.get::<OperationOf>(member)?.0;
    Some(world.get::<AttemptOf>(attempt)?.0)
}

/// An admitted slot not yet released (GROUPS.md:61-62).
fn holds_slot(phase: OperationPhase) -> bool {
    matches!(
        phase,
        OperationPhase::Submitting | OperationPhase::Attached | OperationPhase::Uncertain
    )
}

fn prerequisites_verified(world: &World, member: Entity) -> bool {
    world.get::<After>(member).is_none_or(|after| {
        after
            .0
            .iter()
            .all(|p| task_of(world, *p).is_some_and(|t| world.get::<Seal>(t).is_some()))
    })
}

/// Why a prepared member cannot be admitted now, if anything.
fn ineligible(world: &World, member: Entity, now_ms: u64) -> Option<&'static str> {
    if world.get::<OperationPhase>(member).copied() != Some(OperationPhase::Prepared) {
        return Some("not prepared");
    }
    if !prerequisites_verified(world, member) {
        return Some("dependencies pending");
    }
    let prompt = prompt_of(world, member)?;
    if world.get::<Delivery>(prompt).copied() != Some(Delivery::Prepared) {
        return Some("prompt is no longer prepared");
    }
    if world.get::<Deadline>(prompt).is_some_and(|d| d.0 <= now_ms) {
        return Some("preparation deadline expired");
    }
    if task_of(world, member)
        .and_then(|t| world.get::<TaskState>(t))
        .is_none_or(|s| s.is_closed())
    {
        return Some("task is closed");
    }
    None
}

/// Admits eligible members up to free capacity in rotation order (GROUPS.md:42-44, 61-73).
/// Each admission commits `Submitting` and the advanced cursor before `submit_prompt`; a
/// submission error is retained as the member's `Problem` (the slot stays admitted and a later
/// `admit` retries with the original prompt). Returns the members admitted by this call.
pub fn admit(world: &mut World, group: Entity) -> Result<Vec<Entity>, GroupError> {
    let intent = *world
        .get::<GroupIntent>(group)
        .ok_or_else(|| GroupError::NotFound(format!("group {group}")))?;
    match intent {
        GroupIntent::Manual | GroupIntent::Automatic => {}
        other => {
            return Err(GroupError::Refused(format!("group is {other:?}")));
        }
    }
    let members = members_of(world, group);
    let now_ms = world.resource::<Clock>().now_ms;

    // Retry submissions committed earlier whose input never reached delivery, including a
    // member the lifecycle's recovery marked `Uncertain` (GROUPS.md:76): the prompt's own
    // delivery state decides whether resending is safe (`submit_prompt` refuses otherwise).
    for member in &members {
        let phase = world.get::<OperationPhase>(*member).copied();
        let Some(prompt) = prompt_of(world, *member) else {
            continue;
        };
        let delivery = world.get::<Delivery>(prompt).copied().unwrap_or_default();
        if matches!(
            phase,
            Some(OperationPhase::Submitting | OperationPhase::Uncertain)
        ) && matches!(delivery, Delivery::Prepared | Delivery::Reserved)
        {
            submit(world, *member, prompt);
        }
    }

    let concurrency = world.get::<Concurrency>(group).map_or(1, |c| c.0) as usize;
    let active = members
        .iter()
        .filter(|m| {
            world
                .get::<OperationPhase>(**m)
                .is_some_and(|p| holds_slot(*p))
        })
        .count();
    let mut capacity = concurrency.saturating_sub(active);
    let n = members.len();
    let cursor = world.get::<Cursor>(group).map_or(0, |c| c.0 as usize) % n.max(1);
    let mut admitted = Vec::new();
    for offset in 0..n {
        if capacity == 0 {
            break;
        }
        let i = (cursor + offset) % n;
        let Some(member) = members.get(i).copied() else {
            break;
        };
        if ineligible(world, member, now_ms).is_some() {
            continue;
        }
        let Some(prompt) = prompt_of(world, member) else {
            continue;
        };
        // Intent first (invariant 24): member admitted, cursor rotated, then the submission.
        world
            .entity_mut(member)
            .insert(OperationPhase::Submitting)
            .remove::<Problem>();
        world
            .entity_mut(group)
            .insert(Cursor(((i + 1) % n) as u32))
            .remove::<Problem>();
        submit(world, member, prompt);
        admitted.push(member);
        capacity -= 1;
    }
    Ok(admitted)
}

fn submit(world: &mut World, member: Entity, prompt: Entity) {
    if let Err(e) = lifecycle::submit_prompt(world, prompt) {
        let text: String = format!("submission: {e}").chars().take(512).collect();
        world.entity_mut(member).insert(Problem(text));
    }
}

// ---------------------------------------------------------------------------------------------
// Intent
// ---------------------------------------------------------------------------------------------

fn set_intent(world: &mut World, group: Entity, next: GroupIntent) -> Result<(), GroupError> {
    let current = *world
        .get::<GroupIntent>(group)
        .ok_or_else(|| GroupError::NotFound(format!("group {group}")))?;
    if current == next {
        return Ok(());
    }
    let allowed = match (current, next) {
        (GroupIntent::Cancelled | GroupIntent::Complete, _) => false,
        (_, GroupIntent::Complete) => false,
        (
            _,
            GroupIntent::Cancelled
            | GroupIntent::Paused
            | GroupIntent::Automatic
            | GroupIntent::Manual,
        ) => true,
    };
    if !allowed {
        return Err(GroupError::Refused(format!(
            "group is {current:?}, cannot become {next:?}"
        )));
    }
    world.entity_mut(group).insert(next);
    Ok(())
}

/// Stops new selection; an already-admitted submission finishes (GROUPS.md:47).
pub fn pause(world: &mut World, group: Entity) -> Result<(), GroupError> {
    set_intent(world, group, GroupIntent::Paused)
}

/// Enables durable automatic advancement (GROUPS.md:28-31); clears a retained run problem.
pub fn resume(world: &mut World, group: Entity) -> Result<(), GroupError> {
    set_intent(world, group, GroupIntent::Automatic)?;
    world.entity_mut(group).remove::<Problem>();
    Ok(())
}

/// Cancellation releases coordination only (GROUPS.md:87-92, invariant 18): the intent becomes
/// `Cancelled`, prompts of members never admitted are `Released`, and nothing else changes:
/// no input is retracted, no worker stopped, no worktree removed, no task outcome touched.
/// Repeated cancellation is read-only.
pub fn cancel(world: &mut World, group: Entity) -> Result<(), GroupError> {
    let current = *world
        .get::<GroupIntent>(group)
        .ok_or_else(|| GroupError::NotFound(format!("group {group}")))?;
    if current == GroupIntent::Cancelled {
        return Ok(());
    }
    set_intent(world, group, GroupIntent::Cancelled)?;
    for member in members_of(world, group) {
        if world.get::<OperationPhase>(member).copied() != Some(OperationPhase::Prepared) {
            continue;
        }
        let Some(prompt) = prompt_of(world, member) else {
            continue;
        };
        if world.get::<Delivery>(prompt).copied() == Some(Delivery::Prepared) {
            world.entity_mut(prompt).insert(Delivery::Released);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------------------------

/// Member phases follow the delivery and verification facts they wait on; a group whose
/// members are all released becomes `Complete`.
fn settle(world: &mut World) {
    let groups: Vec<Entity> = world
        .query_filtered::<Entity, With<Group>>()
        .iter(world)
        .collect();
    for group in groups {
        let members = members_of(world, group);
        let mut all_closed = !members.is_empty();
        for member in members {
            let Some(phase) = world.get::<OperationPhase>(member).copied() else {
                continue;
            };
            let delivery = prompt_of(world, member)
                .and_then(|p| world.get::<Delivery>(p).copied())
                .unwrap_or_default();
            let verified = task_of(world, member).is_some_and(|t| world.get::<Seal>(t).is_some());
            let next = match (phase, delivery) {
                (OperationPhase::Submitting | OperationPhase::Uncertain, Delivery::Delivered)
                    if verified =>
                {
                    Some(OperationPhase::Closed)
                }
                (OperationPhase::Submitting | OperationPhase::Uncertain, Delivery::Delivered) => {
                    Some(OperationPhase::Attached)
                }
                (OperationPhase::Attached, _) if verified => Some(OperationPhase::Closed),
                (
                    OperationPhase::Submitting,
                    Delivery::Failed | Delivery::Uncertain | Delivery::Released,
                ) => Some(OperationPhase::Uncertain),
                _ => None,
            };
            let phase = match next {
                Some(next) if phase.may_become(next) => {
                    let mut entity = world.entity_mut(member);
                    entity.insert(next);
                    if next == OperationPhase::Uncertain {
                        entity.insert(Problem(format!("delivery is {delivery:?}")));
                    } else {
                        entity.remove::<Problem>();
                    }
                    next
                }
                _ => phase,
            };
            all_closed &= phase == OperationPhase::Closed;
        }
        if all_closed
            && matches!(
                world.get::<GroupIntent>(group),
                Some(GroupIntent::Manual | GroupIntent::Automatic | GroupIntent::Paused)
            )
        {
            world.entity_mut(group).insert(GroupIntent::Complete);
        }
    }
}

/// Automatic advancement (GROUPS.md:42-54): every `Automatic` group admits what it can; a
/// submission problem pauses it with the diagnostic retained as the group's `Problem`.
fn schedule(world: &mut World) {
    let automatic: Vec<Entity> = world
        .query_filtered::<(Entity, &GroupIntent), With<Group>>()
        .iter(world)
        .filter(|(_, intent)| **intent == GroupIntent::Automatic)
        .map(|(e, _)| e)
        .collect();
    for group in automatic {
        let Ok(admitted) = admit(world, group) else {
            continue;
        };
        let problem = admitted.iter().find_map(|m| {
            world.get::<Problem>(*m).map(|p| {
                format!(
                    "{}: {}",
                    world.get::<OperationId>(*m).map_or("?", |o| o.0.as_str()),
                    p.0
                )
            })
        });
        if let Some(problem) = problem {
            world
                .entity_mut(group)
                .insert((GroupIntent::Paused, Problem(problem)));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------------------------

pub fn inspect(world: &World, group: Entity) -> Option<GroupRecord> {
    let id = world.get::<GroupId>(group)?.0.clone();
    let intent = *world.get::<GroupIntent>(group)?;
    let now_ms = world.resource::<Clock>().now_ms;
    let mut members = Vec::new();
    let mut active_count = 0;
    let mut attention = false;
    for member in members_of(world, group) {
        let phase = world
            .get::<OperationPhase>(member)
            .copied()
            .unwrap_or_default();
        let prompt = prompt_of(world, member);
        let delivery = prompt
            .and_then(|p| world.get::<Delivery>(p).copied())
            .unwrap_or_default();
        let verified = task_of(world, member).is_some_and(|t| world.get::<Seal>(t).is_some());
        let problem = world.get::<Problem>(member).map(|p| p.0.clone());
        let status = match phase {
            OperationPhase::Closed => MemberStatus::Verified,
            OperationPhase::Uncertain => MemberStatus::NeedsAttention,
            OperationPhase::Attached if verified => MemberStatus::Verified,
            OperationPhase::Attached => MemberStatus::VerificationRequired,
            OperationPhase::Submitting if problem.is_some() => MemberStatus::NeedsAttention,
            OperationPhase::Submitting => MemberStatus::Active,
            OperationPhase::Prepared => match ineligible(world, member, now_ms) {
                None => MemberStatus::Queued,
                Some("dependencies pending") => MemberStatus::DependenciesPending,
                Some(_) => MemberStatus::NeedsAttention,
            },
        };
        if holds_slot(phase) {
            active_count += 1;
        }
        attention |= status == MemberStatus::NeedsAttention;
        members.push(MemberRecord {
            operation: world
                .get::<OperationId>(member)
                .map(|o| o.0.clone())
                .unwrap_or_default(),
            task: task_of(world, member)
                .and_then(|t| world.get::<TaskId>(t))
                .map(|t| t.0.clone())
                .unwrap_or_default(),
            prompt: prompt
                .and_then(|p| world.get::<PromptId>(p))
                .map(|p| p.0.clone())
                .unwrap_or_default(),
            phase,
            delivery,
            status,
            after: world
                .get::<After>(member)
                .map(|a| {
                    a.0.iter()
                        .filter_map(|p| world.get::<OperationId>(*p))
                        .map(|o| o.0.clone())
                        .collect()
                })
                .unwrap_or_default(),
            problem,
        });
    }
    let problem = world.get::<Problem>(group).map(|p| p.0.clone());
    let status = match intent {
        GroupIntent::Cancelled => GroupStatus::Cancelled,
        GroupIntent::Complete => GroupStatus::Complete,
        GroupIntent::Paused if problem.is_some() => GroupStatus::NeedsAttention,
        _ if attention => GroupStatus::NeedsAttention,
        _ => GroupStatus::Active,
    };
    Some(GroupRecord {
        id,
        intent,
        status,
        concurrency: world.get::<Concurrency>(group).map_or(0, |c| c.0),
        cursor: world.get::<Cursor>(group).map_or(0, |c| c.0),
        active_count,
        members,
        problem,
    })
}

pub fn list(world: &mut World) -> Vec<GroupRecord> {
    let groups: Vec<Entity> = world
        .query_filtered::<Entity, With<Group>>()
        .iter(world)
        .collect();
    groups
        .into_iter()
        .filter_map(|g| inspect(world, g))
        .collect()
}
