use crate::{
    actions::Action,
    assets::{self, Settings},
    control::{Axis, Chooser, Command, Control, Order, Shutdown, Subject, UserInput},
    frame::{self, sync_view},
    interaction::Prefix,
    model::*,
    presentation::{self, Presentation},
    protocol::{Input, MouseAction, MouseButton, Token},
    terminal::{Terminal, TerminalPlugin},
};
use bevy_app::{App, AppExit, Plugin, Startup, Update};
use bevy_ecs::{prelude::*, system::SystemChangeTick, world::EntityRefExcept};
use bevy_remote::RemotePlugin;
use bevy_ui::{FlexDirection, Node};
use bevy_world_serialization::DynamicWorld;
use std::sync::Arc;

fn route_control(event: On<Control>, mut commands: Commands) {
    let (viewer, command) = (event.event_target(), event.command.clone());
    commands.queue(move |world: &mut World| {
        if sync_view(world, viewer).is_err() {
            return;
        }
        if let Err(error) = execute(world, viewer, command) {
            notify(world, viewer, Notice::error(error));
        }
        world.resource::<Wake>().notify();
    });
}

fn route_input(event: On<UserInput>, mut commands: Commands) {
    let event = event.event().clone();
    commands.queue(move |world: &mut World| {
        if sync_view(world, event.viewer).is_err() {
            return;
        }
        if crate::paste::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::interaction::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::selection::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::interaction::command_input(world, event.viewer, &event.input) {
            return;
        }
        if let Input::Mouse { action, x, y, .. } = &event.input
            && matches!(action, MouseAction::Move | MouseAction::Release)
            && let Some(selection) = world.get::<crate::selection::Selection>(event.viewer)
            && selection.dragging
        {
            let leaf = selection.leaf;
            let rect = frame::rect(world, event.viewer, leaf);
            if let Some(rect) = rect {
                let point = rect.local(*x, *y);
                let _ = crate::selection::mouse(world, event.viewer, leaf, *action, point);
            }
            return;
        }
        if world.get::<Viewer>(event.viewer).is_none() {
            return;
        }
        if let Some(token) = event.input.token()
            && world.get::<Prefix>(event.viewer).is_some()
        {
            let settings = world.resource::<Settings>();
            if token != settings.prefix
                && let Some(binding) = settings.bindings.iter().find(|b| b.key == token)
            {
                let binding = binding.action.clone();
                if let Some(target) = crate::actions::Target::of(world, event.viewer) {
                    crate::interaction::dispatch_binding(world, event.viewer, target, &binding);
                }
                return;
            }
        }
        if world.get::<Viewer>(event.viewer).is_none() {
            return;
        }
        if world.get::<Prefix>(event.viewer).is_none()
            && let Input::Mouse {
                action: MouseAction::Press,
                button,
                x,
                y,
                modifiers,
            } = &event.input
        {
            let shift = &modifiers.shift;
            let hit = world
                .get_mut::<Presentation>(event.viewer)
                .and_then(|mut view| view.pointer(*x, *y, false));
            if let Some(hit) = hit {
                let command = if world.get::<Tab>(hit).is_some() {
                    Some(if *button == MouseButton::Right {
                        Command::Menu {
                            subject: Subject::Tab(hit),
                        }
                    } else {
                        Command::TabSelect { tab: hit }
                    })
                } else if world.get::<Workspace>(hit).is_some() {
                    Some(Command::Menu {
                        subject: Subject::Workspace(hit),
                    })
                } else if *button == MouseButton::Right
                    && world.get::<PaneView>(hit).is_some_and(|p| {
                        *shift
                            || world
                                .get::<crate::selection::Selection>(event.viewer)
                                .is_some()
                            || world.get::<Terminal>(p.pane).is_none_or(|t| {
                                t.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None
                            })
                    })
                {
                    Some(Command::Menu {
                        subject: Subject::Pane(hit),
                    })
                } else {
                    None
                };
                if let Some(command) = command {
                    if let Err(error) = execute(world, event.viewer, command) {
                        notify(world, event.viewer, Notice::error(error));
                    }
                    return;
                }
            }
        }
        if let Input::Mouse {
            action: MouseAction::Press,
            button: MouseButton::Left,
            x,
            y,
            modifiers,
        } = &event.input
            && world.get::<Prefix>(event.viewer).is_none()
        {
            let shift = &modifiers.shift;
            let hit = world
                .get_mut::<Presentation>(event.viewer)
                .and_then(|mut view| view.pointer(*x, *y, false));
            if let Some(hit) = hit
                && let Some(pane) = world.get::<PaneView>(hit).map(|p| p.pane)
                && (*shift
                    || world
                        .get::<crate::selection::Selection>(event.viewer)
                        .is_some()
                    || world.get::<Terminal>(pane).is_some_and(|t| {
                        t.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None
                    }))
            {
                let rect = frame::rect(world, event.viewer, hit);
                if let Some(rect) = rect {
                    if focused(world, event.viewer) != Some(hit)
                        && let Some(mut v) = world.get_mut::<Viewer>(event.viewer)
                    {
                        v.scrollback = 0;
                    }
                    if let Ok(mut viewer) = world.get_entity_mut(event.viewer) {
                        viewer.insert(Focused(hit));
                    }
                    if let Err(error) = crate::selection::mouse(
                        world,
                        event.viewer,
                        hit,
                        MouseAction::Press,
                        rect.local(*x, *y),
                    ) {
                        notify(world, event.viewer, Notice::error(error));
                    }
                    return;
                }
            }
        }
        if matches!(event.input, Input::Mouse { .. })
            && world
                .get::<crate::selection::Selection>(event.viewer)
                .is_some()
        {
            return;
        }
        if let Input::Mouse { y, .. } = &event.input
            && world
                .get::<Viewer>(event.viewer)
                .is_some_and(|v| *y >= v.rows.saturating_sub(1))
        {
            return;
        }
        if let Err(error) = terminal_input(world, event.viewer, &event.input) {
            notify(world, event.viewer, Notice::error(error));
        }
        world.resource::<Wake>().notify();
    });
}

