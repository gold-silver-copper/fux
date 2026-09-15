//! Components grouped by mutation owner (`docs/model.md`): identity (immutable), lifecycle
//! states with their transition tables, adapter completions, and presence markers.

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids::*;
use super::relations::*;

// ---------------------------------------------------------------------------------------------
// Entity kind markers
// ---------------------------------------------------------------------------------------------

macro_rules! marker {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq)]
        #[reflect(Component, Default)]
        pub struct $name;
    };
}

marker!(Task);
marker!(Attempt);
marker!(Prompt);
marker!(Operation);
marker!(Check);
marker!(
    /// Evidence of a finished or uncertain check (`ResultOf`).
    CheckResult
);
marker!(Source);
marker!(Artifact);
marker!(Group);
marker!(Worktree);
marker!(Machine);
marker!(Service);
marker!(ObservedAgent);
marker!(
    /// A plugin record of the host surface (prompt 4.4); `HostedPlugin` so it cannot be
    /// confused with `bevy_app::Plugin`.
    HostedPlugin
);
marker!(PluginAction);
marker!(
    /// Marker on every projection entity.
    ProjectionEntity
);

// ---------------------------------------------------------------------------------------------
// Presence markers
// ---------------------------------------------------------------------------------------------

marker!(
    /// `stop_requested: true` committed before requesting closure (TASKS.md:156).
    StopRequested
);
marker!(
    /// Missing live/final evidence, lost reply or unresolved arm; retained until resolved
    /// (TASKS.md:145, INTEGRATIONS.md:88-91).
    Uncertain
);
marker!(
    /// The endpoint proved replaced by another fux incarnation; persists through outages
    /// (RECOVERY.md:98-102).
    Lost
);
marker!(
    /// The agent asked for input (attempt `NeedsInput`, lifecycle-transitions.md:55-59).
    NeedsInput
);
marker!(
    /// `arm.input_started`: monotonic, persisted before any input-submit (TASKS.md:382-384).
    ArmInputStarted
);
marker!(
    /// A check or artifact requirement (CHECKS.md:33, ARTIFACTS.md:20).
    Required
);
marker!(
    /// A plugin that is enabled.
    Enabled
);

// ---------------------------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct Title(pub String);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
#[component(immutable)]
pub struct CreatedMs(pub u64);

/// When the task closed (`Clock` milliseconds); the archive age is measured from here.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
#[component(immutable)]
pub struct ClosedMs(pub u64);

/// Exactly one location selector per managed launch (TASKS.md:98, WORKTREES.md:23).
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub enum Location {
    Cwd(String),
    Worktree(String),
}

#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Cancelled,
    Verified,
}

/// Task lifecycle (`docs/model.md`): `Closed` is terminal, `Verified` only through the seal.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Open,
    Running,
    Blocked,
    Closed {
        outcome: TaskOutcome,
    },
}

impl TaskState {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Closed { .. })
    }

    /// The transition table of `docs/model.md`.
    pub fn may_become(self, next: Self) -> bool {
        match (self, next) {
            (Self::Closed { .. }, _) => false,
            (Self::Open, Self::Running) => true,
            (Self::Running, Self::Blocked | Self::Open) => true,
            (Self::Blocked, Self::Running | Self::Open) => true,
            (_, Self::Closed { .. }) => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Attempt
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    /// Observation only: no termination, cleanup or check authority (TASKS.md:68-69).
    Adopted,
    Managed,
}

/// The exact-process identity of service-ownership-contract.md:19-24: fux instance nonce, origin
/// route, pane id and the originally observed root pid (`None` until observed, TASKS.md:135-136).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct PaneHandle {
    pub instance: String,
    pub workspace: String,
    pub stream: String,
    pub pane: u64,
    pub pid: Option<u32>,
}

