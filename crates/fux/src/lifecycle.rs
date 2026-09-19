//! Pane and workspace state machine (prompt 3.1, 3.5, 3.6): consumes `Inbound` viewer traffic
//! and signals in the `Requests` phase, turns `Starting` panes into `Effect::SpawnPane` once,
//! lifts `Disabled` when a pane goes `Live`, retires exited panes into `FinalRecord`s, keeps
//! viewers targeting something, and drives the `ShuttingDown` state to `Effect::Exit`.
//!
//! Every mutation goes through `layout::ops`; this module only decides *when*. Timing uses the
//! [`Clock`] the runner writes before each step (wall-clock milliseconds), never an OS handle.

use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_log::{debug, warn};
use bevy_picking::PickingSystems;
use bevy_state::prelude::*;
use bevy_ui::Node;

use crate::events::{PaneExited, PaneSpawned};
use crate::layout::{self, NavDirection, instances, ops};
use crate::model::*;
use crate::scene::templates;
use crate::session::{Historical, RestorePending};
use crate::terminal::Terminal;

/// Requests a viewer may have queued behind a creation barrier; beyond this the newest are
/// dropped with a warning so a stalled spawn cannot grow memory.
pub const MAX_QUEUED_REQUESTS: usize = 1024;
/// After `OnEnter(ShuttingDown)` the runner exits even if panes are still terminating.
pub const SHUTDOWN_DEADLINE_MS: u64 = 5_000;

/// Wall-clock milliseconds, written by the runner before every `update` (tests write it
/// directly). The single clock behind `since_ms`, `exited_ms` and `expires_ms`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    pub now_ms: u64,
}

/// The configured default command (`default-command` in fux.toml, else the login shell); the
/// argv of every pane created without an explicit template.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultCommand(pub Vec<String>);

/// Milliseconds at which `ShuttingDown` was entered.
#[derive(Resource, Clone, Copy, Debug)]
struct ShutdownSince(u64);

/// An exited pane whose record, leaf and viewers were handled; despawned on the next update so
/// the same update's projection still sees it.
#[derive(Component, Debug, Default)]
struct Closing;

/// A `Starting` pane whose close was requested before it went `Live`; terminated on completion.
#[derive(Component, Debug, Default)]
struct CloseRequested;

/// Public ordering handles for other plugins.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LifecycleSystems {
    /// `PostUpdate`: `Starting` panes get their `Terminal` and `Effect::SpawnPane`.
    Materialize,
}

pub struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Clock>()
            .init_resource::<DefaultCommand>()
            .add_systems(Startup, start_serving)
            // Viewer pointer events are picked in the same update.
            .add_systems(
                PreUpdate,
                requests
                    .in_set(Phase::Requests)
                    .before(PickingSystems::ProcessInput),
            )
            .add_systems(PreUpdate, completions.in_set(Phase::Completions))
            .add_systems(
                Update,
                (exits, workspaces, shutdown_progress)
                    .chain()
                    .in_set(Phase::Lifecycle),
            )
            .add_systems(
                PostUpdate,
                materialize
                    .in_set(Phase::Projection)
                    .in_set(LifecycleSystems::Materialize)
                    .after(layout::LayoutSystems::SizeFold)
                    .after(crate::pty::TerminalSystems::Resize)
                    .run_if(not(in_state(ServerMode::ShuttingDown))),
            )
            .add_systems(OnEnter(ServerMode::ShuttingDown), enter_shutdown);
    }
}

// ---------------------------------------------------------------------------------------------
// Public helpers shared with remote/cli
// ---------------------------------------------------------------------------------------------

/// The one clock everyone stamps `since_ms`/`exited_ms` with.
pub fn now_ms(world: &World) -> u64 {
    world.get_resource::<Clock>().map_or(0, |c| c.now_ms)
}

/// The configured default command as a template for a pane created without an explicit one.
pub fn default_template(world: &World) -> PaneTemplate {
    let argv = world
        .get_resource::<DefaultCommand>()
        .filter(|c| !c.0.is_empty())
        .map_or_else(|| vec![default_shell()], |c| c.0.clone());
    PaneTemplate {
        argv,
        cwd: None,
        env: Vec::new(),
        stream: String::new(),
    }
}

