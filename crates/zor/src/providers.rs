//! Provider adapters and passive observation (milestone 6, owner Providers; INTEGRATIONS.md,
//! OBSERVATION-CONTRACT.md, DASHBOARD.md:35-38; `docs/model.md` invariants 20, 26).
//!
//! Three sources of agent evidence, kept apart:
//!
//! - **Native channel** ([`adapter::ProviderAdapter`], [`claims`]): a sidecar subprocess per
//!   managed attempt (Codex app-server over JSON-RPC lines, the OpenCode sidecar over its line
//!   protocol) spawned from the journaled [`Provider`] intent. Its frames become [`Claim`]s;
//!   a claim binds or reports a prompt only when it quotes that prompt's [`ReportToken`] and the
//!   prompt already carries a fux [`Receipt`] (invariant 7). A mismatched token is refused and
//!   recorded as a problem: the merged observation becomes `Unknown` (26).
//! - **Passive observation** ([`rules`]): `fux/pane.capture` tails, requested by this module's
//!   heartbeat for every observed pane, classified by the rule bundles into an [`Observation`];
//!   an unmatched screen is `Unknown`, never `Idle`.
//! - **Merge** ([`Evidence`]): for an attempt with a native provider, a fresh native state claim
//!   takes precedence; a missing, expired or mismatched claim is `Unknown` even when the passive
//!   rules say idle. Claude has no native channel and stays passive.
//!
//! None of this satisfies a wait or verifies a task (invariant 20): `ResponseEvent`/`Binding`
//! are facts the lifecycle reads; `Observation` only informs `NeedsInput` and the dashboard.

pub mod adapter;
pub mod claims;
pub mod rules;

use std::path::PathBuf;

use bevy_app::prelude::*;
use bevy_asset::{AssetApp, AssetEvent, AssetLoadFailedEvent, Assets};
use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_platform::collections::HashMap;
use bevy_reflect::prelude::*;
use fux::remote::methods::Capture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{
    AgentState, Attempt, AttemptId, AttemptOf, AttemptState, Binding, Clock, Effect, Inbound,
    Observation, ObservedAgent, ObservedAgentId, PaneHandle, Phase, ProducerLifetime, PromptId,
    PromptOf, PromptText, Prompts, Receipt, ReportToken, ResponseEvent, TaskId,
    spawn_observed_agent,
};

pub use adapter::ProviderAdapter;
pub use claims::{Claim, ReportKind};
pub use rules::{BundleInfo, Rules, RulesBundle, RulesLoader, Screen, Verdict};

/// Native state claims older than this are stale (INTEGRATIONS.md:157 six-second TTL).
pub const NATIVE_TTL_MS: u64 = 6_000;
/// Passive evidence older than this is stale (DASHBOARD.md:40 five-second bound).
pub const PASSIVE_TTL_MS: u64 = 5_000;
/// Default `fux/pane.capture` period.
pub const CAPTURE_INTERVAL_MS: u64 = 1_000;
/// A capture reply older than this is abandoned.
pub const CAPTURE_TIMEOUT_MS: u64 = 10_000;
/// Tag in the high byte of every `Effect::FuxCall.call` this module issues.
pub const CALL_TAG: u64 = 0x03 << 56;
/// Bound on one provider frame; longer lines are dropped with a problem.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
/// Retired producers retained per attempt (INTEGRATIONS.md:85).
pub const MAX_RETIRED: usize = 16;
/// Bound on diagnostics kept on a session.
const MAX_PROBLEM_BYTES: usize = 512;

// ---------------------------------------------------------------------------------------------
// Journaled intent
// ---------------------------------------------------------------------------------------------

/// Which integration a managed launch configured (INTEGRATIONS.md:57-59).
#[derive(Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// `codex app-server`: JSON-RPC over stdio, `turn/*` and `item/*` events.
    Codex,
    /// The OpenCode sidecar: zor's line protocol (`t: hello|bound|report|state`).
    OpenCode,
    /// No native channel: passive observation only.
    Claude,
}

impl ProviderKind {
    /// Whether the kind speaks a native channel zor spawns and reads.
    pub fn native(self) -> bool {
        matches!(self, Self::Codex | Self::OpenCode)
    }

    /// The rule bundle id that interprets this agent's screens, and the wire name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Claude => "claude",
        }
    }
}

