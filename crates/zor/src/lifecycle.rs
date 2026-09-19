//! Task/attempt/prompt lifecycle (TASKS.md, RECOVERY.md, `docs/zor-lifecycle-transitions.md`;
//! `docs/model.md` invariants 3, 5-7, 12-20, 27, 28).
//!
//! Every intent is a typed transition on the World: it validates, commits the record (an
//! Operation in `Submitting`, a prompt in `Prepared`, a `StopRequested` marker) and only then
//! writes the `Effect::FuxCall` that acts on fux. The journal commits in `PostUpdate` and the
//! runner drains effects in `Last`, so the record is durable before the call leaves the
//! process (invariant 13). Replies and `fux/events+watch` items come back as `Inbound` messages
//! and are applied in `Phase::Completions` ([`calls`]); wait outcomes and task states derive from
//! them in `Phase::Lifecycle` ([`waits`]). A lost reply leaves the record `Uncertain`; nothing
//! is ever resent under a new identity (15). `Attempt.state` is written only by the shared
//! attached-observation transition [`observe`] (28).

pub mod calls;
pub mod recovery;
pub mod resume;
pub mod waits;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use bevy_time::{Timer, TimerMode};
use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::providers::{Provider, ProviderKind};
use crate::remote::events::{AttemptChanged, PromptChanged, TaskClosed, TaskOpened};

/// Tag in the high byte of every `Effect::FuxCall.call` this module issues (providers use
/// `0x03 << 56`).
pub const CALL_TAG: u64 = 0x01 << 56;
/// Prompt timeouts are 1 ms through 24 hours (TASKS.md:178).
pub const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
/// The fux-escaped prompt plus Enter must fit 64 KiB (TASKS.md:179-180).
pub const MAX_KEYS_BYTES: usize = 64 * 1024;
/// fux retains input receipts for 60 s from reservation (TASKS.md:219).
pub const RECEIPT_RETAIN_MS: u64 = 60_000;
/// Heartbeat period of a live attempt: receipt status and capture refreshes.
pub const HEARTBEAT_MS: u64 = 1_000;
/// The environment variable carrying the launch marker (TASKS.md:123).
pub const LAUNCH_ID_VAR: &str = "ZOR_LAUNCH_ID";
/// The recovery wrapper: `/usr/bin/env -- ZOR_LAUNCH_ID=<marker> argv...` runs the supplied
/// argv without a zor PTY and leaves the marker visible in `fux/workspace.list` (TASKS.md:123-124).
pub const ENV_BINARY: &str = "/usr/bin/env";

// ---------------------------------------------------------------------------------------------
// Errors and specs
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    Model(ModelError),
    /// An id names nothing.
    NotFound(String),
    /// Different intent under a retained id (invariant 14).
    Conflict(String),
    /// Well-formed but not applicable in the current state.
    Refused(String),
    /// The outcome of an earlier mutation is unknown; reconcile or abandon first.
    Uncertain(String),
}

impl core::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Conflict(why) => write!(f, "conflicting intent: {why}"),
            Self::Refused(why) => f.write_str(why),
            Self::Uncertain(why) => write!(f, "uncertain: {why}"),
        }
    }
}

impl core::error::Error for LifecycleError {}

impl From<ModelError> for LifecycleError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

type Res<T> = Result<T, LifecycleError>;

fn refused<T>(why: impl Into<String>) -> Res<T> {
    Err(LifecycleError::Refused(why.into()))
}

fn conflict<T>(why: impl Into<String>) -> Res<T> {
    Err(LifecycleError::Conflict(why.into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    pub location: Location,
}

/// A configured integration (INTEGRATIONS.md): the sidecar command Providers spawns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub kind: ProviderKind,
    #[serde(default)]
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The caller-chosen operation id (shared namespace with prompts).
    pub operation: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The fux workspace: an existing one gets a new root (`fux/root.new`); with `ephemeral`
    /// the workspace itself is created (`fux/workspace.new`) and killed after the attempt.
    pub workspace: String,
    pub ephemeral: bool,
    pub integration: Option<Integration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSpec {
    pub id: String,
    pub text: String,
    pub timeout_ms: u64,
}

// ---------------------------------------------------------------------------------------------
// Components and resources
// ---------------------------------------------------------------------------------------------

/// Immutable launch intent on a Launch operation (TASKS.md:122-127): what was asked of fux,
/// compared on retry (invariant 14) and matched by the recovery sweep.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct LaunchTemplate {
    /// The wrapped command as sent: `/usr/bin/env -- ZOR_LAUNCH_ID=<marker> argv...`.
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub workspace: String,
    pub stream: String,
    pub ephemeral: bool,
    pub integration: Option<String>,
}

/// A `fux/*` call in flight for this record; never persisted, so a restart leaves nothing
/// pending and the record is reconciled instead of resent.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCall(pub u64);

/// `fux/pane.close` was accepted for this attempt; the heartbeat now asks for final evidence.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CloseSent;

/// fux announced the pane's exit or closure; `fux/pane.final` is due.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AwaitFinal;

/// The newest capture sequence seen for a live attempt (TASKS.md:322-323).
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LastCapture {
    pub seq: u64,
}

/// Per-live-attempt heartbeat.
#[derive(Component, Debug)]
pub struct Heartbeat(pub Timer);

impl Default for Heartbeat {
    fn default() -> Self {
        Self(Timer::new(
            core::time::Duration::from_millis(HEARTBEAT_MS),
            TimerMode::Repeating,
        ))
    }
}

/// What the events consumer last told us about fux: the instance nonce of the connected
/// stream, or `None` while the link is down.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Link {
    pub instance: Option<String>,
    /// Set by recovery: the one startup sweep of abandoned launches is still due (27).
    pub sweep_due: bool,
}

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

