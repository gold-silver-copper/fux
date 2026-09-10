//! Durable coordination identities. Terminal observations are never task outcomes.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub const MAX_TASKS: usize = 128;
pub const MAX_ATTEMPTS: usize = 512;
pub const MAX_PROMPTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub runtime: PathBuf,
    pub instance: String,
    pub workspace: String,
    pub stream: u64,
    pub pane: u32,
    pub pid: Option<u32>,
}
impl Target {
    /// Routing directories are not part of the identity of a live pane/process.
    pub(crate) fn identity(&self) -> (&str, &str, u64, u32, Option<u32>) {
        (
            &self.instance,
            &self.workspace,
            self.stream,
            self.pane,
            self.pid,
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ownership {
    Adopted,
    Managed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub id: String,
    pub target: Target,
    pub agent: Option<String>,
    pub ownership: Ownership,
    #[serde(default)]
    pub launch: Option<String>,
    pub created_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskOutcome {
    Open,
    Cancelled,
    Failed,
    Verified,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub requested_runtime: PathBuf,
    pub title: String,
    pub created_ms: u64,
    pub outcome: TaskOutcome,
    pub attempt: String,
    #[serde(default, deserialize_with = "unique_records")]
    pub required_checks: BTreeMap<String, Vec<String>>,
    #[serde(default, deserialize_with = "unique_records")]
    pub required_artifacts: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptState {
    Active,
    NeedsInput,
    Lost,
    Uncertain,
    Finished,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub id: String,
    pub task: String,
    pub session: String,
    pub state: AttemptState,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    Prepared,
    Reserved,
    Submitting,
    Queued,
    Delivered,
    Failed,
    Uncertain,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WaitOutcome {
    Pending,
    NeedsInput,
    ResponseObserved,
    ProcessExited,
    TimedOut,
    Cancelled,
    Uncertain,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub operation: u64,
    pub revision: u64,
    pub input_sequence: u64,
    pub expires_server_ms: u64,
    pub bytes_written: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseKind {
    NeedsInput,
    ResponseObserved,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseReport {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub kind: ResponseKind,
    pub received_ms: u64,
    #[serde(default)]
    pub message: Option<AgentMessage>,
}
/// Native application ancestry, scoped by a retained managed-session binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessage {
    pub session: String,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportBinding {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub message: AgentMessage,
    pub bound_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationKind {
    Opencode,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub kind: IntegrationKind,
    pub root: PathBuf,
    pub binary: PathBuf,
    pub plugin: PathBuf,
    pub socket: PathBuf,
    pub producer: Option<String>,
    pub registered_ms: Option<u64>,
    /// Actual agent storage environment observed through the registered adapter.
    #[serde(default)]
    pub storage_environment: Option<BTreeMap<String, Option<String>>>,
    #[serde(default)]
    pub retired: Vec<RetiredProducer>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredProducer {
    pub producer: String,
    pub registered_ms: u64,
    pub retired_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptArm {
    pub producer: String,
    pub input_operation: u64,
    pub acknowledged: bool,
    /// Monotonic: set durably before any input-submit request, never reset by status lookup.
    #[serde(default)]
    pub input_started: bool,
    #[serde(default)]
    pub disarm_requested: bool,
    #[serde(default)]
    pub disarmed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub id: String,
    pub attempt: String,
    pub text: String,
    #[serde(default)]
    pub handoff: Option<Handoff>,
    pub created_ms: u64,
    pub deadline_ms: u64,
    pub delivery: Delivery,
    pub receipt: Option<Receipt>,
    pub wait: WaitOutcome,
    /// Explicitly released coordination; never evidence that queued input was cancelled.
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub report_token: Option<String>,
    #[serde(default)]
    pub response: Option<ResponseReport>,
    #[serde(default)]
    pub report_binding: Option<ReportBinding>,
    #[serde(default)]
    pub arm: Option<PromptArm>,
    #[serde(default)]
    pub wait_problem: Option<String>,
    #[serde(default)]
    pub wait_exit_status: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub state_directory: PathBuf,
    #[serde(deserialize_with = "unique_records")]
    pub predecessors: BTreeMap<String, u64>,
    pub instruction: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchPhase {
    Prepared,
    Submitting,
    Attached,
    Closed,
    Uncertain,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchFinal {
    pub exit_status: Option<u32>,
    pub text: String,
    pub truncated: bool,
    pub input_sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub id: String,
    /// Historical launch owner; absent for the task's current launch keyed by task ID.
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub resume: Option<Resume>,
    pub title: String,
    pub agent: Option<String>,
    pub requested_runtime: PathBuf,
    pub runtime: PathBuf,
    pub requested_cwd: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub worktree: Option<String>,
    pub instance: String,
    pub workspace: String,
    pub stream: u64,
    pub event_sequence: u64,
    pub argv: Vec<String>,
    pub marker: String,
    #[serde(default)]
    pub integration: Option<Integration>,
    #[serde(default)]
    pub final_evidence: Option<LaunchFinal>,
    #[serde(default)]
    pub stop_requested: bool,
    pub created_ms: u64,
    pub phase: LaunchPhase,
    pub pane: Option<u32>,
    pub session: Option<String>,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    pub operation: String,
    pub previous_attempt: String,
    pub native_session: String,
    pub environment: BTreeMap<String, Option<String>>,
}

impl Launch {
    pub fn task_id(&self) -> &str {
        self.task.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreePhase {
    Allocating,
    Prepared,
    Creating,
    Ready,
    Uncertain,
    Removing,
    Removed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worktree {
    pub id: String,
    pub requested_repo: PathBuf,
    pub repo: PathBuf,
    pub common: PathBuf,
    pub parent: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
    pub commit: String,
    pub repo_dev: u64,
    pub repo_ino: u64,
    pub common_dev: u64,
    pub common_ino: u64,
    pub parent_dev: u64,
    pub parent_ino: u64,
    #[serde(default)]
    pub checkout_identity: Option<(u64, u64)>,
    pub phase: WorktreePhase,
    #[serde(default)]
    pub remove_force: Option<bool>,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckPhase {
    Submitted,
    Finished,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub source: Option<String>,
    #[serde(default, deserialize_with = "unique_records")]
    pub artifact_requests: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "unique_records")]
    pub artifact_problems: BTreeMap<String, String>,
    pub requirement: Option<String>,
    pub created_generation: u64,
    pub task: String,
    pub attempt: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub timeout_ms: u64,
    pub created_ms: u64,
    pub phase: CheckPhase,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub generation: u64,
    #[serde(deserialize_with = "unique_records")]
    pub tasks: BTreeMap<String, Task>,
    #[serde(deserialize_with = "unique_records")]
    pub sessions: BTreeMap<String, Session>,
    #[serde(deserialize_with = "unique_records")]
    pub attempts: BTreeMap<String, Attempt>,
    #[serde(deserialize_with = "unique_records")]
    pub prompts: BTreeMap<String, Prompt>,
    #[serde(default, deserialize_with = "unique_records")]
    pub launches: BTreeMap<String, Launch>,
    #[serde(default, deserialize_with = "unique_records")]
    pub worktrees: BTreeMap<String, Worktree>,
    #[serde(default, deserialize_with = "unique_records")]
    pub checks: BTreeMap<String, Check>,
    #[serde(default, deserialize_with = "unique_records")]
    pub artifacts: BTreeMap<String, Artifact>,
    #[serde(default, deserialize_with = "unique_records")]
    pub changes: BTreeMap<String, Changes>,
    #[serde(default, deserialize_with = "unique_records")]
    pub sources: BTreeMap<String, Source>,
    #[serde(default, deserialize_with = "unique_records")]
    pub verifications: BTreeMap<String, Verification>,
    #[serde(default, deserialize_with = "unique_records")]
    pub groups: BTreeMap<String, super::group::Group>,
    #[serde(default, deserialize_with = "unique_records")]
    pub native_workers: BTreeMap<String, super::codex::state::Worker>,
    #[serde(default, deserialize_with = "unique_records")]
    pub native_turns: BTreeMap<String, super::codex::state::Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    pub task: String,
    pub attempt: String,
    pub source: String,
    pub created_ms: u64,
    pub created_generation: u64,
    #[serde(deserialize_with = "unique_records")]
    pub checks: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique_records")]
    pub artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub commit_bytes: Vec<u8>,
    pub id: String,
    pub task: String,
    pub attempt: String,
    pub worktree: String,
    pub revision: String,
    pub commit: String,
    pub tree: String,
    pub created_ms: u64,
    pub created_generation: u64,
    pub files: Vec<SourceFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub path: String,
    pub executable: bool,
    pub blob: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    pub status: String,
    pub path: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Changes {
    pub id: String,
    pub task: String,
    pub attempt: String,
    pub worktree: String,
    pub base_commit: String,
    pub head: String,
    pub created_ms: u64,
    pub created_generation: u64,
    pub committed: Vec<ChangedFile>,
    pub working: Vec<ChangedFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub check: Option<String>,
    pub source: Option<String>,
    pub requirement: Option<String>,
    pub created_generation: u64,
    pub task: String,
    pub attempt: String,
    pub path: String,
    pub created_ms: u64,
    pub bytes: Vec<u8>,
}
pub(crate) fn unique_records<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Records<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Records<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded records with unique valid IDs")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut records = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if !id(&key) || records.len() >= MAX_PROMPTS || records.contains_key(&key) {
                    return Err(serde::de::Error::custom(
                        "invalid, duplicate or excessive journal records",
                    ));
                }
                records.insert(key, map.next_value()?);
            }
            Ok(records)
        }
    }
    deserializer.deserialize_map(Records(std::marker::PhantomData))
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            version: 1,
            generation: 0,
            tasks: BTreeMap::new(),
            sessions: BTreeMap::new(),
            attempts: BTreeMap::new(),
            prompts: BTreeMap::new(),
            launches: BTreeMap::new(),
            worktrees: BTreeMap::new(),
            checks: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            changes: BTreeMap::new(),
            sources: BTreeMap::new(),
            verifications: BTreeMap::new(),
            groups: BTreeMap::new(),
            native_workers: BTreeMap::new(),
            native_turns: BTreeMap::new(),
        }
    }
}

pub fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
pub fn workspace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !matches!(value, "." | "..")
        && !value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}
impl Journal {
    fn launch_for_attempt(&self, attempt: &str) -> Option<&Launch> {
        self.attempts
            .get(attempt)
            .and_then(|attempt| self.sessions.get(&attempt.session))
            .and_then(|session| session.launch.as_ref())
            .and_then(|launch| self.launches.get(launch))
    }
    pub fn check_reserve_bytes(&self) -> usize {
        self.checks
            .values()
            .filter(|check| check.phase == CheckPhase::Submitted)
            .map(|check| 65536 + check.artifact_requests.len() * 300000)
            .sum::<usize>()
            + self
                .native_turns
                .values()
                .map(super::codex::state::Entry::remaining_reservation)
                .sum::<usize>()
            + self
                .native_workers
                .values()
                .map(super::codex::state::Worker::remaining_reservation)
                .sum::<usize>()
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.version == 1, "unsupported zor journal version");
        super::group::validate(self)?;
        super::check_artifacts::validate(self)?;
        super::codex::state::validate(self)?;
        anyhow::ensure!(
            self.changes.len() <= 128,
            "change evidence record limit exceeded"
        );
        let mut change_generations = std::collections::BTreeSet::new();
        for (key, changes) in &self.changes {
            super::changes::validate(changes)?;
            anyhow::ensure!(
                id(key)
                    && key == &changes.id
                    && changes.created_generation > 0
                    && changes.created_generation <= self.generation
                    && change_generations.insert(changes.created_generation)
                    && self
                        .attempts
                        .get(&changes.attempt)
                        .is_some_and(|attempt| attempt.task == changes.task)
                    && self
                        .launch_for_attempt(&changes.attempt)
                        .is_some_and(|launch| launch.worktree.as_ref() == Some(&changes.worktree))
                    && self
                        .worktrees
                        .get(&changes.worktree)
                        .is_some_and(|tree| tree.commit == changes.base_commit),
                "invalid task change evidence association"
            );
        }
        anyhow::ensure!(
            self.artifacts.len() + super::check_artifacts::pending_count(self) <= 128
                && self
                    .artifacts
                    .values()
                    .map(|a| a.bytes.len())
                    .sum::<usize>()
                    + super::check_artifacts::pending_count(self) * 65536
                    <= 524288,
            "artifact journal limit exceeded"
        );
        let mut artifact_generations = std::collections::BTreeMap::new();
        for (key, artifact) in &self.artifacts {
            anyhow::ensure!(
                artifact.created_generation > 0 && artifact.created_generation <= self.generation,
                "invalid artifact collection generation"
            );
            if let Some(previous) =
                artifact_generations.insert(artifact.created_generation, artifact.check.as_deref())
            {
                anyhow::ensure!(
                    previous.is_some() && previous == artifact.check.as_deref(),
                    "invalid artifact collection generation"
                );
            }
            if let Some(name) = &artifact.requirement {
                anyhow::ensure!(
                    id(name)
                        && self
                            .tasks
                            .get(&artifact.task)
                            .and_then(|task| task.required_artifacts.get(name))
                            == Some(&artifact.path),
                    "artifact evidence does not match required path"
                );
            }
            anyhow::ensure!(
                id(key)
                    && key == &artifact.id
                    && artifact.bytes.len() <= 65536
                    && super::artifact::valid_path(&artifact.path)
                    && self
                        .attempts
                        .get(&artifact.attempt)
                        .is_some_and(|attempt| attempt.task == artifact.task)
                    && self.launch_for_attempt(&artifact.attempt).is_some(),
                "invalid artifact record"
            );
        }
        for task in self.tasks.values() {
            anyhow::ensure!(
                task.required_artifacts.len() <= 32,
                "required artifact limit exceeded"
            );
            anyhow::ensure!(
                task.required_artifacts.is_empty() || self.launches.contains_key(&task.id),
                "required artifacts need a managed task"
            );
            for (name, path) in &task.required_artifacts {
                anyhow::ensure!(
                    id(name) && super::artifact::valid_path(path),
                    "invalid required artifact name or path"
                );
            }
            anyhow::ensure!(
                task.required_checks.len() <= 32,
                "required check limit exceeded"
            );
            anyhow::ensure!(
                task.required_checks.is_empty() || self.launches.contains_key(&task.id),
                "required checks need a managed task"
            );
            for (check_id, argv) in &task.required_checks {
                anyhow::ensure!(id(check_id), "required check name is invalid");
                super::launch::validate_argv(argv)?;
            }
        }
        anyhow::ensure!(
            self.sources.len() <= 128
                && self
                    .sources
                    .values()
                    .flat_map(|source| &source.files)
                    .map(|file| file.bytes.len())
                    .sum::<usize>()
                    <= 524288,
            "source journal limit exceeded"
        );
        let mut source_generations = std::collections::BTreeSet::new();
        for (key, source) in &self.sources {
            super::source::validate(source)?;
            anyhow::ensure!(
                id(key)
                    && key == &source.id
                    && source.created_generation > 0
                    && source.created_generation <= self.generation
                    && source_generations.insert(source.created_generation)
                    && self
                        .attempts
                        .get(&source.attempt)
                        .is_some_and(|attempt| attempt.task == source.task)
                    && self
                        .launch_for_attempt(&source.attempt)
                        .is_some_and(|launch| launch.worktree.as_ref() == Some(&source.worktree))
                    && self.worktrees.contains_key(&source.worktree),
                "invalid source association"
            );
        }
        anyhow::ensure!(self.checks.len() <= 128, "check record limit exceeded");
        let mut check_generations = std::collections::BTreeSet::new();
        for (key, check) in &self.checks {
            anyhow::ensure!(
                check.created_generation > 0
                    && check.created_generation <= self.generation
                    && check_generations.insert(check.created_generation),
                "invalid check submission generation"
            );
            if let Some(requirement) = &check.requirement {
                anyhow::ensure!(
                    id(requirement)
                        && self
                            .tasks
                            .get(&check.task)
                            .and_then(|task| task.required_checks.get(requirement))
                            == Some(&check.argv),
                    "check evidence does not match required command"
                );
            }
            anyhow::ensure!(
                id(key)
                    && key == &check.id
                    && self
                        .attempts
                        .get(&check.attempt)
                        .is_some_and(|attempt| attempt.task == check.task)
                    && self
                        .launch_for_attempt(&check.attempt)
                        .is_some_and(|launch| match &check.source {
                            None => launch.cwd == check.cwd,
                            Some(id) =>
                                self.sources
                                    .get(id)
                                    .is_some_and(|source| source.task == check.task
                                        && source.attempt == check.attempt
                                        && source.created_generation < check.created_generation)
                                    && super::source::valid_check_directory(&check.cwd),
                        })
                    && check.cwd.is_absolute()
                    && (1..=300_000).contains(&check.timeout_ms)
                    && check.stdout.len() <= 4096
                    && check.stderr.len() <= 4096
                    && check.problem.as_ref().is_none_or(|p| p.len() <= 512),
                "invalid check record"
            );
            super::launch::validate_argv(&check.argv)?;
            let terminal = check.phase == CheckPhase::Finished;
            anyhow::ensure!(
                if terminal {
                    (check.exit_code.is_some() != check.signal.is_some())
                        && check.exit_code.is_none_or(|code| (0..=255).contains(&code))
                        && check.signal.is_none_or(|signal| (1..=64).contains(&signal))
                        && check.problem.is_none()
                } else {
                    check.exit_code.is_none()
                        && check.signal.is_none()
                        && check.stdout.is_empty()
                        && check.stderr.is_empty()
                        && !check.truncated
                        && (check.phase == CheckPhase::Uncertain) == check.problem.is_some()
                },
                "invalid check outcome evidence"
            );
        }
        let pending_sessions = self
            .launches
            .values()
            .filter(|launch| launch.session.is_none())
            .count();
        anyhow::ensure!(
            self.tasks.len()
                + self
                    .launches
                    .values()
                    .filter(|launch| !matches!(
                        launch.phase,
                        LaunchPhase::Attached | LaunchPhase::Closed
                    ))
                    .count()
                <= MAX_TASKS
                && self.worktrees.len() <= MAX_TASKS
                && self.launches.len() <= MAX_ATTEMPTS
                && self
                    .launches
                    .values()
                    .filter(|launch| launch.task.is_none())
                    .count()
                    <= MAX_TASKS
                && self.sessions.len() + pending_sessions <= MAX_TASKS
                && self.attempts.len() + pending_sessions <= MAX_ATTEMPTS
                && self.prompts.len() <= MAX_PROMPTS,
            "zor journal record limit exceeded"
        );
        let mut worktree_paths = std::collections::BTreeSet::new();
        for (key, tree) in &self.worktrees {
            anyhow::ensure!(
                !matches!(
                    tree.phase,
                    WorktreePhase::Ready | WorktreePhase::Removing | WorktreePhase::Removed
                ) || tree.checkout_identity.is_some(),
                "confirmed worktree checkout identity missing"
            );
            anyhow::ensure!(
                matches!(tree.phase, WorktreePhase::Removing | WorktreePhase::Removed)
                    == tree.remove_force.is_some(),
                "invalid worktree removal intent"
            );
            anyhow::ensure!(worktree_paths.insert(&tree.path), "duplicate worktree path");
            anyhow::ensure!(
                id(key)
                    && *key == tree.id
                    && [
                        &tree.repo,
                        &tree.requested_repo,
                        &tree.common,
                        &tree.parent,
                        &tree.path
                    ]
                    .iter()
                    .all(|p| p.is_absolute()
                        && p.to_str()
                            .is_some_and(|s| s.len() <= 4096 && !s.chars().any(char::is_control)))
                    && tree.path == tree.parent.join("tree")
                    && tree
                        .parent
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.strip_prefix("worktree-"))
                        .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    && !tree.branch.is_empty()
                    && tree.branch.len() <= 128
                    && !tree.branch.starts_with('-')
                    && !tree.branch.chars().any(char::is_control)
                    && !tree.base.is_empty()
                    && tree.base.len() <= 256
                    && !tree.base.chars().any(char::is_control)
                    && matches!(tree.commit.len(), 40 | 64)
                    && tree.commit.bytes().all(|b| b.is_ascii_hexdigit())
                    && tree.problem.as_ref().is_none_or(|p| p.len() <= 512),
                "invalid worktree record"
            );
        }
        for (key, session) in &self.sessions {
            anyhow::ensure!(id(key) && *key == session.id, "invalid session identity");
            let target = &session.target;
            anyhow::ensure!(
                target.runtime.is_absolute()
                    && workspace(&target.workspace)
                    && !target.instance.is_empty()
                    && target.instance.len() <= 128
                    && target
                        .instance
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    && target.stream != 0
                    && target.pane != 0
                    && target.pid != Some(0),
                "invalid pane target"
            );
            match session.ownership {
                Ownership::Adopted => anyhow::ensure!(
                    session.launch.is_none() && target.pid.is_some(),
                    "adopted session has launch authority"
                ),
                Ownership::Managed => {
                    let launch = session
                        .launch
                        .as_ref()
                        .and_then(|id| self.launches.get(id))
                        .ok_or_else(|| anyhow::anyhow!("managed session launch missing"))?;
                    anyhow::ensure!(
                        matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed)
                            && (target.pid.is_some() || launch.phase == LaunchPhase::Closed)
                            && launch.session.as_deref() == Some(key)
                            && launch.pane == Some(target.pane)
                            && launch.runtime == target.runtime
                            && launch.instance == target.instance
                            && launch.workspace == target.workspace
                            && launch.stream == target.stream,
                        "managed launch target mismatch"
                    );
                }
            }
            if let Some(agent) = &session.agent {
                crate::osc::AgentId::new(agent)?;
            }
        }
        for (key, task) in &self.tasks {
            anyhow::ensure!(
                task.requested_runtime.is_absolute()
                    && task.requested_runtime.as_os_str().len() <= 4096,
                "invalid original adoption runtime"
            );
            anyhow::ensure!(
                id(key)
                    && *key == task.id
                    && !task.title.is_empty()
                    && task.title.len() <= 512
                    && !task.title.chars().any(char::is_control),
                "invalid task identity/title"
            );
            let attempt = self
                .attempts
                .get(&task.attempt)
                .ok_or_else(|| anyhow::anyhow!("task attempt missing"))?;
            anyhow::ensure!(
                attempt.task == task.id,
                "task attempt relationship mismatch"
            );
            anyhow::ensure!(
                (task.outcome == TaskOutcome::Verified) == self.verifications.contains_key(key),
                "verified outcome requires its sealed verification record"
            );
        }
        let mut managed_attempt_sessions = std::collections::BTreeSet::new();
        for (key, attempt) in &self.attempts {
            anyhow::ensure!(
                id(key)
                    && *key == attempt.id
                    && self.tasks.contains_key(&attempt.task)
                    && self.sessions.contains_key(&attempt.session),
                "invalid attempt relationship"
            );
            if let Some(launch) = self.launch_for_attempt(key) {
                anyhow::ensure!(
                    launch.task_id() == attempt.task
                        && managed_attempt_sessions.insert(&attempt.session),
                    "managed attempt owner/session mismatch"
                );
            }
        }
        for (key, launch) in &self.launches {
            if let Some(id) = &launch.worktree {
                let tree = self
                    .worktrees
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("launch worktree missing"))?;
                anyhow::ensure!(
                    launch.cwd == tree.path && launch.requested_cwd == tree.path,
                    "launch worktree path mismatch"
                );
            }
            anyhow::ensure!(
                id(key)
                    && *key == launch.id
                    && launch.task.as_ref().is_none_or(|task| id(task)
                        && task != key
                        && self
                            .launches
                            .get(task)
                            .is_some_and(|current| current.task.is_none())
                        && (matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed)
                            || launch.resume.is_some()))
                    && workspace(&launch.workspace)
                    && !launch.instance.is_empty()
                    && launch.instance.len() <= 128
                    && launch
                        .instance
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    && launch.stream != 0
                    && launch.runtime.is_absolute()
                    && launch.requested_runtime.is_absolute()
                    && launch.cwd.is_absolute()
                    && launch.requested_cwd.is_absolute()
                    && !launch.title.is_empty()
                    && launch.title.len() <= 512
                    && !launch.title.chars().any(char::is_control)
                    && launch.marker.len() == 32
                    && launch.marker.bytes().all(|b| b.is_ascii_hexdigit())
                    && launch.pane != Some(0)
                    && launch.problem.as_ref().is_none_or(|p| p.len() <= 512),
                "invalid launch intent"
            );
            super::launch::validate_argv(&launch.argv)?;
            if let Some(agent) = &launch.agent {
                crate::osc::AgentId::new(agent)?;
            }
            anyhow::ensure!(
                (launch.phase == LaunchPhase::Closed) == launch.final_evidence.is_some()
                    && launch
                        .final_evidence
                        .as_ref()
                        .is_none_or(|e| e.text.len() <= 4096),
                "invalid launch final evidence"
            );
            if matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed) {
                let session = launch
                    .session
                    .as_ref()
                    .and_then(|id| self.sessions.get(id))
                    .ok_or_else(|| anyhow::anyhow!("attached launch session missing"))?;
                let task = self
                    .tasks
                    .get(launch.task_id())
                    .ok_or_else(|| anyhow::anyhow!("attached launch task missing"))?;
                anyhow::ensure!(
                    !launch.stop_requested
                        || matches!(task.outcome, TaskOutcome::Cancelled | TaskOutcome::Verified),
                    "managed stop requires cancelled or verified task coordination"
                );
                anyhow::ensure!(
                    session.launch.as_deref() == Some(key)
                        && session.ownership == Ownership::Managed
                        && self.attempts.values().any(|attempt| attempt.task == task.id
                            && attempt.session == session.id
                            && (launch.task.is_some() || attempt.id == task.attempt)),
                    "attached launch relationship mismatch"
                );
                if launch.phase == LaunchPhase::Closed {
                    anyhow::ensure!(
                        self.attempts.values().any(|attempt| attempt.task == task.id
                            && attempt.session == session.id
                            && attempt.state == AttemptState::Finished),
                        "closed launch attempt must be finished"
                    );
                }
            } else {
                anyhow::ensure!(
                    launch.session.is_none()
                        && !self.tasks.contains_key(key)
                        && !launch.stop_requested,
                    "pending launch already has a task/session"
                );
            }
        }
        let mut active_targets = std::collections::BTreeSet::new();
        for (key, prompt) in &self.prompts {
            super::handoff::validate_prompt(self, prompt)?;
            let verified = self
                .attempts
                .get(&prompt.attempt)
                .and_then(|attempt| self.tasks.get(&attempt.task))
                .is_some_and(|task| {
                    task.outcome == TaskOutcome::Verified && task.attempt == prompt.attempt
                });
            anyhow::ensure!(
                !prompt.released
                    || prompt.wait == WaitOutcome::Cancelled
                    || (verified
                        && matches!(
                            prompt.wait,
                            WaitOutcome::ResponseObserved | WaitOutcome::ProcessExited
                        )),
                "released prompt must retain cancelled or verified resolved coordination"
            );
            anyhow::ensure!(
                !verified || prompt.released,
                "verified task has unreleased prompt coordination"
            );
            anyhow::ensure!(
                id(key)
                    && *key == prompt.id
                    && self.attempts.contains_key(&prompt.attempt)
                    && !prompt.text.is_empty()
                    && prompt.text.len() <= 65536
                    && prompt.deadline_ms > prompt.created_ms
                    && prompt.deadline_ms - prompt.created_ms <= 24 * 60 * 60 * 1000,
                "invalid prompt identity/content/deadline"
            );
            anyhow::ensure!(
                prompt.delivery != Delivery::Prepared || prompt.receipt.is_none(),
                "prepared prompt cannot have a receipt"
            );
            if matches!(
                prompt.delivery,
                Delivery::Reserved | Delivery::Submitting | Delivery::Queued | Delivery::Delivered
            ) {
                anyhow::ensure!(
                    prompt.receipt.is_some(),
                    "delivery phase requires a receipt"
                );
            }
            let task = self
                .attempts
                .get(&prompt.attempt)
                .and_then(|attempt| self.tasks.get(&attempt.task))
                .ok_or_else(|| anyhow::anyhow!("prompt task missing"))?;
            anyhow::ensure!(
                task.outcome != TaskOutcome::Cancelled
                    || (prompt.released && prompt.wait == WaitOutcome::Cancelled),
                "cancelled task retains active prompt coordination"
            );
            if let Some(token) = &prompt.report_token {
                anyhow::ensure!(
                    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid prompt report token"
                );
            }
            anyhow::ensure!(
                prompt.wait_problem.as_ref().is_none_or(|p| p.len() <= 512),
                "wait problem too large"
            );
            if let Some(report) = &prompt.response {
                anyhow::ensure!(
                    prompt.report_token.is_some()
                        && id(&report.producer)
                        && report.sequence > 0
                        && report.received_ms >= prompt.created_ms
                        && report.received_ms < prompt.deadline_ms
                        && prompt
                            .receipt
                            .as_ref()
                            .is_some_and(|r| r.operation == report.input_operation),
                    "invalid prompt response evidence"
                );
            }
            if matches!(
                prompt.wait,
                WaitOutcome::NeedsInput | WaitOutcome::ResponseObserved
            ) {
                anyhow::ensure!(
                    prompt.delivery == Delivery::Delivered
                        && prompt.response.as_ref().is_some_and(|r| matches!(
                            (&prompt.wait, &r.kind),
                            (WaitOutcome::NeedsInput, ResponseKind::NeedsInput)
                                | (
                                    WaitOutcome::ResponseObserved,
                                    ResponseKind::ResponseObserved
                                )
                        )),
                    "semantic wait lacks delivered response evidence"
                );
            }
            anyhow::ensure!(
                prompt.wait != WaitOutcome::ProcessExited || prompt.wait_exit_status.is_some(),
                "process-exited wait lacks exit status"
            );
            if super::active(prompt) {
                let session = self
                    .attempts
                    .get(&prompt.attempt)
                    .and_then(|attempt| self.sessions.get(&attempt.session))
                    .ok_or_else(|| anyhow::anyhow!("prompt target missing"))?;
                anyhow::ensure!(
                    active_targets.insert(session.target.identity()),
                    "multiple active prompts for one pane target"
                );
            }
            if let Some(receipt) = &prompt.receipt {
                anyhow::ensure!(
                    receipt.operation != 0 && receipt.bytes_written <= 65536,
                    "invalid input receipt"
                );
            }
        }
        super::resume::validate(self)?;
        super::integration::validate(self)?;
        super::binding::validate(self)?;
        super::verify::validate(self)?;
        Ok(())
    }
}
