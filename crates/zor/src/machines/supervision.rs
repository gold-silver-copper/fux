//! Per-machine supervision (multi-machine-supervision.md:186-211): one bounded reader per
//! machine on `IoTaskPool`, polling the remote zor's `zor/server.info` and the `TaskView` /
//! `AgentView` projections through `world.query` at one-second intervals under a six-second
//! read budget. Freshness expires after five seconds; a failed read preserves the last view as
//! stale without renewing its timestamp; failures are named by the transport-loss taxonomy.
//! The same worker performs guarded remote actions: it re-reads the remote's instance nonce
//! and the task's current attempt/pane identity and refuses a changed service, attempt or pane
//! before dispatching one call. The owner commits intent before handing work to this adapter.

use core::time::Duration;
use std::collections::VecDeque;
use std::time::Instant;

use async_channel::{Receiver, Sender};
use async_io::Timer;
use bevy_ecs::prelude::*;
use bevy_reflect::TypePath;
use bevy_remote::builtin_methods::BrpQueryRow;
use bevy_tasks::futures_lite::future;
use bevy_tasks::{IoTaskPool, Task};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::transport::{Descriptor, Transport, Unavailable};
use crate::fux_client::call_async;
use crate::remote::client::ClientError;
use crate::remote::methods::codes;
use crate::remote::projection::{AgentView, TaskView};

pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const READ_BUDGET: Duration = Duration::from_secs(6);
pub const FRESH_MS: u64 = 5_000;
/// Rows retained per remote view (tasks and agents each).
pub const MAX_ROWS: usize = 256;
/// Action records retained per machine.
pub const MAX_ACTIONS: usize = 32;

// ---------------------------------------------------------------------------------------------
// Published state
// ---------------------------------------------------------------------------------------------

/// The transport-loss taxonomy (multi-machine-supervision.md:18-22, 186-189).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "freshness")]
pub enum Freshness {
    Fresh,
    /// The retained view is older than [`FRESH_MS`] or the last read failed; `since_ms` is
    /// when freshness lapsed.
    Stale { since_ms: u64 },
    /// The remote refused the token.
    Unauthorized,
    /// The bound incarnation is over: the remote refused the recorded instance nonce.
    Expired,
    /// The peer answered, but not as the bound service (protocol/HTTP-level failure).
    Ended,
    /// Unreachable, or the transport cannot be used (`unavailable`).
    #[default]
    Offline,
}

impl Freshness {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale { .. } => "stale",
            Self::Unauthorized => "unauthorized",
            Self::Expired => "expired",
            Self::Ended => "ended",
            Self::Offline => "offline",
        }
    }
}

/// One remote `TaskView` row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteTask {
    pub id: String,
    pub title: String,
    pub state: String,
    pub attempts: usize,
    pub current_attempt: Option<u64>,
    pub uncertain: bool,
    pub needs_input: bool,
}

/// One remote `AgentView` row: the pane identity of an observed attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteAgent {
    pub id: u64,
    pub instance: String,
    pub workspace: String,
    pub pane: u64,
    pub pid: Option<u32>,
    pub state: String,
    pub task: Option<String>,
    pub attempt: Option<u64>,
}

/// One bounded read of a remote zor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteView {
    /// The remote's current instance nonce.
    pub instance: String,
    pub name: String,
    pub pid: u32,
    pub tasks: usize,
    pub attempts: usize,
    pub checks: usize,
    pub rows: Vec<RemoteTask>,
    pub agents: Vec<RemoteAgent>,
}

/// The exact-process identity a guarded action was validated against.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneIdentity {
    pub instance: String,
    pub workspace: String,
    pub pane: u64,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ActionKind {
    Cancel,
    Stop,
    /// Re-reads retained lifecycle evidence under the same guards; sends no mutation.
    Reconcile,
    Resume {
        operation: String,
        fux_instance: String,
    },
}

impl ActionKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Stop => "stop",
            Self::Reconcile => "reconcile",
            Self::Resume { .. } => "resume",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    /// Queued or in flight; a lost reply leaves it here and is never replayed.
    #[default]
    Submitting,
    Done,
    Failed,
    /// The remote's nonce, attempt or pane identity no longer matched at dispatch time.
    Refused,
    /// Dispatch may have reached the peer; never retried automatically.
    Uncertain,
}

