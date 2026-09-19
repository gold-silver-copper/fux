//! A bounded provider of BSN scenes hosted by fux. Dashboard ownership never includes a task,
//! attempt, workspace, or process. The scene World is an inert export stage, not workflow state.
pub mod scene;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_scene::ScenePlugin;
use bevy_ui::{BackgroundColor, BorderColor, Node, ScrollPosition, ZIndex};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::lifecycle;
use crate::machines::supervision::{ActionKind, Freshness, PaneIdentity};
use crate::machines::{self, MachineGuard};
use crate::model::{
    AttemptId, AttemptState, Ids, Lost, NeedsInput, PaneHandle, TaskState, Title, Uncertain,
};
use crate::model::{Clock, Effect, Inbound, Phase, ServerInstance};

pub const CALL_TAG: u64 = 0x06 << 56;
const TAG_MASK: u64 = 0xff << 56;
pub const MAX_ROWS: usize = 256;
pub const MAX_SESSIONS: usize = 8;
pub const CALL_TIMEOUT_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub machine: String,
    pub instance: String,
    pub task: String,
    pub attempt: Option<u64>,
    pub pane: Option<PaneIdentity>,
}
impl Target {
    pub fn guard(&self) -> MachineGuard {
        MachineGuard {
            instance: self.instance.clone(),
            attempt: self.attempt,
            pane: self.pane.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub key: String,
    pub text: String,
    pub target: Option<Target>,
    pub freshness: Freshness,
    pub attention: bool,
}

/// Read-only snapshot suitable for `zor dashboard --once`. Identity includes the entire
/// incarnation/attempt/pane tuple; a task restarted under its old name is a different row.
pub fn rows(world: &World, machine: Option<&str>) -> Result<Vec<Row>, String> {
    let snapshots = match machine {
        Some("local" | "Local") => Vec::new(),
        Some(machine) => vec![machines::snapshot(world, machine)?],
        None => machines::snapshots(world),
    };
    let mut rows = if machine.is_none_or(|m| m.eq_ignore_ascii_case("local")) {
        local_rows(world)?
    } else {
        Vec::new()
    };
    for snapshot in snapshots {
        let fresh = snapshot.freshness;
        let Some(view) = snapshot.view else {
            rows.push(Row {
                key: format!("machine:{}", snapshot.id),
                text: format!("{} [{}]", clean(&snapshot.name), fresh.name()),
                target: None,
                freshness: fresh,
                attention: false,
            });
            continue;
        };
        for task in view.rows {
            let pane = view
                .agents
                .iter()
                .find(|agent| {
                    agent.task.as_deref() == Some(&task.id) && agent.attempt == task.current_attempt
                })
                .map(|agent| PaneIdentity {
                    instance: agent.instance.clone(),
                    workspace: agent.workspace.clone(),
                    pane: agent.pane,
                    pid: agent.pid,
                });
            let target = Target {
                machine: snapshot.id.clone(),
                instance: view.instance.clone(),
                task: task.id.clone(),
                attempt: task.current_attempt,
                pane,
            };
            let key = serde_json::to_string(&target).map_err(|e| e.to_string())?;
            let attention = task.needs_input || task.uncertain;
            rows.push(Row {
                key,
                text: format!(
                    "{} {} [{}] {} {}{}",
                    clean(&snapshot.name),
                    clean(&task.id),
                    fresh.name(),
                    clean(&task.state),
                    if attention { "! " } else { "" },
                    clean(&task.title)
                ),
                target: Some(target),
                freshness: fresh,
                attention,
            });
        }
    }
    rows.sort_by(|a, b| {
        (a.freshness != Freshness::Fresh, !a.attention, &a.key).cmp(&(
            b.freshness != Freshness::Fresh,
            !b.attention,
            &b.key,
        ))
    });
    rows.truncate(MAX_ROWS);
    Ok(rows)
}

fn local_rows(world: &World) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (id, &task) in &world.resource::<Ids>().tasks {
        let attempt = lifecycle::attempt_of(world, task);
        let pane = attempt
            .and_then(|a| world.get::<PaneHandle>(a))
            .filter(|p| p.pane != 0)
            .map(|p| PaneIdentity {
                instance: p.instance.clone(),
                workspace: p.workspace.clone(),
                pane: p.pane,
                pid: p.pid,
            });
        let target = Target {
            machine: "local".into(),
            instance: world.resource::<ServerInstance>().nonce.clone(),
            task: id.0.clone(),
            attempt: attempt.and_then(|a| world.get::<AttemptId>(a)).map(|a| a.0),
            pane,
        };
        let attention = attempt.is_some_and(|a| {
            world.get::<NeedsInput>(a).is_some() || world.get::<Uncertain>(a).is_some()
        });
        let fresh = if attempt.is_some_and(|a| world.get::<Lost>(a).is_some()) {
            Freshness::Expired
        } else {
            Freshness::Fresh
        };
        let title = world.get::<Title>(task).map_or("", |t| t.0.as_str());
        rows.push(Row {
            key: serde_json::to_string(&target).map_err(|e| e.to_string())?,
            text: format!(
                "Local {} [{}] {:?} {}{}",
                clean(&id.0),
                fresh.name(),
                world.get::<TaskState>(task).copied().unwrap_or_default(),
                if attention { "! " } else { "" },
                clean(title)
            ),
            target: Some(target),
            freshness: fresh,
            attention,
        });
    }
    Ok(rows)
}

/// In-process local handoff guard. The CLI subsequently revalidates the configured fux
/// descriptor and exact pane with its attachment handshake; no descriptor is accepted here.
pub fn validate_local_attachment(world: &World, target: &Target) -> Result<(), String> {
    if target.machine != "local"
        || !local_rows(world)?
            .iter()
            .any(|r| r.target.as_ref() == Some(target))
    {
        return Err("local target changed".into());
    }
    let task = world
        .resource::<Ids>()
        .task(&target.task)
        .ok_or("local task disappeared")?;
    let attempt = lifecycle::attempt_of(world, task).ok_or("task has no live attempt")?;
    let pane = target.pane.as_ref().ok_or("task has no pane")?;
    if world.get::<AttemptState>(attempt) != Some(&AttemptState::Live)
        || world.get::<Lost>(attempt).is_some()
        || world.resource::<lifecycle::Link>().instance.as_deref() != Some(pane.instance.as_str())
    {
        return Err("local pane is not freshly connected".into());
    }
    Ok(())
}
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(256).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub workspace: String,
    pub node: u64,
    pub generation: u64,
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub viewer: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub id: u64,
    pub viewer: u64,
    pub target: Target,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub id: u64,
    pub provider: String,
    pub workspace: String,
    pub node: u64,
    pub revision: u64,
    pub status: String,
    pub rows: Vec<Row>,
    pub row_nodes: Vec<(String, u64)>,
    pub selected: Option<String>,
    pub handoff: Option<Handoff>,
    pub problem: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Select,
    Next,
    Previous,
    Scroll { rows: i32 },
    Attach,
    Cancel,
    Stop,
    Reconcile,
    Close,
}
#[derive(Component)]
struct Dashboard {
    state: State,
    fux_instance: String,
    machine: Option<String>,
    viewer: Option<u64>,
    scene: scene::SceneWorld,
    pending: Option<Pending>,
    next_handoff: u64,
    late_open: Option<u64>,
}
#[derive(Clone, Copy)]
enum CallKind {
    Open,
    Update,
    Scroll,
    Close,
}
struct Pending {
    id: u64,
    kind: CallKind,
    deadline: u64,
}
#[derive(Resource, Default)]
struct Sequence {
    session: u64,
    call: u64,
}