/// The task's attempt outside `Finished`, if any (invariant 3 makes it unique).
pub fn attempt_of(world: &World, task: Entity) -> Option<Entity> {
    world.get::<Attempts>(task)?.iter().find(|a| {
        world
            .get::<AttemptState>(*a)
            .is_some_and(|s| *s != AttemptState::Finished)
    })
}

pub fn pane_handle(world: &World, attempt: Entity) -> Option<PaneHandle> {
    world.get::<PaneHandle>(attempt).cloned()
}

/// The task an attempt belongs to.
pub fn task_of(world: &World, attempt: Entity) -> Option<Entity> {
    world.get::<AttemptOf>(attempt).map(|a| a.0)
}

/// The Launch operation of an attempt.
pub fn launch_of(world: &World, attempt: Entity) -> Option<Entity> {
    world
        .get::<Operations>(attempt)?
        .iter()
        .find(|op| world.get::<OperationKind>(*op) == Some(&OperationKind::Launch))
}

fn now(world: &World) -> u64 {
    world.resource::<Clock>().now_ms
}

fn task_state(world: &World, task: Entity) -> Res<TaskState> {
    world
        .get::<TaskState>(task)
        .copied()
        .ok_or_else(|| LifecycleError::NotFound(format!("task {task}")))
}

fn attempt_state(world: &World, attempt: Entity) -> Res<AttemptState> {
    world
        .get::<AttemptState>(attempt)
        .copied()
        .ok_or_else(|| LifecycleError::NotFound(format!("attempt {attempt}")))
}

fn same_pane(a: &PaneHandle, b: &PaneHandle) -> bool {
    a.instance == b.instance && a.pane == b.pane
}

/// Writer exclusion on a pane (TASKS.md:184-185, 336, 404-405): a prompt holds it until it is
/// released, or until its delivery is settled and its wait ended in a response, an exit or a
/// needs-input claim. A timed-out or uncertain prompt keeps it until explicit abandonment.
pub fn holds_exclusion(delivery: Delivery, wait: WaitState) -> bool {
    delivery != Delivery::Released
        && (delivery.is_pending()
            || !wait.is_terminal()
            || matches!(wait, WaitState::TimedOut | WaitState::Uncertain))
}

fn effect(world: &mut World, effect: Effect) {
    world.resource_mut::<Messages<Effect>>().write(effect);
}

// ---------------------------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------------------------

/// Records a task (TASKS.md:66). An identical retry returns the record; a different title or
/// location under the same id fails (invariant 14).
pub fn create_task(world: &mut World, spec: TaskSpec) -> Res<Entity> {
    if let Some(existing) = world.resource::<Ids>().task(&spec.id) {
        let same = world
            .get::<Title>(existing)
            .is_some_and(|t| t.0 == spec.title)
            && world.get::<Location>(existing) == Some(&spec.location);
        return if same {
            Ok(existing)
        } else {
            conflict(format!(
                "task {} exists with another title or location",
                spec.id
            ))
        };
    }
    match &spec.location {
        Location::Cwd(path) if !path.starts_with('/') => {
            return refused("cwd must be an absolute path");
        }
        Location::Worktree(id) => {
            world
                .resource::<Ids>()
                .worktree(id)
                .ok_or_else(|| LifecycleError::NotFound(format!("worktree {id}")))?;
        }
        Location::Cwd(_) => {}
    }
    let created_ms = now(world);
    let task = spawn_task(
        world,
        crate::model::TaskSpec {
            id: &spec.id,
            title: &spec.title,
            location: spec.location,
            created_ms,
        },
    )?;
    let id = world
        .get::<TaskId>(task)
        .cloned()
        .unwrap_or(TaskId(String::new()));
    world.trigger(TaskOpened {
        entity: task,
        task: id,
    });
    Ok(task)
}

