//! Structural invariants (`docs/model.md`, rules marked S) that must hold after every `update`.
//! Called by tests after every step and by the journal before a snapshot and after a restore.

use bevy_ecs::prelude::*;
use bevy_ecs::query::Has;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_platform::collections::{HashMap, HashSet};

use super::*;

/// Returns the first violated invariant as `(number, description)`.
pub fn check_invariants(world: &mut World) -> Result<(), (u8, String)> {
    ids(world)?;
    relationships(world)?;
    attempts(world)?;
    seals(world)?;
    prompts(world)?;
    checks(world)?;
    worktrees(world)?;
    groups(world)?;
    counts(world)?;
    stop_requested(world)
}

fn ids(world: &mut World) -> Result<(), (u8, String)> {
    let ids = world.resource::<Ids>().clone();
    macro_rules! indexed {
        ($ty:ty, $map:ident, $what:literal, $valid:expr) => {{
            let live: Vec<(Entity, $ty)> = world
                .query::<(Entity, &$ty)>()
                .iter(world)
                .map(|(e, id)| (e, id.clone()))
                .collect();
            for (entity, id) in &live {
                if !$valid(id) {
                    return Err((1, format!("{} id {id} is not valid", $what)));
                }
                if ids.$map.get(id) != Some(entity) {
                    return Err((1, format!("{} {id} is not registered to {entity}", $what)));
                }
            }
            if ids.$map.len() != live.len() {
                return Err((1, format!("Ids.{} has stale entries", stringify!($map))));
            }
        }};
    }
    let text = |id: &String| valid_id(id);
    let any = |_: &u64| true;
    indexed!(TaskId, tasks, "task", |id: &TaskId| text(&id.0));
    indexed!(AttemptId, attempts, "attempt", |id: &AttemptId| any(&id.0));
    indexed!(PromptId, prompts, "prompt", |id: &PromptId| text(&id.0));
    indexed!(OperationId, operations, "operation", |id: &OperationId| {
        text(&id.0)
    });
    indexed!(CheckId, checks, "check", |id: &CheckId| text(&id.0));
    indexed!(ResultId, results, "result", |id: &ResultId| any(&id.0));
    indexed!(SourceId, sources, "source", |id: &SourceId| text(&id.0));
    indexed!(ArtifactId, artifacts, "artifact", |id: &ArtifactId| text(
        &id.0
    ));
    indexed!(GroupId, groups, "group", |id: &GroupId| text(&id.0));
    indexed!(WorktreeId, worktrees, "worktree", |id: &WorktreeId| text(
        &id.0
    ));
    indexed!(MachineId, machines, "machine", |id: &MachineId| text(&id.0));
    indexed!(ServiceId, services, "service", |id: &ServiceId| text(&id.0));
    indexed!(
        ObservedAgentId,
        agents,
        "agent",
        |id: &ObservedAgentId| any(&id.0)
    );
    indexed!(PluginId, plugins, "plugin", |id: &PluginId| text(&id.0));
    indexed!(
        PluginActionId,
        plugin_actions,
        "plugin action",
        |id: &PluginActionId| text(&id.0)
    );
    // The operation namespace is shared (docs/model.md).
    for id in ids.prompts.keys() {
        if ids.operations.contains_key(id.0.as_str()) {
            return Err((
                1,
                format!("operation id {id} names both a prompt and an operation"),
            ));
        }
    }
    Ok(())
}