/// The provider sidecar of one attempt: committed with the launch, spawned by
/// `spawn_sidecars` once journaled (intent before effect), never edited.
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct Provider {
    pub kind: ProviderKind,
    /// Sidecar command; empty for kinds without a native channel.
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------------------------
// Runtime session state (never persisted)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionState {
    /// `Effect::SpawnProvider` emitted; no `ProviderStarted` yet.
    #[default]
    Spawning,
    Running,
    /// Stdin closed by zor (attempt finished); the exit code is still to come.
    Closing,
    Exited {
        code: i32,
    },
}

/// The live native channel of one attempt (on the Attempt entity, beside [`Provider`]).
#[derive(Component, Debug, Default)]
pub struct ProviderSession {
    pub state: SessionState,
    pub pid: Option<u32>,
    /// `<kind>:<pid>` (or the producer's own name); the `producer` of every binding and
    /// report from this channel.
    pub producer: String,
    pub registered_ms: u64,
    /// Frames decoded so far; the `sequence` of bindings and reports.
    pub sequence: u64,
    /// Native session (Codex thread id, OpenCode session id) once established.
    pub session: Option<String>,
    /// Last native state claim and when it was received.
    pub claim: Option<(AgentState, u64)>,
    /// A refused claim (mismatched token, missing receipt, malformed frame, oversized line)
    /// since the last accepted binding: the merged observation is `Unknown` until a new
    /// managed input establishes a binding (INTEGRATIONS.md:187-188).
    pub problem: Option<String>,
    pub last_event: Option<String>,
    pub last_event_ms: u64,
    /// Codex turn id → report token of the prompt that started it.
    pub(crate) turns: HashMap<String, String>,
    buffer: Vec<u8>,
    /// The current line exceeded [`MAX_FRAME_BYTES`]; bytes are dropped until its newline.
    overflow: bool,
}

impl ProviderSession {
    fn refuse(&mut self, problem: String) {
        let mut problem = problem;
        clip(&mut problem, MAX_PROBLEM_BYTES);
        self.problem = Some(problem);
    }

    fn event(&mut self, what: &str, now_ms: u64) {
        let mut what = what.to_owned();
        clip(&mut what, 128);
        self.last_event = Some(what);
        self.last_event_ms = now_ms;
    }
}

fn clip(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

/// Where the merged observation came from (DASHBOARD.md:38-40 `evidence`).
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EvidenceSource {
    /// Nothing answered yet.
    #[default]
    None,
    Passive,
    Native,
}

impl EvidenceSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Passive => "passive",
            Self::Native => "native",
        }
    }
}

/// The evidence behind an [`Observation`], on the ObservedAgent entity: the matched rule, when
/// the merged state last changed, the secondary passive state and the last capture.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct Evidence {
    pub source: EvidenceSource,
    /// The passive rule that matched the last capture.
    pub rule: Option<String>,
    /// `Clock` milliseconds when the merged state last changed.
    pub since_ms: u64,
    pub passive: AgentState,
    /// Whether a capture answered yet, and `Clock` milliseconds of the last reply.
    pub captured: bool,
    pub captured_ms: u64,
    pub capture_seq: u64,
    /// Why the merged state is `Unknown`, if it is.
    pub problem: Option<String>,
    /// The capture problem, kept apart from the native one.
    capture_problem: Option<String>,
}

/// ObservedAgents this module spawned for live attempts; despawned when the attempt finishes.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct AutoObserved;

/// Pending `fux/pane.capture` calls and the heartbeat schedule.
#[derive(Resource, Debug)]
pub struct Captures {
    pub interval_ms: u64,
    next: u64,
    /// call id → (observed agent, `Clock` ms when issued).
    pending: HashMap<u64, (Entity, u64)>,
    /// observed agent → last request time.
    last: HashMap<Entity, u64>,
}

impl Default for Captures {
    fn default() -> Self {
        Self {
            interval_ms: CAPTURE_INTERVAL_MS,
            next: 0,
            pending: HashMap::default(),
            last: HashMap::default(),
        }
    }
}

impl Captures {
    fn allocate(&mut self) -> u64 {
        self.next += 1;
        CALL_TAG | self.next
    }

    fn inflight(&self, agent: Entity) -> bool {
        self.pending.values().any(|(e, _)| *e == agent)
    }
}

/// This module's share of the update's `Inbound` batch, copied by [`collect_inbound`].
#[derive(Resource, Default)]
struct Inbox {
    provider: Vec<ProviderItem>,
    replies: Vec<(u64, Result<Value, String>)>,
}