/// Cancels coordination (TASKS.md:394-399): the task closes `Cancelled`, every prompt of its
/// attempts is released, nothing is signalled and the attempt records stay. Already cancelled
/// returns the retained state; a verified task is never overwritten (invariant 23).
pub fn cancel_task(world: &mut World, task: Entity) -> Res<()> {
    match task_state(world, task)? {
        TaskState::Closed {
            outcome: TaskOutcome::Cancelled,
        } => return Ok(()),
        TaskState::Closed {
            outcome: TaskOutcome::Verified,
        } => return refused("a verified task is not cancelled"),
        _ => {}
    }
    if world.get::<Seal>(task).is_some() {
        return refused("a sealed task is not cancelled");
    }
    close(world, task, TaskOutcome::Cancelled)?;
    release_prompts(world, task);
    Ok(())
}

/// The only writer of `Closed{Verified}` (TASKS.md:189, RESULTS.md:69-70): the seal must name
/// one source of this task, only passed checks of this task on that source and only collected
/// artifacts of this task's attempts (invariant 4); the current attempt must not be `Uncertain`
/// or `Lost` and no prompt may still be `Reserved|Submitting|Uncertain`.
pub fn close_verified(world: &mut World, task: Entity, seal: Seal) -> Res<()> {
    if task_state(world, task)?.is_closed() {
        return refused("task is closed");
    }
    if world.get::<SourceOf>(seal.source).map(|s| s.0) != Some(task) {
        return refused("seal source is not this task's");
    }
    if seal.checks.is_empty() {
        return refused("seal selects no check");
    }
    for check in &seal.checks {
        if world.get::<CheckOf>(*check).map(|c| c.0) != Some(task) {
            return refused("sealed check is not this task's");
        }
        if world.get::<CheckState>(*check) != Some(&CheckState::Passed) {
            return refused("sealed check did not pass");
        }
        if world.get::<CheckOn>(*check).map(|c| c.0) != Some(seal.source) {
            return refused("sealed check ran on another source");
        }
    }
    for artifact in &seal.artifacts {
        let attempt = world.get::<ArtifactOf>(*artifact).map(|a| a.0);
        if attempt.and_then(|a| task_of(world, a)) != Some(task) {
            return refused("sealed artifact is not this task's");
        }
        if world.get::<ArtifactState>(*artifact) != Some(&ArtifactState::Collected) {
            return refused("sealed artifact was not collected");
        }
    }
    if let Some(attempt) = attempt_of(world, task) {
        let entity = world.entity(attempt);
        if entity.contains::<Uncertain>() || entity.contains::<Lost>() {
            return refused("the current attempt is uncertain or lost");
        }
    }
    let unsettled = prompts_of_task(world, task).into_iter().any(|p| {
        matches!(
            world.get::<Delivery>(p),
            Some(Delivery::Reserved | Delivery::Submitting | Delivery::Uncertain)
        )
    });
    if unsettled {
        return refused("a prompt is still reserved, submitting or uncertain");
    }
    world.entity_mut(task).insert(seal);
    if let Err(e) = close(world, task, TaskOutcome::Verified) {
        world.entity_mut(task).remove::<Seal>();
        return Err(e);
    }
    Ok(())
}

fn close(world: &mut World, task: Entity, outcome: TaskOutcome) -> Res<()> {
    let now_ms = now(world);
    close_task(world, task, outcome, now_ms)?;
    let id = world
        .get::<TaskId>(task)
        .cloned()
        .unwrap_or(TaskId(String::new()));
    world.trigger(TaskClosed {
        entity: task,
        task: id,
        outcome,
    });
    Ok(())
}

fn prompts_of_task(world: &World, task: Entity) -> Vec<Entity> {
    world
        .get::<Attempts>(task)
        .into_iter()
        .flat_map(|attempts| attempts.iter())
        .flat_map(|a| world.get::<Prompts>(a).into_iter().flat_map(|p| p.iter()))
        .collect()
}

fn release_prompts(world: &mut World, task: Entity) {
    for prompt in prompts_of_task(world, task) {
        release_prompt(world, prompt);
    }
}

/// Abandonment (TASKS.md:387-392): the wait becomes `Cancelled` unless already terminal, and
/// delivery is `Released` where its outcome is known. A `Submitting` prompt keeps that state
/// until its reply resolves it, then releases ([`calls`] finishes the job).
fn release_prompt(world: &mut World, prompt: Entity) {
    let Some(delivery) = world.get::<Delivery>(prompt).copied() else {
        return;
    };
    if let Some(wait) = world.get::<WaitState>(prompt).copied()
        && !wait.is_terminal()
    {
        world.entity_mut(prompt).insert(WaitState::Cancelled);
    }
    if delivery != Delivery::Released && delivery.may_become(Delivery::Released) {
        set_delivery(world, prompt, Delivery::Released);
    }
}