/// `ZOR_LAUNCH_ID` recovery metadata, not authority (TASKS.md:123-127).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct LaunchMarker(pub String);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    #[default]
    Pending,
    Launching,
    Live,
    Finishing,
    Finished,
}

impl AttemptState {
    pub fn may_become(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Launching)
                | (Self::Launching, Self::Live | Self::Finished)
                | (Self::Live, Self::Finishing | Self::Finished)
                | (Self::Finishing, Self::Finished)
        )
    }
}

/// Retained final evidence of the pane (TASKS.md:134-135): exit code when observed, the input
/// sequence, at most 4096 UTF-8 bytes of final output.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct FinalEvidence {
    pub exit_code: Option<i32>,
    pub seq: u64,
    pub output: String,
    pub truncated: bool,
}

pub const MAX_FINAL_OUTPUT_BYTES: usize = 4096;

/// Bounded diagnostic on an `Uncertain` or `Lost` record.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Problem(pub String);

// ---------------------------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct PromptText(pub String);

/// Stored intent, not a timer (TASKS.md:177-178): Unix milliseconds.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
#[component(immutable)]
pub struct Deadline(pub u64);

/// Random 128-bit token retained with the immutable prompt (TASKS.md:232); never enters
/// terminal input.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct ReportToken(pub String);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Prepared,
    Reserved,
    Submitting,
    Delivered,
    Failed,
    Uncertain,
    Released,
}

impl Delivery {
    /// Holds writer exclusion on its pane (TASKS.md:184-185).
    pub fn is_pending(self) -> bool {
        matches!(
            self,
            Self::Prepared | Self::Reserved | Self::Submitting | Self::Uncertain
        )
    }

    pub fn may_become(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::Reserved | Self::Released)
                | (Self::Reserved, Self::Submitting | Self::Released)
                | (
                    Self::Submitting,
                    Self::Delivered | Self::Failed | Self::Uncertain
                )
                | (
                    Self::Uncertain,
                    Self::Reserved | Self::Delivered | Self::Failed | Self::Released
                )
                | (Self::Delivered | Self::Failed, Self::Released)
        )
    }
}

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum WaitState {
    #[default]
    Pending,
    NeedsInput,
    ResponseObserved,
    ProcessExited,
    TimedOut,
    Cancelled,
    Uncertain,
}

impl WaitState {
    /// Terminal outcomes are retained; later receipt refreshes cannot erase them (TASKS.md:341-342).
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::NeedsInput)
    }
}

/// fux's input receipt (`fux/input.*`), scoped to the instance nonce (TASKS.md:219-222).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Receipt {
    pub instance: String,
    pub pane: u64,
    pub operation: u64,
    /// `reserved`, `submitted`, `uncertain` or `expired` as fux reports it.
    pub state: String,
    pub bytes_written: usize,
    pub seq: Option<u64>,
    pub expires_ms: u64,
}

/// The one immutable response event per prompt (TASKS.md:233-234, 256-258).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct ResponseEvent {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub kind: String,
}

/// Native binding of a prompt to a producer's session/message (TASKS.md:286-287).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct Binding {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub session: String,
    pub message: String,
    pub bound_ms: u64,
}

// ---------------------------------------------------------------------------------------------
// Operation
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Launch,
    Stop,
    Resume,
    Handoff,
    GroupStep,
}

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    #[default]
    Prepared,
    Submitting,
    Attached,
    Closed,
    Uncertain,
}

impl OperationPhase {
    pub fn may_become(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::Submitting)
                | (
                    Self::Submitting,
                    Self::Attached | Self::Closed | Self::Uncertain
                )
                | (Self::Uncertain, Self::Attached | Self::Closed)
                | (Self::Attached, Self::Closed)
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Check, Result, Source, Artifact
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct CheckCommand {
    pub argv: Vec<String>,
    pub timeout_ms: u64,
}

/// A requirement name (CHECKS.md:33, ARTIFACTS.md:20).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct Requirement(pub String);

/// Unique journal generation at submission; requirement status orders by it (CHECKS.md:41).
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
#[component(immutable)]
pub struct CreatedGeneration(pub u64);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    #[default]
    Queued,
    Running,
    Passed,
    Failed,
    Uncertain,
}