/// Every relationship points at the expected kind and every dependent entity has its
/// relationship (invariant 2).
fn relationships(world: &mut World) -> Result<(), (u8, String)> {
    macro_rules! points_at {
        ($rel:ty, $target:ty, $kind:literal) => {
            for (entity, rel) in world.query::<(Entity, &$rel)>().iter(world) {
                if world.get::<$target>(rel.0).is_none() {
                    return Err((
                        2,
                        format!(
                            "{entity}: {} → {} is not a {}",
                            stringify!($rel),
                            rel.0,
                            $kind
                        ),
                    ));
                }
            }
        };
    }
    macro_rules! requires {
        ($marker:ty, $rel:ty) => {
            if let Some(entity) = world
                .query_filtered::<Entity, (With<$marker>, Without<$rel>)>()
                .iter(world)
                .next()
            {
                return Err((
                    2,
                    format!(
                        "{entity}: {} without {}",
                        stringify!($marker),
                        stringify!($rel)
                    ),
                ));
            }
        };
    }
    points_at!(AttemptOf, Task, "task");
    points_at!(PromptOf, Attempt, "attempt");
    points_at!(OperationOf, Attempt, "attempt");
    points_at!(ArtifactOf, Attempt, "attempt");
    points_at!(CheckOf, Task, "task");
    points_at!(SourceOf, Task, "task");
    points_at!(CheckOn, Source, "source");
    points_at!(ResultOf, Check, "check");
    points_at!(MemberOf, Group, "group");
    points_at!(OwnedWorktree, Task, "task");
    points_at!(Bound, Machine, "machine");
    points_at!(ActionOf, HostedPlugin, "plugin");
    requires!(Attempt, AttemptOf);
    requires!(Prompt, PromptOf);
    requires!(Operation, OperationOf);
    requires!(Artifact, ArtifactOf);
    requires!(Check, CheckOf);
    requires!(Source, SourceOf);
    requires!(CheckResult, ResultOf);
    requires!(Worktree, OwnedWorktree);
    requires!(PluginAction, ActionOf);
    // Only operations are group members.
    if let Some((entity, _)) = world
        .query_filtered::<(Entity, &MemberOf), Without<Operation>>()
        .iter(world)
        .next()
    {
        return Err((
            2,
            format!("{entity} is a group member but not an operation"),
        ));
    }
    Ok(())
}

/// One attempt outside `Finished` per task; one attempt per pane handle (invariant 3);
/// `Finished` carries `FinalEvidence`, `Lost` never on `Finished` (invariant 5).
fn attempts(world: &mut World) -> Result<(), (u8, String)> {
    let mut current: HashSet<Entity> = HashSet::default();
    let mut handles: HashSet<(String, u64)> = HashSet::default();
    for (entity, of, state, handle, evidence, lost) in world
        .query_filtered::<(
            Entity,
            &AttemptOf,
            &AttemptState,
            &PaneHandle,
            Option<&FinalEvidence>,
            Has<Lost>,
        ), With<Attempt>>()
        .iter(world)
    {
        if *state != AttemptState::Finished {
            if !current.insert(of.0) {
                return Err((3, format!("task {} has two current attempts", of.0)));
            }
            if !handle.instance.is_empty()
                && !handles.insert((handle.instance.clone(), handle.pane))
            {
                return Err((
                    3,
                    format!(
                        "pane {}/{} belongs to two attempts",
                        handle.instance, handle.pane
                    ),
                ));
            }
        }
        if *state == AttemptState::Finished && evidence.is_none() {
            return Err((
                5,
                format!("attempt {entity} is Finished without FinalEvidence"),
            ));
        }
        if *state == AttemptState::Finished && lost {
            return Err((5, format!("attempt {entity} is Finished and Lost")));
        }
    }
    Ok(())
}

/// `Closed{Verified}` ⇔ `Seal`, and the seal's selection is coherent (invariant 4).
fn seals(world: &mut World) -> Result<(), (u8, String)> {
    let tasks: Vec<(Entity, TaskState, Option<Seal>)> = world
        .query_filtered::<(Entity, &TaskState, Option<&Seal>), With<Task>>()
        .iter(world)
        .map(|(e, s, seal)| (e, *s, seal.cloned()))
        .collect();
    for (task, state, seal) in tasks {
        let verified = state
            == TaskState::Closed {
                outcome: TaskOutcome::Verified,
            };
        let Some(seal) = seal else {
            if verified {
                return Err((4, format!("task {task} is Verified without a Seal")));
            }
            continue;
        };
        if !verified {
            return Err((4, format!("task {task} has a Seal but is not Verified")));
        }
        if world.get::<SourceOf>(seal.source).map(|s| s.0) != Some(task) {
            return Err((4, format!("task {task}: seal source is not its own")));
        }
        if seal.checks.is_empty() {
            return Err((4, format!("task {task}: seal selects no check")));
        }
        for check in &seal.checks {
            if world.get::<CheckOf>(*check).map(|c| c.0) != Some(task) {
                return Err((
                    4,
                    format!("task {task}: sealed check {check} is not its own"),
                ));
            }
            if world.get::<CheckState>(*check) != Some(&CheckState::Passed) {
                return Err((4, format!("task {task}: sealed check {check} did not pass")));
            }
            if world.get::<CheckOn>(*check).map(|c| c.0) != Some(seal.source) {
                return Err((
                    4,
                    format!("task {task}: sealed check {check} ran on another source"),
                ));
            }
        }
        for artifact in &seal.artifacts {
            let attempt = world.get::<ArtifactOf>(*artifact).map(|a| a.0);
            let owner = attempt.and_then(|a| world.get::<AttemptOf>(a)).map(|a| a.0);
            if owner != Some(task) {
                return Err((
                    4,
                    format!("task {task}: sealed artifact {artifact} is not its own"),
                ));
            }
        }
    }
    Ok(())
}

