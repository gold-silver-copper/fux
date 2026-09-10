//! Bounded service task executor. No terminal parsing or agent policy enters fux.
use crate::tasks::{self, model::ResponseKind, submit};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Request {
    CodexStart {
        id: String,
        title: String,
        instance: String,
        workspace: String,
        cwd: Option<PathBuf>,
        worktree: Option<String>,
        argv: Vec<String>,
    },
    CodexSubmit {
        id: String,
        operation: String,
        text: String,
        timeout_ms: u64,
    },
    CodexInspect {
        operation: String,
    },
    CodexRecreate {
        operation: String,
        request: String,
        timeout_ms: u64,
    },
    CodexReconcile {
        operation: String,
        request: String,
        timeout_ms: u64,
    },
    CodexInterrupt {
        operation: String,
        request: String,
        timeout_ms: u64,
    },
    GroupCreate {
        id: String,
        concurrency: usize,
        members: Vec<tasks::group::RequestMember>,
    },
    GroupInspect {
        id: String,
    },
    GroupList {},
    GroupStep {
        id: String,
    },
    GroupRun {
        id: String,
    },
    GroupPause {
        id: String,
    },
    GroupCancel {
        id: String,
    },
    Overview {},
    Recover {
        after: Option<String>,
    },
    Handoff {
        id: String,
        operation: String,
        predecessors: Vec<String>,
        text: String,
        timeout_ms: u64,
    },
    Verify {
        id: String,
        source: String,
    },
    SourceCollect {
        id: String,
        source: String,
        revision: String,
    },
    SourceInspect {
        source: String,
    },
    SourceFile {
        source: String,
        path: String,
    },
    ChangesCollect {
        id: String,
        changes: String,
    },
    ChangesInspect {
        changes: String,
    },
    Result {
        id: String,
    },
    RequireArtifact {
        id: String,
        artifact: String,
        path: String,
    },
    ArtifactCollect {
        id: String,
        artifact: String,
        path: String,
        requirement: Option<String>,
    },
    ArtifactInspect {
        artifact: String,
    },
    RequireCheck {
        id: String,
        check: String,
        argv: Vec<String>,
    },
    Check {
        id: String,
        check: String,
        timeout_ms: u64,
        requirement: Option<String>,
        source: Option<String>,
        #[serde(default, deserialize_with = "tasks::model::unique_records")]
        artifacts: std::collections::BTreeMap<String, String>,
        argv: Vec<String>,
    },
    CheckInspect {
        check: String,
    },
    WorktreeRemove {
        id: String,
        #[serde(default)]
        force: bool,
    },
    WorktreeCreate {
        id: String,
        repo: PathBuf,
        branch: String,
        base: String,
    },
    WorktreeList {},
    WorktreeInspect {
        id: String,
    },
    WorktreeReconcile {
        id: String,
    },
    Start {
        id: String,
        title: String,
        instance: String,
        workspace: String,
        cwd: Option<PathBuf>,
        worktree: Option<String>,
        argv: Vec<String>,
        agent: Option<String>,
        #[serde(default)]
        integration: Option<tasks::model::IntegrationKind>,
        #[serde(default)]
        headless_agent: Option<String>,
    },
    Resume {
        id: String,
        operation: String,
        instance: String,
    },
    LaunchReconcile {
        id: String,
    },
    Adopt {
        id: String,
        title: String,
        instance: String,
        workspace: String,
        pane: u32,
        agent: Option<String>,
    },
    List {},
    Inspect {
        id: String,
    },
    Prepare {
        id: String,
        operation: String,
        text: String,
        timeout_ms: u64,
    },
    Reserve {
        operation: String,
    },
    Submit {
        operation: String,
    },
    Reconcile {
        operation: String,
    },
    AdapterStatus {
        id: String,
    },
    AdapterCapabilities {
        agent: String,
    },
    HeartbeatAdapter {
        id: String,
        marker: String,
        producer: String,
        sequence: u64,
        #[serde(default)]
        observation: Option<tasks::heartbeat::Observation>,
    },
    RegisterAdapter {
        id: String,
        marker: String,
        producer: String,
    },
    BindReport {
        operation: String,
        token: String,
        producer: String,
        sequence: u64,
        input_operation: u64,
        message: tasks::model::AgentMessage,
    },
    Report {
        operation: String,
        token: String,
        producer: String,
        sequence: u64,
        input_operation: u64,
        kind: ResponseKind,
        #[serde(default)]
        message: Option<tasks::model::AgentMessage>,
    },
    Wait {
        operation: String,
    },
    Abandon {
        operation: String,
    },
    Cancel {
        id: String,
    },
    Stop {
        id: String,
    },
    DiscardPrepared {
        operation: String,
    },
    Forget {
        id: String,
    },
}
impl Request {
    fn execute(self, root: &Path, runtime: &Path) -> Result<Value> {
        match self {
            Self::Overview {} => crate::dashboard::overview(root, runtime),
            Self::Recover { after } => tasks::recovery::resume(root, after.as_deref()),
            Self::Handoff {
                id,
                operation,
                predecessors,
                text,
                timeout_ms,
            } => tasks::handoff::prepare(root, &id, &operation, predecessors, text, timeout_ms),
            Self::Verify { id, source } => tasks::verify::run(root, &id, &source),
            Self::SourceCollect {
                id,
                source,
                revision,
            } => tasks::source::collect(root, &id, &source, revision),
            Self::SourceInspect { source } => tasks::source::inspect(root, &source),
            Self::SourceFile { source, path } => tasks::source::file(root, &source, &path),
            Self::ChangesCollect { id, changes } => tasks::changes::collect(root, &id, &changes),
            Self::ChangesInspect { changes } => tasks::changes::inspect(root, &changes),
            Self::Result { id } => tasks::result::read(root, &id),
            Self::RequireArtifact { id, artifact, path } => {
                tasks::artifact::require(root, &id, &artifact, path)
            }
            Self::ArtifactCollect {
                id,
                artifact,
                path,
                requirement,
            } => tasks::artifact::collect(root, &id, &artifact, path, requirement),
            Self::ArtifactInspect { artifact } => tasks::artifact::inspect(root, &artifact),
            Self::RequireCheck { id, check, argv } => {
                tasks::check::require(root, &id, &check, argv)
            }
            Self::Check {
                id,
                check,
                timeout_ms,
                requirement,
                source,
                artifacts,
                argv,
            } => tasks::check::run(
                root,
                tasks::check::Run {
                    task: id,
                    id: check,
                    argv,
                    timeout_ms,
                    requirement,
                    source,
                    artifacts,
                },
            ),
            Self::CheckInspect { check } => tasks::check::inspect(root, &check),
            Self::WorktreeCreate {
                id,
                repo,
                branch,
                base,
            } => tasks::worktree::create(
                root,
                tasks::worktree::Create {
                    id,
                    repo,
                    branch,
                    base,
                },
            ),
            Self::WorktreeRemove { id, force } => tasks::worktree::remove(root, &id, force),
            Self::WorktreeList {} => tasks::worktree::list(root),
            Self::CodexStart {
                id,
                title,
                instance,
                workspace,
                cwd,
                worktree,
                argv,
            } => tasks::codex::worker::start(
                root,
                tasks::launch::Start {
                    id,
                    title,
                    instance,
                    workspace,
                    cwd,
                    worktree,
                    argv,
                    runtime: runtime.into(),
                    agent: Some("codex".into()),
                    integration: None,
                },
            ),
            Self::CodexSubmit {
                id,
                operation,
                text,
                timeout_ms,
            } => tasks::codex::state::prepare(root, &id, &operation, &text, timeout_ms),
            Self::CodexInspect { operation } => tasks::codex::state::inspect(root, &operation),
            Self::CodexRecreate {
                operation,
                request,
                timeout_ms,
            } => tasks::codex::state::request_control(
                root,
                &operation,
                &request,
                tasks::codex::state::ControlKind::Recreate,
                timeout_ms,
            ),
            Self::CodexReconcile {
                operation,
                request,
                timeout_ms,
            } => tasks::codex::state::request_control(
                root,
                &operation,
                &request,
                tasks::codex::state::ControlKind::Read,
                timeout_ms,
            ),
            Self::CodexInterrupt {
                operation,
                request,
                timeout_ms,
            } => tasks::codex::state::request_control(
                root,
                &operation,
                &request,
                tasks::codex::state::ControlKind::Interrupt,
                timeout_ms,
            ),
            Self::WorktreeInspect { id } => tasks::worktree::inspect(root, &id),
            Self::WorktreeReconcile { id } => tasks::worktree::reconcile(root, &id),
            Self::Start {
                id,
                title,
                instance,
                workspace,
                cwd,
                worktree,
                argv,
                agent,
                integration,
                headless_agent,
            } => tasks::launch::start(
                root,
                tasks::launch::Start {
                    argv: tasks::headless::argv(
                        headless_agent.as_deref(),
                        argv,
                        integration.is_some(),
                    )?,
                    integration,
                    id,
                    title,
                    instance,
                    workspace,
                    cwd,
                    worktree,
                    agent,
                    runtime: runtime.into(),
                },
            ),
            Self::Resume {
                id,
                operation,
                instance,
            } => tasks::resume::run(root, &id, &operation, &instance),
            Self::LaunchReconcile { id } => tasks::launch::reconcile(root, &id),
            Self::Adopt {
                id,
                title,
                instance,
                workspace,
                pane,
                agent,
            } => tasks::adopt(
                root,
                tasks::Adopt {
                    id,
                    title,
                    instance,
                    workspace,
                    pane,
                    agent,
                    runtime: runtime.into(),
                },
            ),
            Self::GroupCreate {
                id,
                concurrency,
                members,
            } => tasks::group::create(root, &id, concurrency, members),
            Self::GroupInspect { id } => tasks::group::inspect(root, &id),
            Self::GroupList {} => tasks::group::list(root),
            Self::GroupStep { id } => tasks::group::step(root, &id),
            Self::GroupRun { id } => tasks::group::configure(root, &id, true),
            Self::GroupPause { id } => tasks::group::configure(root, &id, false),
            Self::GroupCancel { id } => tasks::group::cancel(root, &id),
            Self::List {} => tasks::list(root),
            Self::Inspect { id } => tasks::inspect(root, &id),
            Self::Prepare {
                id,
                operation,
                text,
                timeout_ms,
            } => tasks::prepare(root, &id, &operation, &text, timeout_ms),
            Self::Reserve { operation } => submit::run(root, &operation, submit::Action::Reserve),
            Self::Submit { operation } => submit::run(root, &operation, submit::Action::Submit),
            Self::Reconcile { operation } => {
                submit::run(root, &operation, submit::Action::Reconcile)
            }
            Self::AdapterStatus { id } => tasks::integration::status(root, &id),
            Self::AdapterCapabilities { agent } => tasks::capabilities::inspect(&agent),
            Self::HeartbeatAdapter {
                id,
                marker,
                producer,
                sequence,
                observation,
            } => tasks::heartbeat::record(root, &id, &marker, &producer, sequence, observation),
            Self::RegisterAdapter {
                id,
                marker,
                producer,
            } => tasks::integration::register(root, &id, &marker, &producer),
            Self::BindReport {
                operation,
                token,
                producer,
                sequence,
                input_operation,
                message,
            } => tasks::binding::run(
                root,
                &operation,
                &token,
                tasks::binding::Bind {
                    producer,
                    sequence,
                    input_operation,
                    message,
                },
            ),
            Self::Report {
                operation,
                token,
                producer,
                sequence,
                input_operation,
                kind,
                message,
            } => tasks::wait::report(
                root,
                &operation,
                &token,
                tasks::wait::Report {
                    producer,
                    sequence,
                    input_operation,
                    kind,
                    message,
                },
            ),
            Self::Wait { operation } => tasks::wait::check(root, &operation),
            Self::Abandon { operation } => tasks::abandon(root, &operation),
            Self::Cancel { id } => tasks::cancel(root, &id),
            Self::Stop { id } => tasks::stop::run(root, &id),
            Self::DiscardPrepared { operation } => tasks::discard_prepared(root, &operation),
            Self::Forget { id } => tasks::forget(root, &id),
        }
    }
}
struct Job {
    request: Request,
    id: u64,
    deadline: Instant,
    reply: mpsc::SyncSender<Value>,
}
pub(crate) struct Worker {
    lanes: Vec<Lane>,
    stop: Arc<AtomicBool>,
    scheduler: Option<std::thread::JoinHandle<()>>,
}
struct Lane {
    sender: mpsc::SyncSender<Job>,
    thread: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}