#[derive(Resource)]
pub struct Disconnected(pub async_channel::Receiver<Entity>);

/// A scene save or load in flight for the viewer that asked. Detaching the
/// viewer drops the task with it; the file operation still completes.
#[derive(Component)]
struct PendingScene(bevy_tasks::Task<SceneDone>);
struct SceneDone {
    root: Entity,
    path: String,
    mapping: Vec<(Entity, Entity)>,
    result: Result<Option<String>, String>,
}

/// While a scene task runs, the runner polls on a deadline instead of parking:
/// a task cannot wake the runner after its own result is stored.
pub(crate) fn pending_scenes(world: &mut World) -> bool {
    world
        .query_filtered::<(), With<PendingScene>>()
        .iter(world)
        .next()
        .is_some()
}

fn scene_completions(mut pending: Query<(Entity, &mut PendingScene)>, mut commands: Commands) {
    for (viewer, mut task) in &mut pending {
        let Some(done) = bevy_tasks::block_on(bevy_tasks::poll_once(&mut task.0)) else {
            continue;
        };
        commands.entity(viewer).remove::<PendingScene>();
        commands.queue(move |world: &mut World| {
            let result = match done.result {
                Ok(Some(text)) => {
                    assets::deserialize_layout(world, &text, &done.mapping).map(|new| {
                        replace_workspace(world, done.root, new);
                        format!("loaded {}", done.path)
                    })
                }
                Ok(None) => Ok(format!("saved {}", done.path)),
                Err(error) => Err(error),
            };
            notify(
                world,
                viewer,
                match result {
                    Ok(text) => Notice::info(text),
                    Err(error) => Notice::error(error),
                },
            );
        });
    }
}

pub struct ServerPlugin;
impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Workspace>()
            .register_type::<Split>()
            .register_type::<Tab>()
            .register_type::<WorkspaceOrder>()
            .register_type::<Launch>()
            .register_type::<ProcessState>()
            .register_type::<Status>()
            .register_type::<Notice>()
            .register_type::<PaneView>()
            .register_type::<PaneViews>()
            .register_type::<Viewer>()
            .register_type::<Viewing>()
            .register_type::<OnTab>()
            .register_type::<Focused>()
            .register_type::<Name>()
            .register_type::<Action>()
            .register_type::<Command>()
            .register_type::<Subject>()
            .register_type::<Axis>()
            .register_type::<Order>()
            .register_type::<Chooser>()
            .register_type::<Control>()
            .register_type::<UserInput>()
            .register_type::<Shutdown>()
            .register_type::<Input>()
            .register_type::<crate::protocol::Key>()
            .register_type::<crate::protocol::Modifiers>()
            .register_type::<crate::protocol::MouseAction>()
            .register_type::<crate::protocol::MouseButton>()
            .register_type::<crate::protocol::Direction>()
            .register_type::<crate::protocol::Token>();
        presentation::register_types(app);
        crate::navigation::observe(app.world_mut());
        app.add_plugins(TerminalPlugin)
            .add_observer(route_control)
            .add_observer(route_input)
            .add_observer(|removed: On<Remove, Viewer>, mut commands: Commands| {
                commands.entity(removed.entity).try_remove::<Presentation>();
            })
            .add_observer(|_: On<Shutdown>, mut exits: MessageWriter<AppExit>| {
                exits.write(AppExit::Success);
            })
            .add_systems(Startup, initialize)
            .add_systems(
                Update,
                (
                    disconnected,
                    reload_layouts,
                    scene_completions,
                    collapse_layout,
                    invalidate_layouts,
                )
                    .chain(),
            )
            .add_systems(
                bevy_remote::RemoteLast,
                settle_remote_requests.before(bevy_remote::RemoteSystems::ProcessRequests),
            );
    }
}

