//! Typed task reads and guarded supervision. Retained evidence alone never authorizes a pane action.
use super::{Client, decode, exchange, identity, same_instance};
use crate::tasks::model::{self, Attempt, Launch, Session, Task, TaskOutcome};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskList {
    pub generation: u64,
    pub tasks: Vec<Task>,
    pub launches: Vec<Launch>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskInspection {
    pub generation: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub task: Option<Task>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub attempt: Option<Attempt>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub session: Option<Session>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub launch: Option<Launch>,
    /// Existing check/artifact/receipt summaries retain their original names and claim scopes.
    #[serde(flatten)]
    pub evidence: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskResult {
    pub v: u32,
    pub generation: u64,
    pub task_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub task_outcome: Option<TaskOutcome>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub attempt: Option<Attempt>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub session: Option<Session>,
    #[serde(flatten)]
    pub evidence: BTreeMap<String, Value>,
}

impl Client {
    pub fn observed_attachment(
        &self,
        instance: &str,
        handle: &crate::watch::Handle,
        deadline: Instant,
    ) -> Result<model::Target> {
        let target: model::Target = self.task_read(
            instance,
            json!({"action":"observed-attachment","handle":handle}),
            deadline,
        )?;
        ensure!(
            target.instance == handle.instance
                && target.pane == handle.pane
                && target.pid == handle.pid
                && target.pid.is_some_and(|pid| pid > 0)
                && target.runtime.is_absolute()
                && crate::fux::endpoint::valid_name(&target.workspace)
                && target.stream > 0
                && target
                    .origin
                    .as_ref()
                    .is_some_and(|origin| crate::fux::endpoint::valid_name(&origin.workspace)
                        && origin.stream > 0),
            "observed attachment returned an invalid or replaced process"
        );
        Ok(target)
    }
    fn task_request<T: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        task: Value,
        deadline: Instant,
    ) -> Result<T> {
        identity(instance)?;
        let mut body = exchange(
            &self.socket,
            json!({"v":1,"id":1,"op":"task","service_instance":instance,"task":task}),
            deadline,
        )?;
        same_instance(&body, instance)?;
        let value = body
            .as_object_mut()
            .and_then(|body| body.remove("value"))
            .ok_or(super::MalformedReply)?;
        decode(value)
    }
    pub(super) fn task_read<T: serde::de::DeserializeOwned>(
        &self,
        instance: &str,
        task: Value,
        deadline: Instant,
    ) -> Result<T> {
        loop {
            match self.task_request(instance, task.clone(), deadline) {
                Err(error) if error.is::<crate::tasks::store::Busy>() => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(error);
                    }
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
                }
                result => return result,
            }
        }
    }
    /// Sends once. A failure may follow a committed transition: inspect before any explicit retry.
    pub fn task_supervise(
        &self,
        instance: &str,
        expected: &crate::tasks::supervise::Expected,
        action: crate::tasks::supervise::Action,
        deadline: Instant,
    ) -> Result<TaskInspection> {
        use anyhow::Context;
        let inspection: TaskInspection = self.task_request(instance,
            json!({"action":"supervise","expected":expected,"action_kind":action}),deadline)
            .context("supervision result unconfirmed; inspect the same task/attempt before an explicit retry; request was not replayed")?;
        inspection.validate(&expected.task)?;
        ensure!(
            inspection
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.id == expected.attempt),
            "supervision reply changed task attempt; refresh required"
        );
        Ok(inspection)
    }
    /// Read retained operation evidence without resubmitting the resume mutation.
    pub fn task_resume_status(
        &self,
        service_instance: &str,
        task: &str,
        operation: &str,
        deadline: Instant,
    ) -> Result<crate::tasks::resume::OperationStatus> {
        ensure!(
            model::id(task) && model::id(operation) && task != operation,
            "invalid task/resume operation identity"
        );
        let status: crate::tasks::resume::OperationStatus = self.task_read(
            service_instance,
            json!({"action":"resume-status","id":task,"operation":operation}),
            deadline,
        )?;
        ensure!(
            status.task == task && status.operation == operation,
            "resume status identity mismatch"
        );
        if let Some(record) = &status.record {
            identity(&record.instance)?;
            ensure!(
                model::id(&record.launch)
                    && model::id(&record.previous_attempt)
                    && record
                        .session
                        .as_ref()
                        .is_none_or(|session| model::id(session))
                    && record.pane.is_none_or(|pane| pane > 0),
                "invalid resume operation evidence"
            );
        }
        Ok(status)
    }
    /// One explicit native-session resume operation. Never retry a lost reply here.
    pub fn task_resume(
        &self,
        service_instance: &str,
        expected: &crate::tasks::supervise::Expected,
        operation: &str,
        fux_instance: &str,
        deadline: Instant,
    ) -> Result<TaskInspection> {
        use anyhow::Context;
        ensure!(
            model::id(operation) && operation != expected.task,
            "resume requires a distinct stable operation ID"
        );
        identity(fux_instance)?;
        let inspection: TaskInspection = self.task_request(service_instance,
            json!({"action":"guarded-resume","expected":expected,"operation":operation,"instance":fux_instance}),deadline)
            .context("resume result unconfirmed; inspect the task and retained operation before an explicit retry; request was not replayed")?;
        inspection.validate(&expected.task)?;
        Ok(inspection)
    }
    pub fn task_attachment(
        &self,
        instance: &str,
        expected: &crate::tasks::supervise::Expected,
        deadline: Instant,
    ) -> Result<model::Target> {
        let target: model::Target = self.task_read(
            instance,
            json!({"action":"attachment-target","expected":expected}),
            deadline,
        )?;
        ensure!(
            target.identity() == expected.target.identity()
                && target.pid.is_some_and(|pid| pid > 0)
                && crate::fux::endpoint::valid_name(&target.workspace)
                && target.stream > 0,
            "attachment resolution changed process identity or returned an invalid route"
        );
        ensure!(
            expected
                .target
                .origin
                .as_ref()
                .is_none_or(|origin| target.origin.as_ref() == Some(origin)),
            "attachment resolution changed process origin"
        );
        Ok(target)
    }
    pub fn task_list(&self, instance: &str, deadline: Instant) -> Result<TaskList> {
        let list: TaskList = self.task_read(instance, json!({"action":"list"}), deadline)?;
        ensure!(
            list.tasks.len() <= model::MAX_TASKS && list.launches.len() <= model::MAX_ATTEMPTS,
            "task list limit exceeded"
        );
        let mut tasks = BTreeSet::new();
        let mut launches = BTreeSet::new();
        for task in &list.tasks {
            ensure!(
                model::id(&task.id) && model::id(&task.attempt) && tasks.insert(&task.id),
                "invalid task list identity"
            );
        }
        for launch in &list.launches {
            ensure!(
                model::id(&launch.id) && launches.insert(&launch.id),
                "invalid launch list identity"
            );
        }
        Ok(list)
    }
    pub fn task_inspect(
        &self,
        instance: &str,
        id: &str,
        deadline: Instant,
    ) -> Result<TaskInspection> {
        ensure!(model::id(id), "invalid task ID");
        let inspection: TaskInspection =
            self.task_read(instance, json!({"action":"inspect","id":id}), deadline)?;
        inspection.validate(id)?;
        Ok(inspection)
    }
    pub fn task_result(&self, instance: &str, id: &str, deadline: Instant) -> Result<TaskResult> {
        ensure!(model::id(id), "invalid task ID");
        let result: TaskResult =
            self.task_read(instance, json!({"action":"result","id":id}), deadline)?;
        ensure!(
            result.v == 1 && result.task_id == id,
            "task result identity/version mismatch"
        );
        validate_attempt(id, result.attempt.as_ref(), result.session.as_ref())?;
        ensure!(
            result.task_outcome.is_some() == result.attempt.is_some(),
            "task result outcome identity missing"
        );
        Ok(result)
    }
}
impl TaskInspection {
    pub fn check_action(&self, action: crate::tasks::supervise::Action) -> Result<()> {
        use crate::tasks::{model::Ownership, supervise::Action};
        use anyhow::Context;
        let task = self.task.as_ref().context("task has no attached attempt")?;
        self.validate(&task.id)?;
        match action {
            Action::Cancel => ensure!(
                matches!(task.outcome, TaskOutcome::Open | TaskOutcome::Cancelled),
                "task outcome is already final"
            ),
            Action::Stop | Action::Reconcile => ensure!(
                self.launch.is_some()
                    && self
                        .session
                        .as_ref()
                        .is_some_and(|session| session.ownership == Ownership::Managed),
                "this action requires a managed launch; adoption grants no termination authority"
            ),
        }
        Ok(())
    }
    pub fn expected(&self) -> Result<crate::tasks::supervise::Expected> {
        use anyhow::Context;
        let task = self.task.as_ref().context("task has no attached attempt")?;
        self.validate(&task.id)?;
        let attempt = self.attempt.as_ref().context("task attempt missing")?;
        let session = self.session.as_ref().context("task session missing")?;
        Ok(crate::tasks::supervise::Expected {
            task: task.id.clone(),
            attempt: attempt.id.clone(),
            session: session.id.clone(),
            target: session.target.clone(),
        })
    }
    fn validate(&self, id: &str) -> Result<()> {
        ensure!(
            self.task.is_some() || self.launch.is_some(),
            "task inspection has no task or launch"
        );
        ensure!(
            self.launch
                .as_ref()
                .is_none_or(|launch| launch.id == id && launch.task_id() == id),
            "task launch identity mismatch"
        );
        if let Some(task) = &self.task {
            ensure!(
                task.id == id
                    && self
                        .attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.id == task.attempt),
                "task attempt identity mismatch"
            );
        } else {
            ensure!(
                self.attempt.is_none() && self.session.is_none(),
                "unattached launch has task identity"
            );
        }
        validate_attempt(id, self.attempt.as_ref(), self.session.as_ref())
    }
}
fn validate_attempt(id: &str, attempt: Option<&Attempt>, session: Option<&Session>) -> Result<()> {
    ensure!(
        attempt.is_some() == session.is_some(),
        "task session identity missing"
    );
    if let (Some(attempt), Some(session)) = (attempt, session) {
        ensure!(
            attempt.task == id
                && model::id(&attempt.id)
                && model::id(&session.id)
                && attempt.session == session.id,
            "task/session identity mismatch"
        );
        let target = &session.target;
        identity(&target.instance)?;
        ensure!(
            crate::fux::endpoint::valid_name(&target.workspace)
                && target.stream > 0
                && target.pane > 0
                && target.pid.is_none_or(|pid| pid > 0),
            "invalid retained target identity"
        );
    }
    Ok(())
}
