//! Guarded native-session recreation and read-only eligibility. `fux_instance` is deliberately
//! distinct from the zor `instance` consumed by the authorization envelope.

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult};

use super::methods::{
    MethodSpec, Request, codes, described, error, handler, invalid, spec, to_value,
};
use super::task_methods::{AttemptRecord, TaskGuard, TaskParams, attempt_record, validate_guard};
use crate::lifecycle::LifecycleError;
use crate::lifecycle::resume::{self, ResumeSpec};
use crate::model::*;

described!(
    pub struct TaskResumeParams {
        pub task: String,
        pub operation: String,
        pub fux_instance: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub guard: Option<TaskGuard>,
    }
);
described!(
    pub struct ResumeStatus {
        pub task: String,
        pub eligible: bool,
        pub reason: Option<String>,
        pub previous_attempt: Option<u64>,
        pub provider: Option<String>,
        pub native_session: Option<String>,
    }
);

fn lifecycle_error(error_value: LifecycleError) -> BrpError {
    match error_value {
        LifecycleError::NotFound(_) => error(codes::NOT_FOUND, error_value.to_string()),
        LifecycleError::Uncertain(_) => error(codes::UNCERTAIN, error_value.to_string()),
        _ => invalid(error_value.to_string()),
    }
}

fn task_entity(world: &World, id: &str) -> Result<Entity, BrpError> {
    world
        .resource::<Ids>()
        .task(id)
        .ok_or_else(|| error(codes::NOT_FOUND, format!("task {id} not found")))
}

fn task_resume(mut request: Request, world: &mut World) -> BrpResult {
    request.mutation(world)?;
    let params: TaskResumeParams = request.parse()?;
    let task = task_entity(world, &params.task)?;
    let spec = ResumeSpec {
        operation: params.operation,
        fux_instance: params.fux_instance,
    };
    if let Some(attempt) = resume::retained(world, task, &spec).map_err(lifecycle_error)? {
        // A successful new resume necessarily invalidates the original no-live-attempt guard.
        // Returning retained evidence cannot mutate anything. Non-null guards were never
        // eligible for a new resume and therefore cannot be used as an alternate retry intent.
        if params
            .guard
            .as_ref()
            .is_some_and(|guard| guard.attempt.is_some() || guard.pane.is_some())
        {
            return Err(invalid(
                "resume retry guard differs from the original no-live-attempt intent",
            ));
        }
        return to_value(attempt_record(world, attempt));
    }
    validate_guard(world, task, params.guard.as_ref())?;
    let attempt = resume::resume(world, task, spec).map_err(lifecycle_error)?;
    to_value(attempt_record(world, attempt))
}

fn task_resume_status(mut request: Request, world: &mut World) -> BrpResult {
    let params: TaskParams = request.parse()?;
    let task = task_entity(world, &params.task)?;
    let status = match resume::eligibility(world, task) {
        Ok(eligible) => ResumeStatus {
            task: params.task,
            eligible: true,
            reason: None,
            previous_attempt: Some(eligible.previous_attempt),
            provider: Some(eligible.provider.name().into()),
            native_session: Some(eligible.native_session),
        },
        Err(problem) => ResumeStatus {
            task: params.task,
            eligible: false,
            reason: Some(problem.to_string()),
            previous_attempt: None,
            provider: None,
            native_session: None,
        },
    };
    to_value(status)
}

handler!(brp_task_resume, task_resume);
handler!(brp_task_resume_status, task_resume_status);

pub const METHODS: &[MethodSpec] = &[
    spec!(
        "zor/task.resume",
        brp_task_resume,
        TaskResumeParams,
        AttemptRecord
    ),
    spec!(
        "zor/task.resume-status",
        brp_task_resume_status,
        TaskParams,
        ResumeStatus
    ),
];