/// A guarded remote action and its outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub id: u64,
    pub kind: Option<ActionKind>,
    pub task: String,
    pub attempt: Option<u64>,
    /// The remote zor incarnation the action was guarded by.
    pub instance: String,
    pub pane: Option<PaneIdentity>,
    pub phase: ActionPhase,
    pub result: Option<Value>,
    pub problem: Option<String>,
    pub requested_ms: u64,
    pub finished_ms: Option<u64>,
}

/// Slice-private supervision state on a `Machine` entity; never journaled.
#[derive(Component, Clone, Debug, Default)]
pub struct Supervision {
    pub freshness: Freshness,
    /// When the current freshness was entered.
    pub since_ms: u64,
    pub problem: Option<String>,
    /// When `view` was read; unchanged by failures.
    pub observed_ms: u64,
    /// The last successful read, preserved across transport loss.
    pub view: Option<RemoteView>,
    pub actions: VecDeque<ActionRecord>,
}

impl Supervision {
    pub fn action(&self, id: u64) -> Option<&ActionRecord> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn push_action(&mut self, record: ActionRecord) {
        if self.actions.len() >= MAX_ACTIONS {
            if let Some(index) = self.actions.iter().position(|a| a.phase != ActionPhase::Submitting) {
                self.actions.remove(index);
            } else {
                // Admission reserves a retained slot before an action reaches the worker.
                return;
            }
        }
        self.actions.push_back(record);
    }

    fn enter(&mut self, freshness: Freshness, now_ms: u64) {
        if self.freshness != freshness {
            self.freshness = freshness;
            self.since_ms = now_ms;
        }
    }

