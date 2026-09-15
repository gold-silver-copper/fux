//! Read-only projections of the model for BRP (`world.query`, `world.get_components`): one
//! reflected view component per public entity kind, on projection entities of their own so
//! `Disabled` model entities (starting panes, retiring workspaces) stay visible with their state
//! and no authoritative component is ever reachable through the reflected read path.
//!
//! Each projection entity carries `Mirrors(model)`; the `Projections` target on the model is
//! `linked_spawn`, so a projection dies with its model and no sweep is needed. Maintained every
//! update in `PostUpdate`/`Phase::Projection` with in-place writes: steady state allocates
//! nothing (scratch rows are reused; `String`/`Vec` fields are `clone_from`ed into existing
//! capacity) and change ticks move only when a value changed.

use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::{Allow, Has};
use bevy_ecs::relationship::RelationshipTarget;
use bevy_reflect::prelude::*;

use crate::layout::instances;
use crate::model::{
    ExactTarget, LaunchAttribution, LayoutGeneration, Mirrors, NodeId, Open, Pane, PaneId, PaneIn,
    PaneSize, PlacedIn, Places, Process, Projections, RootOf, RootOrder, Roots, ServerInstance,
    Showing, Targets, TemplateNode, TemplateRoot, Title, ViewedBy, Viewer, ViewerId, Viewing,
    Viewport, Workspace, WorkspaceName, WorkspacePanes,
};
use crate::terminal::Terminal;

/// Marker on every projection entity.
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct ProjectionEntity;

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct PaneView {
    pub id: u64,
    pub workspace: String,
    /// The template leaf placing this pane.
    pub node: Option<u64>,
    /// `starting`, `live`, `eof`, `terminating`, `exited`.
    pub state: String,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub title: String,
    pub rows: u16,
    pub cols: u16,
    /// Emulator sequence: advances once per observable change.
    pub seq: u64,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct NodeView {
    pub id: u64,
    pub root: u64,
    pub parent: Option<u64>,
    /// Position among the parent's children.
    pub index: u32,
    pub children: u32,
    pub pane: Option<u64>,
    pub name: String,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct RootView {
    pub id: u64,
    pub workspace: String,
    pub name: String,
    pub generation: u64,
    /// Position in the workspace's `RootOrder`.
    pub index: u32,
    pub panes: Vec<u64>,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct WorkspaceView {
    pub name: String,
    pub roots: Vec<u64>,
    pub panes: Vec<u64>,
    pub viewers: u32,
    pub open: bool,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ViewerView {
    pub id: u64,
    pub workspace: String,
    pub showing: Option<u64>,
    pub target: Option<u64>,
    pub rows: u16,
    pub cols: u16,
    pub exact: bool,
}

#[derive(Component, Reflect, Clone, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct ServerView {
    pub name: String,
    pub instance: String,
    pub pid: u32,
    pub started_ms: u64,
    pub workspaces: u32,
    pub panes: u32,
    pub viewers: u32,
}

/// Fully qualified type paths of everything the wrapped `world.*` reads may name.
pub const ALLOWED_TYPE_PATHS: [&str; 7] = [
    "fux::remote::projection::ProjectionEntity",
    "fux::remote::projection::PaneView",
    "fux::remote::projection::NodeView",
    "fux::remote::projection::RootView",
    "fux::remote::projection::WorkspaceView",
    "fux::remote::projection::ViewerView",
    "fux::remote::projection::ServerView",
];

pub fn is_allowed_type_path(path: &str) -> bool {
    ALLOWED_TYPE_PATHS.contains(&path)
}

pub fn register_types(app: &mut App) {
    app.register_type::<ProjectionEntity>()
        .register_type::<PaneView>()
        .register_type::<NodeView>()
        .register_type::<RootView>()
        .register_type::<WorkspaceView>()
        .register_type::<ViewerView>()
        .register_type::<ServerView>();
}

/// Reused rows: phase one fills them from the model without touching projection entities,
/// phase two writes them through change detection.
#[derive(Default)]
pub struct Scratch {
    panes: Vec<(Entity, PaneView)>,
    nodes: Vec<(Entity, NodeView)>,
    roots: Vec<(Entity, RootView)>,
    workspaces: Vec<(Entity, WorkspaceView)>,
    viewers: Vec<(Entity, ViewerView)>,
    server: ServerView,
}

/// The system's retained state: the server projection entity (the one projection without a
/// model entity to mirror) plus scratch rows.
#[derive(Default)]
pub struct SyncState {
    server: Option<Entity>,
    scratch: Scratch,
}

type PaneRow<'w> = (
    Entity,
    &'w PaneId,
    &'w PaneIn,
    Option<&'w PlacedIn>,
    &'w Process,
    Option<&'w Title>,
    Option<&'w PaneSize>,
    Option<&'w LaunchAttribution>,
    Option<&'w Terminal>,
);
type PaneFilter = (With<Pane>, Allow<Disabled>);
type NodeRow<'w> = (
    Entity,
    &'w NodeId,
    Option<&'w ChildOf>,
    Option<&'w Children>,
    Option<&'w Places>,
    Option<&'w Name>,
);
type RootRow<'w> = (
    Entity,
    &'w NodeId,
    &'w RootOf,
    Option<&'w Name>,
    Option<&'w LayoutGeneration>,
);
type WorkspaceRow<'w> = (
    Entity,
    &'w WorkspaceName,
    Option<&'w RootOrder>,
    Option<&'w Roots>,
    Option<&'w WorkspacePanes>,
    Option<&'w ViewedBy>,
    Has<Open>,
);
type WorkspaceFilter = (With<Workspace>, Allow<Disabled>);
type ViewerRow<'w> = (
    Entity,
    &'w ViewerId,
    &'w Viewing,
    Option<&'w Showing>,
    Option<&'w Targets>,
    Option<&'w Viewport>,
    Has<ExactTarget>,
);