impl CheckState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Uncertain)
    }

    pub fn may_become(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Running | Self::Uncertain)
                | (Self::Running, Self::Passed | Self::Failed | Self::Uncertain)
        )
    }
}

#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    Failed,
    Uncertain,
}

impl Verdict {
    pub fn matches(self, state: CheckState) -> bool {
        matches!(
            (self, state),
            (Self::Passed, CheckState::Passed)
                | (Self::Failed, CheckState::Failed)
                | (Self::Uncertain, CheckState::Uncertain)
        )
    }
}

/// Retained command output (CHECKS.md:48-49): at most 4096 UTF-8 bytes per stream.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct OutputTail {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct SourceRevision {
    pub expression: String,
    pub commit: String,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct ArtifactPath(pub String);

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    #[default]
    Pending,
    Collected,
    Failed,
}

// ---------------------------------------------------------------------------------------------
// Group, Worktree
// ---------------------------------------------------------------------------------------------

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum GroupIntent {
    #[default]
    Manual,
    Automatic,
    Paused,
    Cancelled,
    Complete,
}

/// 1..=members (GROUPS.md:102).
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct Concurrency(pub u32);

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct WorktreeSpec {
    pub repo: String,
    pub branch: String,
    pub base: String,
    /// The reserved parent directory inside the state dir (WORKTREES.md:35-37).
    pub parent: String,
}

#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[reflect(Component)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    #[default]
    Allocating,
    Prepared,
    Creating,
    Ready,
    Uncertain,
    Removing,
    Removed,
}

impl WorktreeState {
    pub fn is_removing(self) -> bool {
        matches!(self, Self::Removing | Self::Removed)
    }

    pub fn may_become(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Allocating, Self::Prepared)
                | (Self::Prepared, Self::Creating)
                | (Self::Creating, Self::Ready | Self::Uncertain)
                | (Self::Uncertain, Self::Ready)
                | (Self::Ready, Self::Removing)
                | (Self::Removing, Self::Removed)
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Machine, Service, ObservedAgent, Plugin
// ---------------------------------------------------------------------------------------------

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct MachineName(pub String);

/// koh control binding (multi-machine-supervision.md:50-56); no key material.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct ControlBinding {
    pub endpoint: String,
    pub key_file: String,
    pub direct: Option<String>,
    pub relay_url: Option<String>,
}

/// The fux instance a `Service` entity currently reaches; never persisted.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceEndpoint {
    pub instance: String,
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    #[default]
    Unknown,
    Working,
    Blocked,
    Idle,
    None,
}

/// A passive observation (OBSERVATION-CONTRACT.md:92-94): raw state and its evidence age.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct Observation {
    pub state: AgentState,
    pub seq: u64,
    pub age_upper_bound_ms: u64,
}

/// A registered producer lifetime (INTEGRATIONS.md:67-70).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct ProducerLifetime {
    pub producer: String,
    pub registered_ms: u64,
    pub retired: Vec<(String, u64, u64)>,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct PluginManifest {
    pub path: String,
    pub version: String,
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

/// Server instance identity: nonce that every mutating request carries. Never persisted.
#[derive(Resource, Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerInstance {
    pub name: String,
    pub nonce: String,
    pub pid: u32,
    pub started_ms: u64,
}

/// Wall clock stamped by the runner before every update; systems read time from here only.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: u64,
}

/// Monotonic journal generation: bumped by every committed snapshot.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Generation(pub u64);

/// A raw JSON payload kept on an entity for a later slice to interpret (plugin manifests).
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct RawJson(pub Value);

