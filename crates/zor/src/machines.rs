//! Machine catalog assets, independent supervision, exact-target routing and durable actions.
//! Catalog configuration is desired state; entities own runtime observations and workers.
pub mod catalog;
pub mod intents;
pub mod supervision;
pub mod transport;

use std::path::PathBuf;
use async_channel::Sender;
use bevy_app::{App, Plugin, PostUpdate, PreUpdate, Startup};
use bevy_asset::{Asset, AssetApp, AssetEvent, AssetEventSystems, AssetLoader, AssetLoadFailedEvent, AssetServer, Assets, Handle, LoadContext, io::Reader};
use bevy_ecs::prelude::*;
use bevy_reflect::TypePath;
use serde::{Deserialize, Serialize};
use crate::model::{Clock, Ids, Inbound, Machine, MachineId, MachineName, Phase};
use catalog::{Catalog, CatalogError, MachineEntry};
use intents::{ActionIntent, IntentLog};
use supervision::{ActionFailure, ActionKind, ActionPhase, ActionRecord, ActionRequest, Freshness, PaneIdentity, RemoteView, Report, Reports, Supervision, Worker, MAX_ACTIONS, FRESH_MS};
use transport::Descriptor;

pub const CATALOG_FILE: &str = "machines.json";
#[derive(Resource, Clone, Default)]
pub struct MachinesFile { pub path: PathBuf, pub asset_root: Option<PathBuf> }
#[derive(Resource)]
struct CatalogState { current: Catalog, pending: Option<Catalog>, problem: Option<String>, generation: u64 }
impl Default for CatalogState {
    fn default() -> Self { Self { current: Catalog::empty(), pending: None, problem: None, generation: 0 } }
}
#[derive(Component)]
struct Binding { entry: MachineEntry, generation: u64 }
#[derive(Resource, Default)]
struct Wake(Option<Sender<Inbound>>);
#[derive(Asset, TypePath)]
struct CatalogAsset(Catalog);
#[derive(Resource)]
struct CatalogHandle(Handle<CatalogAsset>);
#[derive(TypePath)]
struct CatalogLoader { path: PathBuf }
impl AssetLoader for CatalogLoader {
    type Asset = CatalogAsset;
    type Settings = ();
    type Error = CatalogError;
    async fn load(&self, _reader: &mut dyn Reader, _settings: &(), _context: &mut LoadContext<'_>) -> Result<CatalogAsset, CatalogError> {
        // The private descriptor catalog must be read with O_NOFOLLOW and checked ownership;
        // the asset reader alone does not enforce that security boundary.
        Catalog::load(&self.path).map(CatalogAsset)
    }
    fn extensions(&self) -> &[&str] { &["json"] }
}

pub struct MachinesPlugin;
impl Plugin for MachinesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MachinesFile>().init_resource::<CatalogState>()
            .init_resource::<IntentLog>().init_resource::<Reports>().init_resource::<Wake>()
            .add_systems(Startup, initialize)
            .add_systems(PreUpdate, (activate_catalog, start_workers, ingest_reports, expire_views).chain().in_set(Phase::Completions));
        let file = app.world().resource::<MachinesFile>().clone();
        if let Some(root) = file.asset_root && app.world().contains_resource::<AssetServer>() {
            app.init_asset::<CatalogAsset>().register_asset_loader(CatalogLoader { path: file.path.clone() });
            let parent = file.path.parent().and_then(|p| p.canonicalize().ok());
            if parent.as_deref() == Some(root.as_path()) && let Some(name) = file.path.file_name() {
                let handle = app.world().resource::<AssetServer>().load(name.to_string_lossy().into_owned());
                app.insert_resource(CatalogHandle(handle)).add_systems(PostUpdate, asset_changes.after(AssetEventSystems));
            }
        }
    }
}

