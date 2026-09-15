//! `zor/check.*`, `zor/source.*`, `zor/artifact.*`, `zor/task.verify`, `zor/task.results`
//! (milestone 6, owner ChecksSources). Handlers open the envelope, resolve public ids and call
//! the typed transitions of `crate::checks`; views carry public ids only.

use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_remote::{BrpError, BrpResult};

use super::methods::{
    MethodSpec, Request, codes, described, error, handler, invalid, spec, to_value,
};
use crate::checks::artifacts::artifacts_of;
use crate::checks::sources::sources_of;
use crate::checks::verify::SCOPE;
use crate::checks::{
    self, ArtifactBytes, ArtifactPolicy, CaptureRequest, CaptureRequests, CapturedBy, CheckCwd,
    CheckError, CheckPolicy, DEFAULT_TIMEOUT_MS, SourceState, checks_of,
};
use crate::model::*;

// ---------------------------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------------------------

described!(
    pub struct TaskParams {
        pub task: String,
    }
);
described!(
    pub struct CheckRequireParams {
        pub task: String,
        pub name: String,
        pub argv: Vec<String>,
    }
);
described!(
    pub struct ArtifactRequireParams {
        pub task: String,
        pub name: String,
        pub path: String,
    }
);
described!(
    pub struct RequiredCheckView {
        pub name: String,
        pub argv: Vec<String>,
        pub status: String,
        pub latest: Option<String>,
    }
);
described!(
    pub struct RequiredArtifactView {
        pub name: String,
        pub path: String,
        pub status: String,
        pub latest: Option<String>,
    }
);
described!(
    /// Both policies of a task with the status of every requirement (CHECKS.md:40-44).
    pub struct TaskPolicy {
        pub task: String,
        pub checks_sealed: bool,
        pub required_checks: Vec<RequiredCheckView>,
        pub passed_count: usize,
        pub artifacts_sealed: bool,
        pub required_artifacts: Vec<RequiredArtifactView>,
        pub collected_count: usize,
    }
);
described!(
    pub struct CaptureRequestParams {
        pub name: String,
        pub artifact: String,
    }
);
described!(
    pub struct CheckSubmitParams {
        pub task: String,
        pub check: String,
        pub argv: Vec<String>,
        pub timeout_ms: Option<u64>,
        pub requirement: Option<String>,
        pub source: Option<String>,
        pub artifacts: Option<Vec<CaptureRequestParams>>,
    }
);
described!(
    pub struct CheckParams {
        pub check: String,
    }
);
described!(
    pub struct CaptureView {
        pub name: String,
        pub artifact: String,
        pub state: String,
        pub problem: Option<String>,
    }
);
described!(
    /// One check's full evidence (CHECKS.md:76).
    pub struct CheckView {
        pub check: String,
        pub task: String,
        pub state: String,
        pub argv: Vec<String>,
        pub timeout_ms: u64,
        pub cwd: String,
        pub requirement: Option<String>,
        pub source: Option<String>,
        pub created_generation: u64,
        pub passed: Option<bool>,
        pub exit_code: Option<i32>,
        pub stdout: String,
        pub stderr: String,
        pub truncated: bool,
        pub problem: Option<String>,
        pub artifacts: Vec<CaptureView>,
    }
);
described!(
    pub struct CheckSummary {
        pub check: String,
        pub state: String,
        pub requirement: Option<String>,
        pub source: Option<String>,
        pub created_generation: u64,
        pub passed: Option<bool>,
    }
);
described!(
    pub struct CheckList {
        pub task: String,
        pub checks: Vec<CheckSummary>,
    }
);
described!(
    pub struct SourceRetainParams {
        pub task: String,
        pub source: String,
        pub expression: String,
    }
);
described!(
    pub struct SourceParams {
        pub source: String,
    }
);
described!(
    pub struct SourceView {
        pub source: String,
        pub task: String,
        pub state: String,
        pub expression: String,
        pub commit: Option<String>,
        pub dirty: Option<bool>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct SourceList {
        pub task: String,
        pub sources: Vec<SourceView>,
    }
);
described!(
    pub struct ArtifactRetainParams {
        pub task: String,
        pub artifact: String,
        pub path: String,
        pub requirement: Option<String>,
    }
);
described!(
    pub struct ArtifactParams {
        pub artifact: String,
    }
);
described!(
    pub struct ArtifactView {
        pub artifact: String,
        pub task: String,
        pub attempt: u64,
        pub path: String,
        pub state: String,
        pub requirement: Option<String>,
        pub check: Option<String>,
        pub generation: Option<u64>,
        pub bytes: Option<usize>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct ArtifactList {
        pub task: String,
        pub artifacts: Vec<ArtifactView>,
    }
);
described!(
    pub struct ArtifactFetched {
        pub artifact: String,
        pub path: String,
        pub bytes: Vec<u8>,
    }
);
described!(
    pub struct VerifyParams {
        pub task: String,
        pub source: String,
    }
);
described!(
    /// The retained verification record (RESULTS.md:69-76).
    pub struct SealView {
        pub task: String,
        pub status: String,
        pub scope: String,
        pub source: Option<String>,
        pub checks: Vec<String>,
        pub artifacts: Vec<String>,
        pub sealed_ms: Option<u64>,
        pub generation: Option<u64>,
    }
);
described!(
    /// `task result` (RESULTS.md:10-59): outcome, policies, evidence references, blockers.
    pub struct TaskResults {
        pub task: String,
        pub state: String,
        pub outcome: Option<String>,
        pub policy: TaskPolicy,
        pub checks: Vec<CheckSummary>,
        pub sources: Vec<SourceView>,
        pub artifacts: Vec<ArtifactView>,
        pub blockers: Vec<String>,
        pub verification: SealView,
    }
);

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn brp(e: CheckError) -> BrpError {
    match &e {
        CheckError::NotFound(_) => error(codes::NOT_FOUND, e.to_string()),
        CheckError::Lifecycle(reason) if reason.contains("uncertain") => {
            error(codes::UNCERTAIN, e.to_string())
        }
        _ => invalid(e.to_string()),
    }
}

fn task_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .task(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("task {id} not found")))
}

