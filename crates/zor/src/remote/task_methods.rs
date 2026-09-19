//! `zor/task.*`, `zor/attempt.*`, `zor/prompt.*` (milestone 6, owner TaskLifecycle). Handlers
//! open the envelope, resolve public ids and call the typed transitions of `crate::lifecycle`;
//! records carry public ids and retained evidence only.

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};

use super::methods::{
    MethodSpec, NoParams, Request, codes, described, error, handler, invalid, spec, to_value,
};
use crate::journal::Journal;
use crate::lifecycle::{
    self, Integration, LaunchSpec, LaunchTemplate, LifecycleError, PromptSpec, TaskSpec,
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
    /// Exact identity observed by a controller before a mutation.
    pub struct PaneGuard {
        pub instance: String,
        pub workspace: String,
        pub pane: u64,
        pub pid: Option<u32>,
    }
);
described!(
    pub struct TaskGuard {
        pub attempt: Option<u64>,
        pub pane: Option<PaneGuard>,
    }
);
described!(
    pub struct TaskMutationParams {
        pub task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub guard: Option<TaskGuard>,
    }
);
described!(
    /// Exactly one of `cwd` (absolute) or `worktree` (a zor-owned worktree id).
    pub struct TaskCreateParams {
        pub task: String,
        pub title: String,
        #[serde(default)]
        pub cwd: Option<String>,
        #[serde(default)]
        pub worktree: Option<String>,
    }
);
described!(
    pub struct TaskLaunchParams {
        pub task: String,
        pub operation: String,
        pub argv: Vec<String>,
        #[serde(default)]
        pub env: Vec<(String, String)>,
        pub workspace: String,
        /// Create `workspace` for this launch and kill it once the attempt finished.
        #[serde(default)]
        pub ephemeral: bool,
        #[serde(default)]
        pub integration: Option<Integration>,
    }
);
described!(
    pub struct TaskAdoptParams {
        pub task: String,
        pub fux_instance: String,
        pub workspace: String,
        #[serde(default)]
        pub stream: Option<String>,
        pub pane: u64,
        #[serde(default)]
        pub pid: Option<u32>,
    }
);
described!(
    /// Prepares `prompt` on the task's current attempt and, unless `submit` is false, submits it.
    pub struct TaskPromptParams {
        pub task: String,
        pub prompt: String,
        pub text: String,
        pub timeout_ms: u64,
        #[serde(default = "default_true")]
        pub submit: bool,
    }
);
described!(
    pub struct PromptParams {
        pub prompt: String,
    }
);
described!(
    pub struct AttemptListParams {
        #[serde(default)]
        pub task: Option<String>,
    }
);
described!(
    pub struct AttemptParams {
        pub attempt: u64,
    }
);