pub struct DashboardPlugin;
impl Plugin for DashboardPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<ScenePlugin>() {
            app.add_plugins(ScenePlugin);
        }
        app.init_resource::<Sequence>()
            .register_type::<Node>()
            .register_type::<Name>()
            .register_type::<ZIndex>()
            .register_type::<BackgroundColor>()
            .register_type::<BorderColor>()
            .register_type::<ScrollPosition>()
            .register_type::<fux::surface::Text>()
            .register_type::<ChildOf>()
            .register_type::<Children>()
            .add_systems(PreUpdate, ingest.in_set(Phase::Completions))
            .add_systems(PostUpdate, refresh.in_set(Phase::Projection));
    }
}
pub fn state(world: &mut World, id: u64) -> Result<State, String> {
    world
        .query::<&Dashboard>()
        .iter(world)
        .find(|d| d.state.id == id)
        .map(|d| d.state.clone())
        .ok_or_else(|| "dashboard not found".into())
}
fn call(
    world: &mut World,
    kind: CallKind,
    method: &str,
    mut params: Map<String, Value>,
    fux_instance: &str,
) -> Pending {
    params.insert(
        "_expected_instance".into(),
        Value::String(fux_instance.to_owned()),
    );
    let id = {
        let mut seq = world.resource_mut::<Sequence>();
        seq.call += 1;
        CALL_TAG | seq.call
    };
    world
        .resource_mut::<Messages<Effect>>()
        .write(Effect::FuxCall {
            call: id,
            method: method.into(),
            params: Value::Object(params),
        });
    Pending {
        id,
        kind,
        deadline: world
            .resource::<Clock>()
            .now_ms
            .saturating_add(CALL_TIMEOUT_MS),
    }
}
pub fn open(world: &mut World, params: Open) -> Result<State, String> {
    if params.workspace.is_empty() || params.workspace.len() > 256 {
        return Err("invalid dashboard workspace".into());
    }
    if world
        .query::<&Dashboard>()
        .iter(world)
        .filter(|d| d.state.status != "closed" || d.late_open.is_some())
        .count()
        >= MAX_SESSIONS
    {
        return Err("dashboard session limit".into());
    }
    if world.query::<&Dashboard>().iter(world).any(|d| {
        d.state.node == params.node && (d.state.status != "closed" || d.late_open.is_some())
    }) {
        return Err("dashboard node already owned".into());
    }
    // Closed sessions retain no ownership. Keep only a bounded recent result window.
    let closed: Vec<_> = world
        .query::<(Entity, &Dashboard)>()
        .iter(world)
        .filter(|(_, d)| d.state.status == "closed" && d.late_open.is_none())
        .map(|(e, _)| e)
        .collect();
    for e in closed {
        world.despawn(e);
    }
    let fux_instance = world
        .resource::<lifecycle::Link>()
        .instance
        .clone()
        .ok_or("dashboard requires a connected fux incarnation")?;
    let rows = rows(world, params.machine.as_deref())?;
    let scene = scene::SceneWorld::new(world, &rows)?;
    let id = {
        let mut seq = world.resource_mut::<Sequence>();
        seq.session += 1;
        seq.session
    };
    let provider = format!(
        "zor-dashboard:{}:{id}",
        world.resource::<ServerInstance>().nonce
    );
    let pending = call(
        world,
        CallKind::Open,
        "fux/surface.open",
        Map::from_iter([
            ("workspace".into(), json!(params.workspace)),
            ("node".into(), json!(params.node)),
            ("provider".into(), json!(provider)),
            ("generation".into(), json!(params.generation)),
        ]),
        &fux_instance,
    );
    let state = State {
        id,
        provider,
        workspace: params.workspace,
        node: params.node,
        revision: 0,
        status: "opening".into(),
        selected: rows.first().map(|r| r.key.clone()),
        rows,
        row_nodes: scene.row_nodes(),
        handoff: None,
        problem: None,
    };
    world.spawn(Dashboard {
        state: state.clone(),
        fux_instance,
        machine: params.machine,
        viewer: params.viewer,
        scene,
        pending: Some(pending),
        next_handoff: 0,
        late_open: None,
    });
    Ok(state)
}
pub fn close(world: &mut World, id: u64) -> Result<State, String> {
    let mut dashboards = world.query::<(Entity, &Dashboard)>();
    let (e, d) = dashboards
        .iter(world)
        .find(|(_, d)| d.state.id == id)
        .ok_or("dashboard not found")?;
    if d.state.status == "closed" || d.state.status == "closing" {
        return Ok(d.state.clone());
    }
    let node = d.state.node;
    let provider = d.state.provider.clone();
    let fux_instance = d.fux_instance.clone();
    let opening = d
        .pending
        .as_ref()
        .is_some_and(|p| matches!(p.kind, CallKind::Open));
    let pending = if opening {
        None
    } else {
        Some(call(
            world,
            CallKind::Close,
            "fux/surface.close",
            Map::from_iter([
                ("surface".into(), json!(node)),
                ("expected_provider".into(), json!(provider)),
            ]),
            &fux_instance,
        ))
    };
    let mut d = world
        .get_mut::<Dashboard>(e)
        .ok_or("dashboard disappeared while closing")?;
    d.state.status = "closing".into();
    d.state.handoff = None;
    d.viewer = None;
    if !opening {
        d.pending = pending;
    }
    Ok(d.state.clone())
}

