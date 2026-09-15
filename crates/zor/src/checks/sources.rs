//! Retained committed inputs (SOURCES.md): a source resolves a revision expression to a commit
//! in the task's launch directory and records whether the index/worktree was dirty at that
//! moment. The intent (`Resolving`) is committed first; the git commands go out through
//! `git::request` only after the journal took the update; the replies land in `Completions`.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;

use super::{CheckError, SourceState, managed_attempt, open_task, refused, task_cwd};
use crate::git;
use crate::model::*;

/// Revision expressions: one line, no option-like prefix (SOURCES.md:13).
pub const MAX_EXPRESSION_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub id: String,
    pub expression: String,
}

/// One in-flight resolution: the git op awaited and the step it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    RevParse(u64),
    Status(u64),
}

/// Sources awaiting git replies; never persisted (a restart makes them `Failed`).
#[derive(Resource, Default, Debug)]
pub struct Resolutions(HashMap<Entity, Option<Step>>);

pub fn validate_expression(expression: &str) -> Result<(), CheckError> {
    if expression.is_empty()
        || expression.len() > MAX_EXPRESSION_BYTES
        || expression.starts_with('-')
        || expression
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return refused("revision expression must be one bounded word not starting with '-'");
    }
    Ok(())
}

/// Commits the retention intent (SOURCES.md:10-15): an identical retry under the id reads the
/// record whatever its state; a different task or expression conflicts.
pub fn retain_source(
    world: &mut World,
    task: Entity,
    spec: SourceSpec,
) -> Result<Entity, CheckError> {
    validate_expression(&spec.expression)?;
    if let Some(existing) = world.resource::<Ids>().source(&spec.id) {
        let same = world.get::<SourceOf>(existing).map(|s| s.0) == Some(task)
            && world
                .get::<SourceRevision>(existing)
                .is_some_and(|r| r.expression == spec.expression);
        return if same {
            Ok(existing)
        } else {
            Err(CheckError::Conflict(format!(
                "source {} already exists with different intent",
                spec.id
            )))
        };
    }
    open_task(world, task)?;
    managed_attempt(world, task)?;
    task_cwd(world, task)?;
    let source = spawn_source(
        world,
        &spec.id,
        task,
        SourceRevision {
            expression: spec.expression,
            commit: String::new(),
        },
    )?;
    world.entity_mut(source).insert(SourceState::Resolving);
    world.resource_mut::<Resolutions>().0.insert(source, None);
    Ok(source)
}

/// Every source of `task`.
pub fn sources_of(world: &World, task: Entity) -> Vec<Entity> {
    world
        .get::<Sources>(task)
        .map(|s| s.iter().collect())
        .unwrap_or_default()
}

/// `Last`, after the journal committed: resolving sources without an outstanding op get their
/// next git command.
pub(super) fn dispatch(world: &mut World) {
    let waiting: Vec<Entity> = world
        .resource::<Resolutions>()
        .0
        .iter()
        .filter(|(_, step)| step.is_none())
        .map(|(e, _)| *e)
        .collect();
    for source in waiting {
        let Some((task, expression)) = world.get::<SourceOf>(source).map(|s| s.0).zip(
            world
                .get::<SourceRevision>(source)
                .map(|r| r.expression.clone()),
        ) else {
            world.resource_mut::<Resolutions>().0.remove(&source);
            continue;
        };
        let cwd = match task_cwd(world, task) {
            Ok(cwd) => cwd,
            Err(e) => {
                fail(world, source, &format!("launch directory unavailable: {e}"));
                continue;
            }
        };
        let op = git::request(
            world,
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "--end-of-options".into(),
                format!("{expression}^{{commit}}"),
            ],
            cwd,
        );
        world
            .resource_mut::<Resolutions>()
            .0
            .insert(source, Some(Step::RevParse(op)));
    }
}

/// `Completions`: consumes git replies; `rev-parse` then `status --porcelain`.
pub(super) fn resolve(world: &mut World) {
    let pending: Vec<(Entity, Step)> = world
        .resource::<Resolutions>()
        .0
        .iter()
        .filter_map(|(e, step)| step.map(|s| (*e, s)))
        .collect();
    for (source, step) in pending {
        let op = match step {
            Step::RevParse(op) | Step::Status(op) => op,
        };
        let Some(reply) = git::take_reply(world, op) else {
            continue;
        };
        if reply.code.is_none() {
            fail(
                world,
                source,
                &format!("git outcome unknown: {}", reply.stderr.trim()),
            );
            continue;
        }
        if !reply.succeeded() {
            fail(
                world,
                source,
                &format!("git failed: {}", reply.stderr.trim()),
            );
            continue;
        }
        match step {
            Step::RevParse(_) => {
                let commit = reply.stdout.trim().to_owned();
                if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                    fail(world, source, "revision did not resolve to a commit");
                    continue;
                }
                let Some((task, expression)) = world.get::<SourceOf>(source).map(|s| s.0).zip(
                    world
                        .get::<SourceRevision>(source)
                        .map(|r| r.expression.clone()),
                ) else {
                    world.resource_mut::<Resolutions>().0.remove(&source);
                    continue;
                };
                world
                    .entity_mut(source)
                    .insert(SourceRevision { expression, commit });
                let cwd = match task_cwd(world, task) {
                    Ok(cwd) => cwd,
                    Err(e) => {
                        fail(world, source, &format!("launch directory unavailable: {e}"));
                        continue;
                    }
                };
                let op = git::request(
                    world,
                    vec![
                        "status".into(),
                        "--porcelain".into(),
                        "--untracked-files=no".into(),
                    ],
                    cwd,
                );
                world
                    .resource_mut::<Resolutions>()
                    .0
                    .insert(source, Some(Step::Status(op)));
            }
            Step::Status(_) => {
                let dirty = !reply.stdout.trim().is_empty();
                world
                    .entity_mut(source)
                    .insert(SourceState::Retained { dirty });
                world.resource_mut::<Resolutions>().0.remove(&source);
            }
        }
    }
}

fn fail(world: &mut World, source: Entity, reason: &str) {
    world
        .entity_mut(source)
        .insert((SourceState::Failed, super::bounded_problem(reason)));
    world.resource_mut::<Resolutions>().0.remove(&source);
}

/// A restart loses every pending git reply: resolving sources fail, never re-resolved.
pub(super) fn recover(world: &mut World) {
    let resolving: Vec<Entity> = world
        .query_filtered::<(Entity, &SourceState), With<Source>>()
        .iter(world)
        .filter(|(_, s)| **s == SourceState::Resolving)
        .map(|(e, _)| e)
        .collect();
    for source in resolving {
        fail(world, source, "unresolved when the service restarted");
    }
}