pub(super) fn register_types(app: &mut App) {
    app.register_type::<TaskId>()
        .register_type::<AttemptId>()
        .register_type::<PromptId>()
        .register_type::<OperationId>()
        .register_type::<CheckId>()
        .register_type::<ResultId>()
        .register_type::<SourceId>()
        .register_type::<ArtifactId>()
        .register_type::<GroupId>()
        .register_type::<WorktreeId>()
        .register_type::<MachineId>()
        .register_type::<ServiceId>()
        .register_type::<ObservedAgentId>()
        .register_type::<PluginId>()
        .register_type::<PluginActionId>()
        .register_type::<Task>()
        .register_type::<Attempt>()
        .register_type::<Prompt>()
        .register_type::<Operation>()
        .register_type::<Check>()
        .register_type::<CheckResult>()
        .register_type::<Source>()
        .register_type::<Artifact>()
        .register_type::<Group>()
        .register_type::<Worktree>()
        .register_type::<Machine>()
        .register_type::<Service>()
        .register_type::<ObservedAgent>()
        .register_type::<HostedPlugin>()
        .register_type::<PluginAction>()
        .register_type::<ProjectionEntity>()
        .register_type::<StopRequested>()
        .register_type::<Uncertain>()
        .register_type::<Lost>()
        .register_type::<NeedsInput>()
        .register_type::<ArmInputStarted>()
        .register_type::<Required>()
        .register_type::<Enabled>()
        .register_type::<Title>()
        .register_type::<CreatedMs>()
        .register_type::<ClosedMs>()
        .register_type::<Location>()
        .register_type::<TaskState>()
        .register_type::<Ownership>()
        .register_type::<PaneHandle>()
        .register_type::<LaunchMarker>()
        .register_type::<AttemptState>()
        .register_type::<FinalEvidence>()
        .register_type::<Problem>()
        .register_type::<PromptText>()
        .register_type::<Deadline>()
        .register_type::<ReportToken>()
        .register_type::<Delivery>()
        .register_type::<WaitState>()
        .register_type::<Receipt>()
        .register_type::<ResponseEvent>()
        .register_type::<Binding>()
        .register_type::<OperationKind>()
        .register_type::<OperationPhase>()
        .register_type::<CheckCommand>()
        .register_type::<Requirement>()
        .register_type::<CreatedGeneration>()
        .register_type::<CheckState>()
        .register_type::<Verdict>()
        .register_type::<OutputTail>()
        .register_type::<SourceRevision>()
        .register_type::<ArtifactPath>()
        .register_type::<ArtifactState>()
        .register_type::<GroupIntent>()
        .register_type::<Concurrency>()
        .register_type::<WorktreeSpec>()
        .register_type::<WorktreeState>()
        .register_type::<MachineName>()
        .register_type::<ControlBinding>()
        .register_type::<Observation>()
        .register_type::<ProducerLifetime>()
        .register_type::<PluginManifest>()
        .register_type::<AttemptOf>()
        .register_type::<Attempts>()
        .register_type::<PromptOf>()
        .register_type::<Prompts>()
        .register_type::<OperationOf>()
        .register_type::<Operations>()
        .register_type::<ArtifactOf>()
        .register_type::<Artifacts>()
        .register_type::<CheckOf>()
        .register_type::<TaskChecks>()
        .register_type::<CheckOn>()
        .register_type::<Checks>()
        .register_type::<SourceOf>()
        .register_type::<Sources>()
        .register_type::<ResultOf>()
        .register_type::<Results>()
        .register_type::<MemberOf>()
        .register_type::<Members>()
        .register_type::<OwnedWorktree>()
        .register_type::<Worktrees>()
        .register_type::<Bound>()
        .register_type::<BoundAgents>()
        .register_type::<ActionOf>()
        .register_type::<Actions>()
        .register_type::<Mirrors>()
        .register_type::<Projections>()
        .register_type::<After>()
        .register_type::<Seal>();
}