pub(crate) fn set_delivery(world: &mut World, prompt: Entity, next: Delivery) {
    if world.get::<Delivery>(prompt) == Some(&next) {
        return;
    }
    world.entity_mut(prompt).insert(next);
    let id = world
        .get::<PromptId>(prompt)
        .cloned()
        .unwrap_or(PromptId(String::new()));
    world.trigger(PromptChanged {
        entity: prompt,
        prompt: id,
        delivery: next,
    });
}

// ---------------------------------------------------------------------------------------------
// Attempts: launch, adopt, the shared observation transition
// ---------------------------------------------------------------------------------------------

/// A managed launch (TASKS.md:122-133): commits the Launch operation and a `Pending` attempt,
/// then moves them to `Submitting`/`Launching` and asks fux to create the pane in one update
/// (the journal commits before the effect drains). An identical retry under the operation id
/// returns the attempt; different arguments fail; a `Submitting`/`Uncertain` operation is never
/// resent (invariants 13-15).
pub fn launch(world: &mut World, task: Entity, mut spec: LaunchSpec) -> Res<Entity> {
    resume::capture_storage_env(world, &mut spec);
    let state = task_state(world, task)?;
    if state.is_closed() {
        return refused("task is closed");
    }
    if spec.argv.first().is_none_or(String::is_empty) {
        return refused("argv needs a non-empty executable");
    }
    if !valid_id(&spec.workspace) {
        return refused("workspace must be a valid fux workspace name");
    }
    let cwd = match world.get::<Location>(task) {
        Some(Location::Cwd(path)) => path.clone(),
        Some(Location::Worktree(id)) => {
            let worktree = world
                .resource::<Ids>()
                .worktree(id)
                .ok_or_else(|| LifecycleError::NotFound(format!("worktree {id}")))?;
            match crate::worktrees::path_of(world, worktree) {
                Some(path) => path.display().to_string(),
                None => return refused("worktree has no ready checkout"),
            }
        }
        None => return Err(LifecycleError::NotFound(format!("task {task}"))),
    };
    if let Some(existing) = world.resource::<Ids>().operation(&spec.operation) {
        return retry_launch(world, task, existing, &spec, &cwd);
    }
    if world.resource::<Ids>().prompt(&spec.operation).is_some() {
        return conflict(format!("{} is a prompt id", spec.operation));
    }
    if let Some(current) = attempt_of(world, task) {
        return refused(format!("task has a current attempt {current}"));
    }
    if world.resource::<Link>().instance.is_none() {
        return refused("fux is not connected; launch intent not accepted");
    }
    let marker = random_hex(32)?;
    let mut argv = Vec::with_capacity(spec.argv.len() + 3);
    argv.push(ENV_BINARY.to_owned());
    argv.push("--".to_owned());
    argv.push(format!("{LAUNCH_ID_VAR}={marker}"));
    argv.extend(spec.argv.iter().cloned());
    let template = LaunchTemplate {
        argv,
        cwd: cwd.clone(),
        env: spec.env.clone(),
        workspace: spec.workspace.clone(),
        stream: spec.operation.clone(),
        ephemeral: spec.ephemeral,
        integration: spec.integration.as_ref().map(|i| i.kind.name().to_owned()),
    };

    // Intent first: operation Prepared, attempt Pending.
    let attempt = spawn_attempt(
        world,
        AttemptSpec {
            task,
            ownership: Ownership::Managed,
            handle: PaneHandle {
                instance: String::new(),
                workspace: spec.workspace.clone(),
                stream: spec.operation.clone(),
                pane: 0,
                pid: None,
            },
        },
    )?;
    let operation = match spawn_operation(world, &spec.operation, attempt, OperationKind::Launch) {
        Ok(op) => op,
        Err(e) => {
            world.despawn(attempt);
            return Err(e.into());
        }
    };
    world.entity_mut(operation).insert(template.clone());
    let mut entity = world.entity_mut(attempt);
    entity.insert(LaunchMarker(marker));
    if let Some(integration) = &spec.integration {
        entity.insert(Provider {
            kind: integration.kind,
            argv: integration.argv.clone(),
            cwd: Some(cwd),
            env: spec.env.clone(),
        });
    }
    submit_launch(world, operation, attempt, &template);
    Ok(attempt)
}