/// Writer exclusion per pane (invariant 6) and receipt presence (invariant 7).
fn prompts(world: &mut World) -> Result<(), (u8, String)> {
    let mut pending: HashSet<(String, u64)> = HashSet::default();
    let rows: Vec<(Entity, Entity, Delivery, bool, bool)> = world
        .query_filtered::<(
            Entity,
            &PromptOf,
            &Delivery,
            Has<Receipt>,
            (Has<ResponseEvent>, Has<Binding>),
        ), With<Prompt>>()
        .iter(world)
        .map(|(e, of, d, r, (response, binding))| (e, of.0, *d, r, response || binding))
        .collect();
    for (prompt, attempt, delivery, receipt, evidence) in rows {
        if delivery.is_pending()
            && let Some(handle) = world.get::<PaneHandle>(attempt)
            && !handle.instance.is_empty()
            && !pending.insert((handle.instance.clone(), handle.pane))
        {
            return Err((
                6,
                format!(
                    "pane {}/{} has two pending prompts",
                    handle.instance, handle.pane
                ),
            ));
        }
        match delivery {
            Delivery::Prepared if receipt => {
                return Err((7, format!("prompt {prompt} is Prepared with a Receipt")));
            }
            Delivery::Reserved | Delivery::Submitting | Delivery::Delivered if !receipt => {
                return Err((
                    7,
                    format!("prompt {prompt} is {delivery:?} without a Receipt"),
                ));
            }
            _ => {}
        }
        if evidence && !receipt {
            return Err((
                7,
                format!("prompt {prompt} has a report or binding without a Receipt"),
            ));
        }
    }
    Ok(())
}