enum ProviderItem {
    Started { attempt: Entity, pid: u32 },
    Output { attempt: Entity, bytes: Vec<u8> },
    Exited { attempt: Entity, code: i32 },
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

/// Registers the provider session, observation and rule systems. `asset_root` is the on-disk
/// `AssetPlugin::file_path` (`<config_dir>`): external bundles are `<asset_root>/rules/*.toml`;
/// `None` keeps the embedded defaults only.
#[derive(Default)]
pub struct ProvidersPlugin {
    pub asset_root: Option<PathBuf>,
}

impl Plugin for ProvidersPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Provider>()
            .register_type::<ProviderKind>()
            .register_type::<Evidence>()
            .init_asset::<RulesBundle>()
            .register_asset_loader(RulesLoader)
            .insert_resource(Rules::new(self.asset_root.clone()))
            .init_resource::<Captures>()
            .init_resource::<Inbox>()
            .add_systems(Startup, rules::load_external)
            .add_systems(
                PreUpdate,
                (
                    collect_inbound,
                    track_rules,
                    ingest_provider,
                    ingest_captures,
                )
                    .chain()
                    .in_set(Phase::Completions),
            )
            .add_systems(
                Update,
                (spawn_sidecars, observe_panes, heartbeat)
                    .chain()
                    .in_set(Phase::Lifecycle),
            );
    }
}

// ---------------------------------------------------------------------------------------------
// Public API for the lifecycle slice
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    NotAPrompt(Entity),
    /// The prompt has no `Receipt`: arming follows reservation.
    NoReceipt,
    /// The sidecar has not started or has no native session yet.
    NotReady(&'static str),
    Exited(i32),
}

impl core::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAPrompt(entity) => write!(f, "{entity} is not a prompt"),
            Self::NoReceipt => f.write_str("prompt has no receipt to arm against"),
            Self::NotReady(what) => write!(f, "provider not ready: {what}"),
            Self::Exited(code) => write!(f, "provider exited with {code}"),
        }
    }
}

impl core::error::Error for ProviderError {}

/// Arms the attempt's native channel for `prompt` (INTEGRATIONS.md:71-74): after the receipt is
/// reserved and before the terminal write. Codex: `turn/start` whose `clientUserMessageId` is
/// the prompt's report token; OpenCode: `arm {operation, token, text}`. A no-op without a
/// native provider. The write is an `Effect`, drained after this update's journal commit.
pub fn arm(world: &mut World, prompt: Entity) -> Result<(), ProviderError> {
    let attempt = world
        .get::<PromptOf>(prompt)
        .map(|p| p.0)
        .ok_or(ProviderError::NotAPrompt(prompt))?;
    let Some(kind) = world.get::<Provider>(attempt).map(|p| p.kind) else {
        return Ok(());
    };
    if !kind.native() {
        return Ok(());
    }
    if world.get::<Receipt>(prompt).is_none() {
        return Err(ProviderError::NoReceipt);
    }
    let (session, state) = match world.get::<ProviderSession>(attempt) {
        Some(s) => (s.session.clone(), s.state),
        None => return Err(ProviderError::NotReady("not spawned")),
    };
    match state {
        SessionState::Spawning => return Err(ProviderError::NotReady("starting")),
        SessionState::Closing => return Err(ProviderError::NotReady("closing")),
        SessionState::Exited { code } => return Err(ProviderError::Exited(code)),
        SessionState::Running => {}
    }
    let token = world
        .get::<ReportToken>(prompt)
        .map(|t| t.0.clone())
        .unwrap_or_default();
    let operation = world
        .get::<PromptId>(prompt)
        .map(|p| p.0.clone())
        .unwrap_or_default();
    let text = world
        .get::<PromptText>(prompt)
        .map(|t| t.0.clone())
        .unwrap_or_default();
    let frame = match kind {
        ProviderKind::Codex => {
            let Some(thread) = session else {
                return Err(ProviderError::NotReady("no thread yet"));
            };
            claims::codex_turn_start(&thread, &token, &text)
        }
        ProviderKind::OpenCode => claims::opencode_arm(&operation, &token, &text),
        ProviderKind::Claude => return Ok(()),
    };
    write_frame(world, attempt, &frame);
    Ok(())
}

/// The merged observation of an attempt's pane, once the pane is observed. Informational
/// only: it never resolves a wait (invariant 26).
pub fn observation_of(world: &mut World, attempt: Entity) -> Option<Observation> {
    let handle = world.get::<PaneHandle>(attempt)?.clone();
    let agent = agent_for(world, &handle)?;
    world.get::<Observation>(agent).cloned()
}

/// The ObservedAgent entity watching this pane identity (instance, workspace, pane).
pub fn agent_for(world: &mut World, handle: &PaneHandle) -> Option<Entity> {
    world
        .query_filtered::<(Entity, &PaneHandle), With<ObservedAgent>>()
        .iter(world)
        .find(|(_, h)| same_pane(h, handle))
        .map(|(e, _)| e)
}

