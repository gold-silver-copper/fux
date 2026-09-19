//! Checks, sources, artifacts, results and verification (CHECKS.md, SOURCES.md, ARTIFACTS.md,
//! RESULTS.md; `docs/model.md` invariants 4, 8, 9, 11, 14, 20-23).
//!
//! Every intent is committed to the World first: a submitted check is spawned `Queued` in the
//! caller's update and only dispatched (`Running` + `Effect::RunCheck`) in `Last`, once the
//! journal committed the update it was spawned in; the same holds for source retention and its
//! `Effect::RunGit`. Identical retries under an id return the record, conflicting intent fails
//! (invariant 14). The first check submission seals both policies, the first retained artifact
//! seals the artifact policy (invariant 22). After `task verify` sealed the task, every new
//! evidence operation is refused (invariant 23).

pub mod artifacts;
pub mod runner;
pub mod sources;
pub mod verify;

use bevy_app::prelude::*;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_log::error;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};

use crate::journal::Journal;
use crate::lifecycle::{self, LifecycleError};
use crate::model::graph::ModelError;
use crate::model::*;
use crate::remote::events::CheckChanged;

pub use artifacts::{ArtifactSpec, retain_artifact};
pub use sources::{SourceSpec, retain_source};
pub use verify::{VerifySpec, validate_seal, verify};

/// Check timeouts: 1 ms through five minutes (CHECKS.md:46).
pub const MAX_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Capture requests per check (ARTIFACTS.md:73).
pub const MAX_CAPTURES_PER_CHECK: usize = 8;
/// Argv bounds shared with launches (TASKS.md:98).
pub const MAX_ARGV: usize = 128;
pub const MAX_ARG_BYTES: usize = 4096;
pub const MAX_ARGV_BYTES: usize = 16_384;
/// Bounded cause chain on an uncertain record (CHECKS.md:92).
pub const MAX_PROBLEM_CHARS: usize = 256;

// ---------------------------------------------------------------------------------------------
// Components (persisted; appended to the journal vocabulary)
// ---------------------------------------------------------------------------------------------

/// A declared required check: task-scoped name and the exact argv (CHECKS.md:33-38).
#[derive(Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredCheck {
    pub name: String,
    pub argv: Vec<String>,
}

/// A task's check policy: sealed by the first submission, never reduced (invariant 22).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct CheckPolicy {
    pub sealed: bool,
    pub required: Vec<RequiredCheck>,
}

/// A declared required artifact: task-scoped name and exact relative path (ARTIFACTS.md:20-26).
#[derive(Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredArtifact {
    pub name: String,
    pub path: String,
}

/// A task's artifact policy: sealed by the first retained artifact or check submission.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct ArtifactPolicy {
    pub sealed: bool,
    pub required: Vec<RequiredArtifact>,
}

/// `--artifact NAME=ID`: a declared artifact name captured under a reserved id (ARTIFACTS.md:73-76).
#[derive(Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub name: String,
    pub artifact: String,
}

/// Immutable check intent: the capture requests, in submission order.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct CaptureRequests(pub Vec<CaptureRequest>);

/// The directory a check ran in, recorded at submission (CHECKS.md:10-11).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct CheckCwd(pub String);

/// The check that reserved and captured this artifact (ARTIFACTS.md:78-90).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
#[reflect(Component)]
#[component(immutable)]
pub struct CapturedBy(#[entities] pub Entity);

/// Exact retained bytes (ARTIFACTS.md:10): at most [`artifacts::MAX_ARTIFACT_BYTES`].
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct ArtifactBytes(pub Vec<u8>);

/// Source resolution: intent committed (`Resolving`), the revision retained with its dirty
/// status, or the resolution failed/lost (never retried under the same id, SOURCES.md:13-15).
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    #[default]
    Resolving,
    Retained {
        dirty: bool,
    },
    Failed,
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    NotFound(String),
    /// The id exists with different intent (invariant 14).
    Conflict(String),
    /// Well-formed but not applicable: closed or sealed task, adopted attempt, policy rules.
    Refused(String),
    Model(ModelError),
    Lifecycle(String),
}