described!(
    pub struct OperationRecord {
        pub operation: String,
        pub kind: OperationKind,
        pub phase: OperationPhase,
        pub launch: Option<LaunchTemplate>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct AttemptRecord {
        pub attempt: u64,
        pub task: String,
        pub ownership: Ownership,
        pub state: AttemptState,
        pub instance: String,
        pub workspace: String,
        pub stream: String,
        pub pane: u64,
        pub pid: Option<u32>,
        pub marker: Option<String>,
        pub uncertain: bool,
        pub lost: bool,
        pub needs_input: bool,
        pub problem: Option<String>,
        #[serde(rename = "final")]
        pub final_evidence: Option<FinalEvidence>,
        pub operations: Vec<OperationRecord>,
    }
);
described!(
    pub struct PromptRecord {
        pub prompt: String,
        pub attempt: u64,
        pub task: String,
        pub text: String,
        pub deadline_ms: u64,
        pub delivery: Delivery,
        pub wait: WaitState,
        pub receipt: Option<Receipt>,
        pub response: Option<ResponseEvent>,
        pub binding: Option<Binding>,
        pub problem: Option<String>,
    }
);
described!(
    pub struct TaskSummary {
        pub task: String,
        pub title: String,
        pub state: TaskState,
        pub attempt: Option<u64>,
    }
);
described!(
    pub struct TaskList {
        pub tasks: Vec<TaskSummary>,
    }
);
described!(
    pub struct AttemptList {
        pub attempts: Vec<AttemptRecord>,
    }
);
described!(
    /// The full task record; from the archive only `task`, `live`, `archive` and `state` are
    /// known.
    pub struct TaskInspect {
        pub task: String,
        pub live: bool,
        pub archive: Option<String>,
        pub state: Option<String>,
        pub title: Option<String>,
        pub location: Option<Location>,
        pub created_ms: Option<u64>,
        pub closed_ms: Option<u64>,
        pub stop_requested: bool,
        pub sealed: bool,
        pub attempts: Vec<AttemptRecord>,
        pub prompts: Vec<PromptRecord>,
    }
);

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------------------------

fn lifecycle_error(e: LifecycleError) -> BrpError {
    match e {
        LifecycleError::NotFound(_) => error(codes::NOT_FOUND, e.to_string()),
        LifecycleError::Uncertain(_) => error(codes::UNCERTAIN, e.to_string()),
        LifecycleError::Model(_) | LifecycleError::Conflict(_) | LifecycleError::Refused(_) => {
            invalid(e.to_string())
        }
    }
}

fn task_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .task(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("task {id} not found")))
}

/// Check the controller's observation in the same World transaction as the mutation.
/// A present guard with no attempt means "still no live attempt", not a wildcard.
pub(super) fn validate_guard(
    world: &World,
    task: Entity,
    guard: Option<&TaskGuard>,
) -> Result<(), BrpError> {
    let Some(guard) = guard else {
        return Ok(());
    };
    let current = lifecycle::attempt_of(world, task);
    let attempt = current.and_then(|entity| world.get::<AttemptId>(entity)).map(|id| id.0);
    if attempt != guard.attempt {
        return Err(invalid("task attempt changed since the controller observation"));
    }
    if let Some(expected) = &guard.pane {
        let handle = current.and_then(|entity| world.get::<PaneHandle>(entity));
        if !handle.is_some_and(|actual| {
            actual.instance == expected.instance
                && actual.workspace == expected.workspace
                && actual.pane == expected.pane
                && actual.pid == expected.pid
        }) {
            return Err(invalid("task pane identity changed since the controller observation"));
        }
    }
    Ok(())
}

fn prompt_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .prompt(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("prompt {id} not found")))
}

fn attempt_entity(world: &World, id: u64) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .attempt(AttemptId(id))
        .ok_or_else(|| error(codes::NOT_FOUND, format!("attempt {id} not found")))
}

fn problem(world: &World, entity: Entity) -> Option<String> {
    world.get::<Problem>(entity).map(|p| p.0.clone())
}

fn task_id(world: &World, task: Entity) -> String {
    world
        .get::<TaskId>(task)
        .map(|t| t.0.clone())
        .unwrap_or_default()
}

pub fn operation_record(world: &World, op: Entity) -> OperationRecord {
    OperationRecord {
        operation: world
            .get::<OperationId>(op)
            .map(|o| o.0.clone())
            .unwrap_or_default(),
        kind: world
            .get::<OperationKind>(op)
            .copied()
            .unwrap_or(OperationKind::Launch),
        phase: world.get::<OperationPhase>(op).copied().unwrap_or_default(),
        launch: world.get::<LaunchTemplate>(op).cloned(),
        problem: problem(world, op),
    }
}