fn task_id(world: &World, task: Entity) -> String {
    world
        .get::<TaskId>(task)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

fn state_name<T: core::fmt::Debug>(state: Option<&T>) -> String {
    state
        .map(|s| format!("{s:?}").to_lowercase())
        .unwrap_or_default()
}

fn source_view(world: &World, source: Entity) -> SourceView {
    let (state, commit, dirty) = match world.get::<SourceState>(source) {
        Some(SourceState::Retained { dirty }) => (
            "retained".to_owned(),
            world
                .get::<SourceRevision>(source)
                .map(|r| r.commit.clone()),
            Some(*dirty),
        ),
        Some(SourceState::Failed) => ("failed".to_owned(), None, None),
        _ => ("resolving".to_owned(), None, None),
    };
    SourceView {
        source: world
            .get::<SourceId>(source)
            .map(|s| s.0.clone())
            .unwrap_or_default(),
        task: world
            .get::<SourceOf>(source)
            .map(|s| task_id(world, s.0))
            .unwrap_or_default(),
        state,
        expression: world
            .get::<SourceRevision>(source)
            .map(|r| r.expression.clone())
            .unwrap_or_default(),
        commit,
        dirty,
        problem: world.get::<Problem>(source).map(|p| p.0.clone()),
    }
}

fn artifact_view(world: &World, artifact: Entity) -> ArtifactView {
    let attempt = world.get::<ArtifactOf>(artifact).map(|a| a.0);
    ArtifactView {
        artifact: world
            .get::<ArtifactId>(artifact)
            .map(|a| a.0.clone())
            .unwrap_or_default(),
        task: attempt
            .and_then(|a| world.get::<AttemptOf>(a))
            .map(|t| task_id(world, t.0))
            .unwrap_or_default(),
        attempt: attempt
            .and_then(|a| world.get::<AttemptId>(a))
            .map_or(0, |a| a.0),
        path: world
            .get::<ArtifactPath>(artifact)
            .map(|p| p.0.clone())
            .unwrap_or_default(),
        state: state_name(world.get::<ArtifactState>(artifact)),
        requirement: world
            .get::<Required>(artifact)
            .and(world.get::<Requirement>(artifact))
            .map(|r| r.0.clone()),
        check: world
            .get::<CapturedBy>(artifact)
            .and_then(|c| world.get::<CheckId>(c.0))
            .map(|c| c.0.clone()),
        generation: world.get::<CreatedGeneration>(artifact).map(|g| g.0),
        bytes: world.get::<ArtifactBytes>(artifact).map(|b| b.0.len()),
        problem: world.get::<Problem>(artifact).map(|p| p.0.clone()),
    }
}

fn passed(state: Option<&CheckState>) -> Option<bool> {
    match state {
        Some(CheckState::Passed) => Some(true),
        Some(CheckState::Failed) => Some(false),
        _ => None,
    }
}

fn check_summary(world: &World, check: Entity) -> CheckSummary {
    CheckSummary {
        check: world
            .get::<CheckId>(check)
            .map(|c| c.0.clone())
            .unwrap_or_default(),
        state: state_name(world.get::<CheckState>(check)),
        requirement: world.get::<Requirement>(check).map(|r| r.0.clone()),
        source: world
            .get::<CheckOn>(check)
            .and_then(|s| world.get::<SourceId>(s.0))
            .map(|s| s.0.clone()),
        created_generation: world.get::<CreatedGeneration>(check).map_or(0, |g| g.0),
        passed: passed(world.get::<CheckState>(check)),
    }
}

fn check_view(world: &mut World, check: Entity) -> CheckView {
    let summary = check_summary(world, check);
    let command = world
        .get::<CheckCommand>(check)
        .cloned()
        .unwrap_or_default();
    let output = world
        .get::<Results>(check)
        .and_then(|r| r.iter().next())
        .and_then(|r| world.get::<OutputTail>(r))
        .cloned()
        .unwrap_or_default();
    let captures: Vec<CaptureView> = world
        .get::<CaptureRequests>(check)
        .cloned()
        .unwrap_or_default()
        .0
        .into_iter()
        .map(|request| {
            let artifact = world.resource::<Ids>().artifact(&request.artifact);
            CaptureView {
                name: request.name,
                artifact: request.artifact,
                state: state_name(artifact.and_then(|a| world.get::<ArtifactState>(a))),
                problem: artifact
                    .and_then(|a| world.get::<Problem>(a))
                    .map(|p| p.0.clone()),
            }
        })
        .collect();
    CheckView {
        check: summary.check,
        task: world
            .get::<CheckOf>(check)
            .map(|t| task_id(world, t.0))
            .unwrap_or_default(),
        state: summary.state,
        argv: command.argv,
        timeout_ms: command.timeout_ms,
        cwd: world
            .get::<CheckCwd>(check)
            .map(|c| c.0.clone())
            .unwrap_or_default(),
        requirement: summary.requirement,
        source: summary.source,
        created_generation: summary.created_generation,
        passed: summary.passed,
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        truncated: output.truncated,
        problem: world.get::<Problem>(check).map(|p| p.0.clone()),
        artifacts: captures,
    }
}

fn latest_artifact(world: &World, task: Entity, name: &str) -> Option<Entity> {
    artifacts_of(world, task)
        .into_iter()
        .filter(|a| {
            world.get::<Required>(*a).is_some()
                && world.get::<ArtifactState>(*a) == Some(&ArtifactState::Collected)
                && world.get::<Requirement>(*a).is_some_and(|r| r.0 == name)
        })
        .max_by_key(|a| world.get::<CreatedGeneration>(*a).map(|g| g.0))
}

fn policy_view(world: &World, task: Entity) -> TaskPolicy {
    let checks = world.get::<CheckPolicy>(task).cloned().unwrap_or_default();
    let artifacts = world
        .get::<ArtifactPolicy>(task)
        .cloned()
        .unwrap_or_default();
    let required_checks: Vec<RequiredCheckView> = checks
        .required
        .iter()
        .map(|r| {
            let status = checks::requirement_status(world, task, &r.name);
            RequiredCheckView {
                name: r.name.clone(),
                argv: r.argv.clone(),
                status: status.name().to_owned(),
                latest: status
                    .latest()
                    .and_then(|c| world.get::<CheckId>(c))
                    .map(|c| c.0.clone()),
            }
        })
        .collect();
    let required_artifacts: Vec<RequiredArtifactView> = artifacts
        .required
        .iter()
        .map(|r| {
            let latest = latest_artifact(world, task, &r.name);
            RequiredArtifactView {
                name: r.name.clone(),
                path: r.path.clone(),
                status: if latest.is_some() {
                    "collected"
                } else {
                    "missing"
                }
                .to_owned(),
                latest: latest
                    .and_then(|a| world.get::<ArtifactId>(a))
                    .map(|a| a.0.clone()),
            }
        })
        .collect();
    TaskPolicy {
        task: task_id(world, task),
        checks_sealed: checks.sealed,
        passed_count: required_checks
            .iter()
            .filter(|r| r.status == "passed")
            .count(),
        required_checks,
        artifacts_sealed: artifacts.sealed,
        collected_count: required_artifacts
            .iter()
            .filter(|r| r.status == "collected")
            .count(),
        required_artifacts,
    }
}

fn seal_view(world: &World, task: Entity, seal: Option<&Seal>) -> SealView {
    let id = |e: Entity| world.get::<CheckId>(e).map(|c| c.0.clone());
    SealView {
        task: task_id(world, task),
        status: if seal.is_some() {
            "verified"
        } else {
            "unverified"
        }
        .to_owned(),
        scope: SCOPE.to_owned(),
        source: seal
            .and_then(|s| world.get::<SourceId>(s.source))
            .map(|s| s.0.clone()),
        checks: seal
            .map(|s| s.checks.iter().filter_map(|c| id(*c)).collect())
            .unwrap_or_default(),
        artifacts: seal
            .map(|s| {
                s.artifacts
                    .iter()
                    .filter_map(|a| world.get::<ArtifactId>(*a).map(|a| a.0.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        sealed_ms: seal.map(|s| s.sealed_ms),
        generation: seal.map(|s| s.generation),
    }
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

fn check_require(mut req: Request, world: &mut World) -> BrpResult {
    let params: CheckRequireParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    checks::require_check(world, task, &params.name, params.argv).map_err(brp)?;
    to_value(policy_view(world, task))
}

fn artifact_require(mut req: Request, world: &mut World) -> BrpResult {
    let params: ArtifactRequireParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    checks::require_artifact(world, task, &params.name, &params.path).map_err(brp)?;
    to_value(policy_view(world, task))
}

fn check_submit(mut req: Request, world: &mut World) -> BrpResult {
    let params: CheckSubmitParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    let source = params
        .source
        .as_deref()
        .map(|id| {
            world
                .resource::<Ids>()
                .source(id)
                .ok_or_else(|| error(codes::NOT_FOUND, format!("source {id} not found")))
        })
        .transpose()?;
    let check = checks::submit_check(
        world,
        task,
        checks::CheckSpec {
            id: params.check,
            argv: params.argv,
            timeout_ms: params.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            requirement: params.requirement,
            source,
            artifacts: params
                .artifacts
                .unwrap_or_default()
                .into_iter()
                .map(|c| CaptureRequest {
                    name: c.name,
                    artifact: c.artifact,
                })
                .collect(),
        },
    )
    .map_err(brp)?;
    to_value(check_view(world, check))
}

fn check_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .check(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("check {id} not found")))
}

fn check_list(mut req: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let mut checks: Vec<CheckSummary> = checks_of(world, task)
        .into_iter()
        .map(|c| check_summary(world, c))
        .collect();
    checks.sort_by(|a, b| {
        a.created_generation
            .cmp(&b.created_generation)
            .then_with(|| a.check.cmp(&b.check))
    });
    to_value(CheckList {
        task: params.task,
        checks,
    })
}

fn check_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: CheckParams = req.parse()?;
    let check = check_entity(world, &params.check)?;
    to_value(check_view(world, check))
}

fn check_cancel(mut req: Request, world: &mut World) -> BrpResult {
    let params: CheckParams = req.parse()?;
    req.mutation(world)?;
    let check = check_entity(world, &params.check)?;
    checks::cancel_check(world, check).map_err(brp)?;
    to_value(check_view(world, check))
}

fn source_retain(mut req: Request, world: &mut World) -> BrpResult {
    let params: SourceRetainParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    let source = checks::retain_source(
        world,
        task,
        checks::SourceSpec {
            id: params.source,
            expression: params.expression,
        },
    )
    .map_err(brp)?;
    to_value(source_view(world, source))
}

fn source_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .source(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("source {id} not found")))
}

fn source_list(mut req: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let mut sources: Vec<SourceView> = sources_of(world, task)
        .into_iter()
        .map(|s| source_view(world, s))
        .collect();
    sources.sort_by(|a, b| a.source.cmp(&b.source));
    to_value(SourceList {
        task: params.task,
        sources,
    })
}

fn source_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: SourceParams = req.parse()?;
    let source = source_entity(world, &params.source)?;
    to_value(source_view(world, source))
}