impl Lane {
    pub fn new(
        root: Option<PathBuf>,
        runtime: PathBuf,
        instance: String,
        stop: Arc<AtomicBool>,
        name: &str,
        sender: mpsc::SyncSender<Job>,
        receiver: Arc<Mutex<mpsc::Receiver<Job>>>,
    ) -> Result<Self> {
        let worker_stop = Arc::clone(&stop);
        let recovery_lane = name == "zor-tasks-0";
        let thread = std::thread::Builder::new().name(name.into()).spawn(move || {
            let mut recovery_after = None;
            let mut startup = tasks::recovery::Startup::default();
            let mut recovery_at = Instant::now();
            while !worker_stop.load(Ordering::Acquire) {
                if recovery_lane && Instant::now() >= recovery_at {
                    if let Ok(value) = tasks::state_root(root.clone()).and_then(|root|
                        tasks::recovery::resume(&root, recovery_after.as_deref())) {
                        recovery_after = value.get("selected").and_then(Value::as_str).map(str::to_owned);
                    }
                    let _ = tasks::state_root(root.clone()).and_then(|root| startup.step(&root));
                    recovery_at = Instant::now() + Duration::from_secs(1);
                }
                // The receive guard is released before executing any task. The two check
                // workers share one queue so either idle worker can accept the next check.
                let pending = match receiver.lock() {
                    Ok(receiver) => receiver.recv_timeout(Duration::from_millis(100)),
                    Err(_) => break,
                };
                let job = match pending {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if worker_stop.load(Ordering::Acquire) { break; }
                let response = if Instant::now() >= job.deadline {
                    json!({"v":1,"id":job.id,"status":"failed","service_instance":instance,"error":"task-expired-before-start"})
                } else {
                    match tasks::state_root(root.clone()).and_then(|root| job.request.execute(&root, &runtime)) {
                        Ok(value) => json!({"v":1,"id":job.id,"status":"completed","service_instance":instance,"value":value}),
                        Err(error) => json!({"v":1,"id":job.id,"status":"failed","service_instance":instance,
                            "error":if error.is::<tasks::store::Busy>() {"task-busy"} else {"task-failed"},
                            "message":format!("{error:#}").chars().take(512).collect::<String>()}),
                    }
                };
                let _ = job.reply.try_send(response);
            }
        })?;
        Ok(Self {
            sender,
            thread: Some(thread),
            stop,
        })
    }
    fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
}
impl Worker {
    pub fn new(
        root: Option<PathBuf>,
        runtime: PathBuf,
        instance: String,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let mut lanes = Vec::new();
        for (name, capacity, count) in [("zor-tasks", 8, 1), ("zor-checks", 4, 2)] {
            let (sender, receiver) = mpsc::sync_channel::<Job>(capacity);
            let receiver = Arc::new(Mutex::new(receiver));
            for index in 0..count {
                lanes.push(Lane::new(
                    root.clone(),
                    runtime.clone(),
                    instance.clone(),
                    Arc::clone(&stop),
                    &format!("{name}-{index}"),
                    sender.clone(),
                    Arc::clone(&receiver),
                )?);
            }
        }
        let scheduler_stop = Arc::clone(&stop);
        let scheduler = std::thread::Builder::new()
            .name("zor-groups".into())
            .spawn(move || {
                let mut after = None;
                let mut pending = None;
                let mut at = Instant::now();
                while !scheduler_stop.load(Ordering::Acquire) {
                    if Instant::now() >= at {
                        if let Ok(selected) = tasks::state_root(root.clone()).and_then(|root| {
                            tasks::group::advance(&root, after.as_deref(), &mut pending)
                        }) && selected.is_some()
                        {
                            after = selected;
                        }
                        at = Instant::now() + Duration::from_secs(1);
                    }
                    std::thread::park_timeout(Duration::from_millis(100));
                }
            })?;
        Ok(Self {
            lanes,
            stop,
            scheduler: Some(scheduler),
        })
    }
    pub fn submit(
        &self,
        request: Request,
        id: u64,
        deadline: Instant,
    ) -> Result<mpsc::Receiver<Value>> {
        let (reply, response) = mpsc::sync_channel(1);
        let is_check = matches!(&request, Request::Check { .. });
        let job = Job {
            request,
            id,
            deadline,
            reply,
        };
        let lane = self
            .lanes
            .get(usize::from(is_check))
            .ok_or_else(|| anyhow::anyhow!("task worker unavailable"))?;
        lane.sender
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("task worker unavailable or queue full"))?;
        Ok(response)
    }
    pub fn is_finished(&self) -> bool {
        self.lanes.iter().any(Lane::is_finished)
            || self
                .scheduler
                .as_ref()
                .is_none_or(|thread| thread.is_finished())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.scheduler.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for Lane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_dispatch_needs_no_journal_and_rejects_unknown_agents() {
        let absent = Path::new("/no-journal-needed-for-capabilities");
        for agent in ["claude", "codex", "opencode"] {
            let request: Request = serde_json::from_value(json!({
                "action":"adapter-capabilities", "agent":agent
            }))
            .expect("capability request");
            assert_eq!(
                request
                    .execute(absent, absent)
                    .expect("capability result")
                    .get("agent"),
                Some(&json!(agent))
            );
        }
        let error = Request::AdapterCapabilities {
            agent: "unknown".into(),
        }
        .execute(absent, absent)
        .expect_err("unknown agent rejected");
        assert!(error.to_string().starts_with("unsupported-agent:"));
    }

    #[test]
    fn native_message_schema_requires_complete_identity() {
        let request = json!({"action":"bind-report","operation":"op","token":"token",
            "producer":"lifetime","sequence":1,"input_operation":1,
            "message":{"session":"native-session","id":"message"}});
        assert!(serde_json::from_value::<Request>(request.clone()).is_ok());
        for message in [
            json!(null),
            json!({"id":"message"}),
            json!({"session":"native-session","id":"message","unexpected":true}),
        ] {
            let mut invalid = request.clone();
            *invalid.get_mut("message").expect("message field") = message;
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
    }
    #[test]
    fn expired_queued_task_has_no_storage_effect_and_schema_is_strict() {
        assert!(
            serde_json::from_value::<Request>(json!({"action":"list","unexpected":true})).is_err()
        );
        assert!(serde_json::from_str::<Request>(r#"{"action":"check","id":"task","check":"check","timeout_ms":1000,"source":"source","argv":["/usr/bin/true"],"artifacts":{"report":"one","report":"two"}}"#).is_err());
        let root = std::env::temp_dir().join(format!(
            "zor-expired-{}",
            tasks::store::nonce().expect("nonce")
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker::new(
            Some(root.clone()),
            root.join("unused-runtime"),
            "instance".into(),
            stop,
        )
        .expect("worker");
        let reply = worker
            .submit(Request::List {}, 7, Instant::now())
            .expect("enqueue")
            .recv_timeout(Duration::from_secs(2))
            .expect("expired reply");
        assert_eq!(
            reply.get("error").and_then(Value::as_str),
            Some("task-expired-before-start")
        );
        assert!(!root.exists());
        let reply = worker
            .submit(
                Request::Check {
                    id: "absent".into(),
                    check: "expired".into(),
                    timeout_ms: 1000,
                    requirement: None,
                    source: None,
                    artifacts: Default::default(),
                    argv: vec!["/bin/true".into()],
                },
                8,
                Instant::now(),
            )
            .expect("enqueue expired check")
            .recv_timeout(Duration::from_secs(2))
            .expect("expired check reply");
        assert_eq!(
            reply.get("error").and_then(Value::as_str),
            Some("task-expired-before-start")
        );
        assert!(!root.exists());
    }
}