fn retry_launch(
    world: &mut World,
    task: Entity,
    operation: Entity,
    spec: &LaunchSpec,
    cwd: &str,
) -> Res<Entity> {
    let attempt = world
        .get::<OperationOf>(operation)
        .map(|o| o.0)
        .ok_or_else(|| LifecycleError::NotFound(format!("operation {}", spec.operation)))?;
    if world.get::<OperationKind>(operation) != Some(&OperationKind::Launch)
        || task_of(world, attempt) != Some(task)
    {
        return conflict(format!(
            "{} is another kind of operation or belongs to another task",
            spec.operation
        ));
    }
    let Some(template) = world.get::<LaunchTemplate>(operation) else {
        return conflict(format!("{} has no launch template", spec.operation));
    };
    let same = template.argv.get(3..) == Some(spec.argv.as_slice())
        && template.cwd == cwd
        && template.env == spec.env
        && template.workspace == spec.workspace
        && template.ephemeral == spec.ephemeral
        && template.integration == spec.integration.as_ref().map(|i| i.kind.name().to_owned());
    if !same {
        return conflict(format!(
            "launch {} was recorded with different arguments",
            spec.operation
        ));
    }
    // A Prepared record (fux was down when it was committed and the call never left) may be
    // submitted now; anything past Prepared is reconciled, never resent.
    if world.get::<OperationPhase>(operation) == Some(&OperationPhase::Prepared)
        && world.get::<PendingCall>(operation).is_none()
        && world.resource::<Link>().instance.is_some()
    {
        let template = template.clone();
        submit_launch(world, operation, attempt, &template);
    }
    Ok(attempt)
}

fn submit_launch(world: &mut World, operation: Entity, attempt: Entity, template: &LaunchTemplate) {
    let template_spec = serde_json::json!({
        "argv": template.argv,
        "cwd": template.cwd,
        "env": template.env,
        "stream": template.stream,
    });
    let (method, params) = if template.ephemeral {
        (
            "fux/workspace.new",
            serde_json::json!({
                "name": template.workspace,
                "template": template_spec,
                "_expected_instance": world.resource::<Link>().instance,
            }),
        )
    } else {
        (
            "fux/root.new",
            serde_json::json!({
                "workspace": template.workspace,
                "name": template.stream,
                "template": template_spec,
                "_expected_instance": world.resource::<Link>().instance,
            }),
        )
    };
    world
        .entity_mut(operation)
        .insert(OperationPhase::Submitting);
    observe(world, attempt, Observed::Submitted);
    let call = calls::call(world, calls::CallKind::Launch, operation, method, params);
    world.entity_mut(operation).insert(PendingCall(call));
}

/// Adopts an existing pane (TASKS.md:18-38, 68-69): an observation-only attempt on the given
/// handle, validated against fux's listing before it goes `Live`. Grants no stop, cleanup or
/// check authority (invariant 17). Repeating the same handle returns the attempt.
pub fn adopt(world: &mut World, task: Entity, handle: PaneHandle) -> Res<Entity> {
    if task_state(world, task)?.is_closed() {
        return refused("task is closed");
    }
    if handle.instance.is_empty() || handle.pane == 0 || handle.workspace.is_empty() {
        return refused("adoption needs the fux instance, workspace and pane");
    }
    if let Some(current) = attempt_of(world, task) {
        let same = world.get::<Ownership>(current) == Some(&Ownership::Adopted)
            && world
                .get::<PaneHandle>(current)
                .is_some_and(|h| same_pane(h, &handle) && h.workspace == handle.workspace);
        return if same {
            Ok(current)
        } else {
            refused(format!("task has a current attempt {current}"))
        };
    }
    let taken = world
        .query_filtered::<(&PaneHandle, &AttemptState, &Ownership), With<Attempt>>()
        .iter(world)
        .any(|(h, s, o)| {
            *s != AttemptState::Finished && same_pane(h, &handle) && *o == Ownership::Managed
        });
    if taken {
        return refused("adoption never reuses a managed session");
    }
    match world.resource::<Link>().instance.as_deref() {
        Some(instance) if instance == handle.instance => {}
        Some(_) => return refused("fux is another incarnation than the handle names"),
        None => return refused("fux is not connected; adoption not validated"),
    }
    let attempt = spawn_attempt(
        world,
        AttemptSpec {
            task,
            ownership: Ownership::Adopted,
            handle,
        },
    )?;
    observe(world, attempt, Observed::Submitted);
    calls::request_listing(world);
    Ok(attempt)
}

/// One fact about an attempt's pane, applied by [`observe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// Creation was sent (or adoption validation started): `Pending → Launching`.
    Submitted,
    /// fux accepted creation and returned the pane id.
    Created { pane: u64 },
    /// The pane's process is running (`PaneSpawned` or a live listing entry).
    Live { pid: Option<u32> },
    /// Authenticated final evidence (`fux/pane.final`).
    Final(FinalEvidence),
    /// Live/final evidence is unavailable right now (TASKS.md:145-146).
    Missing(String),
    /// The endpoint proved to be another fux incarnation (RECOVERY.md:98-102).
    Replaced(String),
    /// Stop intent committed (TASKS.md:153-157): `Live → Finishing`.
    Stopping,
}

