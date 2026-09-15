//! `task verify TASK SOURCE` (RESULTS.md:61-76): selects one retained source, the latest
//! execution of every required check (all `Passed` on that source, without capture failures)
//! and the artifacts those checks captured, then hands the `Seal` to
//! `lifecycle::close_verified`, which writes it atomically with `Closed{Verified}`.
//! [`validate_seal`] re-derives the same conditions on a retained seal (journal load).

use bevy_ecs::prelude::*;

use super::artifacts::{artifacts_of, captured_by};
use super::{
    ArtifactPolicy, CheckError, CheckPolicy, RequirementStatus, SourceState, checks_of, refused,
    requirement_status,
};
use super::{CaptureRequests, CapturedBy, managed_attempt};
use crate::lifecycle;
use crate::model::*;

/// The scope string every verification record carries (RESULTS.md:76).
pub const SCOPE: &str = "declared-checks-and-artifacts-for-retained-source";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifySpec {
    pub source: Entity,
}

/// The seal a verification would write, or why it cannot.
fn select(world: &mut World, task: Entity, source: Entity) -> Result<Seal, CheckError> {
    if world.get::<SourceOf>(source).map(|s| s.0) != Some(task) {
        return refused("source belongs to another task");
    }
    if !matches!(
        world.get::<SourceState>(source),
        Some(SourceState::Retained { .. })
    ) {
        return refused("source is not retained");
    }
    if checks_of(world, task).into_iter().any(|c| {
        matches!(
            world.get::<CheckState>(c),
            Some(CheckState::Queued | CheckState::Running)
        )
    }) {
        return refused("outstanding submitted checks must finish first");
    }
    let policy = world.get::<CheckPolicy>(task).cloned().unwrap_or_default();
    if policy.required.is_empty() {
        return refused("verification needs at least one declared required check");
    }
    let mut checks = Vec::with_capacity(policy.required.len());
    for requirement in &policy.required {
        let check = match requirement_status(world, task, &requirement.name) {
            RequirementStatus::Passed(check) => check,
            other => {
                return refused(format!(
                    "required check {} is {}",
                    requirement.name,
                    other.name()
                ));
            }
        };
        if world.get::<CheckOn>(check).map(|s| s.0) != Some(source) {
            return refused(format!(
                "required check {} did not run on the selected source",
                requirement.name
            ));
        }
        checks.push(check);
    }
    let mut artifacts = Vec::new();
    for check in &checks {
        for artifact in captured_by(world, *check) {
            if world.get::<ArtifactState>(artifact) != Some(&ArtifactState::Collected) {
                return refused(format!(
                    "check {} has an artifact capture failure",
                    world.get::<CheckId>(*check).map_or("?", |c| c.0.as_str())
                ));
            }
            artifacts.push(artifact);
        }
    }
    let artifact_policy = world
        .get::<ArtifactPolicy>(task)
        .cloned()
        .unwrap_or_default();
    for requirement in &artifact_policy.required {
        // Latest capture request for the name, by check submission generation.
        let latest_request = checks_of(world, task)
            .into_iter()
            .filter(|c| {
                world
                    .get::<CaptureRequests>(*c)
                    .is_some_and(|r| r.0.iter().any(|q| q.name == requirement.name))
            })
            .max_by_key(|c| world.get::<CreatedGeneration>(*c).map(|g| g.0));
        // Latest retained bytes for the name, by collection generation.
        let latest_bytes = artifacts_of(world, task)
            .into_iter()
            .filter(|a| {
                world.get::<Required>(*a).is_some()
                    && world.get::<ArtifactState>(*a) == Some(&ArtifactState::Collected)
                    && world
                        .get::<Requirement>(*a)
                        .is_some_and(|r| r.0 == requirement.name)
            })
            .max_by_key(|a| world.get::<CreatedGeneration>(*a).map(|g| g.0));
        let (Some(request), Some(bytes)) = (latest_request, latest_bytes) else {
            return refused(format!(
                "required artifact {} was not captured by a selected check",
                requirement.name
            ));
        };
        let captured_by_selected = checks.contains(&request)
            && world
                .get::<CapturedBy>(bytes)
                .is_some_and(|by| checks.contains(&by.0));
        if !captured_by_selected || !artifacts.contains(&bytes) {
            return refused(format!(
                "required artifact {} was not captured by a selected check",
                requirement.name
            ));
        }
    }
    Ok(Seal {
        source,
        checks,
        artifacts,
        sealed_ms: world.resource::<Clock>().now_ms,
        generation: world.resource::<Generation>().0 + 1,
    })
}

/// Verifies `task` against `spec.source`; repeating with the same source returns the retained
/// seal, another source is refused (RESULTS.md:71-72).
pub fn verify(world: &mut World, task: Entity, spec: VerifySpec) -> Result<Seal, CheckError> {
    if world.get::<Task>(task).is_none() {
        return Err(CheckError::NotFound("task".into()));
    }
    if let Some(seal) = world.get::<Seal>(task).cloned() {
        return if seal.source == spec.source {
            Ok(seal)
        } else {
            refused("task is already verified against another source")
        };
    }
    if world.get::<TaskState>(task).is_some_and(|s| s.is_closed()) {
        return refused("task is no longer open");
    }
    managed_attempt(world, task)?;
    let seal = select(world, task, spec.source)?;
    lifecycle::close_verified(world, task, seal.clone())?;
    Ok(seal)
}

/// Re-derives the retained seal's conditions (invariant 4 plus RESULTS.md:63-66): its source is
/// the task's and retained, its checks are exactly the passed latest executions of every
/// required check on that source, its artifacts are exactly their collected captures.
pub fn validate_seal(world: &mut World, task: Entity) -> Result<(), String> {
    let Some(seal) = world.get::<Seal>(task).cloned() else {
        return Err("no seal".into());
    };
    if world.get::<TaskState>(task)
        != Some(&TaskState::Closed {
            outcome: TaskOutcome::Verified,
        })
    {
        return Err("sealed task is not Closed{Verified}".into());
    }
    if world.get::<SourceOf>(seal.source).map(|s| s.0) != Some(task) {
        return Err("seal source is not the task's".into());
    }
    if !matches!(
        world.get::<SourceState>(seal.source),
        Some(SourceState::Retained { .. })
    ) {
        return Err("seal source is not retained".into());
    }
    let policy = world.get::<CheckPolicy>(task).cloned().unwrap_or_default();
    if policy.required.is_empty() || seal.checks.len() != policy.required.len() {
        return Err("seal does not cover every required check".into());
    }
    for requirement in &policy.required {
        match requirement_status(world, task, &requirement.name) {
            RequirementStatus::Passed(check)
                if seal.checks.contains(&check)
                    && world.get::<CheckOn>(check).map(|s| s.0) == Some(seal.source) => {}
            _ => {
                return Err(format!(
                    "required check {} is not a sealed pass on the sealed source",
                    requirement.name
                ));
            }
        }
    }
    let mut expected = Vec::new();
    for check in &seal.checks {
        for artifact in captured_by(world, *check) {
            if world.get::<ArtifactState>(artifact) != Some(&ArtifactState::Collected) {
                return Err("a sealed check has an artifact capture failure".into());
            }
            expected.push(artifact);
        }
    }
    let mut sealed = seal.artifacts.clone();
    sealed.sort();
    expected.sort();
    if sealed != expected {
        return Err("seal artifacts differ from the sealed checks' captures".into());
    }
    Ok(())
}