fn initialize(file: Res<MachinesFile>, mut state: ResMut<CatalogState>, mut log: ResMut<IntentLog>, mut reports: ResMut<Reports>) {
    if file.path.as_os_str().is_empty() { return; }
    match Catalog::load(&file.path) { Ok(catalog) => state.pending = Some(catalog), Err(error) => state.problem = Some(error.to_string()) }
    match IntentLog::load(&file.path) {
        Ok(loaded) => { reports.next_action = loaded.records().iter().map(|r| r.record.id).max().unwrap_or(0).saturating_add(1); *log = loaded; }
        Err(error) => state.problem = Some(error.to_string()),
    }
}
fn asset_changes(mut events: MessageReader<AssetEvent<CatalogAsset>>, mut failures: MessageReader<AssetLoadFailedEvent<CatalogAsset>>, handle: Res<CatalogHandle>, assets: Res<Assets<CatalogAsset>>, mut state: ResMut<CatalogState>, wake: Res<Wake>) {
    if events.read().any(|e| matches!(e, AssetEvent::Added{id} | AssetEvent::Modified{id} if *id == handle.0.id())) {
        if let Some(asset) = assets.get(&handle.0) {
            state.pending = Some(asset.0.clone());
            state.problem = None;
            // Activation is in PreUpdate; a sleeping runner needs another update after
            // PostUpdate publishes the desired catalog.
            if let Some(inbound) = &wake.0 { let _ = inbound.try_send(Inbound::Wake); }
        }
    }
    for failure in failures.read().filter(|f| f.id == handle.0.id()) { state.problem = Some(failure.error.to_string()); }
}
fn activate_catalog(mut commands: Commands, mut state: ResMut<CatalogState>, mut machines: Query<(Entity, &mut Binding, &mut MachineName, &mut Supervision)>, mut log: ResMut<IntentLog>) {
    let Some(catalog) = state.pending.take() else { return; };
    if catalog == state.current { return; }
    state.generation = state.generation.saturating_add(1);
    let generation = state.generation;
    for (entity, mut binding, mut name, mut supervision) in &mut machines {
        let Some(entry) = catalog.machines.iter().find(|m| m.id == binding.entry.id) else {
            retire_actions(&mut supervision, &mut log, &mut state);
            commands.entity(entity).despawn();
            continue;
        };
        if *entry == binding.entry { continue; }
        if entry.control != binding.entry.control || entry.attachments != binding.entry.attachments {
            retire_actions(&mut supervision, &mut log, &mut state);
            commands.entity(entity).remove::<Worker>();
            // Old action evidence is retained, but the old view cannot authorize new actions.
            supervision.freshness = Freshness::Offline;
            supervision.problem = Some("catalog endpoint changed; waiting for a fresh observation".into());
            binding.generation = generation;
        }
        if name.0 != entry.name { name.0.clone_from(&entry.name); }
        binding.entry = entry.clone();
    }
    for entry in &catalog.machines {
        if state.current.machines.iter().any(|old| old.id == entry.id) { continue; }
        let mut supervision = Supervision::default();
        for intent in log.records().iter().filter(|r| r.machine == entry.id).rev().take(MAX_ACTIONS).collect::<Vec<_>>().into_iter().rev() {
            let mut record = intent.record.clone();
            if record.phase == ActionPhase::Submitting { record.phase = ActionPhase::Uncertain; record.problem = Some("controller restarted; inspect remote evidence, never replay".into()); }
            supervision.push_action(record);
        }
        commands.spawn((Machine, MachineId(entry.id.clone()), MachineName(entry.name.clone()), Binding { entry: entry.clone(), generation }, supervision));
    }
    state.current = catalog;
}
fn retire_actions(supervision: &mut Supervision, log: &mut IntentLog, state: &mut CatalogState) {
    for record in &mut supervision.actions {
        if record.phase != ActionPhase::Submitting { continue; }
        record.phase = ActionPhase::Uncertain;
        record.problem = Some("machine binding retired; outcome is unknown and is never replayed".into());
        if let Err(error) = log.complete(record) { state.problem = Some(error.to_string()); }
    }
}
fn start_workers(mut commands: Commands, machines: Query<(Entity, &Binding), Without<Worker>>, reports: Res<Reports>, wake: Res<Wake>) {
    for (entity, binding) in &machines {
        if let Some(control) = &binding.entry.control {
            commands.entity(entity).insert(Worker::spawn(entity, binding.generation, control.clone(), reports.tx.clone(), wake.0.clone()));
        }
    }
}
fn ingest_reports(reports: Res<Reports>, mut machines: Query<(&Binding, &mut Supervision)>, mut log: ResMut<IntentLog>, mut state: ResMut<CatalogState>) {
    for _ in 0..catalog::MAX_MACHINES * 2 {
        let Ok(report) = reports.rx.try_recv() else { break; };
        match report {
            Report::Poll { machine, generation, at_ms, outcome } => {
                let Ok((binding, mut supervision)) = machines.get_mut(machine) else { continue; };
                if binding.generation != generation { continue; }
                let outcome = outcome.and_then(|view| {
                    if binding.entry.control.as_ref().is_some_and(|t| t.descriptor().instance == view.instance) { Ok(view) }
                    else { Err(supervision::Failure::Expired("service identity changed".into())) }
                });
                supervision.apply_poll(outcome, at_ms);
            }
            Report::Action { machine, generation, id, at_ms, outcome } => {
                let Ok((binding, mut supervision)) = machines.get_mut(machine) else { continue; };
                if binding.generation != generation { continue; }
                let Some(record) = supervision.actions.iter_mut().find(|a| a.id == id) else { continue; };
                record.finished_ms = Some(at_ms);
                match outcome {
                    Ok(value) => { record.phase = ActionPhase::Done; record.result = Some(value); }
                    Err(failure) => { let (phase, problem) = match failure { ActionFailure::Refused(p) => (ActionPhase::Refused,p), ActionFailure::Failed(p) => (ActionPhase::Failed,p), ActionFailure::Uncertain(p) => (ActionPhase::Uncertain,p) }; record.phase = phase; record.problem = Some(problem); }
                }
                if let Err(error) = log.complete(record) { state.problem = Some(error.to_string()); }
            }
        }
    }
}
fn expire_views(clock: Res<Clock>, mut machines: Query<&mut Supervision>) { for mut supervision in &mut machines { supervision.expire(clock.now_ms); } }
pub fn install_wake(world: &mut World, inbound: Sender<Inbound>) { world.resource_mut::<Wake>().0 = Some(inbound); }
pub fn next_deadline(world: &mut World) -> Option<u64> { world.query::<&Supervision>().iter(world).filter(|s| s.freshness == Freshness::Fresh).map(|s| s.observed_ms.saturating_add(FRESH_MS + 1)).min() }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineGuard { pub instance: String, pub attempt: Option<u64>, pub pane: Option<PaneIdentity> }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSnapshot {
    pub id: String, pub name: String, pub freshness: Freshness, pub observed_ms: u64,
    pub view: Option<RemoteView>, pub problem: Option<String>, pub actions: Vec<ActionRecord>,
}
pub struct ExactAttachment { pub machine: String, pub task: String, pub attempt: u64, pub pane: PaneIdentity, pub descriptor: Descriptor }
fn entity(world: &World, selector: &str) -> Result<Entity, String> {
    let entry = world.resource::<CatalogState>().current.find(selector).ok_or_else(|| format!("unknown machine {selector}"))?;
    world.resource::<Ids>().machine(&entry.id).ok_or_else(|| "machine configuration is being activated".into())
}
pub fn snapshot(world: &World, selector: &str) -> Result<MachineSnapshot, String> {
    let entity = entity(world, selector)?;
    let binding = world.get::<Binding>(entity).ok_or("machine has no binding")?;
    let supervision = world.get::<Supervision>(entity).ok_or("machine has no supervision")?;
    Ok(MachineSnapshot { id: binding.entry.id.clone(), name: binding.entry.name.clone(), freshness: supervision.freshness, observed_ms: supervision.observed_ms, view: supervision.view.clone(), problem: supervision.problem.clone(), actions: supervision.actions.iter().cloned().collect() })
}
pub fn snapshots(world: &World) -> Vec<MachineSnapshot> { world.resource::<CatalogState>().current.machines.iter().filter_map(|m| snapshot(world, &m.id).ok()).collect() }
pub fn catalog_problem(world: &World) -> Option<String> { world.resource::<CatalogState>().problem.clone() }
pub fn reload(world: &mut World) -> Result<(), String> {
    let path = world.resource::<MachinesFile>().path.clone();
    match Catalog::load(&path) { Ok(catalog) => { let mut state = world.resource_mut::<CatalogState>(); state.pending = Some(catalog); state.problem = None; Ok(()) }, Err(error) => { let text = error.to_string(); world.resource_mut::<CatalogState>().problem = Some(text.clone()); Err(text) } }
}
pub fn edit_catalog(world: &mut World, change: impl FnOnce(&mut Catalog) -> Result<(), String>) -> Result<(), String> {
    let state = world.resource::<CatalogState>();
    let mut next = state.pending.as_ref().unwrap_or(&state.current).clone();
    change(&mut next)?;
    next.save(&world.resource::<MachinesFile>().path).map_err(|e| e.to_string())?;
    world.resource_mut::<CatalogState>().pending = Some(next);
    Ok(())
}
/// Explicitly selected control credential; caller must not expose it as a read projection.
pub fn control_descriptor(world: &World, selector: &str) -> Result<Descriptor, String> {
    let entity = entity(world,selector)?;
    world.get::<Binding>(entity).and_then(|b| b.entry.control.as_ref()).ok_or("machine has no control binding")?.resolve().map(|r| r.descriptor).map_err(|e| e.to_string())
}
/// Private endpoint resolution uses only the activated binding, never a catalog draft.
pub fn endpoint_descriptor(world: &World, selector: &str, workspace: Option<&str>) -> Result<Descriptor, String> {
    let Some(workspace) = workspace else { return control_descriptor(world, selector); };
    let entity = entity(world, selector)?;
    let binding = world.get::<Binding>(entity).ok_or("machine has no binding")?;
    binding.entry.attachments.get(workspace).ok_or("workspace has no attachment binding")?
        .resolve().map(|r| r.descriptor).map_err(|e| e.to_string())
}
fn validate_guard(world: &World, entity: Entity, task: &str, guard: &MachineGuard) -> Result<(), String> {
    let binding = world.get::<Binding>(entity).ok_or("machine has no binding")?;
    if let Some(pending) = &world.resource::<CatalogState>().pending {
        let next = pending.find(&binding.entry.id).ok_or("machine removal is pending")?;
        if next.control != binding.entry.control || next.attachments != binding.entry.attachments {
            return Err("machine endpoint update is pending".into());
        }
    }
    let supervision = world.get::<Supervision>(entity).ok_or("machine has no observation")?;
    let now = world.resource::<Clock>().now_ms;
    if supervision.freshness != Freshness::Fresh || now.saturating_sub(supervision.observed_ms) > FRESH_MS { return Err("machine observation is not fresh".into()); }
    let view = supervision.view.as_ref().ok_or("machine has no observation")?;
    if view.instance != guard.instance { return Err("remote service identity changed".into()); }
    let row = view.rows.iter().find(|t| t.id == task).ok_or("task is not observed on this machine")?;
    if row.current_attempt != guard.attempt { return Err("task attempt changed".into()); }
    if let Some(expected) = &guard.pane {
        let matches = view.agents.iter().filter(|a| a.task.as_deref() == Some(task) && a.attempt == guard.attempt).filter(|a| a.instance == expected.instance && a.workspace == expected.workspace && a.pane == expected.pane && a.pid == expected.pid).count();
        if matches != 1 { return Err("exact pane identity changed or is ambiguous".into()); }
    } else if guard.attempt.is_some() { return Err("active attempts require exact pane identity".into()); }
    Ok(())
}
pub fn exact_attachment(world: &World, selector: &str, task: &str, guard: &MachineGuard) -> Result<ExactAttachment, String> {
    let entity = entity(world,selector)?;
    validate_guard(world,entity,task,guard)?;
    let pane = guard.pane.clone().ok_or("task has no pane")?;
    let binding = world.get::<Binding>(entity).ok_or("machine has no binding")?;
    let transport = binding.entry.attachments.get(&pane.workspace).ok_or("workspace has no attachment binding")?;
    let descriptor = transport.resolve().map_err(|e| e.to_string())?.descriptor;
    if descriptor.instance != pane.instance || descriptor.attach.is_none() { return Err("attachment binding does not identify the observed fux incarnation".into()); }
    Ok(ExactAttachment { machine:binding.entry.id.clone(), task:task.into(), attempt:guard.attempt.ok_or("task has no attempt")?, pane, descriptor })
}
pub fn queue_action(world: &mut World, selector: &str, task: &str, kind: ActionKind, guard: MachineGuard, operation: &str) -> Result<ActionRecord, String> {
    if !crate::model::valid_id(operation) { return Err("invalid operation id".into()); }
    let entity = entity(world,selector)?;
    validate_guard(world,entity,task,&guard)?;
    if world.get::<Supervision>(entity).is_some_and(|s| s.actions.iter().filter(|a| a.phase == ActionPhase::Submitting).count() >= MAX_ACTIONS) {
        return Err("machine has too many unresolved actions".into());
    }
    if world.resource::<IntentLog>().find(operation).is_some() { return Err("operation intent is retained; inspect rather than replay".into()); }
    if let ActionKind::Resume { operation: resume, .. } = &kind { if resume != operation { return Err("resume operation differs from intent operation".into()); } }
    let actions = world.get::<Worker>(entity).ok_or("machine worker is not available")?.actions.clone();
    if actions.is_full() { return Err("machine action queue is full".into()); }
    let binding = world.get::<Binding>(entity).ok_or("machine has no binding")?;
    let machine = binding.entry.id.clone();
    let control = binding.entry.control.as_ref().ok_or("machine has no control binding")?.address();
    let now = world.resource::<Clock>().now_ms;
    let id = { let mut reports = world.resource_mut::<Reports>(); let id = reports.next_action; reports.next_action = id.checked_add(1).ok_or("action ids exhausted")?; id };
    let record = ActionRecord { id, kind:Some(kind.clone()), task:task.into(), attempt:guard.attempt, instance:guard.instance.clone(), pane:guard.pane.clone(), phase:ActionPhase::Submitting, result:None, problem:None, requested_ms:now, finished_ms:None };
    // This bounded private fsync is the durable authorization boundary. No request reaches
    // the worker unless the complete immutable intent was atomically committed.
    world.resource_mut::<IntentLog>().commit(ActionIntent { operation:operation.into(), machine, control, record:record.clone() }).map_err(|e| e.to_string())?;
    world.get_mut::<Supervision>(entity).ok_or("machine disappeared")?.push_action(record.clone());
    if actions.try_send(ActionRequest {id, kind, task:task.into(), instance:guard.instance, attempt:guard.attempt, pane:guard.pane}).is_err() {
        return Err("intent committed but worker unavailable; inspect intent, never replay".into());
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test assertions")]
    use super::*;
    use supervision::{Failure, RemoteTask, RemoteAgent};

    fn entry(id: &str, nonce: &str) -> MachineEntry {
        MachineEntry { id:id.into(), name:id.into(), attachments:Default::default(),
            control:Some(transport::Transport::Direct { host:"127.0.0.1".into(),port:1234,
                brp:Descriptor {instance:nonce.into(),pid:1,http:crate::remote::Endpoint {host:"127.0.0.1".into(),port:1234},attach:None,token:"private".into()} }) }
    }
    fn app(entries: Vec<MachineEntry>) -> App {
        let mut app = App::new();
        app.init_resource::<Ids>().init_resource::<Clock>().init_resource::<Reports>()
            .init_resource::<IntentLog>().insert_resource(CatalogState {pending:Some(Catalog {version:1,machines:entries}),..Default::default()})
            .add_systems(PreUpdate,(activate_catalog,ingest_reports,expire_views).chain());
        app.update();
        app
    }
    fn poll(app: &App, id: &str, generation: u64, outcome: Result<RemoteView,Failure>) {
        let entity = app.world().resource::<Ids>().machine(id).unwrap();
        app.world().resource::<Reports>().tx.try_send(Report::Poll {machine:entity,generation,at_ms:100,outcome}).unwrap();
    }
    #[test]
    fn stale_worker_and_service_identity_cannot_replace_observations() {
        let mut app = app(vec![entry("one","n1"),entry("two","n2")]);
        let first = RemoteView {instance:"n1".into(),tasks:4,..Default::default()};
        poll(&app,"one",1,Ok(first.clone()));
        poll(&app,"two",1,Err(Failure::Unauthorized("denied".into())));
        app.update();
        assert_eq!(snapshot(app.world(),"one").unwrap().view,Some(first.clone()));
        assert_eq!(snapshot(app.world(),"two").unwrap().freshness,Freshness::Unauthorized);
        app.world_mut().resource_mut::<CatalogState>().pending = Some(Catalog {version:1,machines:vec![entry("one","replacement"),entry("two","n2")]});
        poll(&app,"one",1,Ok(RemoteView {instance:"n1".into(),tasks:99,..Default::default()}));
        app.update();
        assert_eq!(snapshot(app.world(),"one").unwrap().view,Some(first));
        assert_eq!(snapshot(app.world(),"one").unwrap().freshness,Freshness::Offline);
        poll(&app,"one",2,Ok(RemoteView {instance:"wrong-service".into(),..Default::default()}));
        app.update();
        assert_eq!(snapshot(app.world(),"one").unwrap().freshness,Freshness::Expired);
        let retired = app.world().resource::<Ids>().machine("one").unwrap();
        app.world_mut().resource_mut::<CatalogState>().pending = Some(Catalog {version:1,machines:vec![entry("two","n2")]});
        app.world().resource::<Reports>().tx.try_send(Report::Poll {machine:retired,generation:2,at_ms:200,outcome:Ok(RemoteView {instance:"replacement".into(),..Default::default()})}).unwrap();
        app.update();
        assert!(snapshot(app.world(),"one").is_err());
        assert_eq!(snapshot(app.world(),"two").unwrap().freshness,Freshness::Unauthorized);
    }
    #[test]
    fn guarded_selection_refuses_changed_attempt_pane_and_stale_view() {
        let mut app = app(vec![entry("one","n1")]);
        let pane = PaneIdentity {instance:"fux".into(),workspace:"work".into(),pane:9,pid:Some(42)};
        let view = RemoteView {instance:"n1".into(),rows:vec![RemoteTask {id:"task".into(),current_attempt:Some(7),..Default::default()}],
            agents:vec![RemoteAgent {id:1,instance:pane.instance.clone(),workspace:pane.workspace.clone(),pane:pane.pane,pid:pane.pid,task:Some("task".into()),attempt:Some(7),..Default::default()}],..Default::default()};
        poll(&app,"one",1,Ok(view));
        app.update();
        let entity = app.world().resource::<Ids>().machine("one").unwrap();
        let guard = MachineGuard {instance:"n1".into(),attempt:Some(7),pane:Some(pane)};
        assert!(validate_guard(app.world(),entity,"task",&guard).is_ok());
        let mut wrong = guard.clone(); wrong.attempt = Some(8);
        assert!(validate_guard(app.world(),entity,"task",&wrong).is_err());
        wrong = guard.clone(); wrong.pane.as_mut().unwrap().pid = Some(43);
        assert!(validate_guard(app.world(),entity,"task",&wrong).is_err());
        app.world_mut().resource_mut::<Clock>().now_ms = 100 + FRESH_MS + 1;
        assert!(validate_guard(app.world(),entity,"task",&guard).is_err());
        assert_eq!(next_deadline(app.world_mut()),Some(100 + FRESH_MS + 1));
        app.update();
        assert_eq!(next_deadline(app.world_mut()),None);
    }
}