/// The shared attached-observation transition (lifecycle-transitions.md:55-60): the only
/// writer of `AttemptState`. `Finished` comes only from final evidence and is terminal; `Lost`
/// persists through later outages and never sits on a finished attempt (invariant 5).
pub fn observe(world: &mut World, attempt: Entity, fact: Observed) {
    let Some(state) = world.get::<AttemptState>(attempt).copied() else {
        return;
    };
    if state == AttemptState::Finished {
        return;
    }
    match fact {
        Observed::Submitted => {
            set_attempt_state(world, attempt, AttemptState::Launching);
            let mut entity = world.entity_mut(attempt);
            if !entity.contains::<Heartbeat>() {
                entity.insert(Heartbeat::default());
            }
        }
        Observed::Created { pane } => {
            let mut handle = world
                .get::<PaneHandle>(attempt)
                .cloned()
                .unwrap_or_default();
            if handle.pane == 0 {
                handle.pane = pane;
                // The handle names the instance that answered (service-ownership-contract.md:21).
                if handle.instance.is_empty()
                    && let Some(instance) = world.resource::<Link>().instance.clone()
                {
                    handle.instance = instance;
                }
                world.entity_mut(attempt).insert(handle);
            }
            set_attempt_state(world, attempt, AttemptState::Launching);
        }
        Observed::Live { pid } => {
            let mut handle = world
                .get::<PaneHandle>(attempt)
                .cloned()
                .unwrap_or_default();
            if handle.pid.is_none() && pid.is_some() {
                handle.pid = pid;
                world.entity_mut(attempt).insert(handle);
            }
            let mut entity = world.entity_mut(attempt);
            entity.remove::<(Uncertain, Problem)>();
            if !entity.contains::<Heartbeat>() {
                entity.insert(Heartbeat::default());
            }
            if state == AttemptState::Launching {
                set_attempt_state(world, attempt, AttemptState::Live);
                if let Some(op) = launch_of(world, attempt) {
                    set_phase(world, op, OperationPhase::Attached);
                }
            }
        }
        Observed::Final(evidence) => {
            world.entity_mut(attempt).insert(evidence);
            world.entity_mut(attempt).remove::<(
                Uncertain,
                Lost,
                Problem,
                NeedsInput,
                Heartbeat,
                PendingCall,
                CloseSent,
                AwaitFinal,
            )>();
            if state == AttemptState::Pending {
                set_attempt_state(world, attempt, AttemptState::Launching);
            }
            set_attempt_state(world, attempt, AttemptState::Finished);
            for op in world
                .get::<Operations>(attempt)
                .map(|ops| ops.iter().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if world.get::<OperationKind>(op) == Some(&OperationKind::Launch) {
                    set_phase(world, op, OperationPhase::Closed);
                }
            }
            calls::after_finished(world, attempt);
        }
        Observed::Missing(problem) => {
            world
                .entity_mut(attempt)
                .insert((Uncertain, Problem(clip(problem))));
        }
        Observed::Replaced(problem) => {
            world
                .entity_mut(attempt)
                .insert((Lost, Problem(clip(problem))));
        }
        Observed::Stopping => {
            set_attempt_state(world, attempt, AttemptState::Finishing);
        }
    }
}

fn set_attempt_state(world: &mut World, attempt: Entity, next: AttemptState) {
    let Some(current) = world.get::<AttemptState>(attempt).copied() else {
        return;
    };
    if current == next || !current.may_become(next) {
        return;
    }
    world.entity_mut(attempt).insert(next);
    let id = world
        .get::<AttemptId>(attempt)
        .copied()
        .unwrap_or(AttemptId(0));
    world.trigger(AttemptChanged {
        entity: attempt,
        attempt: id,
        state: next,
    });
}

pub(crate) fn set_phase(world: &mut World, operation: Entity, next: OperationPhase) {
    let Some(current) = world.get::<OperationPhase>(operation).copied() else {
        return;
    };
    if current != next && current.may_become(next) {
        world.entity_mut(operation).insert(next);
    }
}