pub fn attempt_record(world: &World, attempt: Entity) -> AttemptRecord {
    let entity = world.entity(attempt);
    let handle = entity.get::<PaneHandle>().cloned().unwrap_or_default();
    let task = lifecycle::task_of(world, attempt).map_or_else(String::new, |t| task_id(world, t));
    AttemptRecord {
        attempt: entity.get::<AttemptId>().map_or(0, |a| a.0),
        task,
        ownership: entity
            .get::<Ownership>()
            .copied()
            .unwrap_or(Ownership::Managed),
        state: entity.get::<AttemptState>().copied().unwrap_or_default(),
        instance: handle.instance,
        workspace: handle.workspace,
        stream: handle.stream,
        pane: handle.pane,
        pid: handle.pid,
        marker: entity.get::<LaunchMarker>().map(|m| m.0.clone()),
        uncertain: entity.contains::<Uncertain>(),
        lost: entity.contains::<Lost>(),
        needs_input: entity.contains::<NeedsInput>(),
        problem: problem(world, attempt),
        final_evidence: entity.get::<FinalEvidence>().cloned(),
        operations: entity
            .get::<Operations>()
            .map(|ops| ops.iter().map(|op| operation_record(world, op)).collect())
            .unwrap_or_default(),
    }
}

pub fn prompt_record(world: &World, prompt: Entity) -> PromptRecord {
    let entity = world.entity(prompt);
    let attempt = entity.get::<PromptOf>().map(|p| p.0);
    let task = attempt
        .and_then(|a| lifecycle::task_of(world, a))
        .map_or_else(String::new, |t| task_id(world, t));
    PromptRecord {
        prompt: entity
            .get::<PromptId>()
            .map(|p| p.0.clone())
            .unwrap_or_default(),
        attempt: attempt
            .and_then(|a| world.get::<AttemptId>(a))
            .map_or(0, |a| a.0),
        task,
        text: entity
            .get::<PromptText>()
            .map(|t| t.0.clone())
            .unwrap_or_default(),
        deadline_ms: entity.get::<Deadline>().map_or(0, |d| d.0),
        delivery: entity.get::<Delivery>().copied().unwrap_or_default(),
        wait: entity.get::<WaitState>().copied().unwrap_or_default(),
        receipt: entity.get::<Receipt>().cloned(),
        response: entity.get::<ResponseEvent>().cloned(),
        binding: entity.get::<Binding>().cloned(),
        problem: problem(world, prompt),
    }
}