pub fn remote() -> RemotePlugin {
    RemotePlugin::default()
        .with_method_main("fux.attach", frame::attach)
        .with_method_main("fux.frame", frame::frame)
        .with_watching_method_main("fux.frame+watch", frame::frame_watch)
}

fn initialize(mut commands: Commands, settings: Res<Settings>) {
    let root = workspace(&mut commands, "main");
    let tab = commands.spawn((Tab, Name::new("main"), ChildOf(root))).id();
    if let Err(error) = spawn_pane(&mut commands, &settings, tab, None, None) {
        bevy_log::error!("initial terminal: {error}");
    }
}

pub(crate) fn workspace(commands: &mut Commands, name: &str) -> Entity {
    commands
        .spawn((Workspace, WorkspaceOrder(0), Name::new(name.to_owned())))
        .id()
}
pub(crate) fn spawn_pane(
    commands: &mut Commands,
    settings: &Settings,
    parent: Entity,
    argv: Option<Vec<String>>,
    cwd: Option<String>,
) -> Result<Entity, String> {
    let launch = Launch {
        argv: argv.unwrap_or_else(|| settings.shell.clone()),
        cwd: cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }),
        history_lines: settings.history_lines,
    };
    if launch.argv.is_empty() {
        return Err("shell command is empty".into());
    }
    let name = launch
        .argv
        .first()
        .and_then(|program| program.rsplit('/').next())
        .unwrap_or("shell")
        .to_owned();
    let pane = commands.spawn((launch, Name::new(name))).id();
    Ok(commands.spawn((PaneView { pane }, ChildOf(parent))).id())
}
pub(crate) fn first_leaf(world: &World, root: Entity) -> Option<Entity> {
    if world.get::<PaneView>(root).is_some() {
        return Some(root);
    }
    world
        .get::<Children>(root)?
        .iter()
        .find_map(|child| first_leaf(world, child))
}
fn settle_remote_requests(receiver: Res<bevy_remote::BrpReceiver>, wake: Res<Wake>) {
    // Stock requests run after Update. Schedule one causal settling pass so their
    // mutations reach native lifecycle/layout systems even when otherwise idle.
    if !receiver.is_empty() {
        wake.notify();
    }
}