/// Results exist exactly for terminal checks and agree with them (invariant 8).
fn checks(world: &mut World) -> Result<(), (u8, String)> {
    let rows: Vec<(Entity, CheckState, Vec<Entity>)> = world
        .query_filtered::<(Entity, &CheckState, Option<&Results>), With<Check>>()
        .iter(world)
        .map(|(e, s, r)| (e, *s, r.map(|r| r.iter().collect()).unwrap_or_default()))
        .collect();
    for (check, state, results) in rows {
        match state {
            CheckState::Queued | CheckState::Running if !results.is_empty() => {
                return Err((8, format!("check {check} is {state:?} with a result")));
            }
            CheckState::Passed | CheckState::Failed if results.is_empty() => {
                return Err((8, format!("check {check} is {state:?} without a result")));
            }
            _ => {}
        }
        for result in results {
            if let Some(verdict) = world.get::<Verdict>(result)
                && !verdict.matches(state)
            {
                return Err((
                    8,
                    format!("result {result} says {verdict:?} but check {check} is {state:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// A worktree with removal intent has no unresolved check and no current attempt (invariant 9).
fn worktrees(world: &mut World) -> Result<(), (u8, String)> {
    let removing: Vec<(Entity, Entity)> = world
        .query_filtered::<(Entity, &OwnedWorktree, &WorktreeState), With<Worktree>>()
        .iter(world)
        .filter(|(_, _, s)| s.is_removing())
        .map(|(e, of, _)| (e, of.0))
        .collect();
    for (worktree, task) in removing {
        for (check, of, state) in world.query::<(Entity, &CheckOf, &CheckState)>().iter(world) {
            if of.0 == task && !matches!(state, CheckState::Passed | CheckState::Failed) {
                return Err((
                    9,
                    format!("worktree {worktree} is removing while check {check} is {state:?}"),
                ));
            }
        }
        for (attempt, of, state) in world
            .query::<(Entity, &AttemptOf, &AttemptState)>()
            .iter(world)
        {
            if of.0 == task && *state != AttemptState::Finished {
                return Err((
                    9,
                    format!("worktree {worktree} is removing while attempt {attempt} is {state:?}"),
                ));
            }
        }
    }
    Ok(())
}

/// Member count, concurrency, `After` within the group, acyclic (invariant 10).
fn groups(world: &mut World) -> Result<(), (u8, String)> {
    if let Some(entity) = world
        .query_filtered::<Entity, (With<After>, Without<MemberOf>)>()
        .iter(world)
        .next()
    {
        return Err((10, format!("{entity} has After edges but no group")));
    }
    let groups: Vec<(Entity, u32, Vec<Entity>)> = world
        .query_filtered::<(Entity, &Concurrency, Option<&Members>), With<Group>>()
        .iter(world)
        .map(|(e, c, m)| (e, c.0, m.map(|m| m.iter().collect()).unwrap_or_default()))
        .collect();
    for (group, concurrency, members) in groups {
        if members.is_empty() || members.len() > MAX_GROUP_MEMBERS {
            return Err((10, format!("group {group} has {} members", members.len())));
        }
        if concurrency == 0 || concurrency as usize > members.len() {
            return Err((
                10,
                format!("group {group} concurrency {concurrency} is out of range"),
            ));
        }
        let mut edges: HashMap<Entity, Vec<Entity>> = HashMap::default();
        for member in &members {
            let after = world
                .get::<After>(*member)
                .map(|a| a.0.clone())
                .unwrap_or_default();
            for prerequisite in &after {
                if !members.contains(prerequisite) {
                    return Err((
                        10,
                        format!(
                            "group {group}: {member} waits on {prerequisite} outside the group"
                        ),
                    ));
                }
            }
            edges.insert(*member, after);
        }
        if has_cycle(&edges) {
            return Err((10, format!("group {group} has a dependency cycle")));
        }
    }
    Ok(())
}

fn has_cycle(edges: &HashMap<Entity, Vec<Entity>>) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    let mut marks: HashMap<Entity, Mark> = HashMap::default();
    for start in edges.keys() {
        if marks.contains_key(start) {
            continue;
        }
        let mut stack: Vec<(Entity, usize)> = vec![(*start, 0)];
        marks.insert(*start, Mark::Visiting);
        while let Some((node, next)) = stack.last_mut() {
            let successors = edges.get(node).map_or(&[][..], Vec::as_slice);
            match successors.get(*next) {
                Some(successor) => {
                    *next += 1;
                    match marks.get(successor) {
                        Some(Mark::Visiting) => return true,
                        Some(Mark::Done) => {}
                        None => {
                            marks.insert(*successor, Mark::Visiting);
                            stack.push((*successor, 0));
                        }
                    }
                }
                None => {
                    marks.insert(*node, Mark::Done);
                    stack.pop();
                }
            }
        }
    }
    false
}

/// Record counts within `Limits` (invariant 11).
fn counts(world: &mut World) -> Result<(), (u8, String)> {
    let limits = world.resource::<Limits>().clone();
    macro_rules! within {
        ($marker:ty, $limit:expr, $what:literal) => {
            let count = world
                .query_filtered::<(), With<$marker>>()
                .iter(world)
                .count();
            if count > $limit {
                return Err((11, format!("{} {} records exceed {}", count, $what, $limit)));
            }
        };
    }
    within!(Task, limits.tasks, "task");
    within!(Attempt, limits.attempts, "attempt");
    within!(Prompt, limits.prompts, "prompt");
    within!(Operation, limits.operations, "operation");
    within!(Check, limits.checks, "check");
    within!(Source, limits.sources, "source");
    within!(Artifact, limits.artifacts, "artifact");
    within!(Group, limits.groups, "group");
    within!(Worktree, limits.worktrees, "worktree");
    within!(Machine, limits.machines, "machine");
    Ok(())
}

/// `StopRequested` only on tasks with a managed attempt (invariant 12).
fn stop_requested(world: &mut World) -> Result<(), (u8, String)> {
    let tasks: Vec<(Entity, Vec<Entity>)> = world
        .query_filtered::<(Entity, Option<&Attempts>), (With<Task>, With<StopRequested>)>()
        .iter(world)
        .map(|(e, a)| (e, a.map(|a| a.iter().collect()).unwrap_or_default()))
        .collect();
    for (task, attempts) in tasks {
        let managed = attempts
            .iter()
            .any(|a| world.get::<Ownership>(*a) == Some(&Ownership::Managed));
        if !managed {
            return Err((
                12,
                format!("task {task} has StopRequested without a managed attempt"),
            ));
        }
    }
    Ok(())
}