/// All input names its acknowledged scene revision. No row-number fallback is permitted.
/// A pending scene update temporarily refuses input rather than resolving it against new rows.
pub fn input(
    world: &mut World,
    id: u64,
    viewer: u64,
    revision: u64,
    provider_node: Option<u64>,
    input: Input,
) -> Result<State, String> {
    if world.resource::<crate::journal::Journal>().is_frozen()
        || world
            .resource::<bevy_state::prelude::State<crate::model::ServerMode>>()
            .get()
            == &crate::model::ServerMode::ShuttingDown
    {
        return Err("dashboard actions are disabled during shutdown or frozen recovery".into());
    }
    let mut dashboards = world.query::<(Entity, &mut Dashboard)>();
    let (e, mut d) = dashboards
        .iter_mut(world)
        .find(|(_, d)| d.state.id == id)
        .ok_or("dashboard not found")?;
    if d.state.status != "open" || d.pending.is_some() || d.state.revision != revision {
        return Err("stale or unavailable dashboard scene".into());
    }
    if d.viewer.is_some_and(|owner| owner != viewer) {
        return Err("dashboard belongs to another viewer".into());
    }
    if d.state.handoff.is_some() {
        return Err("dashboard viewer is attached elsewhere".into());
    }
    let selected = match provider_node {
        Some(node) => Some(
            d.scene
                .key_for_node(node)
                .ok_or("stale dashboard node")?
                .to_owned(),
        ),
        None => d.state.selected.clone(),
    };
    if matches!(input, Input::Close) {
        return close(world, id);
    }
    let scroll = match input {
        Input::Select => {
            d.viewer = Some(viewer);
            d.state.selected = selected;
            return Ok(d.state.clone());
        }
        Input::Scroll { rows } => Some((rows, false)),
        Input::Next | Input::Previous => {
            let current = d
                .state
                .rows
                .iter()
                .position(|r| Some(&r.key) == d.state.selected.as_ref());
            let next = match (current, &input) {
                (Some(n), Input::Previous) => n.saturating_sub(1),
                (Some(n), _) => (n + 1).min(d.state.rows.len().saturating_sub(1)),
                _ => 0,
            };
            let Some(row) = d.state.rows.get(next) else {
                d.viewer = Some(viewer);
                return Ok(d.state.clone());
            };
            d.state.selected = Some(row.key.clone());
            Some((next as i32, true))
        }
        Input::Attach | Input::Cancel | Input::Stop | Input::Reconcile | Input::Close => None,
    };
    if let Some((rows, absolute)) = scroll {
        d.viewer = Some(viewer);
        let params = Map::from_iter([
            ("surface".into(), json!(d.state.node)),
            ("expected_provider".into(), json!(d.state.provider)),
            ("revision".into(), json!(d.state.revision)),
            ("viewer".into(), json!(viewer)),
            ("provider_node".into(), json!(d.scene.root().to_bits())),
            ("rows".into(), json!(rows)),
            ("absolute".into(), json!(absolute)),
        ]);
        let instance = d.fux_instance.clone();
        let pending = call(
            world,
            CallKind::Scroll,
            "fux/surface.scroll",
            params,
            &instance,
        );
        let mut d = world
            .get_mut::<Dashboard>(e)
            .ok_or("dashboard disappeared while scrolling")?;
        d.pending = Some(pending);
        d.state.status = "updating".into();
        return Ok(d.state.clone());
    }
    let row = d
        .state
        .rows
        .iter()
        .find(|r| Some(&r.key) == selected.as_ref())
        .ok_or("selected row no longer exists")?;
    if row.freshness != Freshness::Fresh {
        return Err("selected observation is stale".into());
    }
    let target = row.target.clone().ok_or("row has no task target")?;
    // Re-derive current observations before dispatch, then let Machines atomically guard again.
    if !rows(world, Some(&target.machine))?
        .iter()
        .any(|r| r.target.as_ref() == Some(&target) && r.freshness == Freshness::Fresh)
    {
        return Err("selected target was replaced".into());
    }
    if matches!(input, Input::Attach) {
        if target
            .pane
            .as_ref()
            .is_none_or(|pane| pane.pid.is_none() || pane.pane == 0)
        {
            return Err("attachment requires an identified live pane process".into());
        }
        if target.machine == "local" {
            validate_local_attachment(world, &target)?;
        } else {
            machines::exact_attachment(world, &target.machine, &target.task, &target.guard())?;
        }
        let mut d = world
            .get_mut::<Dashboard>(e)
            .ok_or("dashboard disappeared during attachment")?;
        d.next_handoff += 1;
        d.viewer = Some(viewer);
        d.state.selected = selected;
        d.state.handoff = Some(Handoff {
            id: d.next_handoff,
            viewer,
            target,
        });
        return Ok(d.state.clone());
    }
    let kind = match input {
        Input::Cancel => ActionKind::Cancel,
        Input::Stop => ActionKind::Stop,
        Input::Reconcile => ActionKind::Reconcile,
        _ => return Err("dashboard input is not a task action".into()),
    };
    let operation = {
        let mut seq = world.resource_mut::<Sequence>();
        seq.call += 1;
        let call = seq.call;
        let nonce = &world.resource::<ServerInstance>().nonce;
        format!("dash-{}-{call:x}", nonce.get(..40).unwrap_or(nonce))
    };
    if target.machine == "local" {
        let task = world
            .resource::<Ids>()
            .task(&target.task)
            .ok_or("local task disappeared")?;
        match kind {
            ActionKind::Cancel => lifecycle::cancel_task(world, task).map_err(|e| e.to_string())?,
            ActionKind::Stop => lifecycle::request_stop(world, task).map_err(|e| e.to_string())?,
            ActionKind::Reconcile => {
                // Receipt reconciliation is read-only and never reserves or submits input.
                let prompts: Vec<_> = world
                    .resource::<Ids>()
                    .prompts
                    .values()
                    .copied()
                    .filter(|&p| {
                        world
                            .get::<crate::model::PromptOf>(p)
                            .and_then(|p| lifecycle::task_of(world, p.0))
                            == Some(task)
                    })
                    .collect();
                for prompt in prompts {
                    lifecycle::refresh_receipt(world, prompt);
                }
            }
            ActionKind::Resume { .. } => return Err("dashboard cannot resume a task".into()),
        }
    } else {
        machines::queue_action(
            world,
            &target.machine,
            &target.task,
            kind,
            target.guard(),
            &operation,
        )?;
    }
    let mut d = world
        .get_mut::<Dashboard>(e)
        .ok_or("dashboard disappeared during task action")?;
    d.viewer = Some(viewer);
    d.state.selected = selected;
    Ok(d.state.clone())
}
/// Completion/cancellation relinquishes only the attachment handoff, never its pane. Selection
/// retains the exact old key even if the task changed during attachment; the next action refuses.
pub fn returned(world: &mut World, id: u64, handoff: u64) -> Result<State, String> {
    let mut dashboards = world.query::<&mut Dashboard>();
    let mut d = dashboards
        .iter_mut(world)
        .find(|d| d.state.id == id)
        .ok_or("dashboard not found")?;
    if d.state.handoff.as_ref().map(|h| h.id) != Some(handoff) {
        return Err("stale handoff return".into());
    }
    d.state.handoff = None;
    d.viewer = None;
    Ok(d.state.clone())
}