/// Bounded diagnostic text.
pub(crate) fn clip(mut text: String) -> String {
    const MAX: usize = 512;
    if text.len() > MAX {
        let mut cut = MAX;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

fn random_hex(chars: usize) -> Res<String> {
    let mut hex = ::fux::attach::random_hex256()
        .map_err(|e| LifecycleError::Refused(format!("no randomness: {e}")))?;
    hex.truncate(chars);
    Ok(hex)
}

/// One read-only receipt reconciliation (TASKS.md:210, 314-317): asks fux for the retained
/// operation's status when nothing is in flight. Never reserves or submits.
pub fn refresh_receipt(world: &mut World, prompt: Entity) {
    if world.get::<PendingCall>(prompt).is_some() {
        return;
    }
    let Some(receipt) = world.get::<Receipt>(prompt).cloned() else {
        return;
    };
    if world.resource::<Link>().instance.as_deref() != Some(receipt.instance.as_str()) {
        return;
    }
    if matches!(
        world.get::<Delivery>(prompt),
        Some(Delivery::Submitting | Delivery::Uncertain | Delivery::Delivered)
    ) {
        calls::request_status(world, prompt, receipt.operation);
    }
}

// ---------------------------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------------------------

/// Prepares a prompt (TASKS.md:176-185): text, deadline and a report token, `Prepared`, no
/// receipt, no terminal input. One pending prompt per pane identity across tasks (invariant
/// 6); an identical retry returns the record; a finished attempt or a closed task refuses.
pub fn prepare_prompt(world: &mut World, attempt: Entity, spec: PromptSpec) -> Res<Entity> {
    let state = attempt_state(world, attempt)?;
    let task = task_of(world, attempt).ok_or_else(|| LifecycleError::NotFound("task".into()))?;
    if let Some(existing) = world.resource::<Ids>().prompt(&spec.id) {
        let same = world.get::<PromptOf>(existing).map(|p| p.0) == Some(attempt)
            && world
                .get::<PromptText>(existing)
                .is_some_and(|t| t.0 == spec.text);
        return if same {
            Ok(existing)
        } else {
            conflict(format!(
                "prompt {} exists with other text or target",
                spec.id
            ))
        };
    }
    if world.resource::<Ids>().operation(&spec.id).is_some() {
        return conflict(format!("{} is an operation id", spec.id));
    }
    if task_state(world, task)?.is_closed() {
        return refused("task is closed: no new preparation");
    }
    if state != AttemptState::Live {
        return refused(format!("attempt is {state:?}; prompts need a live attempt"));
    }
    validate_text(&spec.text)?;
    if !(1..=MAX_TIMEOUT_MS).contains(&spec.timeout_ms) {
        return refused("timeout must be 1 ms through 24 hours");
    }
    let handle = pane_handle(world, attempt).unwrap_or_default();
    let holder = world
        .query_filtered::<(Entity, &PromptOf, &Delivery, &WaitState), With<Prompt>>()
        .iter(world)
        .filter(|(_, _, d, w)| holds_exclusion(**d, **w))
        .find(|(_, of, _, _)| {
            world
                .get::<PaneHandle>(of.0)
                .is_some_and(|h| same_pane(h, &handle))
        })
        .map(|(e, _, _, _)| e);
    if let Some(holder) = holder {
        let id = world
            .get::<PromptId>(holder)
            .cloned()
            .unwrap_or(PromptId(String::new()));
        return refused(format!(
            "pane {}/{} already has pending prompt {id}",
            handle.instance, handle.pane
        ));
    }
    let deadline_ms = now(world).saturating_add(spec.timeout_ms);
    let token = random_hex(32)?;
    let prompt = spawn_prompt(
        world,
        crate::model::PromptSpec {
            id: &spec.id,
            attempt,
            text: &spec.text,
            deadline_ms,
            report_token: &token,
        },
    )?;
    let id = world
        .get::<PromptId>(prompt)
        .cloned()
        .unwrap_or(PromptId(String::new()));
    world.trigger(PromptChanged {
        entity: prompt,
        prompt: id,
        delivery: Delivery::Prepared,
    });
    Ok(prompt)
}

fn validate_text(text: &str) -> Res<()> {
    if text.is_empty() {
        return refused("prompt text is empty");
    }
    if text.chars().any(char::is_control) {
        return refused("prompt text must be one line without control characters");
    }
    if keys_for(text).len() > MAX_KEYS_BYTES {
        return refused("prompt text exceeds 64 KiB once escaped");
    }
    Ok(())
}

/// fux escape notation for the prompt: literal backslashes doubled, one carriage return
/// appended (TASKS.md:180).
pub fn keys_for(text: &str) -> String {
    let mut keys = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if c == '\\' {
            keys.push('\\');
        }
        keys.push(c);
    }
    keys.push_str("\\r");
    keys
}

/// Submits a prepared prompt (TASKS.md:202-217): validates the target, then asks fux to
/// reserve; the reserve reply records the receipt (`Reserved`), arms the provider and sends
/// `input.submit` (`Submitting`); that reply records `Delivered`, `Failed` or `Uncertain`.
/// A call already in flight, or an `Uncertain` record, is never doubled: the heartbeat's
/// status refresh resolves it or `abandon` releases it.
pub fn submit_prompt(world: &mut World, prompt: Entity) -> Res<()> {
    let delivery = world
        .get::<Delivery>(prompt)
        .copied()
        .ok_or_else(|| LifecycleError::NotFound(format!("prompt {prompt}")))?;
    let attempt = world
        .get::<PromptOf>(prompt)
        .map(|p| p.0)
        .ok_or_else(|| LifecycleError::NotFound("attempt".into()))?;
    if world.get::<PendingCall>(prompt).is_some() {
        return Ok(());
    }
    match delivery {
        Delivery::Prepared | Delivery::Reserved => {}
        Delivery::Submitting | Delivery::Uncertain => {
            return Err(LifecycleError::Uncertain(
                "input may have been written; reconcile the receipt or abandon".into(),
            ));
        }
        Delivery::Delivered => return Ok(()),
        Delivery::Failed => return refused("delivery failed; abandon and prepare a new prompt"),
        Delivery::Released => return refused("prompt was released"),
    }
    if world.get::<WaitState>(prompt) == Some(&WaitState::Cancelled) {
        return refused("prompt coordination is cancelled");
    }
    let task = task_of(world, attempt).ok_or_else(|| LifecycleError::NotFound("task".into()))?;
    if task_state(world, task)?.is_closed() {
        return refused("task is closed");
    }
    if attempt_state(world, attempt)? != AttemptState::Live {
        return refused("attempt is not live");
    }
    let handle = pane_handle(world, attempt).unwrap_or_default();
    if handle.instance.is_empty() || handle.pane == 0 {
        return refused("attempt has no pane identity yet");
    }
    match world.resource::<Link>().instance.as_deref() {
        Some(instance) if instance == handle.instance => {}
        Some(_) => return refused("fux is another incarnation than the recorded target"),
        None => return refused("fux is not connected"),
    }
    let deadline = world.get::<Deadline>(prompt).map_or(0, |d| d.0);
    if now(world) >= deadline {
        return refused("prompt deadline passed");
    }
    if delivery == Delivery::Reserved {
        // A retained reservation: continue with the submit step.
        let operation = world.get::<Receipt>(prompt).map_or(0, |r| r.operation);
        calls::submit_input(world, prompt, operation);
        return Ok(());
    }
    let call = calls::call(
        world,
        calls::CallKind::Reserve,
        prompt,
        "fux/input.reserve",
        serde_json::json!({ "pane": handle.pane, "retain_ms": RECEIPT_RETAIN_MS }),
    );
    world.entity_mut(prompt).insert(PendingCall(call));
    Ok(())
}

/// Releases a prompt's coordination (TASKS.md:387-392) while retaining its evidence. Already
/// released returns the retained state.
pub fn abandon_prompt(world: &mut World, prompt: Entity) -> Res<()> {
    if world.get::<Prompt>(prompt).is_none() {
        return Err(LifecycleError::NotFound(format!("prompt {prompt}")));
    }
    release_prompt(world, prompt);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Stop
// ---------------------------------------------------------------------------------------------

/// Requests a managed stop (TASKS.md:153-162): commits `StopRequested` and cancellation (a
/// verified outcome is kept), moves the attempt to `Finishing`, then validates the recorded
/// identity against fux's listing before `fux/pane.close`. Success is only final evidence
/// (invariant 19); a retry under the same task re-validates and re-asks, never another pane.
pub fn request_stop(world: &mut World, task: Entity) -> Res<()> {
    let state = task_state(world, task)?;
    let Some(attempt) = attempt_of(world, task) else {
        return refused("task has no current attempt");
    };
    if world.get::<Ownership>(attempt) != Some(&Ownership::Managed) {
        return refused("adoption grants no stop authority");
    }
    let attempt_state = attempt_state(world, attempt)?;
    if matches!(
        attempt_state,
        AttemptState::Pending | AttemptState::Launching
    ) {
        return refused("launch is not reconciled; stop needs a live pane identity");
    }
    let handle = pane_handle(world, attempt).unwrap_or_default();
    if handle.pane == 0 || handle.instance.is_empty() {
        return refused("attempt has no pane identity");
    }
    if world.get::<Lost>(attempt).is_some() {
        return refused("the endpoint was replaced; no replacement pane is closed");
    }
    if world.get::<StopRequested>(task).is_none() {
        world.entity_mut(task).insert(StopRequested);
        if !state.is_closed() {
            close(world, task, TaskOutcome::Cancelled)?;
        }
        release_prompts(world, task);
        observe(world, attempt, Observed::Stopping);
    }
    if world.get::<PendingCall>(attempt).is_some() {
        return Ok(());
    }
    match world.resource::<Link>().instance.as_deref() {
        Some(instance) if instance == handle.instance => {}
        Some(_) => return refused("fux is another incarnation than the recorded target"),
        None => return refused("fux is not connected; stop intent retained"),
    }
    world.entity_mut(attempt).remove::<CloseSent>();
    calls::request_listing(world);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

pub struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<LaunchTemplate>()
            .register_type::<resume::ResumeIntent>()
            .init_resource::<Link>()
            .init_resource::<calls::Calls>()
            .add_systems(PostStartup, recovery::recover)
            .add_systems(PreUpdate, calls::ingest.in_set(Phase::Completions))
            .add_systems(
                Update,
                (
                    calls::heartbeat,
                    waits::needs_input,
                    waits::resolve_waits,
                    waits::derive_task_states,
                )
                    .chain()
                    .in_set(Phase::Lifecycle),
            );
    }
}