fn same_pane(a: &PaneHandle, b: &PaneHandle) -> bool {
    a.instance == b.instance && a.workspace == b.workspace && a.pane == b.pane
}

/// The live (non-`Finished`) attempt whose pane this is, with its provider kind.
fn attempt_for(world: &mut World, handle: &PaneHandle) -> Option<(Entity, Option<ProviderKind>)> {
    world
        .query_filtered::<(Entity, &PaneHandle, &AttemptState, Option<&Provider>), With<Attempt>>()
        .iter(world)
        .find(|(_, h, state, _)| **state != AttemptState::Finished && same_pane(h, handle))
        .map(|(e, _, _, p)| (e, p.map(|p| p.kind)))
}

fn write_frame(world: &mut World, attempt: Entity, frame: &Value) {
    let mut bytes = serde_json::to_vec(frame).unwrap_or_default();
    bytes.push(b'\n');
    world.write_message(Effect::WriteProvider { attempt, bytes });
}

/// One observed pane as `zor/agent.*` and the `AgentView` projection report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentRecord {
    pub id: u64,
    pub instance: String,
    pub workspace: String,
    pub pane: u64,
    pub pid: Option<u32>,
    pub state: String,
    pub source: String,
    pub rule: Option<String>,
    pub passive: String,
    pub since_ms: u64,
    pub age_upper_bound_ms: u64,
    pub attempt: Option<u64>,
    pub task: Option<String>,
    pub provider: Option<String>,
    pub producer: Option<String>,
    pub last_event: Option<String>,
    pub last_event_ms: u64,
    pub problem: Option<String>,
}