fn ingest(mut inbound: MessageReader<Inbound>, mut commands: Commands) {
    for message in inbound.read() {
        match message {
            Inbound::FuxReply { call, result } if call & TAG_MASK == CALL_TAG => {
                let (call, result) = (*call, result.clone());
                commands.queue(move |world: &mut World| {
                    if let Err(error) = reply(world, call, result) {
                        bevy_log::warn!("dashboard reply failed: {error}");
                    }
                });
            }
            Inbound::FuxEvent { name, body, .. } if name.ends_with("SurfaceInput") => {
                let body = body.clone();
                commands.queue(move |world: &mut World| surface_input(world, &body));
            }
            Inbound::FuxEvent { name, body, .. } if name.ends_with("ViewerDetached") => {
                if let Some(viewer) = body.get("viewer").and_then(Value::as_u64) {
                    commands.queue(move |world: &mut World| {
                        let ids: Vec<_> = world
                            .query::<&Dashboard>()
                            .iter(world)
                            .filter(|d| d.viewer == Some(viewer) && d.state.handoff.is_none())
                            .map(|d| d.state.id)
                            .collect();
                        for id in ids {
                            let _ = close(world, id);
                        }
                    });
                }
            }
            Inbound::FuxGap { .. } | Inbound::FuxLink { instance: None } => {
                commands.queue(|world: &mut World| {
                    let ids: Vec<_> = world
                        .query::<&Dashboard>()
                        .iter(world)
                        .filter(|d| d.state.status != "closed")
                        .map(|d| d.state.id)
                        .collect();
                    for id in ids {
                        let _ = close(world, id);
                    }
                });
            }
            _ => {}
        }
    }
}
fn reply(world: &mut World, call_id: u64, result: Result<Value, String>) -> Result<(), String> {
    let mut dashboards = world.query::<(Entity, &mut Dashboard)>();
    let Some((e, mut d)) = dashboards.iter_mut(world).find(|(_, d)| {
        d.late_open == Some(call_id) || d.pending.as_ref().is_some_and(|p| p.id == call_id)
    }) else {
        return Ok(());
    };
    if d.late_open == Some(call_id) {
        d.late_open = None;
        if result.is_ok() {
            let (node, provider, instance) = (
                d.state.node,
                d.state.provider.clone(),
                d.fux_instance.clone(),
            );
            let pending = call(
                world,
                CallKind::Close,
                "fux/surface.close",
                Map::from_iter([
                    ("surface".into(), json!(node)),
                    ("expected_provider".into(), json!(provider)),
                ]),
                &instance,
            );
            let mut d = world
                .get_mut::<Dashboard>(e)
                .ok_or("dashboard disappeared after late open")?;
            d.pending = Some(pending);
            d.state.status = "closing".into();
        }
        return Ok(());
    }
    let pending = d
        .pending
        .take()
        .ok_or("dashboard reply has no pending call")?;
    let was_closing = d.state.status == "closing";
    match (pending.kind, result) {
        (CallKind::Close, result) => {
            d.state.status = "closed".into();
            d.state.problem = result.err();
            d.viewer = None;
            d.state.handoff = None;
        }
        (CallKind::Open, Ok(_)) => {
            d.state.status = "open".into();
        }
        (CallKind::Update | CallKind::Scroll, Ok(_)) => {
            d.state.status = "open".into();
        }
        (_, Err(error)) => {
            d.state.problem = Some(error);
            d.state.status = "failed".into();
            d.viewer = None;
            d.state.handoff = None;
        }
    }
    if was_closing && !matches!(pending.kind, CallKind::Close) {
        let id = d.state.id;
        close(world, id)?;
    }
    Ok(())
}
fn surface_input(world: &mut World, body: &Value) {
    let Some(surface) = body.get("surface").and_then(Value::as_u64) else {
        return;
    };
    let Some(viewer) = body.get("viewer").and_then(Value::as_u64) else {
        return;
    };
    let Some(revision) = body.get("revision").and_then(Value::as_u64) else {
        return;
    };
    let provider = body.get("provider").and_then(Value::as_str);
    let id = world
        .query::<&Dashboard>()
        .iter(world)
        .find(|d| d.state.node == surface && Some(d.state.provider.as_str()) == provider)
        .map(|d| d.state.id);
    let Some(id) = id else {
        return;
    };
    let Some(kind) = body.get("kind") else {
        return;
    };
    let node = body.get("provider_node").and_then(Value::as_u64);
    let action = if kind.as_str() == Some("Press") {
        Some(Input::Select)
    } else if let Some(rows) = kind
        .get("Scroll")
        .and_then(|s| s.get("rows"))
        .and_then(Value::as_i64)
    {
        Some(Input::Scroll {
            rows: rows.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        })
    } else if kind.as_str() == Some("Key") {
        let Some(bytes) = body.get("bytes") else {
            return;
        };
        let Ok(bytes) = serde_json::from_value::<Vec<u8>>(bytes.clone()) else {
            return;
        };
        match bytes.as_slice() {
            b"j" | b"\x1b[B" => Some(Input::Next),
            b"k" | b"\x1b[A" => Some(Input::Previous),
            b"a" | b"\r" => Some(Input::Attach),
            b"c" => Some(Input::Cancel),
            b"s" => Some(Input::Stop),
            b"l" => Some(Input::Reconcile),
            b"q" | b"\x03" => Some(Input::Close),
            _ => None,
        }
    } else {
        None
    };
    if let Some(action) = action {
        // Keys and wheel belong to the list, not whichever row happens to be under the cursor.
        let node = if matches!(action, Input::Select) {
            node
        } else {
            None
        };
        if let Err(error) = input(world, id, viewer, revision, node, action)
            && let Some(mut d) = world
                .query::<&mut Dashboard>()
                .iter_mut(world)
                .find(|d| d.state.id == id)
        {
            d.state.problem = Some(error);
        }
    }
}