impl core::fmt::Display for CheckError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Conflict(reason) | Self::Refused(reason) | Self::Lifecycle(reason) => {
                f.write_str(reason)
            }
            Self::Model(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for CheckError {}

impl From<ModelError> for CheckError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

impl From<LifecycleError> for CheckError {
    fn from(e: LifecycleError) -> Self {
        Self::Lifecycle(e.to_string())
    }
}

fn refused<T>(reason: impl Into<String>) -> Result<T, CheckError> {
    Err(CheckError::Refused(reason.into()))
}

// ---------------------------------------------------------------------------------------------
// Shared preconditions
// ---------------------------------------------------------------------------------------------

/// The task exists, is not closed and is not sealed (CHECKS.md:14, RESULTS.md:72-73).
pub(crate) fn open_task(world: &World, task: Entity) -> Result<(), CheckError> {
    let Some(state) = world
        .get::<TaskState>(task)
        .filter(|_| world.get::<Task>(task).is_some())
    else {
        return Err(CheckError::NotFound("task".into()));
    };
    if world.get::<Seal>(task).is_some() {
        return refused("task is verified and sealed: new evidence is refused");
    }
    if state.is_closed() {
        return refused("task is no longer open");
    }
    Ok(())
}

/// The attempt evidence is attributed to: the current attempt, else the latest one; it must be
/// `Managed` (invariant 17: adopted attempts grant no check authority).
pub(crate) fn managed_attempt(world: &World, task: Entity) -> Result<Entity, CheckError> {
    let attempt = lifecycle::attempt_of(world, task).or_else(|| {
        world
            .get::<Attempts>(task)?
            .iter()
            .max_by_key(|a| world.get::<AttemptId>(*a).copied())
    });
    let Some(attempt) = attempt else {
        return refused("checks and artifacts need a managed task launch");
    };
    if world.get::<Ownership>(attempt) != Some(&Ownership::Managed) {
        return refused("adopted attempts grant no check or artifact authority");
    }
    Ok(attempt)
}

/// The directory of the task's launch: its literal cwd or its worktree's checkout; refused
/// while the worktree carries removal intent (CHECKS.md:12-13).
pub(crate) fn task_cwd(world: &World, task: Entity) -> Result<String, CheckError> {
    match world.get::<Location>(task) {
        Some(Location::Cwd(path)) => Ok(path.clone()),
        Some(Location::Worktree(id)) => {
            let Some(worktree) = world.resource::<Ids>().worktree(id) else {
                return Err(CheckError::NotFound("worktree".into()));
            };
            if matches!(
                world.get::<WorktreeState>(worktree),
                Some(WorktreeState::Removing | WorktreeState::Removed)
            ) {
                return refused("the task's worktree carries removal intent");
            }
            match crate::worktrees::path_of(world, worktree) {
                Some(path)
                    if world.get::<WorktreeState>(worktree) == Some(&WorktreeState::Ready) =>
                {
                    Ok(path.display().to_string())
                }
                _ => refused("the task's worktree is not ready"),
            }
        }
        None => Err(CheckError::NotFound("task".into())),
    }
}

pub(crate) fn validate_argv(argv: &[String]) -> Result<(), CheckError> {
    let total: usize = argv.iter().map(String::len).sum();
    if argv.is_empty()
        || argv.len() > MAX_ARGV
        || total > MAX_ARGV_BYTES
        || argv
            .iter()
            .any(|a| a.len() > MAX_ARG_BYTES || a.contains('\0'))
        || argv.first().is_some_and(String::is_empty)
    {
        return refused("argv must be 1..=128 NUL-free arguments within 16 KiB");
    }
    Ok(())
}

/// The journal generation this record commits in, unique across records submitted within one
/// update (CHECKS.md:41 orders requirement status by it).
pub(crate) fn next_generation(world: &mut World) -> u64 {
    let committed = world.resource::<Generation>().0;
    let mut issued = world.resource_mut::<IssuedGeneration>();
    issued.0 = issued.0.max(committed) + 1;
    issued.0
}

/// The highest `CreatedGeneration` handed out; continues above the restored records.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct IssuedGeneration(pub u64);

fn bounded_problem(text: impl Into<String>) -> Problem {
    let text: String = text.into();
    Problem(text.chars().take(MAX_PROBLEM_CHARS).collect())
}

fn trigger_check_changed(world: &mut World, check: Entity, state: CheckState) {
    if let Some(id) = world.get::<CheckId>(check).cloned() {
        world.trigger(CheckChanged {
            entity: check,
            check: id,
            state,
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------------------------

/// `require-check TASK NAME -- ARGV` (CHECKS.md:33-38): an identical declaration is a read-only
/// retry, changed argv conflicts, a sealed policy refuses additions.
pub fn require_check(
    world: &mut World,
    task: Entity,
    name: &str,
    argv: Vec<String>,
) -> Result<(), CheckError> {
    if !valid_id(name) {
        return refused("invalid requirement name");
    }
    validate_argv(&argv)?;
    open_task(world, task)?;
    managed_attempt(world, task)?;
    let policy = world.get::<CheckPolicy>(task).cloned().unwrap_or_default();
    if let Some(existing) = policy.required.iter().find(|r| r.name == name) {
        if existing.argv == argv {
            return Ok(());
        }
        return Err(CheckError::Conflict(
            "required check already has a different command".into(),
        ));
    }
    if policy.sealed {
        return refused("check policy is sealed once the first check is submitted");
    }
    if policy.required.len() >= MAX_REQUIREMENTS_PER_TASK {
        return refused("at most 32 required checks per task");
    }
    let mut policy = policy;
    policy.required.push(RequiredCheck {
        name: name.to_owned(),
        argv,
    });
    world.entity_mut(task).insert(policy);
    Ok(())
}

/// `require-artifact TASK NAME PATH` (ARTIFACTS.md:20-26).
pub fn require_artifact(
    world: &mut World,
    task: Entity,
    name: &str,
    path: &str,
) -> Result<(), CheckError> {
    if !valid_id(name) {
        return refused("invalid requirement name");
    }
    artifacts::validate_path(path)?;
    if world.get::<Task>(task).is_none() {
        return Err(CheckError::NotFound("task".into()));
    }
    let policy = world
        .get::<ArtifactPolicy>(task)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = policy.required.iter().find(|r| r.name == name) {
        // Exact retries read current state even after sealing or cancellation (ARTIFACTS.md:21-22).
        if existing.path == path {
            return Ok(());
        }
        return Err(CheckError::Conflict(
            "required artifact already has a different path".into(),
        ));
    }
    open_task(world, task)?;
    managed_attempt(world, task)?;
    if policy.sealed {
        return refused(
            "artifact policy is sealed once an artifact was retained or a check submitted",
        );
    }
    if policy.required.len() >= MAX_REQUIREMENTS_PER_TASK {
        return refused("at most 32 required artifacts per task");
    }
    let mut policy = policy;
    policy.required.push(RequiredArtifact {
        name: name.to_owned(),
        path: path.to_owned(),
    });
    world.entity_mut(task).insert(policy);
    Ok(())
}

fn seal_policies(world: &mut World, task: Entity, checks: bool) {
    let mut entity = world.entity_mut(task);
    if checks {
        let mut policy = entity.get::<CheckPolicy>().cloned().unwrap_or_default();
        if !policy.sealed {
            policy.sealed = true;
            entity.insert(policy);
        }
    }
    let mut policy = entity.get::<ArtifactPolicy>().cloned().unwrap_or_default();
    if !policy.sealed {
        policy.sealed = true;
        entity.insert(policy);
    }
}

// ---------------------------------------------------------------------------------------------
// Submission
// ---------------------------------------------------------------------------------------------

/// One check submission (CHECKS.md:17, ARTIFACTS.md:73-76).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSpec {
    pub id: String,
    pub argv: Vec<String>,
    pub timeout_ms: u64,
    pub requirement: Option<String>,
    /// A retained source of the same task (SOURCES.md:45).
    pub source: Option<Entity>,
    pub artifacts: Vec<CaptureRequest>,
}

/// Commits a check intent: `Queued` with its cwd, generation and reserved captures; dispatched
/// by [`dispatch`] after the journal committed it. Returns the existing record for an identical
/// retry (invariant 14).
pub fn submit_check(
    world: &mut World,
    task: Entity,
    spec: CheckSpec,
) -> Result<Entity, CheckError> {
    if !valid_id(&spec.id) {
        return Err(CheckError::Model(ModelError::InvalidId(spec.id)));
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&spec.timeout_ms) {
        return refused("timeout must be 1 ms through 300000 ms");
    }
    validate_argv(&spec.argv)?;
    if let Some(existing) = world.resource::<Ids>().check(&spec.id) {
        return if same_intent(world, existing, task, &spec) {
            Ok(existing)
        } else {
            Err(CheckError::Conflict(format!(
                "check {} already exists with different intent",
                spec.id
            )))
        };
    }
    open_task(world, task)?;
    let attempt = managed_attempt(world, task)?;
    let cwd = task_cwd(world, task)?;
    if let Some(name) = &spec.requirement {
        let declared = world
            .get::<CheckPolicy>(task)
            .and_then(|p| p.required.iter().find(|r| &r.name == name))
            .map(|r| r.argv.clone());
        if declared.as_ref() != Some(&spec.argv) {
            return refused("check does not match its required name and command");
        }
    }
    if let Some(source) = spec.source {
        if world.get::<SourceOf>(source).map(|s| s.0) != Some(task) {
            return refused("source belongs to another task");
        }
        if !matches!(
            world.get::<SourceState>(source),
            Some(SourceState::Retained { .. })
        ) {
            return refused("source is not retained");
        }
    }
    artifacts::validate_requests(world, task, spec.source.is_some(), &spec.artifacts)?;
    let generation = next_generation(world);
    let check = spawn_check(
        world,
        crate::model::CheckSpec {
            id: &spec.id,
            task,
            source: spec.source,
            command: CheckCommand {
                argv: spec.argv,
                timeout_ms: spec.timeout_ms,
            },
            requirement: spec.requirement.as_deref(),
            generation,
        },
    )?;
    world
        .entity_mut(check)
        .insert((CheckCwd(cwd), CaptureRequests(spec.artifacts.clone())));
    for request in &spec.artifacts {
        // Validated above: declared name, unused id, within bounds.
        if let Err(e) = artifacts::reserve(world, attempt, check, request) {
            error!(
                "check {}: capture reservation failed after validation: {e}",
                spec.id
            );
        }
    }
    seal_policies(world, task, true);
    trigger_check_changed(world, check, CheckState::Queued);
    Ok(check)
}

fn same_intent(world: &World, check: Entity, task: Entity, spec: &CheckSpec) -> bool {
    world.get::<CheckOf>(check).map(|c| c.0) == Some(task)
        && world
            .get::<CheckCommand>(check)
            .is_some_and(|c| c.argv == spec.argv && c.timeout_ms == spec.timeout_ms)
        && world.get::<Requirement>(check).map(|r| &r.0) == spec.requirement.as_ref()
        && world.get::<CheckOn>(check).map(|s| s.0) == spec.source
        && world
            .get::<CaptureRequests>(check)
            .is_some_and(|r| r.0 == spec.artifacts)
}

/// `zor/check.cancel`: a `Queued` check is retired before any effect; a `Running` one has its
/// process group killed and lands `Uncertain` through the adapter's reply. Terminal checks are
/// unchanged (their evidence stays).
pub fn cancel_check(world: &mut World, check: Entity) -> Result<(), CheckError> {
    let Some(state) = world.get::<CheckState>(check).copied() else {
        return Err(CheckError::NotFound("check".into()));
    };
    match state {
        CheckState::Queued => {
            finish(
                world,
                check,
                None,
                OutputTail::default(),
                Some("cancelled before the command started"),
            );
            Ok(())
        }
        CheckState::Running => {
            world.write_message(Effect::KillCheck { check });
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------------------------

/// A requirement's status: the latest submitted execution by `created_generation` (RESULTS.md:16-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementStatus {
    Missing,
    Submitted(Entity),
    Uncertain(Entity),
    Failed(Entity),
    Passed(Entity),
}

impl RequirementStatus {
    pub fn latest(self) -> Option<Entity> {
        match self {
            Self::Missing => None,
            Self::Submitted(e) | Self::Uncertain(e) | Self::Failed(e) | Self::Passed(e) => Some(e),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Submitted(_) => "submitted",
            Self::Uncertain(_) => "uncertain",
            Self::Failed(_) => "failed",
            Self::Passed(_) => "passed",
        }
    }
}

/// Every check of `task`.
pub fn checks_of(world: &World, task: Entity) -> Vec<Entity> {
    world
        .get::<TaskChecks>(task)
        .map(|c| c.iter().collect())
        .unwrap_or_default()
}

/// The latest execution submitted for requirement `name` (invariant 21).
pub fn latest_for_requirement(world: &World, task: Entity, name: &str) -> Option<Entity> {
    checks_of(world, task)
        .into_iter()
        .filter(|c| {
            world.get::<Required>(*c).is_some()
                && world.get::<Requirement>(*c).is_some_and(|r| r.0 == name)
        })
        .max_by_key(|c| world.get::<CreatedGeneration>(*c).map(|g| g.0))
}

pub fn requirement_status(world: &World, task: Entity, name: &str) -> RequirementStatus {
    let Some(check) = latest_for_requirement(world, task, name) else {
        return RequirementStatus::Missing;
    };
    match world.get::<CheckState>(check).copied().unwrap_or_default() {
        CheckState::Queued | CheckState::Running => RequirementStatus::Submitted(check),
        CheckState::Uncertain => RequirementStatus::Uncertain(check),
        CheckState::Failed => RequirementStatus::Failed(check),
        CheckState::Passed => RequirementStatus::Passed(check),
    }
}

/// Any check of the task in `Queued|Running|Uncertain` (invariant 9, RECOVERY.md:26-31).
pub fn has_unresolved_checks(world: &World, task: Entity) -> bool {
    checks_of(world, task).into_iter().any(|c| {
        matches!(
            world.get::<CheckState>(c),
            Some(CheckState::Queued | CheckState::Running | CheckState::Uncertain)
        )
    })
}

// ---------------------------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------------------------

/// Publishes terminal evidence: state, one `Result` with the verdict and output, capture of the
/// reserved artifacts (or their problems), all in this update (ARTIFACTS.md:87-89).
pub(crate) fn finish(
    world: &mut World,
    check: Entity,
    code: Option<i32>,
    output: OutputTail,
    problem: Option<&str>,
) {
    let Some(current) = world.get::<CheckState>(check).copied() else {
        return;
    };
    let (next, verdict) = match (problem, code) {
        (Some(_), _) => (CheckState::Uncertain, Verdict::Uncertain),
        (None, Some(0)) => (CheckState::Passed, Verdict::Passed),
        (None, _) => (CheckState::Failed, Verdict::Failed),
    };
    if !current.may_become(next) {
        error!("check {check}: {current:?} cannot become {next:?}; reply ignored");
        return;
    }
    if let Err(e) = spawn_result(world, check, verdict, output) {
        error!("check {check}: result not recorded: {e}");
        return;
    }
    let mut entity = world.entity_mut(check);
    entity.insert(next);
    if let Some(problem) = problem {
        entity.insert(bounded_problem(problem));
    }
    let generation = next_generation(world);
    artifacts::capture(world, check, next, generation);
    trigger_check_changed(world, check, next);
}

/// Collected `Inbound::CheckDone` replies of this update (`Phase::Ingest` → `Completions`).
#[derive(Resource, Default, Debug)]
struct Completions(Vec<Done>);

#[derive(Debug)]
struct Done {
    check: Entity,
    code: Option<i32>,
    output: OutputTail,
    problem: Option<String>,
}

fn ingest(mut inbound: MessageReader<Inbound>, mut completions: ResMut<Completions>) {
    for message in inbound.read() {
        if let Inbound::CheckDone {
            check,
            code,
            stdout,
            stderr,
            truncated,
            problem,
        } = message
        {
            completions.0.push(Done {
                check: *check,
                code: *code,
                output: OutputTail {
                    exit_code: *code,
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                    truncated: *truncated,
                },
                problem: problem.clone(),
            });
        }
    }
}

fn complete(world: &mut World) {
    let done = core::mem::take(&mut world.resource_mut::<Completions>().0);
    for reply in done {
        if world.get::<Check>(reply.check).is_none() {
            continue;
        }
        finish(
            world,
            reply.check,
            reply.code,
            reply.output,
            reply.problem.as_deref(),
        );
    }
}

/// `Last`, before the runner drains effects: every `Queued` check whose intent the journal
/// already committed becomes `Running` with its `Effect::RunCheck`; a refused or pending
/// commit leaves it queued (invariant 13).
fn dispatch(world: &mut World) {
    if world
        .resource::<bevy_state::prelude::State<ServerMode>>()
        .get()
        == &ServerMode::ShuttingDown
    {
        return;
    }
    if !journal_committed(world) {
        return;
    }
    let queued: Vec<(Entity, CheckCommand, String)> = world
        .query_filtered::<(Entity, &CheckState, &CheckCommand, &CheckCwd), With<Check>>()
        .iter(world)
        .filter(|(_, state, _, _)| **state == CheckState::Queued)
        .map(|(e, _, command, cwd)| (e, command.clone(), cwd.0.clone()))
        .collect();
    for (check, command, cwd) in queued {
        world.entity_mut(check).insert(CheckState::Running);
        world.write_message(Effect::RunCheck {
            check,
            argv: command.argv,
            cwd,
            timeout_ms: command.timeout_ms,
        });
        trigger_check_changed(world, check, CheckState::Running);
    }
    sources::dispatch(world);
}

/// Whether the last update's intents reached the journal: nothing dirty and the file writable.
pub(crate) fn journal_committed(world: &World) -> bool {
    world
        .get_resource::<Journal>()
        .is_none_or(|j| !j.is_dirty() && !j.is_frozen())
}

/// `PostStartup`, after the journal restored: retained `Queued|Running` checks become
/// `Uncertain` in one transaction (invariant 27), resolving sources are lost, reserved captures
/// of those checks fail, and every seal is re-validated (RESULTS.md:71).
fn recover(world: &mut World) -> Result<(), BevyError> {
    let issued = world
        .query::<&CreatedGeneration>()
        .iter(world)
        .map(|g| g.0)
        .max()
        .unwrap_or(0);
    world.resource_mut::<IssuedGeneration>().0 = issued;
    let unresolved: Vec<Entity> = world
        .query_filtered::<(Entity, &CheckState), With<Check>>()
        .iter(world)
        .filter(|(_, s)| matches!(s, CheckState::Queued | CheckState::Running))
        .map(|(e, _)| e)
        .collect();
    for check in unresolved {
        finish(
            world,
            check,
            None,
            OutputTail::default(),
            Some("unresolved when the service restarted; the command is never replayed"),
        );
    }
    sources::recover(world);
    let sealed: Vec<Entity> = world
        .query_filtered::<Entity, (With<Task>, With<Seal>)>()
        .iter(world)
        .collect();
    for task in sealed {
        if let Err(reason) = validate_seal(world, task) {
            return Err(BevyError::from(format!(
                "journal: seal of task {task} is invalid: {reason}"
            )));
        }
    }
    Ok(())
}

pub struct ChecksPlugin;

impl Plugin for ChecksPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<IssuedGeneration>()
            .init_resource::<Completions>()
            .init_resource::<sources::Resolutions>()
            .init_resource::<crate::git::GitOps>()
            .register_type::<CheckPolicy>()
            .register_type::<ArtifactPolicy>()
            .register_type::<CaptureRequests>()
            .register_type::<CheckCwd>()
            .register_type::<CapturedBy>()
            .register_type::<ArtifactBytes>()
            .register_type::<SourceState>()
            .add_systems(PostStartup, recover)
            .add_systems(First, ingest.in_set(Phase::Ingest))
            .add_systems(
                PreUpdate,
                (complete, sources::resolve)
                    .chain()
                    .in_set(Phase::Completions),
            )
            .add_systems(Last, dispatch.in_set(Phase::Effects));
    }
}