#[expect(
    clippy::type_complexity,
    reason = "the exception list is the point of this query"
)]
pub(crate) fn invalidate_layouts(
    mut layouts: Query<(Entity, &mut LayoutCache), With<Workspace>>,
    // Every layout entity without the cache itself and without viewer bookkeeping.
    entities: Query<
        EntityRefExcept<(LayoutCache, Viewers, TabViewers, FocusedBy)>,
        Without<bevy_ecs::resource::IsResource>,
    >,
    children: Query<&Children>,
    ticks: SystemChangeTick,
    components: &bevy_ecs::component::Components,
) {
    let ignored = [
        components.component_id::<Viewers>(),
        components.component_id::<TabViewers>(),
        components.component_id::<FocusedBy>(),
    ];
    for (root, mut cache) in &mut layouts {
        if cache.scene.is_none() {
            continue;
        }
        let mut count = 0;
        // Unrestricted reflection requires checking every component, not just Node.
        // Stop at the first change; uncached workspaces need no scan at all.
        let dirty = std::iter::once(root)
            .chain(children.iter_descendants(root))
            .any(|id| {
                let Ok(entity) = entities.get(id) else {
                    return false;
                };
                count += 1;
                let entity = entity.into_filtered();
                let archetype = entity.archetype();
                let mut shape: Vec<_> = archetype
                    .components()
                    .iter()
                    .copied()
                    .filter(|id| !ignored.contains(&Some(*id)))
                    .collect();
                shape.sort();
                cache.members.get(&id).is_none_or(|known| **known != *shape)
                    || shape.iter().any(|id| {
                        entity.get_change_ticks_by_id(*id).is_some_and(|change| {
                            change.is_changed(cache.built_at, ticks.this_run())
                        })
                    })
            });
        // Missing members cover removal/despawn/reparent out of the old root.
        if dirty || count != cache.members.len() {
            cache.scene = None;
        }
    }
}
fn disconnected(mut commands: Commands, closed: Res<Disconnected>) {
    while let Ok(entity) = closed.0.try_recv() {
        commands.entity(entity).try_despawn();
    }
}
fn reload_layouts(mut reloads: MessageReader<assets::LayoutReload>, mut commands: Commands) {
    for reload in reloads.read() {
        let handle = reload.handle.clone();
        commands.queue(move |world: &mut World| {
            world.resource_scope(|world, collection: Mut<bevy_asset::Assets<DynamicWorld>>| {
                if let Some(scene) = collection.get(&handle) {
                    match assets::apply_layout(world, scene, &[]) {
                        Ok(root) => {
                            let scene_name = world.get::<Name>(root).cloned();
                            let old = world
                                .query_filtered::<(Entity, &Name), With<Workspace>>()
                                .iter(world)
                                .find(|(entity, name)| {
                                    *entity != root && scene_name.as_ref() == Some(*name)
                                })
                                .map(|(entity, _)| entity);
                            if let Some(old) = old {
                                replace_workspace(world, old, root);
                            }
                        }
                        Err(error) => bevy_log::error!("layout reload: {error}"),
                    }
                }
            });
        });
    }
}
fn replace_workspace(world: &mut World, old: Entity, new: Entity) {
    let first = first_leaf(world, new);
    let viewers: Vec<Entity> = world
        .get::<Viewers>(old)
        .map(|viewers| viewers.iter().collect())
        .unwrap_or_default();
    for id in viewers {
        let mut viewer = world.entity_mut(id);
        viewer.remove::<(OnTab, Focused)>().insert(Viewing(new));
        if let Some(first) = first {
            viewer.insert(Focused(first));
        }
        if let Some(mut v) = world.get_mut::<Viewer>(id) {
            v.zoom = false;
        }
    }
    let _ = world.despawn(old);
}
pub(crate) fn scene(world: &mut World, root: Entity) -> Result<(u32, Arc<DynamicWorld>), String> {
    if world.get::<Workspace>(root).is_none() {
        return Err("layout root is not a workspace".into());
    }
    let cache = world
        .entity(root)
        .get_ref::<LayoutCache>()
        .ok_or("layout cache is missing")?;
    if let Some(scene) = &cache.scene {
        return Ok((cache.last_changed().get(), Arc::clone(scene)));
    }
    // Advance the native tick at extraction: later mutations in this same
    // exclusive transaction must be distinguishable from the captured scene.
    let built_at = world.increment_change_tick();
    let scene = Arc::new(assets::extract_layout(world, root)?);
    let members = scene
        .entities
        .iter()
        .map(|entity| (entity.entity, shape(world, entity.entity)))
        .collect();
    let mut cache = world
        .get_mut::<LayoutCache>(root)
        .ok_or("layout cache is missing")?;
    cache.built_at = built_at;
    cache.scene = Some(Arc::clone(&scene));
    cache.members = members;
    Ok((cache.last_changed().get(), scene))
}
/// Runs one command for a viewer. Every arm either changes the world here or
/// hands off to the module that owns that part of the model.
pub(crate) fn execute(world: &mut World, id: Entity, command: Command) -> Result<(), String> {
    use crate::interaction::{self, MoveTo, check};
    use crate::navigation::{self as nav, Pick, Scope};
    use Command::*;
    if !matches!(command, CopyMode | Scroll { .. })
        && let Ok(mut entity) = world.get_entity_mut(id)
    {
        entity.remove::<crate::selection::Selection>();
    }
    interaction::close_prefix(world, id);
    notify(world, id, None);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let (rows, scrollback) = (v.rows, v.scrollback);
    let target = crate::actions::Target::of(world, id).ok_or(DETACHED)?;
    let (workspace, tab) = (target.workspace, target.tab);
    let focus = target
        .leaf
        .filter(|leaf| world.get::<PaneView>(*leaf).is_some());
    let focus_on = |world: &mut World, leaf: Entity| -> Result<(), String> {
        world
            .get_entity_mut(id)
            .map_err(|_| DETACHED)?
            .insert(Focused(leaf));
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
        Ok(())
    };
    let pane_of = |world: &World, leaf: Entity| world.get::<PaneView>(leaf).map(|p| p.pane);
    let one_tab = || nav::tabs(world, workspace).len() < 2;
    let one_pane = || tab.is_none_or(|tab| nav::leaves(world, tab).len() < 2);
    match command {
        Split { axis, program } => {
            let settings = world.resource::<Settings>().clone();
            let container_of = tab.unwrap_or(workspace);
            let leaf = focus.or_else(|| first_leaf(world, container_of));
            let cwd = leaf
                .and_then(|leaf| pane_of(world, leaf))
                .and_then(|pane| world.get::<Launch>(pane))
                .map(|launch| launch.cwd.clone());
            let argv = program.map(|program| vec!["/bin/sh".into(), "-lc".into(), program]);
            let new = match leaf {
                Some(leaf) => {
                    let parent = world
                        .get::<ChildOf>(leaf)
                        .ok_or("pane has no parent")?
                        .parent();
                    let index = world
                        .get::<Children>(parent)
                        .and_then(|siblings| siblings.iter().position(|e| e == leaf))
                        .ok_or("missing child")?;
                    let mut container = world.spawn(Split);
                    if axis == Axis::Vertical {
                        container.insert(split_node(FlexDirection::Column));
                    }
                    let container = container.id();
                    let new = spawn_pane(&mut world.commands(), &settings, container, argv, cwd)?;
                    world.flush();
                    world.entity_mut(container).insert_children(0, &[leaf]);
                    world
                        .entity_mut(parent)
                        .insert_children(index, &[container]);
                    new
                }
                None => {
                    let new =
                        spawn_pane(&mut world.commands(), &settings, container_of, argv, cwd)?;
                    world.flush();
                    new
                }
            };
            focus_on(world, new)?;
            world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
        }
        Close { subject } => {
            let entity = check(world, subject)?;
            // Removal observers repair viewer navigation as the hierarchy goes.
            interaction::close(world, entity);
        }
        Terminate => {
            let pane = focus
                .and_then(|leaf| pane_of(world, leaf))
                .ok_or("no pane")?;
            world
                .get_mut::<Terminal>(pane)
                .ok_or("process is not running")?
                .stop()?;
        }
        Zoom => {
            focus.ok_or("no pane")?;
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.zoom = !v.zoom;
        }
        Rename { subject, name } => {
            check(world, subject)?;
            interaction::rename(world, subject, name)?;
        }
        Resize { axis, grow } => {
            let mut child = focus.ok_or("no pane")?;
            let width = axis == Axis::Horizontal;
            while let Some(parent) = world.get::<ChildOf>(child).map(|c| c.parent()) {
                let direction = world
                    .get::<Node>(parent)
                    .ok_or("container has no node")?
                    .flex_direction;
                if width == matches!(direction, FlexDirection::Row | FlexDirection::RowReverse) {
                    let mut node = world.get_mut::<Node>(child).ok_or("pane has no node")?;
                    node.flex_grow = (node.flex_grow + if grow { 0.25 } else { -0.25 }).max(0.1);
                    break;
                }
                child = parent;
            }
        }
        Reorder { order } => {
            let leaf = focus.ok_or("no pane")?;
            if one_pane() {
                return Err("only one pane".into());
            }
            let parent = world
                .get::<ChildOf>(leaf)
                .ok_or("pane has no parent")?
                .parent();
            let mut siblings = world
                .get_mut::<Children>(parent)
                .ok_or("pane has no siblings")?;
            let index = siblings
                .iter()
                .position(|e| e == leaf)
                .ok_or("missing child")?;
            let other = match order {
                Order::Previous => index.saturating_sub(1),
                Order::Next => (index + 1).min(siblings.len() - 1),
            };
            siblings.swap(index, other);
        }
        Swap { with } => {
            let leaf = focus.ok_or("no pane")?;
            interaction::swap(world, leaf, with)?;
        }
        SwapDirection { direction } => {
            if one_pane() {
                return Err("only one pane".into());
            }
            interaction::beside(world, id, target, direction, true)?;
        }
        MoveDirection { direction } => {
            if one_pane() {
                return Err("only one pane".into());
            }
            interaction::beside(world, id, target, direction, false)?;
        }
        MoveToTab { tab } => interaction::move_pane(world, id, target, MoveTo::Tab(tab))?,
        MoveToNewTab { name } => interaction::move_pane(world, id, target, MoveTo::NewTab(name))?,
        MoveToWorkspace { workspace } => {
            interaction::move_pane(world, id, target, MoveTo::Workspace(workspace))?;
        }
        MoveToNewWorkspace { name } => {
            interaction::move_pane(world, id, target, MoveTo::NewWorkspace(name))?;
        }
        CopyMode => crate::selection::start(world, id, focus.ok_or("no pane")?)?,
        Scroll { order } => {
            focus.ok_or("no pane")?;
            let step = usize::from(rows / 2).max(1);
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.scrollback = match order {
                Order::Previous => scrollback.saturating_add(step),
                Order::Next => scrollback.saturating_sub(step),
            };
        }
        Copy => {
            let settings = world.resource::<Settings>();
            crate::selection::validate_clipboard(settings, "")?;
            let pane = focus
                .and_then(|leaf| pane_of(world, leaf))
                .ok_or("no pane")?;
            if world
                .get::<Presentation>(id)
                .ok_or("presentation not initialized")?
                .clipboard
                .len()
                == 16
            {
                return Err("clipboard delivery queue is full".into());
            }
            let text = world
                .get_mut::<Terminal>(pane)
                .ok_or("terminal not found")?
                .copy_text(scrollback);
            crate::selection::validate_clipboard(world.resource::<Settings>(), &text)?;
            if let Some(mut view) = world.get_mut::<Presentation>(id) {
                view.clipboard.push(text);
            }
            notify(world, id, Notice::info("visible pane copied via OSC52"));
        }
        Focus { pane } => {
            check(world, Subject::Pane(pane))?;
            let shown = frame::rect(world, id, pane).is_some();
            let in_tab = tab.is_some_and(|tab| nav::leaves(world, tab).contains(&pane));
            if !shown || !in_tab {
                return Err("target is not a pane in this workspace".into());
            }
            focus_on(world, pane)?;
        }
        FocusNext | FocusPrevious => {
            focus.ok_or("no pane")?;
            if one_pane() {
                return Err("only one pane".into());
            }
            let next = world
                .get_mut::<Presentation>(id)
                .ok_or("presentation not initialized")?
                .focus_step(command == FocusPrevious);
            if let Some(next) = next {
                focus_on(world, next)?;
            }
        }
        FocusLast => {
            focus.ok_or("no pane")?;
            if one_pane() {
                return Err("only one pane".into());
            }
            nav::focus_last(world, id)?;
        }
        FocusDirection { direction } => {
            let leaf = focus.ok_or("no pane")?;
            if let Some(next) = crate::frame::neighbor(world, id, leaf, direction) {
                focus_on(world, next)?;
            }
        }
        TabNew { name } => {
            tab.ok_or("no tab")?;
            nav::tab_new(world, id, name)?;
        }
        TabSelect { tab: chosen } => {
            tab.ok_or("no tab")?;
            nav::select(world, id, Scope::Tab, Pick::Entity(chosen))?;
        }
        TabNext | TabPrevious => {
            tab.ok_or("no tab")?;
            if one_tab() {
                return Err("only one tab".into());
            }
            let pick = if command == TabNext {
                Pick::Next
            } else {
                Pick::Previous
            };
            nav::select(world, id, Scope::Tab, pick)?;
        }
        TabReorder { order } => {
            let tab = tab.ok_or("no tab")?;
            if one_tab() {
                return Err("only one tab".into());
            }
            interaction::reorder(world, Subject::Tab(tab), order)?;
        }
        TabClose { tab } => {
            check(world, Subject::Tab(tab))?;
            interaction::close(world, tab);
        }
        WorkspaceNew { name } => {
            let settings = world.resource::<Settings>().clone();
            let roots = nav::workspaces(world);
            let title = name.unwrap_or_else(|| format!("workspace-{}", roots.len() + 1));
            let order = roots
                .iter()
                .filter_map(|e| world.get::<WorkspaceOrder>(*e).map(|o| o.0))
                .max()
                .unwrap_or(-1)
                .saturating_add(1);
            let root = self::workspace(&mut world.commands(), &title);
            world.commands().entity(root).insert(WorkspaceOrder(order));
            let tab = world.spawn((Tab, Name::new("main"), ChildOf(root))).id();
            let leaf = spawn_pane(&mut world.commands(), &settings, tab, None, None)?;
            world.flush();
            world.get_entity_mut(id).map_err(|_| DETACHED)?.insert((
                Viewing(root),
                OnTab(tab),
                Focused(leaf),
            ));
            world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
        }
        WorkspaceSelect { workspace } => {
            nav::select(world, id, Scope::Workspace, Pick::Entity(workspace))?;
        }
        WorkspaceNext | WorkspacePrevious => {
            let pick = if command == WorkspaceNext {
                Pick::Next
            } else {
                Pick::Previous
            };
            nav::select(world, id, Scope::Workspace, pick)?;
        }
        WorkspaceReorder { order } => {
            interaction::reorder(world, Subject::Workspace(workspace), order)?;
        }
        WorkspaceClose { workspace } => {
            check(world, Subject::Workspace(workspace))?;
            interaction::close(world, workspace);
        }
        SaveLayout { workspace, path } => {
            check(world, Subject::Workspace(workspace))?;
            notify(world, id, Notice::info("saving layout..."));
            scene_io(world, id, workspace, path, None);
        }
        LoadLayout {
            workspace,
            path,
            mapping,
        } => {
            check(world, Subject::Workspace(workspace))?;
            notify(world, id, Notice::info("loading layout..."));
            scene_io(world, id, workspace, path, Some(mapping));
        }
        // Help is the prefix command column itself; there is no second surface.
        Help => {
            world.entity_mut(id).insert(Prefix::default());
        }
        Detach => {
            world.despawn(id);
        }
        Menu { subject } => interaction::menu(world, id, target, subject)?,
        Choose { chooser } => interaction::choose(world, id, target, chooser)?,
    }
    Ok(())
}