/// Every observed pane, live from the model.
pub fn agent_records(world: &mut World) -> Vec<(Entity, AgentRecord)> {
    let rows: Vec<(Entity, u64, PaneHandle, Observation, Option<Evidence>)> = world
        .query_filtered::<(
            Entity,
            &ObservedAgentId,
            &PaneHandle,
            &Observation,
            Option<&Evidence>,
        ), With<ObservedAgent>>()
        .iter(world)
        .map(|(e, id, h, o, ev)| (e, id.0, h.clone(), o.clone(), ev.cloned()))
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    for (entity, id, handle, observation, evidence) in rows {
        let evidence = evidence.unwrap_or_default();
        let mut record = AgentRecord {
            id,
            instance: handle.instance.clone(),
            workspace: handle.workspace.clone(),
            pane: handle.pane,
            pid: handle.pid,
            state: format!("{:?}", observation.state),
            source: evidence.source.name().to_owned(),
            rule: evidence.rule.clone(),
            passive: format!("{:?}", evidence.passive),
            since_ms: evidence.since_ms,
            age_upper_bound_ms: observation.age_upper_bound_ms,
            problem: evidence.problem.clone(),
            ..Default::default()
        };
        if let Some((attempt, kind)) = attempt_for(world, &handle) {
            record.attempt = world.get::<AttemptId>(attempt).map(|a| a.0);
            record.task = world
                .get::<AttemptOf>(attempt)
                .and_then(|of| world.get::<TaskId>(of.0))
                .map(|t| t.0.clone());
            record.provider = kind.map(|k| k.name().to_owned());
            if let Some(session) = world.get::<ProviderSession>(attempt) {
                record.producer = Some(session.producer.clone()).filter(|p| !p.is_empty());
                record.last_event = session.last_event.clone();
                record.last_event_ms = session.last_event_ms;
            }
        }
        out.push((entity, record));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------------------------

/// Copies this module's messages out of the shared batch (other readers keep theirs).
fn collect_inbound(mut inbound: MessageReader<Inbound>, mut inbox: ResMut<Inbox>) {
    for message in inbound.read() {
        match message {
            Inbound::ProviderStarted { attempt, pid } => {
                inbox.provider.push(ProviderItem::Started {
                    attempt: *attempt,
                    pid: *pid,
                })
            }
            Inbound::ProviderOutput { attempt, bytes } => {
                inbox.provider.push(ProviderItem::Output {
                    attempt: *attempt,
                    bytes: bytes.clone(),
                })
            }
            Inbound::ProviderExited { attempt, code } => {
                inbox.provider.push(ProviderItem::Exited {
                    attempt: *attempt,
                    code: *code,
                })
            }
            Inbound::FuxReply { call, result } if *call & CALL_TAG == CALL_TAG => {
                inbox.replies.push((*call, result.clone()));
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sidecar lifecycle
// ---------------------------------------------------------------------------------------------

/// Spawns the sidecar of every attempt whose journaled [`Provider`] has no session yet, and
/// closes the channel of finished attempts (an empty write is the adapter's close request).
fn spawn_sidecars(world: &mut World) {
    let fresh: Vec<(Entity, Provider)> = world
        .query_filtered::<(Entity, &Provider, &AttemptState), (With<Attempt>, Without<ProviderSession>)>()
        .iter(world)
        .filter(|(_, p, state)| p.kind.native() && **state != AttemptState::Finished)
        .map(|(e, p, _)| (e, p.clone()))
        .collect();
    for (attempt, provider) in fresh {
        let mut session = ProviderSession::default();
        if provider.argv.is_empty() {
            session.state = SessionState::Exited { code: 127 };
            session.refuse("provider has no command".into());
            world.entity_mut(attempt).insert(session);
            continue;
        }
        world.entity_mut(attempt).insert(session);
        world.write_message(Effect::SpawnProvider {
            attempt,
            argv: provider.argv,
            cwd: provider.cwd,
            env: provider.env,
        });
    }
    let finished: Vec<Entity> = world
        .query_filtered::<(Entity, &AttemptState, &ProviderSession), With<Attempt>>()
        .iter(world)
        .filter(|(_, state, s)| {
            **state == AttemptState::Finished
                && matches!(s.state, SessionState::Spawning | SessionState::Running)
        })
        .map(|(e, _, _)| e)
        .collect();
    for attempt in finished {
        world.write_message(Effect::WriteProvider {
            attempt,
            bytes: Vec::new(),
        });
        if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
            s.state = SessionState::Closing;
        }
    }
}

/// `Inbound::Provider*` → session state; decoded frames → bindings, reports, state claims.
fn ingest_provider(world: &mut World) {
    let items = core::mem::take(&mut world.resource_mut::<Inbox>().provider);
    let now = world.resource::<Clock>().now_ms;
    for item in items {
        match item {
            ProviderItem::Started { attempt, pid } => started(world, attempt, pid, now),
            ProviderItem::Output { attempt, bytes } => output(world, attempt, &bytes, now),
            ProviderItem::Exited { attempt, code } => exited(world, attempt, code, now),
        }
    }
}

fn started(world: &mut World, attempt: Entity, pid: u32, now: u64) {
    let Some(kind) = world.get::<Provider>(attempt).map(|p| p.kind) else {
        return;
    };
    let producer = format!("{}:{pid}", kind.name());
    {
        let Some(mut session) = world.get_mut::<ProviderSession>(attempt) else {
            return;
        };
        if session.state != SessionState::Spawning {
            return;
        }
        session.state = SessionState::Running;
        session.pid = Some(pid);
        session.producer = producer.clone();
        session.registered_ms = now;
        session.event("started", now);
    }
    match world.get_mut::<ProducerLifetime>(attempt) {
        Some(mut lifetime) => {
            lifetime.producer = producer;
            lifetime.registered_ms = now;
        }
        None => {
            world.entity_mut(attempt).insert(ProducerLifetime {
                producer,
                registered_ms: now,
                retired: Vec::new(),
            });
        }
    }
    if kind == ProviderKind::Codex {
        write_frame(world, attempt, &claims::codex_initialize());
        write_frame(world, attempt, &claims::codex_thread_start());
    }
}

fn exited(world: &mut World, attempt: Entity, code: i32, now: u64) {
    let producer = {
        let Some(mut session) = world.get_mut::<ProviderSession>(attempt) else {
            return;
        };
        session.state = SessionState::Exited { code };
        session.claim = None;
        session.event(&format!("exited {code}"), now);
        core::mem::take(&mut session.producer)
    };
    if let Some(mut lifetime) = world.get_mut::<ProducerLifetime>(attempt)
        && !producer.is_empty()
    {
        let registered = lifetime.registered_ms;
        if lifetime.retired.len() < MAX_RETIRED {
            lifetime.retired.push((producer, registered, now));
        }
        lifetime.producer.clear();
    }
}

/// Splits the byte stream into bounded lines and applies each frame's claims.
fn output(world: &mut World, attempt: Entity, bytes: &[u8], now: u64) {
    let Some(kind) = world.get::<Provider>(attempt).map(|p| p.kind) else {
        return;
    };
    let mut lines: Vec<Vec<u8>> = Vec::new();
    {
        let Some(mut session) = world.get_mut::<ProviderSession>(attempt) else {
            return;
        };
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|b| *b == b'\n') {
            let (head, tail) = rest.split_at(at);
            if session.overflow {
                session.overflow = false;
                session.buffer.clear();
            } else {
                session.buffer.extend_from_slice(head);
                let line = core::mem::take(&mut session.buffer);
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            rest = tail.get(1..).unwrap_or_default();
        }
        if !session.overflow {
            session.buffer.extend_from_slice(rest);
            if session.buffer.len() > MAX_FRAME_BYTES {
                session.buffer.clear();
                session.overflow = true;
                session.refuse(format!("frame exceeds {MAX_FRAME_BYTES} bytes"));
            }
        }
    }
    for line in lines {
        let claims = {
            let Some(mut session) = world.get_mut::<ProviderSession>(attempt) else {
                return;
            };
            session.sequence += 1;
            claims::decode(kind, &mut session, &line)
        };
        for claim in claims {
            apply_claim(world, attempt, claim, now);
        }
    }
}

/// A binding or report is accepted only for a prompt of this attempt whose report token (and
/// operation id, when quoted) matches and which carries a receipt (invariant 7); anything else
/// is refused and remembered as a problem (invariant 26).
fn apply_claim(world: &mut World, attempt: Entity, claim: Claim, now: u64) {
    let sequence = world
        .get::<ProviderSession>(attempt)
        .map_or(0, |s| s.sequence);
    match claim {
        Claim::Hello { producer, session } => {
            if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                if let Some(producer) = producer {
                    s.producer = producer;
                }
                if session.is_some() {
                    s.session = session;
                }
                s.event("hello", now);
            }
        }
        Claim::State(state) => {
            if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                s.claim = Some((state, now));
                s.event(&format!("state {state:?}"), now);
            }
        }
        Claim::Malformed(problem) => {
            if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                s.refuse(problem);
                s.event("malformed frame", now);
            }
        }
        Claim::Bound {
            token,
            operation,
            session,
            message,
        } => match correlate(world, attempt, &token, operation.as_deref()) {
            Ok((prompt, input_operation)) => {
                let producer = world
                    .get::<ProviderSession>(attempt)
                    .map(|s| s.producer.clone())
                    .unwrap_or_default();
                if world.get::<Binding>(prompt).is_none() {
                    world.entity_mut(prompt).insert(Binding {
                        producer,
                        sequence,
                        input_operation,
                        session,
                        message,
                        bound_ms: now,
                    });
                }
                if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                    s.problem = None;
                    s.event("bound", now);
                }
            }
            Err(problem) => {
                if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                    s.refuse(format!("binding refused: {problem}"));
                    s.event("binding refused", now);
                }
            }
        },
        Claim::Report {
            token,
            operation,
            kind,
        } => match correlate(world, attempt, &token, operation.as_deref()) {
            Ok((prompt, input_operation)) => {
                if world.get::<Binding>(prompt).is_none() {
                    if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                        s.refuse("report refused: prompt has no binding".into());
                        s.event("report refused", now);
                    }
                    return;
                }
                let producer = world
                    .get::<ProviderSession>(attempt)
                    .map(|s| s.producer.clone())
                    .unwrap_or_default();
                if world.get::<ResponseEvent>(prompt).is_none() {
                    world.entity_mut(prompt).insert(ResponseEvent {
                        producer,
                        sequence,
                        input_operation,
                        kind: kind.name().to_owned(),
                    });
                }
                if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                    s.event(&format!("report {}", kind.name()), now);
                }
            }
            Err(problem) => {
                if let Some(mut s) = world.get_mut::<ProviderSession>(attempt) {
                    s.refuse(format!("report refused: {problem}"));
                    s.event("report refused", now);
                }
            }
        },
    }
}