    /// Fresh views lapse into stale after [`FRESH_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        if self.freshness == Freshness::Fresh && now_ms.saturating_sub(self.observed_ms) > FRESH_MS {
            self.enter(Freshness::Stale { since_ms: now_ms }, now_ms);
        }
    }

    pub fn apply_poll(&mut self, outcome: Result<RemoteView, Failure>, at_ms: u64) {
        match outcome {
            Ok(view) => {
                self.view = Some(view);
                self.observed_ms = at_ms;
                self.problem = None;
                self.enter(Freshness::Fresh, at_ms);
            }
            Err(failure) => {
                let (freshness, problem) = match failure {
                    Failure::Offline(p) | Failure::Unavailable(p) => (Freshness::Offline, p),
                    Failure::Unauthorized(p) => (Freshness::Unauthorized, p),
                    Failure::Expired(p) => (Freshness::Expired, p),
                    Failure::Ended(p) => (Freshness::Ended, p),
                    Failure::Budget(p) => (Freshness::Stale { since_ms: at_ms }, p),
                };
                self.problem = Some(problem);
                self.enter(freshness, at_ms);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker protocol
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Offline(String),
    Unauthorized(String),
    Expired(String),
    Ended(String),
    Unavailable(String),
    /// The read budget elapsed: the previous view stays, stale.
    Budget(String),
}

pub fn classify(error: &ClientError) -> Failure {
    match error {
        ClientError::Rpc { code, message } if *code == codes::UNAUTHORIZED => {
            Failure::Unauthorized(message.clone())
        }
        ClientError::Rpc { code, message }
            if *code == codes::INVALID && message.contains("instance mismatch") =>
        {
            Failure::Expired(message.clone())
        }
        ClientError::Io(_) | ClientError::Descriptor(_) | ClientError::Params => {
            Failure::Offline(error.to_string())
        }
        ClientError::Http { .. } | ClientError::Malformed(_) | ClientError::Rpc { .. } => {
            Failure::Ended(error.to_string())
        }
    }
}

/// A guarded action handed to the worker.
#[derive(Clone, Debug)]
pub struct ActionRequest {
    pub id: u64,
    pub kind: ActionKind,
    pub task: String,
    /// The remote incarnation the caller observed; the dispatch carries exactly this nonce.
    pub instance: String,
    pub attempt: Option<u64>,
    pub pane: Option<PaneIdentity>,
}

#[derive(Debug)]
pub enum Report {
    Poll {
        machine: Entity,
        generation: u64,
        at_ms: u64,
        outcome: Result<RemoteView, Failure>,
    },
    Action {
        machine: Entity,
        generation: u64,
        id: u64,
        at_ms: u64,
        outcome: Result<Value, ActionFailure>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionFailure {
    Refused(String),
    Uncertain(String),
    Failed(String),
}

/// The reports channel: workers send, `ingest_reports` drains.
#[derive(Resource)]
pub struct Reports {
    pub tx: Sender<Report>,
    pub rx: Receiver<Report>,
    pub next_action: u64,
}

impl Default for Reports {
    fn default() -> Self {
        let (tx, rx) = async_channel::bounded(super::catalog::MAX_MACHINES * 2);
        Self {
            tx,
            rx,
            next_action: 1,
        }
    }
}

/// The running reader of one machine. Dropping it cancels its future and closes its sockets.
#[derive(Component)]
pub struct Worker {
    pub generation: u64,
    pub actions: Sender<ActionRequest>,
    _task: Task<()>,
}

impl Worker {
    pub fn spawn(
        machine: Entity,
        generation: u64,
        transport: Transport,
        reports: Sender<Report>,
        wake: Option<Sender<crate::model::Inbound>>,
    ) -> Self {
        let (actions, requests) = async_channel::bounded(MAX_ACTIONS);
        let task = IoTaskPool::get().spawn(run(machine, generation, transport, reports, requests, wake));
        Self {
            generation,
            actions,
            _task: task,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------------------------

async fn run(
    machine: Entity,
    generation: u64,
    transport: Transport,
    reports: Sender<Report>,
    requests: Receiver<ActionRequest>,
    wake: Option<Sender<crate::model::Inbound>>,
) {
    let descriptor = match transport.resolve() {
        Ok(resolved) => resolved.descriptor,
        Err(Unavailable(reason)) => {
            publish(&reports, &wake, Report::Poll {
                machine, generation, at_ms: fux::runner::wall_ms(),
                outcome: Err(Failure::Unavailable(reason)),
            }).await;
            return;
        }
    };
    while !requests.is_closed() && !reports.is_closed() {
        let outcome = budgeted(read(&descriptor)).await;
        if !publish(&reports, &wake, Report::Poll {
            machine, generation, at_ms: fux::runner::wall_ms(), outcome,
        }).await { return; }
        let next = Instant::now() + POLL_INTERVAL;
        while let Some(request) = until(&requests, next).await {
            let id = request.id;
            let outcome = budgeted(dispatch(&descriptor, request)).await;
            if !publish(&reports, &wake, Report::Action {
                machine, generation, id, at_ms: fux::runner::wall_ms(), outcome,
            }).await { return; }
        }
    }
}

async fn publish(reports: &Sender<Report>, wake: &Option<Sender<crate::model::Inbound>>, report: Report) -> bool {
    if reports.send(report).await.is_err() { return false; }
    if let Some(wake) = wake { let _ = wake.try_send(crate::model::Inbound::Wake); }
    true
}

/// The next queued action before `deadline`, else `None` at the deadline (or when closed).
async fn until(requests: &Receiver<ActionRequest>, deadline: Instant) -> Option<ActionRequest> {
    future::or(async { requests.recv().await.ok() }, async {
        Timer::at(deadline).await;
        None
    })
    .await
}

trait Budget {
    fn budget_exceeded() -> Self;
}

impl Budget for Failure {
    fn budget_exceeded() -> Self {
        Self::Budget(format!("read budget of {READ_BUDGET:?} exceeded"))
    }
}

impl Budget for ActionFailure {
    fn budget_exceeded() -> Self {
        Self::Uncertain(format!(
            "no reply within {READ_BUDGET:?}; the outcome is unknown and is not replayed"
        ))
    }
}

async fn budgeted<T, E: Budget>(work: impl Future<Output = Result<T, E>>) -> Result<T, E> {
    future::or(work, async {
        Timer::after(READ_BUDGET).await;
        Err(E::budget_exceeded())
    })
    .await
}

fn query(component: &str) -> Value {
    json!({ "data": { "components": [component] } })
}

fn rows<T: serde::de::DeserializeOwned>(reply: Value, component: &str) -> Result<Vec<T>, Failure> {
    let rows: Vec<BrpQueryRow> =
        serde_json::from_value(reply).map_err(|e| Failure::Ended(format!("world.query: {e}")))?;
    rows.into_iter()
        .take(MAX_ROWS)
        .filter_map(|mut row| row.components.remove(component))
        .map(|value| {
            serde_json::from_value(value).map_err(|e| Failure::Ended(format!("{component}: {e}")))
        })
        .collect()
}

/// One bounded read: server info, then the task and agent projections.
async fn read(descriptor: &Descriptor) -> Result<RemoteView, Failure> {
    let info = call_async(descriptor, "zor/server.info", json!({}))
        .await
        .map_err(|e| classify(&e))?;
    if info.get("nonce").and_then(Value::as_str) != Some(descriptor.instance.as_str()) {
        return Err(Failure::Expired("remote service identity changed".into()));
    }
    let task_view = TaskView::type_path();
    let agent_view = AgentView::type_path();
    let tasks = call_async(descriptor, "world.query", query(task_view))
        .await
        .map_err(|e| classify(&e))?;
    let agents = call_async(descriptor, "world.query", query(agent_view))
        .await
        .map_err(|e| classify(&e))?;
    let field = |name: &str| info.get(name).cloned().unwrap_or(Value::Null);
    let count = |name: &str| field(name).as_u64().unwrap_or(0) as usize;
    Ok(RemoteView {
        instance: field("nonce").as_str().unwrap_or_default().to_owned(),
        name: field("name").as_str().unwrap_or_default().to_owned(),
        pid: field("pid").as_u64().unwrap_or(0) as u32,
        tasks: count("tasks"),
        attempts: count("attempts"),
        checks: count("checks"),
        rows: rows(tasks, task_view)?,
        agents: rows(agents, agent_view)?,
    })
}

fn refused(message: impl Into<String>) -> ActionFailure {
    ActionFailure::Refused(message.into())
}

/// The remote's current attempt of `task` with its pane identity, from `zor/task.inspect`.
fn current_attempt(inspect: &Value) -> Option<(u64, PaneIdentity)> {
    inspect
        .get("attempts")?
        .as_array()?
        .iter()
        .find(|a| a.get("state").and_then(Value::as_str) != Some("finished"))
        .and_then(|a| {
            Some((
                a.get("attempt")?.as_u64()?,
                PaneIdentity {
                    instance: a.get("instance")?.as_str()?.to_owned(),
                    workspace: a.get("workspace")?.as_str()?.to_owned(),
                    pane: a.get("pane")?.as_u64()?,
                    pid: a.get("pid").and_then(Value::as_u64).map(|p| p as u32),
                },
            ))
        })
}

/// Validates the remote's instance nonce and the task's attempt/pane identity, then sends
/// exactly one call carrying the caller's nonce (service-ownership-contract.md:22,45-49).
async fn dispatch(descriptor: &Descriptor, request: ActionRequest) -> Result<Value, ActionFailure> {
    let failed = |e: ClientError| ActionFailure::Failed(e.to_string());
    let info = call_async(descriptor, "zor/server.info", json!({}))
        .await
        .map_err(failed)?;
    let nonce = info.get("nonce").and_then(Value::as_str).unwrap_or_default();
    if nonce != request.instance {
        return Err(refused(format!(
            "instance mismatch: the remote is now incarnation {nonce}, the action was guarded by {}",
            request.instance
        )));
    }
    let inspect = call_async(
        descriptor,
        "zor/task.inspect",
        json!({ "task": request.task }),
    )
    .await
    .map_err(failed)?;
    let current = current_attempt(&inspect);
    if current.as_ref().map(|(attempt, _)| *attempt) != request.attempt {
        return Err(refused("current attempt no longer matches the observed selection"));
    }
    if let Some(expected) = request.attempt {
        match &current {
            Some((attempt, _)) if *attempt == expected => {}
            Some((attempt, _)) => {
                return Err(refused(format!(
                    "attempt changed: task {} is on attempt {attempt}, the action named {expected}",
                    request.task
                )));
            }
            None => {
                return Err(refused(format!(
                    "attempt {expected} of task {} is finished; nothing to act on",
                    request.task
                )));
            }
        }
    }
    if let Some(expected) = &request.pane {
        match &current {
            Some((_, pane)) if pane == expected => {}
            Some((_, pane)) => {
                return Err(refused(format!(
                    "pane identity changed: {}:{}/{} pid {:?} is not the validated {}:{}/{} pid {:?}",
                    pane.instance,
                    pane.workspace,
                    pane.pane,
                    pane.pid,
                    expected.instance,
                    expected.workspace,
                    expected.pane,
                    expected.pid
                )));
            }
            None => return Err(refused("the validated pane belongs to a finished attempt")),
        }
    }
    // The dispatch carries the caller's nonce, so a service replaced between the check and
    // the call is refused by the remote itself.
    let mut guarded = descriptor.clone();
    guarded.instance = request.instance.clone();
    let task = request.task;
    let guard = json!({ "attempt": request.attempt, "pane": request.pane });
    let mutation_failed = |e: ClientError| match e {
        ClientError::Rpc { code, .. } if code == codes::UNCERTAIN => ActionFailure::Uncertain(e.to_string()),
        ClientError::Rpc { .. } => ActionFailure::Failed(e.to_string()),
        _ => ActionFailure::Uncertain(format!("{e}; dispatch is never replayed")),
    };
    match request.kind {
        ActionKind::Cancel => call_async(&guarded, "zor/task.cancel", json!({ "task": task, "guard": guard }))
            .await
            .map_err(mutation_failed),
        ActionKind::Stop => call_async(&guarded, "zor/task.stop", json!({ "task": task, "guard": guard }))
            .await
            .map_err(mutation_failed),
        ActionKind::Reconcile => Ok(inspect),
        ActionKind::Resume {
            operation,
            fux_instance,
        } => call_async(
            &guarded,
            "zor/task.resume",
            json!({ "task": task, "operation": operation, "fux_instance": fux_instance, "guard": guard }),
        )
        .await
        .map_err(mutation_failed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_preserve_the_view_and_name_the_taxonomy() {
        let mut s = Supervision::default();
        let view = RemoteView {
            instance: "n1".into(),
            tasks: 3,
            ..Default::default()
        };
        s.apply_poll(Ok(view.clone()), 1_000);
        assert_eq!(s.freshness, Freshness::Fresh);
        assert_eq!(s.observed_ms, 1_000);
        s.apply_poll(Err(Failure::Offline("refused".into())), 2_000);
        assert_eq!(s.freshness, Freshness::Offline);
        assert_eq!(s.since_ms, 2_000);
        assert_eq!(s.observed_ms, 1_000);
        assert_eq!(s.view.as_ref(), Some(&view));
        s.apply_poll(Err(Failure::Unauthorized("unknown token".into())), 3_000);
        assert_eq!(s.freshness, Freshness::Unauthorized);
        s.apply_poll(Ok(view), 4_000);
        s.expire(4_000 + FRESH_MS);
        assert_eq!(s.freshness, Freshness::Fresh);
        s.expire(4_001 + FRESH_MS);
        assert!(matches!(s.freshness, Freshness::Stale { since_ms } if since_ms == 4_001 + FRESH_MS));
    }

    #[test]
    fn current_attempt_reads_the_unfinished_pane() {
        let inspect = json!({ "attempts": [
            { "attempt": 1, "state": "finished", "instance": "a", "workspace": "w", "pane": 1, "pid": 5 },
            { "attempt": 2, "state": "live", "instance": "a", "workspace": "w", "pane": 2, "pid": null },
        ]});
        let (attempt, pane) = current_attempt(&inspect).unwrap();
        assert_eq!(attempt, 2);
        assert_eq!(pane.pane, 2);
        assert_eq!(pane.pid, None);
    }
}