/// Serializes on the World (only native scene serialization needs it), then
/// reads or writes the file on the task pool and returns through ECS.
fn scene_io(
    world: &mut World,
    id: Entity,
    root: Entity,
    path: String,
    load: Option<Vec<(Entity, Entity)>>,
) {
    let serialized = match &load {
        Some(_) => Ok(None),
        None => assets::serialize_layout(world, root).map(Some),
    };
    let mapping = load.unwrap_or_default();
    let wake = world.resource::<Wake>().clone();
    let task = bevy_tasks::IoTaskPool::get().spawn(async move {
        let result = serialized.and_then(|text| match text {
            Some(text) => std::fs::write(&path, text)
                .map(|_| None)
                .map_err(|e| e.to_string()),
            None => std::fs::read_to_string(&path)
                .map(Some)
                .map_err(|e| e.to_string()),
        });
        // Wakes the runner for the common case; `pending_scenes` covers the rest.
        wake.notify();
        SceneDone {
            root,
            path,
            mapping,
            result,
        }
    });
    if let Ok(mut viewer) = world.get_entity_mut(id) {
        viewer.insert(PendingScene(task));
    }
}

type Collapsible = (With<Split>, Without<Tab>, Without<Workspace>);
fn collapse_layout(
    mut commands: Commands,
    containers: Query<(Entity, &ChildOf, Option<&Children>), Collapsible>,
    children: Query<&Children>,
    wake: Res<Wake>,
) {
    for (entity, parent, descendants) in &containers {
        let count = descendants.map_or(0, Children::len);
        if count > 1 {
            continue;
        }
        // Collapse bottom-up so two deferred operations never destroy each
        // other's still-parented children.
        let child = descendants.and_then(|children| children.first()).copied();
        if child.is_some_and(|child| containers.contains(child)) {
            continue;
        }
        if let Some(child) = child {
            let Ok(siblings) = children.get(parent.parent()) else {
                continue;
            };
            let Some(index) = siblings.iter().position(|e| e == entity) else {
                continue;
            };
            commands
                .entity(parent.parent())
                .insert_children(index, &[child]);
        }
        commands.entity(entity).despawn();
        wake.notify();
    }
}