/// The prompt of `attempt` armed under `token`, and its receipt's input operation.
fn correlate(
    world: &mut World,
    attempt: Entity,
    token: &str,
    operation: Option<&str>,
) -> Result<(Entity, u64), String> {
    let prompts: Vec<Entity> = world
        .get::<Prompts>(attempt)
        .map(|p| p.iter().collect())
        .unwrap_or_default();
    let prompt = prompts
        .into_iter()
        .find(|p| world.get::<ReportToken>(*p).is_some_and(|t| t.0 == token))
        .ok_or_else(|| "mismatched report token".to_owned())?;
    if let Some(operation) = operation
        && world
            .get::<PromptId>(prompt)
            .is_none_or(|id| id.0 != operation)
    {
        return Err("operation does not name the token's prompt".into());
    }
    let receipt = world
        .get::<Receipt>(prompt)
        .ok_or_else(|| "prompt has no receipt".to_owned())?;
    Ok((prompt, receipt.operation))
}

// ---------------------------------------------------------------------------------------------
// Passive observation
// ---------------------------------------------------------------------------------------------

/// Rule asset events → generation and problem bookkeeping.
fn track_rules(
    mut rules: ResMut<Rules>,
    mut events: MessageReader<AssetEvent<RulesBundle>>,
    mut failures: MessageReader<AssetLoadFailedEvent<RulesBundle>>,
) {
    for event in events.read() {
        if matches!(
            event,
            AssetEvent::Added { .. } | AssetEvent::Modified { .. }
        ) {
            rules.generation += 1;
            rules.problem = None;
        }
    }
    for failure in failures.read() {
        let mut problem = format!("{}: {}", failure.path, failure.error);
        clip(&mut problem, MAX_PROBLEM_BYTES);
        bevy_log::warn!("rules: {problem} (previous bundle kept)");
        rules.problem = Some(problem);
    }
}