/// Exclusive only for atomic BSN scene resolution/export against the App's AssetServer and
/// registry. Ordinary ingress is a typed system; no adapter work runs in this World operation.
fn refresh(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    let entities: Vec<_> = world
        .query_filtered::<Entity, With<Dashboard>>()
        .iter(world)
        .collect();
    for e in entities {
        let Ok(mut entity) = world.get_entity_mut(e) else {
            bevy_log::warn!("dashboard entity disappeared before refresh");
            continue;
        };
        let Some(mut d) = entity.take::<Dashboard>() else {
            bevy_log::warn!("dashboard component disappeared before refresh");
            continue;
        };
        if d.pending.as_ref().is_some_and(|p| now >= p.deadline) {
            d.state.problem = Some("dashboard surface call timed out; outcome unknown".into());
            d.state.handoff = None;
            d.viewer = None;
            if let Some(pending) = &d.pending
                && matches!(pending.kind, CallKind::Open)
            {
                d.late_open = Some(pending.id);
            }
            if d.pending
                .as_ref()
                .is_some_and(|p| matches!(p.kind, CallKind::Close))
            {
                d.pending = None;
                d.state.status = "closed".into();
            } else {
                d.state.status = "closing".into();
                d.pending = Some(call(
                    world,
                    CallKind::Close,
                    "fux/surface.close",
                    Map::from_iter([
                        ("surface".into(), json!(d.state.node)),
                        ("expected_provider".into(), json!(d.state.provider)),
                    ]),
                    &d.fux_instance,
                ));
            }
        }
        if d.state.status == "open" && d.pending.is_none() {
            match rows(world, d.machine.as_deref()).and_then(|rows| {
                let mut rendered = rows.clone();
                for row in &mut rendered {
                    row.text = format!(
                        "{} {}",
                        if d.state.selected.as_ref() == Some(&row.key) {
                            ">"
                        } else {
                            " "
                        },
                        row.text
                    );
                }
                let delta = d.scene.sync(world, &rendered)?;
                d.state.rows = rows;
                d.state.row_nodes = d.scene.row_nodes();
                Ok(delta)
            }) {
                Ok(Some(delta)) => {
                    d.state.revision += 1;
                    d.state.status = "updating".into();
                    d.pending = Some(call(
                        world,
                        CallKind::Update,
                        "fux/surface.update",
                        Map::from_iter([
                            ("surface".into(), json!(d.state.node)),
                            ("expected_provider".into(), json!(d.state.provider)),
                            ("revision".into(), json!(d.state.revision)),
                            ("full".into(), json!(delta.full)),
                            ("delta".into(), json!(delta.ron)),
                        ]),
                        &d.fux_instance,
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    d.state.problem = Some(error);
                }
            }
        }
        match world.get_entity_mut(e) {
            Ok(mut entity) => {
                entity.insert(d);
            }
            Err(error) => bevy_log::warn!("dashboard entity disappeared during refresh: {error}"),
        }
    }
}
pub fn next_deadline(world: &mut World) -> Option<u64> {
    world
        .query::<&Dashboard>()
        .iter(world)
        .filter_map(|d| d.pending.as_ref().map(|p| p.deadline))
        .filter(|deadline| *deadline != u64::MAX)
        .min()
}