fn slot<T: Default>(rows: &mut Vec<(Entity, T)>, i: usize, entity: Entity) -> Option<&mut T> {
    if rows.len() <= i {
        rows.push((entity, T::default()));
    }
    let row = rows.get_mut(i)?;
    row.0 = entity;
    Some(&mut row.1)
}

fn set_str(dst: &mut String, src: &str) {
    if dst != src {
        dst.clear();
        dst.push_str(src);
    }
}

fn set_opt_str(dst: &mut Option<String>, src: Option<&str>) {
    match (dst.as_mut(), src) {
        (Some(d), Some(s)) => set_str(d, s),
        (None, Some(s)) => *dst = Some(s.to_owned()),
        (Some(_), None) => *dst = None,
        (None, None) => {}
    }
}

fn workspace_name(world: &World, ws: Entity) -> &str {
    world.get::<WorkspaceName>(ws).map_or("", |n| n.0.as_str())
}

fn node_id(world: &World, node: Entity) -> Option<u64> {
    world.get::<NodeId>(node).map(|id| id.0)
}

fn pane_id(world: &World, pane: Entity) -> Option<u64> {
    world.get::<PaneId>(pane).map(|id| id.0)
}

/// Walks `ChildOf` up to the template root.
fn root_of(world: &World, mut node: Entity) -> Option<Entity> {
    for _ in 0..=crate::model::MAX_DEPTH {
        if world.get::<TemplateRoot>(node).is_some() {
            return Some(node);
        }
        node = world.get::<ChildOf>(node)?.parent();
    }
    None
}