/// `$SHELL` when set, else `/bin/sh`.
pub fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Creates workspace `name` with one root `main` holding one pane running `default_argv`
/// (the configured default command when empty): the `default_session` template (prompt 3.7).
pub fn bootstrap(world: &mut World, name: &str, default_argv: &[String]) -> Result<(), BevyError> {
    let template = if default_argv.is_empty() {
        default_template(world)
    } else {
        PaneTemplate {
            argv: default_argv.to_vec(),
            ..default_template(world)
        }
    };
    templates::spawn_workspace(world, templates::default_session(name, template))?;
    Ok(())
}

/// Closes a pane whatever its state: not yet spawned → exited immediately; spawn in flight →
/// terminated as soon as it is live; live → `Terminating` + `Effect::Terminate`; exited → no-op.
pub fn close_pane(world: &mut World, pane: Entity) -> Result<(), BevyError> {
    let process = *world
        .get::<Process>(pane)
        .ok_or_else(|| BevyError::from(format!("{pane} is not a pane")))?;
    match process {
        Process::Starting if world.get::<Terminal>(pane).is_none() => {
            world.entity_mut(pane).insert(Process::Exited { code: 0 });
        }
        Process::Starting => {
            world.entity_mut(pane).insert(CloseRequested);
        }
        Process::Live { .. } | Process::Eof { .. } => {
            let now = now_ms(world);
            terminate(world, pane, now);
        }
        Process::Terminating { .. } | Process::Exited { .. } => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

fn start_serving(mut next: ResMut<NextState<ServerMode>>) {
    next.set(ServerMode::Serving);
}

/// `PreUpdate`/`Requests`: viewer requests are queued per viewer and drained in order while
/// the viewer has no creation barrier; `ViewerGone` detaches; signals begin shutdown.
fn requests(
    world: &mut World,
    queued: &mut QueryState<(Entity, &RequestQueue), With<Viewer>>,
    barriers: &mut BarrierQuery,
) {
    let mut incoming: Vec<(Entity, ViewerRequest)> = Vec::new();
    let mut gone: Vec<Entity> = Vec::new();
    let mut signal = false;
    for message in world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
    {
        match message {
            Inbound::ViewerRequest { viewer, request } => incoming.push((*viewer, request.clone())),
            Inbound::ViewerGone { viewer } => gone.push(*viewer),
            Inbound::Signal(_) => signal = true,
            Inbound::PaneOutput { .. }
            | Inbound::PaneEof { .. }
            | Inbound::PaneSpawned { .. }
            | Inbound::PaneSpawnFailed { .. }
            | Inbound::PaneExited { .. }
            | Inbound::ViewerAttached { .. }
            | Inbound::Wake => {}
        }
    }
    if signal {
        world
            .resource_mut::<NextState<ServerMode>>()
            .set(ServerMode::ShuttingDown);
    }
    for viewer in gone {
        if world.get::<Viewer>(viewer).is_none() {
            continue;
        }
        if let Err(error) = ops::detach_viewer(world, viewer) {
            warn!("detach {viewer}: {error}");
        }
    }
    for (viewer, request) in incoming {
        let Some(mut queue) = world.get_mut::<RequestQueue>(viewer) else {
            debug!("request for unknown viewer {viewer} dropped");
            continue;
        };
        if queue.0.len() >= MAX_QUEUED_REQUESTS {
            let (kind, bytes) = request_shape(&request);
            warn!("viewer {viewer} request queue full; dropping {kind} ({bytes} bytes)");
            continue;
        }
        queue.0.push_back(request);
    }
    release_barriers(world, barriers);
    let viewers: Vec<Entity> = queued
        .iter(world)
        .filter(|(_, q)| !q.0.is_empty())
        .map(|(e, _)| e)
        .collect();
    for viewer in viewers {
        drain_queue(world, viewer);
    }
}

fn drain_queue(world: &mut World, viewer: Entity) {
    loop {
        if world.get::<Viewer>(viewer).is_none() {
            return;
        }
        if world
            .get::<CreationBarrier>(viewer)
            .is_some_and(|b| b.0.is_some())
        {
            return;
        }
        let Some(request) = world
            .get_mut::<RequestQueue>(viewer)
            .and_then(|mut q| q.0.pop_front())
        else {
            return;
        };
        if world.get::<Detaching>(viewer).is_some() {
            continue;
        }
        if let Err(error) = handle(world, viewer, request) {
            warn!("viewer {viewer}: {error}");
        }
    }
}

fn handle(world: &mut World, viewer: Entity, request: ViewerRequest) -> Result<(), BevyError> {
    match request {
        ViewerRequest::Input(bytes) => {
            if let Some(pane) = target_of(world, viewer)
                && world
                    .get::<Process>(pane)
                    .is_some_and(|p| !matches!(p, Process::Exited { .. }))
            {
                effect(world, Effect::WritePty { pane, bytes });
            }
        }
        ViewerRequest::Target(id) => {
            let pane = world
                .resource::<Ids>()
                .pane(id)
                .ok_or_else(|| BevyError::from(format!("no pane {id}")))?;
            ops::target(world, viewer, pane)?;
        }
        ViewerRequest::Show(id) => {
            let root = node_entity(world, id)?;
            ops::show_root(world, viewer, root)?;
        }
        ViewerRequest::Resize { rows, cols } => {
            ops::resize_viewer(world, viewer, Viewport { rows, cols })?;
        }
        ViewerRequest::Pointer(event) => crate::pointer::forward(world, viewer, event),
        ViewerRequest::Scroll { node, rows } => {
            let node = node_entity(world, node)?;
            ops::scroll(world, viewer, node, rows)?;
        }
        ViewerRequest::Zoom(id) => {
            let node = node_entity(world, id)?;
            ops::zoom(world, viewer, node)?;
        }
        ViewerRequest::Unzoom => ops::unzoom(world, viewer)?,
        ViewerRequest::Split {
            direction,
            template,
        } => {
            let pane = target_of(world, viewer)
                .ok_or_else(|| BevyError::from("split: viewer targets nothing"))?;
            let template = template.unwrap_or_else(|| default_template(world));
            let (_leaf, new_pane) = ops::split(world, pane, direction, template)?;
            begin_creation(world, viewer, new_pane);
        }
        ViewerRequest::ClosePane => {
            let pane = target_of(world, viewer)
                .ok_or_else(|| BevyError::from("close: viewer targets nothing"))?;
            close_pane(world, pane)?;
        }
        ViewerRequest::Swap { direction } => {
            ops::swap(world, viewer, direction)?;
        }
        ViewerRequest::SurfaceKey {
            revision,
            node,
            bytes,
        } => {
            if !crate::attach::projection::admits_input(world, viewer, revision) {
                return Ok(());
            }
            crate::surface::key_input(world, viewer, node, &bytes)?;
        }
        ViewerRequest::NewRoot { template } => {
            let ws = world
                .get::<Viewing>(viewer)
                .map(|v| v.0)
                .ok_or_else(|| BevyError::from("new root: viewer views no workspace"))?;
            let name = world
                .get::<RootOrder>(ws)
                .map_or(1, |o| o.0.len() + 1)
                .to_string();
            let template = template.unwrap_or_else(|| default_template(world));
            let root = ops::new_root(world, ws, &name)?;
            let node = ops::spawn_node(world, root, None, leaf_node(), Some(template))?;
            ops::show_root(world, viewer, root)?;
            if let Some(pane) = placed_pane(world, node) {
                begin_creation(world, viewer, pane);
            }
        }
        ViewerRequest::Detach => {
            world.entity_mut(viewer).insert(Detaching);
        }
    }
    Ok(())
}

/// Marks `pane` as being created for `viewer`, targets it and raises the viewer's barrier so
/// the input that followed the split in the same read reaches the new pane once it is live.
fn begin_creation(world: &mut World, viewer: Entity, pane: Entity) {
    if world.get::<Creation>(pane).is_none() {
        world.entity_mut(pane).insert(Creation {
            requesters: vec![Requester::Viewer(viewer)],
            kind: CreationKind::Spawn,
        });
    }
    if world.get::<ExactTarget>(viewer).is_none()
        && let Err(error) = ops::target(world, viewer, pane)
    {
        warn!("target new pane {pane} for {viewer}: {error}");
    }
    world.entity_mut(viewer).insert(CreationBarrier(Some(pane)));
}

// ---------------------------------------------------------------------------------------------
// Completions and materialisation
// ---------------------------------------------------------------------------------------------

/// `PreUpdate`/`Completions`: panes that went `Live` in this update's ingest leave `Disabled`
/// and `Creation`, announce `PaneSpawned`; barriers on them release; a close requested while
/// starting is honoured.
fn completions(
    world: &mut World,
    live: &mut QueryState<(Entity, &Process, Has<CloseRequested>), (With<Pane>, With<Disabled>)>,
    barriers: &mut BarrierQuery,
) {
    let now = now_ms(world);
    let done: Vec<(Entity, u32, bool)> = live
        .iter(world)
        .filter_map(|(e, p, close)| match *p {
            Process::Live { pid } | Process::Eof { pid } => Some((e, pid, close)),
            _ => None,
        })
        .collect();
    for (pane, pid, close) in done {
        world
            .entity_mut(pane)
            .remove::<(Disabled, Creation, CloseRequested)>();
        if let (Some(id), Some(ws)) = (world.get::<PaneId>(pane), world.get::<PaneIn>(pane)) {
            world.trigger(PaneSpawned {
                entity: pane,
                scope: ws.0,
                pane: *id,
                pid,
            });
        }
        if close {
            terminate(world, pane, now);
        }
    }
    release_barriers(world, barriers);
}

/// What a log line may say about a request: the variant and, for input, its size — never the
/// bytes themselves (prompt 3.3).
fn request_shape(request: &ViewerRequest) -> (&'static str, usize) {
    match request {
        ViewerRequest::Input(bytes) => ("Input", bytes.len()),
        ViewerRequest::Target(_) => ("Target", 0),
        ViewerRequest::Show(_) => ("Show", 0),
        ViewerRequest::Resize { .. } => ("Resize", 0),
        ViewerRequest::Pointer(_) => ("Pointer", 0),
        ViewerRequest::Scroll { .. } => ("Scroll", 0),
        ViewerRequest::Zoom(_) => ("Zoom", 0),
        ViewerRequest::Unzoom => ("Unzoom", 0),
        ViewerRequest::Split { .. } => ("Split", 0),
        ViewerRequest::ClosePane => ("ClosePane", 0),
        ViewerRequest::Swap { .. } => ("Swap", 0),
        ViewerRequest::SurfaceKey { bytes, .. } => ("SurfaceKey", bytes.len()),
        ViewerRequest::NewRoot { .. } => ("NewRoot", 0),
        ViewerRequest::Detach => ("Detach", 0),
    }
}

type BarrierQuery = QueryState<(Entity, &'static CreationBarrier), With<Viewer>>;

/// A barrier whose pane has no `Creation` any more (live, exited or gone) releases.
fn release_barriers(world: &mut World, barriers: &mut BarrierQuery) {
    let stale: Vec<Entity> = barriers
        .iter(world)
        .filter_map(|(viewer, barrier)| {
            let pane = barrier.0?;
            let creating = world.get::<Creation>(pane).is_some()
                && world
                    .get::<Process>(pane)
                    .is_some_and(|p| matches!(p, Process::Starting));
            (!creating).then_some(viewer)
        })
        .collect();
    for viewer in stale {
        world.entity_mut(viewer).insert(CreationBarrier(None));
    }
}

/// `PostUpdate` after the size fold: every `Starting` pane without a `Terminal` gets one at its
/// folded size and exactly one `Effect::SpawnPane`; `Terminal` presence is the "emitted" mark.
/// A restored pane awaiting a `fux/session.{restore,skip}` decision waits; a restored pane's
/// `Historical` screen is fed into the new terminal first (prompt 3.8).
fn materialize(
    world: &mut World,
    starting: &mut QueryState<
        (Entity, &Process, &PaneTemplate, &PaneSize, Has<Creation>),
        (
            With<Pane>,
            Without<Terminal>,
            Without<RestorePending>,
            Allow<Disabled>,
        ),
    >,
) {
    let scrollback = world.resource::<Limits>().scrollback_lines;
    let todo: Vec<(Entity, PaneTemplate, PaneSize, bool)> = starting
        .iter(world)
        .filter(|(_, p, ..)| matches!(p, Process::Starting))
        .map(|(e, _, t, s, c)| (e, t.clone(), *s, c))
        .collect();
    for (pane, template, size, has_creation) in todo {
        let (rows, cols) = clamp_dims(size.rows, size.cols);
        let mut entity = world.entity_mut(pane);
        let mut terminal = Terminal::new(rows, cols, scrollback);
        if let Some(history) = entity.take::<Historical>() {
            crate::session::replay(&mut terminal, &history, cols);
        }
        entity.insert(terminal);
        if !has_creation {
            entity.insert(Creation {
                requesters: vec![Requester::Server],
                kind: CreationKind::Spawn,
            });
        }
        effect(
            world,
            Effect::SpawnPane {
                pane,
                argv: template.argv,
                cwd: template.cwd,
                env: template.env,
                rows,
                cols,
            },
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Exits, workspaces, records
// ---------------------------------------------------------------------------------------------

/// `Update`/`Lifecycle`: panes closed last update are despawned; newly exited panes leave a
/// `FinalRecord`, lose their leaf, hand their viewers a new target and release their PTY.
fn exits(
    world: &mut World,
    closing: &mut QueryState<Entity, (With<Closing>, Allow<Disabled>)>,
    exited: &mut QueryState<(Entity, &Process), (With<Pane>, Without<Closing>, Allow<Disabled>)>,
    barriers: &mut BarrierQuery,
) {
    let done: Vec<Entity> = closing.iter(world).collect();
    for pane in done {
        world.despawn(pane);
    }
    let now = now_ms(world);
    let fresh: Vec<(Entity, i32)> = exited
        .iter(world)
        .filter_map(|(e, p)| match p {
            Process::Exited { code } => Some((e, *code)),
            _ => None,
        })
        .collect();
    let any = !fresh.is_empty();
    for (pane, code) in fresh {
        close_exited(world, pane, code, now);
    }
    if any {
        release_barriers(world, barriers);
    }
}

fn close_exited(world: &mut World, pane: Entity, code: i32, now: u64) {
    if let (Some(id), Some(ws)) = (world.get::<PaneId>(pane), world.get::<PaneIn>(pane)) {
        world.trigger(PaneExited {
            entity: pane,
            scope: ws.0,
            pane: *id,
            code,
        });
    }
    let record = final_record(world, pane, code, now);
    let viewers: Vec<(Entity, bool, Option<Entity>)> = world
        .get::<TargetedBy>(pane)
        .map(|t| t.iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|viewer| {
            let exact = world.get::<ExactTarget>(viewer).is_some();
            let next = (!exact)
                .then(|| ops::navigate(world, viewer, NavDirection::Next))
                .flatten()
                .filter(|&p| p != pane);
            (viewer, exact, next)
        })
        .collect();
    let workspace = world.get::<PaneIn>(pane).map(|p| p.0);
    if world.get::<PlacedIn>(pane).is_some()
        && let Err(error) = ops::remove_leaf(world, pane)
    {
        warn!("remove leaf of exited pane {pane}: {error}");
    }
    if let Some(ws) = workspace
        && world.get::<Retiring>(ws).is_none()
        && world.get::<RootOrder>(ws).is_some_and(|o| o.0.is_empty())
        && let Err(error) = ops::retire_workspace(world, ws, now)
    {
        warn!("retire emptied workspace {ws}: {error}");
    }
    for (viewer, exact, next) in viewers {
        if exact {
            world.entity_mut(viewer).insert(Detaching);
        } else {
            retarget(world, viewer, next);
        }
    }
    if world.get::<Terminal>(pane).is_some() {
        effect(world, Effect::ReleasePty { pane });
    }
    if let Some(record) = record {
        world.spawn(record);
    }
    world
        .entity_mut(pane)
        .remove::<Creation>()
        .insert((Disabled, Closing));
}

fn final_record(world: &World, pane: Entity, code: i32, now: u64) -> Option<FinalRecord> {
    let id = *world.get::<PaneId>(pane)?;
    let attribution = world.get::<LaunchAttribution>(pane);
    let terminal = world.get::<Terminal>(pane);
    let retain = world.get::<FinalRetention>(pane).map_or(0, |r| r.0);
    Some(FinalRecord {
        pane: id,
        workspace: attribution
            .map(|a| a.workspace_name.clone())
            .unwrap_or_default(),
        stream: attribution.map(|a| a.stream.clone()).unwrap_or_default(),
        exit_code: code,
        title: world
            .get::<Title>(pane)
            .map(|t| t.0.clone())
            .unwrap_or_default(),
        last_seq: terminal.map_or(0, Terminal::seq),
        exited_ms: now,
        expires_ms: now.saturating_add(retain),
        screen: terminal.map(Terminal::screen_lines).unwrap_or_default(),
    })
}

/// Gives `viewer` a target after the one it had went away: the geometric neighbour computed
/// before the leaf vanished, else the first pane of the shown root, else the first root.
fn retarget(world: &mut World, viewer: Entity, next: Option<Entity>) {
    if world.get::<Viewer>(viewer).is_none() || world.get::<Detaching>(viewer).is_some() {
        return;
    }
    if let Some(pane) = next
        && world.get::<Pane>(pane).is_some()
        && ops::target(world, viewer, pane).is_ok()
    {
        return;
    }
    let showing = world.get::<Showing>(viewer).map(|s| s.0);
    if let Some(root) = showing
        && world.get::<TemplateRoot>(root).is_some()
        && let Some(pane) = first_pane_in_root(world, root)
    {
        if let Err(error) = ops::target(world, viewer, pane) {
            warn!("retarget {viewer} to {pane}: {error}");
        }
        return;
    }
    let first_root = world
        .get::<Viewing>(viewer)
        .and_then(|v| world.get::<RootOrder>(v.0))
        .and_then(|o| o.0.first().copied());
    match first_root {
        Some(root) => {
            if let Err(error) = ops::show_root(world, viewer, root) {
                warn!("show first root for {viewer}: {error}");
            }
        }
        None => {
            world.entity_mut(viewer).remove::<Showing>();
        }
    }
}

/// First placing leaf in document order under a template root, if any.
fn first_pane_in_root(world: &World, root: Entity) -> Option<Entity> {
    let mut first = None;
    instances::walk(world, root, &mut |node, _| {
        if first.is_none()
            && let Some(places) = world.get::<Places>(node)
        {
            first = Some(places.0);
        }
    });
    first
}

/// Retiring workspaces terminate their panes, detach their viewers, close their roots and are
/// despawned once nothing refers to them.
fn workspaces(
    world: &mut World,
    retiring: &mut QueryState<Entity, (With<Workspace>, With<Retiring>, Allow<Disabled>)>,
) {
    let now = now_ms(world);
    let list: Vec<Entity> = retiring.iter(world).collect();
    for ws in list {
        let panes: Vec<Entity> = world
            .get::<WorkspacePanes>(ws)
            .map(|p| p.iter().collect())
            .unwrap_or_default();
        for &pane in &panes {
            match world.get::<Process>(pane).copied() {
                Some(Process::Live { .. } | Process::Eof { .. }) => terminate(world, pane, now),
                Some(Process::Starting) => {
                    if let Err(error) = close_pane(world, pane) {
                        warn!("close starting pane {pane}: {error}");
                    }
                }
                _ => {}
            }
        }
        let viewers: Vec<Entity> = world
            .get::<ViewedBy>(ws)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        for viewer in &viewers {
            if world.get::<Detaching>(*viewer).is_none() {
                world.entity_mut(*viewer).insert(Detaching);
            }
        }
        if !panes.is_empty() {
            continue;
        }
        let roots: Vec<Entity> = world
            .get::<RootOrder>(ws)
            .map(|o| o.0.clone())
            .unwrap_or_default();
        for root in roots {
            if let Err(error) = ops::close_root(world, root) {
                warn!("close root {root} of retiring workspace: {error}");
            }
        }
        let empty = world.get::<RootOrder>(ws).is_none_or(|o| o.0.is_empty());
        if empty && viewers.is_empty() {
            world.despawn(ws);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------------------------

fn enter_shutdown(
    world: &mut World,
    panes: &mut QueryState<(Entity, &Process), (With<Pane>, Allow<Disabled>)>,
    viewers: &mut QueryState<Entity, With<Viewer>>,
) {
    let now = now_ms(world);
    world.insert_resource(ShutdownSince(now));
    let live: Vec<Entity> = panes
        .iter(world)
        .filter(|(_, p)| matches!(p, Process::Live { .. } | Process::Eof { .. }))
        .map(|(e, _)| e)
        .collect();
    for pane in live {
        terminate(world, pane, now);
    }
    let all: Vec<Entity> = viewers.iter(world).collect();
    for viewer in all {
        world.entity_mut(viewer).insert(Detaching);
    }
}

/// Each update while shutting down: panes that went live meanwhile are terminated; once no pane
/// is live, or the deadline passed, the runner is told to exit.
fn shutdown_progress(
    world: &mut World,
    panes: &mut QueryState<(Entity, &Process), (With<Pane>, Allow<Disabled>)>,
) {
    if !world
        .get_resource::<State<ServerMode>>()
        .is_some_and(|s| *s.get() == ServerMode::ShuttingDown)
    {
        return;
    }
    let now = now_ms(world);
    let since = world.get_resource::<ShutdownSince>().map_or(now, |s| s.0);
    let mut live = false;
    let mut fresh: Vec<Entity> = Vec::new();
    for (pane, process) in panes.iter(world) {
        match process {
            Process::Live { .. } | Process::Eof { .. } => fresh.push(pane),
            Process::Terminating { .. } => live = true,
            Process::Starting | Process::Exited { .. } => {}
        }
    }
    for pane in fresh {
        terminate(world, pane, now);
        live = true;
    }
    if !live || now.saturating_sub(since) >= SHUTDOWN_DEADLINE_MS {
        effect(world, Effect::Exit { code: 0 });
    }
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

fn effect(world: &mut World, effect: Effect) {
    world.resource_mut::<Messages<Effect>>().write(effect);
}

/// `Terminating` + `Effect::Terminate`, once: a pane already terminating is left alone.
fn terminate(world: &mut World, pane: Entity, now: u64) {
    let Some(pid) = world.get::<Process>(pane).and_then(|p| match p {
        Process::Live { pid } | Process::Eof { pid } => Some(*pid),
        _ => None,
    }) else {
        return;
    };
    world
        .entity_mut(pane)
        .insert(Process::Terminating { pid, since_ms: now });
    effect(world, Effect::Terminate { pane });
}

fn target_of(world: &World, viewer: Entity) -> Option<Entity> {
    world.get::<Targets>(viewer).map(|t| t.0)
}

fn node_entity(world: &World, id: NodeId) -> Result<Entity, BevyError> {
    world
        .resource::<Ids>()
        .node(id)
        .ok_or_else(|| BevyError::from(format!("no node {id}")))
}

fn placed_pane(world: &World, node: Entity) -> Option<Entity> {
    world.get::<Places>(node).map(|p| p.0)
}

fn leaf_node() -> Node {
    Node {
        flex_grow: 1.0,
        ..Default::default()
    }
}