fn artifact_retain(mut req: Request, world: &mut World) -> BrpResult {
    let params: ArtifactRetainParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    let attempt = checks::managed_attempt(world, task).map_err(brp)?;
    let artifact = checks::retain_artifact(
        world,
        attempt,
        checks::ArtifactSpec {
            id: params.artifact,
            path: params.path,
            requirement: params.requirement,
        },
    )
    .map_err(brp)?;
    to_value(artifact_view(world, artifact))
}

fn artifact_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .artifact(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("artifact {id} not found")))
}

fn artifact_list(mut req: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let mut artifacts: Vec<ArtifactView> = artifacts_of(world, task)
        .into_iter()
        .map(|a| artifact_view(world, a))
        .collect();
    artifacts.sort_by(|a, b| {
        a.generation
            .cmp(&b.generation)
            .then_with(|| a.artifact.cmp(&b.artifact))
    });
    to_value(ArtifactList {
        task: params.task,
        artifacts,
    })
}

fn artifact_fetch(mut req: Request, world: &mut World) -> BrpResult {
    let params: ArtifactParams = req.parse()?;
    let artifact = artifact_entity(world, &params.artifact)?;
    let Some(bytes) = world.get::<ArtifactBytes>(artifact) else {
        return Err(invalid(format!(
            "artifact {} has no retained bytes",
            params.artifact
        )));
    };
    to_value(ArtifactFetched {
        artifact: params.artifact,
        path: world
            .get::<ArtifactPath>(artifact)
            .map(|p| p.0.clone())
            .unwrap_or_default(),
        bytes: bytes.0.clone(),
    })
}