/// Keeps one ObservedAgent per live attempt pane and writes every agent's merged
/// [`Observation`] from its passive evidence and native claims.
fn observe_panes(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    // Attempts that are live and unobserved.
    let live: Vec<PaneHandle> = world
        .query_filtered::<(&PaneHandle, &AttemptState), With<Attempt>>()
        .iter(world)
        .filter(|(_, s)| matches!(s, AttemptState::Live | AttemptState::Finishing))
        .map(|(h, _)| h.clone())
        .collect();
    for handle in live {
        if agent_for(world, &handle).is_none() {
            match spawn_observed_agent(world, handle, None) {
                Ok(agent) => {
                    world
                        .entity_mut(agent)
                        .insert((AutoObserved, Evidence::default()));
                }
                Err(e) => bevy_log::warn!("observe: {e}"),
            }
        }
    }
    // Auto-observed panes whose attempt finished.
    let auto: Vec<(Entity, PaneHandle)> = world
        .query_filtered::<(Entity, &PaneHandle), (With<ObservedAgent>, With<AutoObserved>)>()
        .iter(world)
        .map(|(e, h)| (e, h.clone()))
        .collect();
    for (agent, handle) in auto {
        if attempt_for(world, &handle).is_none() {
            world.despawn(agent);
            let mut captures = world.resource_mut::<Captures>();
            captures.last.remove(&agent);
            captures.pending.retain(|_, (e, _)| *e != agent);
        }
    }
    // Merge.
    let agents: Vec<(Entity, PaneHandle)> = world
        .query_filtered::<(Entity, &PaneHandle), With<ObservedAgent>>()
        .iter(world)
        .map(|(e, h)| (e, h.clone()))
        .collect();
    for (agent, handle) in agents {
        if world.get::<Evidence>(agent).is_none() {
            world.entity_mut(agent).insert(Evidence::default());
        }
        let native = attempt_for(world, &handle)
            .filter(|(_, kind)| kind.is_some_and(ProviderKind::native))
            .and_then(|(attempt, _)| world.get::<ProviderSession>(attempt))
            .map(|s| (s.state, s.claim, s.problem.clone()));
        let Some(evidence) = world.get::<Evidence>(agent).cloned() else {
            continue;
        };
        let passive_age = now.saturating_sub(evidence.captured_ms);
        let (state, source, problem, age) = match native {
            Some((state, claim, problem)) => {
                let (s, p, age) = match (state, claim, problem) {
                    (_, _, Some(problem)) => (AgentState::Unknown, Some(problem), None),
                    (SessionState::Spawning, _, None) => (
                        AgentState::Unknown,
                        Some("provider not started".to_owned()),
                        None,
                    ),
                    (SessionState::Closing, _, None) => (
                        AgentState::Unknown,
                        Some("provider closing".to_owned()),
                        None,
                    ),
                    (SessionState::Exited { code }, _, None) => (
                        AgentState::Unknown,
                        Some(format!("provider exited with {code}")),
                        None,
                    ),
                    (SessionState::Running, None, None) => (
                        AgentState::Unknown,
                        Some("no native claim yet".to_owned()),
                        None,
                    ),
                    (SessionState::Running, Some((claimed, at)), None) => {
                        let age = now.saturating_sub(at);
                        if age <= NATIVE_TTL_MS {
                            (claimed, None, Some(age))
                        } else {
                            (
                                AgentState::Unknown,
                                Some("native claim expired".to_owned()),
                                Some(age),
                            )
                        }
                    }
                };
                (s, EvidenceSource::Native, p, age.unwrap_or(passive_age))
            }
            None if !evidence.captured => (
                AgentState::Unknown,
                EvidenceSource::None,
                evidence.capture_problem.clone(),
                now,
            ),
            None if passive_age > PASSIVE_TTL_MS => (
                AgentState::Unknown,
                EvidenceSource::Passive,
                Some("capture stale".to_owned()),
                passive_age,
            ),
            None => (
                evidence.passive,
                EvidenceSource::Passive,
                evidence.capture_problem.clone(),
                passive_age,
            ),
        };
        let seq = evidence.capture_seq;
        if let Some(mut observation) = world.get_mut::<Observation>(agent) {
            let changed = observation.state != state;
            if changed || observation.seq != seq {
                observation.state = state;
                observation.seq = seq;
            }
            observation.bypass_change_detection().age_upper_bound_ms = age;
            if let Some(mut evidence) = world.get_mut::<Evidence>(agent) {
                let evidence = evidence.bypass_change_detection();
                if changed || evidence.since_ms == 0 {
                    evidence.since_ms = now;
                }
                evidence.source = source;
                evidence.problem = problem;
            }
        }
    }
}