pub(super) fn sync(
    world: &mut World,
    panes: &mut QueryState<PaneRow, PaneFilter>,
    nodes: &mut QueryState<NodeRow, With<TemplateNode>>,
    roots: &mut QueryState<RootRow, With<TemplateRoot>>,
    workspaces: &mut QueryState<WorkspaceRow, WorkspaceFilter>,
    viewers: &mut QueryState<ViewerRow, With<Viewer>>,
    mut state: Local<SyncState>,
) {
    let SyncState { server, scratch } = &mut *state;
    // Phase one: read the model into reused rows.
    let mut n = 0;
    for (entity, id, pane_in, placed_in, process, title, size, launch, terminal) in
        panes.iter(world)
    {
        let Some(view) = slot(&mut scratch.panes, n, entity) else {
            continue;
        };
        n += 1;
        view.id = id.0;
        set_str(&mut view.workspace, workspace_name(world, pane_in.0));
        view.node = placed_in
            .and_then(|p| p.iter().next())
            .and_then(|leaf| node_id(world, leaf));
        set_str(
            &mut view.state,
            match process {
                Process::Starting => "starting",
                Process::Live { .. } => "live",
                Process::Eof { .. } => "eof",
                Process::Terminating { .. } => "terminating",
                Process::Exited { .. } => "exited",
            },
        );
        view.pid = process.pid();
        view.exit_code = match *process {
            Process::Exited { code } => Some(code),
            _ => None,
        };
        set_str(&mut view.title, title.map_or("", |t| t.0.as_str()));
        let size = size.copied().unwrap_or_default();
        view.rows = size.rows;
        view.cols = size.cols;
        view.seq = terminal.map_or(0, Terminal::seq);
        match launch {
            Some(launch) => {
                if view.argv != launch.argv {
                    view.argv.clone_from(&launch.argv);
                }
                set_opt_str(&mut view.cwd, launch.cwd.as_deref());
            }
            None => {
                view.argv.clear();
                view.cwd = None;
            }
        }
    }
    scratch.panes.truncate(n);

    n = 0;
    for (entity, id, child_of, children, places, name) in nodes.iter(world) {
        let Some(view) = slot(&mut scratch.nodes, n, entity) else {
            continue;
        };
        n += 1;
        view.id = id.0;
        view.root = root_of(world, entity)
            .and_then(|r| node_id(world, r))
            .unwrap_or(id.0);
        // A template root's `ChildOf` is its workspace, not a node: it has no parent here.
        let child_of = child_of.filter(|_| world.get::<TemplateRoot>(entity).is_none());
        view.parent = child_of.and_then(|c| node_id(world, c.parent()));
        view.index = child_of
            .and_then(|c| world.get::<Children>(c.parent()))
            .and_then(|siblings| siblings.iter().position(|s| s == entity))
            .map_or(0, |i| i as u32);
        view.children = children.map_or(0, |c| c.len() as u32);
        view.pane = places.and_then(|p| pane_id(world, p.0));
        set_str(&mut view.name, name.map_or("", |n| n.as_str()));
    }
    scratch.nodes.truncate(n);

    n = 0;
    for (entity, id, root_of, name, generation) in roots.iter(world) {
        let Some(view) = slot(&mut scratch.roots, n, entity) else {
            continue;
        };
        n += 1;
        view.id = id.0;
        set_str(&mut view.workspace, workspace_name(world, root_of.0));
        set_str(&mut view.name, name.map_or("", |n| n.as_str()));
        view.generation = generation.map_or(0, |g| g.0);
        view.index = world
            .get::<RootOrder>(root_of.0)
            .and_then(|order| order.0.iter().position(|r| *r == entity))
            .map_or(0, |i| i as u32);
        // Panes placed anywhere in the subtree, in document order.
        view.panes.clear();
        instances::walk(world, entity, &mut |node, _| {
            if let Some(pane) = world.get::<Places>(node).and_then(|p| pane_id(world, p.0)) {
                view.panes.push(pane);
            }
        });
    }
    scratch.roots.truncate(n);

    n = 0;
    for (entity, name, order, roots, panes, viewed_by, open) in workspaces.iter(world) {
        let Some(view) = slot(&mut scratch.workspaces, n, entity) else {
            continue;
        };
        n += 1;
        set_str(&mut view.name, &name.0);
        view.roots.clear();
        match order {
            Some(order) => view
                .roots
                .extend(order.0.iter().filter_map(|r| node_id(world, *r))),
            None => view.roots.extend(
                roots
                    .into_iter()
                    .flat_map(|r| r.iter())
                    .filter_map(|r| node_id(world, r)),
            ),
        }
        view.panes.clear();
        view.panes.extend(
            panes
                .into_iter()
                .flat_map(|p| p.iter())
                .filter_map(|p| pane_id(world, p)),
        );
        view.viewers = viewed_by.map_or(0, |v| v.len() as u32);
        view.open = open;
    }
    scratch.workspaces.truncate(n);

    n = 0;
    for (entity, id, viewing, showing, targets, viewport, exact) in viewers.iter(world) {
        let Some(view) = slot(&mut scratch.viewers, n, entity) else {
            continue;
        };
        n += 1;
        view.id = id.0;
        set_str(&mut view.workspace, workspace_name(world, viewing.0));
        view.showing = showing.and_then(|s| node_id(world, s.0));
        view.target = targets.and_then(|t| pane_id(world, t.0));
        let viewport = viewport.copied().unwrap_or(Viewport { rows: 0, cols: 0 });
        view.rows = viewport.rows;
        view.cols = viewport.cols;
        view.exact = exact;
    }
    scratch.viewers.truncate(n);

    // Phase two: write through change detection. Projections of vanished models died with them.
    upsert(world, &scratch.panes);
    upsert(world, &scratch.nodes);
    upsert(world, &scratch.roots);
    upsert(world, &scratch.workspaces);
    upsert(world, &scratch.viewers);

    let instance = world.resource::<ServerInstance>();
    let view = &mut scratch.server;
    set_str(&mut view.name, &instance.name);
    set_str(&mut view.instance, &instance.nonce);
    view.pid = instance.pid;
    view.started_ms = instance.started_ms;
    view.workspaces = scratch.workspaces.len() as u32;
    view.panes = scratch.panes.len() as u32;
    view.viewers = scratch.viewers.len() as u32;
    match *server {
        Some(entity) => write_view(world, entity, &scratch.server),
        None => {
            *server = Some(world.spawn((ProjectionEntity, scratch.server.clone())).id());
        }
    }
}

/// Writes each row into the model's projection carrying `T` (a template root has two: node and
/// root views), spawning one linked to the model when there is none yet.
fn upsert<T: Component<Mutability = bevy_ecs::component::Mutable> + Clone + PartialEq + Default>(
    world: &mut World,
    rows: &[(Entity, T)],
) {
    for (model, view) in rows {
        let projection = world
            .get::<Projections>(*model)
            .and_then(|p| p.iter().find(|&p| world.get::<T>(p).is_some()));
        match projection {
            Some(projection) => write_view(world, projection, view),
            None => {
                world.spawn((ProjectionEntity, view.clone(), Mirrors(*model)));
            }
        }
    }
}

/// `set_if_neq` without a by-value argument: compares, then clones into existing capacity.
fn write_view<T: Component<Mutability = bevy_ecs::component::Mutable> + Clone + PartialEq>(
    world: &mut World,
    entity: Entity,
    view: &T,
) {
    if let Some(mut current) = world.get_mut::<T>(entity) {
        let inner = current.bypass_change_detection();
        if inner != view {
            inner.clone_from(view);
            current.set_changed();
        }
    }
}