fn attempts_of(world: &World, task: Entity) -> Vec<Entity> {
    world
        .get::<Attempts>(task)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

pub fn task_inspect_record(world: &World, task: Entity) -> TaskInspect {
    let entity = world.entity(task);
    let attempts = attempts_of(world, task);
    let prompts: Vec<PromptRecord> = attempts
        .iter()
        .flat_map(|a| world.get::<Prompts>(*a).into_iter().flat_map(|p| p.iter()))
        .map(|p| prompt_record(world, p))
        .collect();
    TaskInspect {
        task: task_id(world, task),
        live: true,
        archive: None,
        state: entity.get::<TaskState>().map(|s| format!("{s:?}")),
        title: entity.get::<Title>().map(|t| t.0.clone()),
        location: entity.get::<Location>().cloned(),
        created_ms: entity.get::<CreatedMs>().map(|c| c.0),
        closed_ms: entity.get::<ClosedMs>().map(|c| c.0),
        stop_requested: entity.contains::<StopRequested>(),
        sealed: entity.contains::<Seal>(),
        attempts: attempts.iter().map(|a| attempt_record(world, *a)).collect(),
        prompts,
    }
}

fn current_attempt(world: &World, task: Entity) -> Result<Entity, BrpError> {
    lifecycle::attempt_of(world, task).ok_or_else(|| invalid("task has no current attempt"))
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

fn task_create(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskCreateParams = req.parse()?;
    let location = match (params.cwd, params.worktree) {
        (Some(cwd), None) => Location::Cwd(cwd),
        (None, Some(worktree)) => Location::Worktree(worktree),
        _ => return Err(invalid("exactly one of `cwd` or `worktree` is required")),
    };
    let task = lifecycle::create_task(
        world,
        TaskSpec {
            id: params.task,
            title: params.title,
            location,
        },
    )
    .map_err(lifecycle_error)?;
    to_value(task_inspect_record(world, task))
}

fn task_list(mut req: Request, world: &mut World) -> BrpResult {
    let NoParams {} = req.parse()?;
    let mut tasks: Vec<TaskSummary> = world
        .query_filtered::<(Entity, &TaskId, &Title, &TaskState), With<Task>>()
        .iter(world)
        .map(|(e, id, title, state)| TaskSummary {
            task: id.0.clone(),
            title: title.0.clone(),
            state: *state,
            attempt: lifecycle::attempt_of(world, e)
                .and_then(|a| world.get::<AttemptId>(a))
                .map(|a| a.0),
        })
        .collect();
    tasks.sort_by(|a, b| a.task.cmp(&b.task));
    to_value(TaskList { tasks })
}

/// The live record, else the archive read-only (`docs/model.md` "Journal and archive").
fn task_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = req.parse()?;
    if let Some(task) = world.resource::<Ids>().task(&params.task) {
        return to_value(task_inspect_record(world, task));
    }
    let not_found = || error(codes::NOT_FOUND, format!("task {} not found", params.task));
    let Some(journal) = world.get_resource::<Journal>() else {
        return Err(not_found());
    };
    match journal.find_archived(world, &params.task) {
        Ok(Some((archive, state))) => to_value(TaskInspect {
            task: params.task,
            live: false,
            archive: Some(archive),
            state,
            title: None,
            location: None,
            created_ms: None,
            closed_ms: None,
            stop_requested: false,
            sealed: false,
            attempts: Vec::new(),
            prompts: Vec::new(),
        }),
        Ok(None) => Err(not_found()),
        Err(e) => Err(BrpError::internal(e)),
    }
}

fn task_launch(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskLaunchParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let attempt = lifecycle::launch(
        world,
        task,
        LaunchSpec {
            operation: params.operation,
            argv: params.argv,
            env: params.env,
            workspace: params.workspace,
            ephemeral: params.ephemeral,
            integration: params.integration,
        },
    )
    .map_err(lifecycle_error)?;
    to_value(attempt_record(world, attempt))
}

fn task_adopt(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskAdoptParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let attempt = lifecycle::adopt(
        world,
        task,
        PaneHandle {
            instance: params.fux_instance,
            workspace: params.workspace,
            stream: params.stream.unwrap_or_default(),
            pane: params.pane,
            pid: params.pid,
        },
    )
    .map_err(lifecycle_error)?;
    to_value(attempt_record(world, attempt))
}

fn task_prompt(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskPromptParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    let attempt = current_attempt(world, task)?;
    let prompt = lifecycle::prepare_prompt(
        world,
        attempt,
        PromptSpec {
            id: params.prompt,
            text: params.text,
            timeout_ms: params.timeout_ms,
        },
    )
    .map_err(lifecycle_error)?;
    if params.submit {
        lifecycle::submit_prompt(world, prompt).map_err(lifecycle_error)?;
    }
    to_value(prompt_record(world, prompt))
}

/// One bounded evidence check (TASKS.md:311-320): asks fux for the receipt's status when one
/// is retained and nothing is in flight, and returns the record as it stands. Never submits.
fn task_wait(mut req: Request, world: &mut World) -> BrpResult {
    let params: PromptParams = req.parse()?;
    let prompt = prompt_entity(world, &params.prompt)?;
    lifecycle::refresh_receipt(world, prompt);
    to_value(prompt_record(world, prompt))
}

fn task_stop(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskMutationParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    validate_guard(world, task, params.guard.as_ref())?;
    lifecycle::request_stop(world, task).map_err(lifecycle_error)?;
    to_value(task_inspect_record(world, task))
}

fn task_cancel(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: TaskMutationParams = req.parse()?;
    let task = task_entity(world, &params.task)?;
    validate_guard(world, task, params.guard.as_ref())?;
    lifecycle::cancel_task(world, task).map_err(lifecycle_error)?;
    to_value(task_inspect_record(world, task))
}

fn task_abandon(mut req: Request, world: &mut World) -> BrpResult {
    req.mutation(world)?;
    let params: PromptParams = req.parse()?;
    let prompt = prompt_entity(world, &params.prompt)?;
    lifecycle::abandon_prompt(world, prompt).map_err(lifecycle_error)?;
    to_value(prompt_record(world, prompt))
}

fn attempt_list(mut req: Request, world: &mut World) -> BrpResult {
    let params: AttemptListParams = req.parse()?;
    let attempts: Vec<Entity> = match params.task {
        Some(id) => attempts_of(world, task_entity(world, &id)?),
        None => world
            .query_filtered::<Entity, With<Attempt>>()
            .iter(world)
            .collect(),
    };
    let mut attempts: Vec<AttemptRecord> = attempts
        .into_iter()
        .map(|a| attempt_record(world, a))
        .collect();
    attempts.sort_by_key(|a| a.attempt);
    to_value(AttemptList { attempts })
}

fn attempt_inspect(mut req: Request, world: &mut World) -> BrpResult {
    let params: AttemptParams = req.parse()?;
    let attempt = attempt_entity(world, params.attempt)?;
    to_value(attempt_record(world, attempt))
}

fn prompt_status(mut req: Request, world: &mut World) -> BrpResult {
    let params: PromptParams = req.parse()?;
    let prompt = prompt_entity(world, &params.prompt)?;
    to_value(prompt_record(world, prompt))
}

handler!(brp_task_create, task_create);
handler!(brp_task_list, task_list);
handler!(brp_task_inspect, task_inspect);
handler!(brp_task_launch, task_launch);
handler!(brp_task_adopt, task_adopt);
handler!(brp_task_prompt, task_prompt);
handler!(brp_task_wait, task_wait);
handler!(brp_task_stop, task_stop);
handler!(brp_task_cancel, task_cancel);
handler!(brp_task_abandon, task_abandon);
handler!(brp_attempt_list, attempt_list);
handler!(brp_attempt_inspect, attempt_inspect);
handler!(brp_prompt_status, prompt_status);

pub const METHODS: &[MethodSpec] = &[
    spec!(
        "zor/task.create",
        brp_task_create,
        TaskCreateParams,
        TaskInspect
    ),
    spec!("zor/task.list", brp_task_list, NoParams, TaskList),
    spec!(
        "zor/task.inspect",
        brp_task_inspect,
        TaskParams,
        TaskInspect
    ),
    spec!(
        "zor/task.launch",
        brp_task_launch,
        TaskLaunchParams,
        AttemptRecord
    ),
    spec!(
        "zor/task.adopt",
        brp_task_adopt,
        TaskAdoptParams,
        AttemptRecord
    ),
    spec!(
        "zor/task.prompt",
        brp_task_prompt,
        TaskPromptParams,
        PromptRecord
    ),
    spec!("zor/task.wait", brp_task_wait, PromptParams, PromptRecord),
    spec!("zor/task.stop", brp_task_stop, TaskMutationParams, TaskInspect),
    spec!("zor/task.cancel", brp_task_cancel, TaskMutationParams, TaskInspect),
    spec!(
        "zor/task.abandon",
        brp_task_abandon,
        PromptParams,
        PromptRecord
    ),
    spec!(
        "zor/attempt.list",
        brp_attempt_list,
        AttemptListParams,
        AttemptList
    ),
    spec!(
        "zor/attempt.inspect",
        brp_attempt_inspect,
        AttemptParams,
        AttemptRecord
    ),
    spec!(
        "zor/prompt.status",
        brp_prompt_status,
        PromptParams,
        PromptRecord
    ),
];