fn task_verify(mut req: Request, world: &mut World) -> BrpResult {
    let params: VerifyParams = req.parse()?;
    req.mutation(world)?;
    let task = task_entity(world, &params.task)?;
    let source = source_entity(world, &params.source)?;
    let seal = checks::verify(world, task, checks::VerifySpec { source }).map_err(brp)?;
    to_value(seal_view(world, task, Some(&seal)))
}

fn task_results(mut req: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let policy = policy_view(world, task);
    let mut blockers = Vec::new();
    let state = world.get::<TaskState>(task).copied().unwrap_or_default();
    let outcome = match state {
        TaskState::Closed { outcome } => Some(format!("{outcome:?}").to_lowercase()),
        _ => None,
    };
    if state
        == (TaskState::Closed {
            outcome: TaskOutcome::Cancelled,
        })
    {
        blockers.push("task-cancelled".to_owned());
    }
    for r in &policy.required_checks {
        match r.status.as_str() {
            "passed" => {}
            "missing" => blockers.push(format!("required-check-missing:{}", r.name)),
            other => blockers.push(format!("required-check-{other}:{}", r.name)),
        }
    }
    for r in &policy.required_artifacts {
        if r.status != "collected" {
            blockers.push(format!("required-artifacts-missing:{}", r.name));
        }
        let latest_capture = checks_of(world, task)
            .into_iter()
            .filter(|c| {
                world
                    .get::<CaptureRequests>(*c)
                    .is_some_and(|q| q.0.iter().any(|q| q.name == r.name))
            })
            .max_by_key(|c| world.get::<CreatedGeneration>(*c).map(|g| g.0));
        if let Some(check) = latest_capture {
            let artifact = world
                .get::<CaptureRequests>(check)
                .and_then(|q| q.0.iter().find(|q| q.name == r.name).cloned())
                .and_then(|q| world.resource::<Ids>().artifact(&q.artifact));
            match artifact.and_then(|a| world.get::<ArtifactState>(a)) {
                Some(ArtifactState::Collected) => {}
                Some(ArtifactState::Failed) => {
                    blockers.push(format!("artifact-capture-failed:{}", r.name));
                }
                _ => blockers.push(format!("artifact-capture-pending:{}", r.name)),
            }
        }
    }
    let mut checks: Vec<CheckSummary> = checks_of(world, task)
        .into_iter()
        .map(|c| check_summary(world, c))
        .collect();
    checks.sort_by(|a, b| {
        a.created_generation
            .cmp(&b.created_generation)
            .then_with(|| a.check.cmp(&b.check))
    });
    if let Some(attempts) = world.get::<Attempts>(task) {
        for attempt in attempts.iter() {
            if world.get::<Uncertain>(attempt).is_some() || world.get::<Lost>(attempt).is_some() {
                blockers.push("attempt-unresolved".to_owned());
            }
            if let Some(prompts) = world.get::<Prompts>(attempt)
                && prompts
                    .iter()
                    .any(|p| world.get::<Delivery>(p).is_some_and(|d| d.is_pending()))
            {
                blockers.push("prompt-unresolved".to_owned());
            }
        }
    }
    let seal = world.get::<Seal>(task).cloned();
    let verification = seal_view(world, task, seal.as_ref());
    let mut sources: Vec<SourceView> = sources_of(world, task)
        .into_iter()
        .map(|s| source_view(world, s))
        .collect();
    sources.sort_by(|a, b| a.source.cmp(&b.source));
    let mut artifacts: Vec<ArtifactView> = artifacts_of(world, task)
        .into_iter()
        .map(|a| artifact_view(world, a))
        .collect();
    artifacts.sort_by(|a, b| a.artifact.cmp(&b.artifact));
    to_value(TaskResults {
        task: params.task,
        state: match state {
            TaskState::Open => "open",
            TaskState::Running => "running",
            TaskState::Blocked => "blocked",
            TaskState::Closed { .. } => "closed",
        }
        .to_owned(),
        outcome,
        policy,
        checks,
        sources,
        artifacts,
        blockers,
        verification,
    })
}