/// Ordinary input for the focused pane: keys and pastes become PTY bytes,
/// mouse events are picked against the painted layout, and the prefix key
/// opens or literally forwards itself.
fn terminal_input(world: &mut World, id: Entity, input: &Input) -> Result<(), String> {
    let prefix = world.get::<Prefix>(id).is_some();
    let focus = focused(world, id);
    let pane_of = |world: &World| {
        focus
            .and_then(|leaf| world.get::<PaneView>(leaf))
            .map(|view| view.pane)
            .ok_or("no focused pane")
    };
    match input {
        Input::PasteBegin => {}
        Input::Resize { rows, cols } => {
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.rows = (*rows).min(4096);
            v.cols = (*cols).min(4096);
        }
        Input::Key { key, modifiers } => {
            let token = input.token().unwrap_or_else(|| Token::from(""));
            let settings = world.resource::<Settings>();
            // Reserved keys and bound shortcuts were consumed upstream; what
            // reaches here in the column is an unbound key or the literal prefix.
            if prefix {
                if token != settings.prefix {
                    notify(
                        world,
                        id,
                        Notice::info(format!("unbound prefix key {token}")),
                    );
                    return Ok(());
                }
                crate::interaction::close_prefix(world, id);
            } else if token == settings.prefix {
                world.entity_mut(id).insert(Prefix::default());
                notify(world, id, None);
                return Ok(());
            }
            let pane = pane_of(world)?;
            let terminal = world.get::<Terminal>(pane).ok_or("terminal not found")?;
            let application = terminal.screen().application_cursor();
            terminal.input(&crate::encode::key_bytes(*key, *modifiers, application))?;
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.scrollback = 0;
            v.notice = None;
        }
        Input::Paste { text } => {
            if prefix {
                return Ok(());
            }
            let pane = pane_of(world)?;
            let terminal = world.get::<Terminal>(pane).ok_or("terminal not found")?;
            if terminal.screen().bracketed_paste() {
                terminal.input(format!("\x1b[200~{text}\x1b[201~").as_bytes())?;
            } else {
                terminal.input(text.as_bytes())?;
            }
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.scrollback = 0;
            v.notice = None;
        }
        Input::Mouse {
            action,
            button,
            x,
            y,
            modifiers,
        } => {
            if prefix {
                return Ok(());
            }
            // Pick against the last painted native layout, not a second
            // rectangle hit-test implementation.
            let hit = {
                let mut context = world
                    .get_mut::<Presentation>(id)
                    .ok_or("presentation not initialized")?;
                let hit = context.pointer(*x, *y, *action == MouseAction::Press);
                hit.map(|hit| {
                    context
                        .rects()
                        .iter()
                        .find(|r| r.leaf == hit)
                        .copied()
                        .ok_or("picked pane has no rectangle")
                })
            };
            let Some(rect) = hit.transpose()? else {
                return Ok(());
            };
            let hit = rect.leaf;
            if *action == MouseAction::Press {
                world.entity_mut(id).insert(Focused(hit));
                world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
            }
            let terminal = world
                .get::<Terminal>(rect.pane)
                .ok_or("terminal not found")?;
            let screen = terminal.screen();
            let mode = screen.mouse_protocol_mode();
            use vt100::MouseProtocolMode as MouseMode;
            if mode == MouseMode::None || modifiers.shift {
                if matches!(action, MouseAction::ScrollUp | MouseAction::ScrollDown) {
                    let order = if *action == MouseAction::ScrollUp {
                        Order::Previous
                    } else {
                        Order::Next
                    };
                    if focus != Some(hit) {
                        world.entity_mut(id).insert(Focused(hit));
                        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
                    }
                    execute(world, id, Command::Scroll { order })?;
                }
            } else if rect.covers(*x, *y)
                && let Some(bytes) = crate::encode::mouse_bytes(
                    screen,
                    *action,
                    *button,
                    (x - rect.x() + 1, y - rect.y() + 1),
                    *modifiers,
                )
            {
                terminal.input(&bytes)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[derive(Component, bevy_reflect::Reflect)]
    #[reflect(Component)]
    struct Extra(u32);

    #[test]
    fn removing_viewer_drops_presentation_without_despawning_entity() -> crate::testing::Outcome {
        let mut app = App::new();
        app.insert_resource(Wake(std::thread::current()));
        app.add_plugins(ServerPlugin);
        let registry = app.world().resource::<AppTypeRegistry>().clone();
        let viewer = app
            .world_mut()
            .spawn((
                Viewer {
                    rows: 24,
                    cols: 80,
                    zoom: false,
                    scrollback: 0,
                    notice: None,
                },
                Presentation::new(registry),
            ))
            .id();
        assert!(app.world().get::<Presentation>(viewer).is_some());
        app.world_mut().entity_mut(viewer).remove::<Viewer>();
        assert!(app.world().get_entity(viewer).is_ok());
        assert!(app.world().get::<Presentation>(viewer).is_none());
        app.world_mut().despawn(viewer);
        Ok(())
    }

    #[test]
    fn arbitrary_layout_changes_invalidate_only_the_owning_workspace() -> crate::testing::Outcome {
        let mut app = App::new();
        app.register_type::<Workspace>()
            .register_type::<Name>()
            .register_type::<Node>()
            .register_type::<ChildOf>()
            .register_type::<Children>()
            .register_type::<Extra>()
            .add_systems(Update, invalidate_layouts);
        let left = app.world_mut().spawn(Workspace).id();
        let right = app.world_mut().spawn(Workspace).id();
        let nested = app.world_mut().spawn((Node::default(), ChildOf(left))).id();
        let leaf = app
            .world_mut()
            .spawn((Name::new("before"), ChildOf(nested)))
            .id();
        app.update();
        let untouched = scene(app.world_mut(), right)?.1;
        let initial = scene(app.world_mut(), left)?.1;
        app.update();
        assert!(Arc::ptr_eq(&initial, &scene(app.world_mut(), left)?.1));
        // The descendant deliberately has no Node. Newly inserted, previously
        // absent types and ordinary in-place writes must still reach the scene.
        app.world_mut().entity_mut(leaf).insert(Extra(7));
        // A synchronous control can observe this mutation before another Update.
        app.world_mut().run_system_cached(invalidate_layouts)?;
        let inserted = scene(app.world_mut(), left)?.1;
        assert!(!Arc::ptr_eq(&initial, &inserted));
        assert!(Arc::ptr_eq(&untouched, &scene(app.world_mut(), right)?.1));
        app.world_mut().get_mut::<Extra>(leaf).need()?.0 = 9;
        app.update();
        let modified = scene(app.world_mut(), left)?.1;
        assert!(!Arc::ptr_eq(&inserted, &modified));
        app.world_mut().entity_mut(leaf).remove::<Extra>();
        app.update();
        let removed = scene(app.world_mut(), left)?.1;
        assert!(!Arc::ptr_eq(&modified, &removed));
        assert!(Arc::ptr_eq(&untouched, &scene(app.world_mut(), right)?.1));
        app.world_mut().entity_mut(nested).insert(ChildOf(right));
        app.update();
        let emptied = scene(app.world_mut(), left)?.1;
        let moved = scene(app.world_mut(), right)?.1;
        assert!(!Arc::ptr_eq(&removed, &emptied));
        assert!(!Arc::ptr_eq(&untouched, &moved));
        assert!(!emptied.entities.iter().any(|entity| entity.entity == leaf));
        assert!(moved.entities.iter().any(|entity| entity.entity == leaf));
        app.world_mut().despawn(nested);
        app.update();
        let despawned = scene(app.world_mut(), right)?.1;
        assert!(
            !despawned
                .entities
                .iter()
                .any(|entity| entity.entity == leaf)
        );
        assert!(Arc::ptr_eq(&emptied, &scene(app.world_mut(), left)?.1));
        app.world_mut().entity_mut(right).remove::<Workspace>();
        assert!(scene(app.world_mut(), right).is_err());
        app.world_mut().despawn(right);
        let replacement = app.world_mut().spawn(Workspace).id();
        app.update();
        assert_eq!(scene(app.world_mut(), replacement)?.1.entities.len(), 1);
        Ok(())
    }
}