/// Issues `fux/pane.capture` for every observed pane on schedule; abandons stale calls.
fn heartbeat(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    let agents: Vec<(Entity, u64)> = world
        .query_filtered::<(Entity, &PaneHandle), With<ObservedAgent>>()
        .iter(world)
        .map(|(e, h)| (e, h.pane))
        .collect();
    let mut calls = Vec::new();
    {
        let mut captures = world.resource_mut::<Captures>();
        let stale: Vec<(u64, Entity)> = captures
            .pending
            .iter()
            .filter(|(_, (_, at))| now.saturating_sub(*at) > CAPTURE_TIMEOUT_MS)
            .map(|(call, (e, _))| (*call, *e))
            .collect();
        for (call, agent) in stale {
            captures.pending.remove(&call);
            if let Some(mut evidence) = world.get_mut::<Evidence>(agent) {
                evidence.capture_problem = Some("capture timed out".into());
            }
            captures = world.resource_mut::<Captures>();
        }
        let live: Vec<Entity> = agents.iter().map(|(e, _)| *e).collect();
        captures.last.retain(|e, _| live.contains(e));
        for (agent, pane) in agents {
            let due = captures
                .last
                .get(&agent)
                .is_none_or(|last| now.saturating_sub(*last) >= captures.interval_ms);
            if due && !captures.inflight(agent) {
                let call = captures.allocate();
                captures.pending.insert(call, (agent, now));
                captures.last.insert(agent, now);
                calls.push((call, pane));
            }
        }
    }
    for (call, pane) in calls {
        world.write_message(Effect::FuxCall {
            call,
            method: "fux/pane.capture".into(),
            params: json!({ "pane": pane }),
        });
    }
}

/// `Inbound::FuxReply` for this module's capture calls → passive evidence.
fn ingest_captures(world: &mut World) {
    let replies = core::mem::take(&mut world.resource_mut::<Inbox>().replies);
    if replies.is_empty() {
        return;
    }
    let now = world.resource::<Clock>().now_ms;
    for (call, result) in replies {
        let Some((agent, _)) = world.resource_mut::<Captures>().pending.remove(&call) else {
            continue;
        };
        let Some(handle) = world.get::<PaneHandle>(agent).cloned() else {
            continue;
        };
        let bundle = attempt_for(world, &handle).and_then(|(_, kind)| kind.map(ProviderKind::name));
        let outcome = result
            .and_then(|v| serde_json::from_value::<Capture>(v).map_err(|e| e.to_string()))
            .and_then(|capture| {
                if capture.pane != handle.pane {
                    Err(format!("capture answered for pane {}", capture.pane))
                } else if capture.truncated {
                    Err("capture truncated".to_owned())
                } else {
                    Ok(capture)
                }
            });
        let (verdict, seq, problem) = match outcome {
            Ok(capture) => {
                let seq = capture.seq;
                let screen = Screen::from_capture(capture);
                let verdict = world.resource_scope(|world, rules: Mut<Rules>| {
                    let assets = world.resource::<Assets<RulesBundle>>();
                    rules.evaluate(assets, bundle, &screen)
                });
                (verdict, seq, None)
            }
            Err(problem) => {
                let mut problem = problem;
                clip(&mut problem, MAX_PROBLEM_BYTES);
                (Verdict::default(), 0, Some(problem))
            }
        };
        if let Some(mut evidence) = world.get_mut::<Evidence>(agent) {
            evidence.passive = verdict.state;
            evidence.rule = verdict.rule;
            evidence.captured = true;
            evidence.captured_ms = now;
            if seq != 0 {
                evidence.capture_seq = seq;
            }
            evidence.capture_problem = problem;
        }
    }
}