// ---------------------------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------------------------

handler!(brp_check_require, check_require);
handler!(brp_artifact_require, artifact_require);
handler!(brp_check_submit, check_submit);
handler!(brp_check_list, check_list);
handler!(brp_check_inspect, check_inspect);
handler!(brp_check_cancel, check_cancel);
handler!(brp_source_retain, source_retain);
handler!(brp_source_list, source_list);
handler!(brp_source_inspect, source_inspect);
handler!(brp_artifact_retain, artifact_retain);
handler!(brp_artifact_list, artifact_list);
handler!(brp_artifact_fetch, artifact_fetch);
handler!(brp_task_verify, task_verify);
handler!(brp_task_results, task_results);

pub static METHODS: &[MethodSpec] = &[
    spec!(
        "zor/check.require",
        brp_check_require,
        CheckRequireParams,
        TaskPolicy
    ),
    spec!(
        "zor/artifact.require",
        brp_artifact_require,
        ArtifactRequireParams,
        TaskPolicy
    ),
    spec!(
        "zor/check.submit",
        brp_check_submit,
        CheckSubmitParams,
        CheckView
    ),
    spec!("zor/check.list", brp_check_list, TaskParams, CheckList),
    spec!(
        "zor/check.inspect",
        brp_check_inspect,
        CheckParams,
        CheckView
    ),
    spec!("zor/check.cancel", brp_check_cancel, CheckParams, CheckView),
    spec!(
        "zor/source.retain",
        brp_source_retain,
        SourceRetainParams,
        SourceView
    ),
    spec!("zor/source.list", brp_source_list, TaskParams, SourceList),
    spec!(
        "zor/source.inspect",
        brp_source_inspect,
        SourceParams,
        SourceView
    ),
    spec!(
        "zor/artifact.retain",
        brp_artifact_retain,
        ArtifactRetainParams,
        ArtifactView
    ),
    spec!(
        "zor/artifact.list",
        brp_artifact_list,
        TaskParams,
        ArtifactList
    ),
    spec!(
        "zor/artifact.fetch",
        brp_artifact_fetch,
        ArtifactParams,
        ArtifactFetched
    ),
    spec!("zor/task.verify", brp_task_verify, VerifyParams, SealView),
    spec!(
        "zor/task.results",
        brp_task_results,
        TaskParams,
        TaskResults
    ),
];
