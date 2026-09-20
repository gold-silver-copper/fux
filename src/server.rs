use crate::{
    actions::{Action, TargetKind},
    assets::{self, Settings},
    control::{Control, Shutdown, UserInput},
    frame::{self, Views, sync_view, with_views},
    model::*,
    presentation,
    protocol::Input,
    terminal::{Terminal, TerminalPlugin},
};
use bevy_app::{App, AppExit, Plugin, Startup, Update};
use bevy_ecs::{prelude::*, system::SystemChangeTick, world::EntityRefExcept};
use bevy_remote::RemotePlugin;
use bevy_ui::{FlexDirection, Node, Val};
use bevy_world_serialization::DynamicWorld;
use std::sync::Arc;

/// A control whose action name already parsed; the original event is retained
/// for its value, target and mapping.
#[derive(Event)]
struct RoutedControl(Control, Action);
impl std::ops::Deref for RoutedControl {
    type Target = Control;
    fn deref(&self) -> &Control {
        &self.0
    }
}
#[derive(Event)]
struct RoutedInput(UserInput);
impl std::ops::Deref for RoutedInput {
    type Target = UserInput;
    fn deref(&self) -> &UserInput {
        &self.0
    }
}

fn route_control(event: On<Control>, mut commands: Commands) {
    let event = event.event().clone();
    commands.queue(move |world: &mut World| {
        if with_views(world, |world, views| sync_view(world, views, event.viewer)).is_err() {
            return;
        }
        let Some(v) = world.get::<Viewer>(event.viewer) else {
            return;
        };
        let mut target = crate::actions::Target::viewer(v);
        let action = match event.action.parse::<Action>() {
            Ok(action) => action,
            Err(message) => {
                return crate::interaction::unknown(world, event.viewer, target, message);
            }
        };
        if let Some(entity) = event.target {
            if world.get_entity(entity).is_err() {
                notify(world, event.viewer, "target no longer exists", true);
                return;
            }
            let valid_kind = match action.target_kind() {
                TargetKind::Tab => world.get::<Tab>(entity).is_some(),
                TargetKind::Workspace => world.get::<Workspace>(entity).is_some(),
                TargetKind::Pane => world.get::<PaneView>(entity).is_some(),
                TargetKind::Any => true,
            };
            if !valid_kind {
                notify(
                    world,
                    event.viewer,
                    "target has the wrong kind for this action",
                    true,
                );
                return;
            }
            use Action::*;
            if world.get::<PaneView>(entity).is_some() && !matches!(action, Swap | Focus) {
                target.leaf = Some(entity);
            }
            if world.get::<Tab>(entity).is_some()
                && matches!(
                    action,
                    TabClose | RenameTab | TabMenu | TabReorderPrevious | TabReorderNext
                )
            {
                target.tab = Some(entity);
                target.leaf = None;
            }
            if world.get::<Workspace>(entity).is_some()
                && matches!(
                    action,
                    WorkspaceClose
                        | RenameWorkspace
                        | WorkspaceMenu
                        | WorkspaceReorderPrevious
                        | WorkspaceReorderNext
                )
            {
                target.workspace = entity;
                target.tab = None;
                target.leaf = None;
            }
        }
        match crate::interaction::invoke(
            world,
            event.viewer,
            target,
            action,
            event.target,
            &event.value,
            false,
        ) {
            Ok(true) => {}
            Ok(false) => world.trigger(RoutedControl(event, action)),
            Err(error) => notify(world, event.viewer, error, true),
        }
    });
}

fn route_input(event: On<UserInput>, mut commands: Commands) {
    let event = event.event().clone();
    commands.queue(move |world: &mut World| {
        if with_views(world, |world, views| sync_view(world, views, event.viewer)).is_err() {
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
            && matches!(action.as_str(), "move" | "release")
            && let Some(selection) = world.get::<crate::selection::Selection>(event.viewer)
            && selection.dragging
        {
            let leaf = selection.leaf;
            let rect = world
                .non_send::<Views>()
                .get(&event.viewer)
                .and_then(|v| v.presentation.rects().iter().find(|r| r.leaf == leaf))
                .copied();
            if let Some(rect) = rect {
                let point = (
                    y.saturating_sub(rect.y).min(rect.height.saturating_sub(1)),
                    x.saturating_sub(rect.x).min(rect.width.saturating_sub(1)),
                );
                let _ = crate::selection::mouse(world, event.viewer, leaf, action, point);
            }
            return;
        }
        let Some(v) = world.get::<Viewer>(event.viewer) else {
            return;
        };
        if let Some(token) = event.input.token()
            && v.prefix
        {
            let settings = world.resource::<Settings>();
            if token != settings.prefix
                && let Some(binding) = settings.bindings.iter().find(|b| b.key == token)
            {
                let action = binding.action.clone();
                let target = crate::actions::Target::viewer(v);
                crate::interaction::dispatch_named(
                    world,
                    event.viewer,
                    target,
                    &action,
                    None,
                    "",
                    true,
                );
                return;
            }
        }
        let Some(v) = world.get::<Viewer>(event.viewer) else {
            return;
        };
        if !v.prefix
            && let Input::Mouse {
                action,
                button,
                x,
                y,
                shift,
                ..
            } = &event.input
            && action == "press"
        {
            let mut target = crate::actions::Target::viewer(v);
            let hit = world
                .non_send_mut::<Views>()
                .get_mut(&event.viewer)
                .and_then(|view| view.presentation.pointer(*x, *y, false));
            if let Some(hit) = hit {
                let action = if world.get::<Tab>(hit).is_some() {
                    target.tab = Some(hit);
                    target.leaf = None;
                    Some(if *button == 2 {
                        Action::TabMenu
                    } else {
                        Action::TabSelect
                    })
                } else if world.get::<Workspace>(hit).is_some() {
                    target.workspace = hit;
                    target.tab = None;
                    target.leaf = None;
                    Some(Action::WorkspaceMenu)
                } else if *button == 2
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
                    target.leaf = Some(hit);
                    Some(Action::PaneMenu)
                } else {
                    None
                };
                if let Some(action) = action {
                    let destination = (action == Action::TabSelect).then_some(hit);
                    crate::interaction::dispatch(
                        world,
                        event.viewer,
                        target,
                        action,
                        destination,
                        "",
                        true,
                    );
                    return;
                }
            }
        }
        if let Input::Mouse {
            action,
            button,
            x,
            y,
            shift,
            ..
        } = &event.input
            && action == "press"
            && *button == 0
            && world.get::<Viewer>(event.viewer).is_some_and(|v| !v.prefix)
        {
            let hit = world
                .non_send_mut::<Views>()
                .get_mut(&event.viewer)
                .and_then(|view| view.presentation.pointer(*x, *y, false));
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
                let rect = world
                    .non_send::<Views>()
                    .get(&event.viewer)
                    .and_then(|v| v.presentation.rects().iter().find(|r| r.leaf == hit))
                    .copied();
                if let Some(rect) = rect {
                    if let Some(mut v) = world.get_mut::<Viewer>(event.viewer) {
                        if v.focus != Some(hit) {
                            v.scrollback = 0;
                        }
                        v.focus = Some(hit);
                    }
                    if let Err(error) = crate::selection::mouse(
                        world,
                        event.viewer,
                        hit,
                        action,
                        (y.saturating_sub(rect.y), x.saturating_sub(rect.x)),
                    ) {
                        notify(world, event.viewer, error, true);
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
        world.trigger(RoutedInput(event));
    });
}

#[derive(Resource)]
pub struct Disconnected(pub async_channel::Receiver<Entity>);

#[derive(Resource)]
struct SceneIo {
    sender: async_channel::Sender<SceneDone>,
    receiver: async_channel::Receiver<SceneDone>,
}
struct SceneDone {
    viewer: Entity,
    root: Entity,
    path: String,
    mapping: Vec<(Entity, Entity)>,
    result: Result<Option<String>, String>,
}
impl Default for SceneIo {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(16);
        Self { sender, receiver }
    }
}

fn scene_completions(io: Res<SceneIo>, mut commands: Commands) {
    while let Ok(done) = io.receiver.try_recv() {
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
            let error = result.is_err();
            notify(
                world,
                done.viewer,
                result.unwrap_or_else(|error| error),
                error,
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
            .register_type::<PaneView>()
            .register_type::<PaneViews>()
            .register_type::<Viewer>()
            .register_type::<Name>()
            .register_type::<Control>()
            .register_type::<UserInput>()
            .register_type::<Shutdown>()
            .register_type::<Input>();
        presentation::register_types(app);
        app.init_resource::<SceneIo>()
            .insert_non_send(Views::default())
            .add_plugins(TerminalPlugin)
            .add_observer(control_event)
            .add_observer(input_event)
            .add_observer(route_control)
            .add_observer(route_input)
            .add_observer(
                |removed: On<Remove, Viewer>, mut views: NonSendMut<Views>| {
                    views.remove(&removed.entity);
                },
            )
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
                    crate::navigation::repair,
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
    if let Err(error) = spawn_pane(&mut commands, &settings, root, None, None) {
        bevy_log::error!("initial terminal: {error}");
    }
}

fn root_node() -> Node {
    Node {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        flex_grow: 1.0,
        flex_basis: Val::Px(0.0),
        min_width: Val::Px(0.0),
        min_height: Val::Px(0.0),
        ..Default::default()
    }
}
fn leaf_node() -> Node {
    Node {
        flex_grow: 1.0,
        flex_basis: Val::Px(0.0),
        min_width: Val::ZERO,
        min_height: Val::ZERO,
        ..Default::default()
    }
}
pub(crate) fn workspace(commands: &mut Commands, name: &str) -> Entity {
    commands
        .spawn((
            Workspace,
            WorkspaceOrder(0),
            Name::new(name.to_owned()),
            root_node(),
        ))
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
    Ok(commands
        .spawn((PaneView { pane }, leaf_node(), ChildOf(parent)))
        .id())
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

pub(crate) fn invalidate_layouts(
    mut layouts: Query<(Entity, &mut LayoutCache), With<Workspace>>,
    entities: Query<EntityRefExcept<LayoutCache>, Without<bevy_ecs::resource::IsResource>>,
    children: Query<&Children>,
    ticks: SystemChangeTick,
) {
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
                cache.members.get(&id) != Some(&archetype.id())
                    || archetype.components().iter().any(|id| {
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
    for mut v in world.query::<&mut Viewer>().iter_mut(world) {
        if v.workspace == old {
            v.workspace = new;
            v.focus = first;
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
        .map(|entity| (entity.entity, world.entity(entity.entity).archetype().id()))
        .collect();
    let mut cache = world
        .get_mut::<LayoutCache>(root)
        .ok_or("layout cache is missing")?;
    cache.built_at = built_at;
    cache.scene = Some(Arc::clone(&scene));
    cache.members = members;
    Ok((cache.last_changed().get(), scene))
}
// Direct system parameters make ECS access explicit without a borrowing wrapper.
#[expect(
    clippy::too_many_arguments,
    reason = "Bevy injects each declared ECS access"
)]
fn control_event(
    event: On<RoutedControl>,
    mut commands: Commands,
    mut viewers: Query<&mut Viewer>,
    panes: Query<&PaneView>,
    parents: Query<&ChildOf>,
    children: Query<&Children>,
    roots: Query<(Entity, Option<&WorkspaceOrder>), With<Workspace>>,
    mut nodes: Query<&mut Node>,
    launches: Query<&Launch>,
    settings: Res<Settings>,
    mut terminals: Query<&mut Terminal>,
    mut views: NonSendMut<Views>,
    wake: Res<Wake>,
) {
    let id = event.viewer;
    let action = event.1;
    use Action::*;
    if crate::navigation::handles(action) {
        let target = event.target;
        let value = event.value.clone();
        commands.queue(move |world: &mut World| {
            let result = crate::navigation::control(world, id, action, target, &value);
            if let Err(error) = result {
                notify(world, id, error, true);
            }
        });
        wake.notify();
        return;
    }
    if action == Detach {
        commands.entity(id).try_despawn();
        return;
    }
    let Ok(mut v) = viewers.get_mut(id) else {
        return;
    };
    v.notify("", false);
    v.prefix = false;
    let leaf = event
        .target
        .filter(|e| panes.contains(*e))
        .or(v.focus)
        .filter(|e| panes.contains(*e));
    let pane = leaf
        .and_then(|leaf| panes.get(leaf).ok())
        .map(|view| view.pane);
    let first = |root| {
        children
            .iter_descendants(root)
            .find(|entity| panes.contains(*entity))
    };
    let result = (|| -> Result<(), String> {
        match action {
            SplitHorizontal | SplitVertical => {
                let leaf = leaf.or_else(|| first(v.tab.unwrap_or(v.workspace)));
                let cwd = leaf
                    .and_then(|leaf| panes.get(leaf).ok())
                    .and_then(|view| launches.get(view.pane).ok())
                    .map(|launch| launch.cwd.clone());
                let argv = (!event.value.is_empty())
                    .then(|| vec!["/bin/sh".into(), "-lc".into(), event.value.clone()]);
                let new = if let Some(leaf) = leaf {
                    let parent = parents.get(leaf).map_err(|e| e.to_string())?.parent();
                    let siblings = children.get(parent).map_err(|e| e.to_string())?;
                    let index = siblings
                        .iter()
                        .position(|e| e == leaf)
                        .ok_or("missing child")?;
                    let mut node = root_node();
                    node.width = Val::Auto;
                    node.height = Val::Auto;
                    node.flex_direction = if action == SplitVertical {
                        FlexDirection::Column
                    } else {
                        FlexDirection::Row
                    };
                    node.column_gap = Val::Px(1.0);
                    node.row_gap = Val::Px(1.0);
                    let container = commands.spawn((Split, node)).id();
                    let new = spawn_pane(&mut commands, &settings, container, argv, cwd)?;
                    commands.entity(container).insert_children(0, &[leaf]);
                    commands.entity(parent).insert_children(index, &[container]);
                    new
                } else {
                    spawn_pane(
                        &mut commands,
                        &settings,
                        v.tab.unwrap_or(v.workspace),
                        argv,
                        cwd,
                    )?
                };
                v.focus = Some(new);
                v.zoom = false;
                v.scrollback = 0;
            }
            Terminate => terminals
                .get_mut(pane.ok_or("no focused process")?)
                .map_err(|_| "terminal not found")?
                .stop()?,
            FocusPrevious => {
                v.focus = views
                    .get_mut(&id)
                    .ok_or("presentation not initialized")?
                    .presentation
                    .focus_step(true);
                v.scrollback = 0;
            }
            FocusLeft | FocusRight | FocusUp | FocusDown => {
                let presentation = &views
                    .get(&id)
                    .ok_or("presentation not initialized")?
                    .presentation;
                let next = presentation.neighbor(
                    v.focus.ok_or("no visible focus")?,
                    action.direction().ok_or("no direction")?,
                );
                if let Some(next) = next {
                    v.focus = Some(next);
                    v.scrollback = 0;
                }
            }
            FocusNext => {
                v.focus = views
                    .get_mut(&id)
                    .ok_or("presentation not initialized")?
                    .presentation
                    .focus_next();
                v.scrollback = 0;
            }
            Focus => {
                let target = event.target.ok_or("focus target required")?;
                if !panes.contains(target)
                    || !parents.iter_ancestors(target).any(|e| Some(e) == v.tab)
                    || !views.get(&id).is_some_and(|view| {
                        view.presentation.rects().iter().any(|r| r.leaf == target)
                    })
                {
                    return Err("target is not a pane in this workspace".into());
                }
                v.focus = Some(target);
                v.scrollback = 0;
            }
            Zoom => {
                let target = leaf.ok_or("no pane")?;
                v.zoom = v.focus != Some(target) || !v.zoom;
                v.focus = Some(target);
            }
            ScrollUp => v.scrollback = v.scrollback.saturating_add(usize::from(v.rows / 2).max(1)),
            ScrollDown => {
                v.scrollback = v.scrollback.saturating_sub(usize::from(v.rows / 2).max(1))
            }
            Copy => {
                crate::selection::validate_clipboard(&settings, "")?;
                let view = views.get_mut(&id).ok_or("presentation not initialized")?;
                if view.clipboard.len() == 16 {
                    return Err("clipboard delivery queue is full".into());
                }
                let text = terminals
                    .get_mut(pane.ok_or("no focused process")?)
                    .map_err(|_| "terminal not found")?
                    .copy_text(if leaf == v.focus { v.scrollback } else { 0 });
                crate::selection::validate_clipboard(&settings, &text)?;
                view.clipboard.push(text);
                v.notice = "visible pane copied via OSC52".into();
            }
            WorkspaceNew => {
                let title = if event.value.is_empty() {
                    format!("workspace-{}", roots.iter().count() + 1)
                } else {
                    event.value.clone()
                };
                v.workspace = workspace(&mut commands, &title);
                let order = roots
                    .iter()
                    .filter_map(|(_, order)| order.map(|order| order.0))
                    .max()
                    .unwrap_or(-1)
                    .saturating_add(1);
                commands.entity(v.workspace).insert(WorkspaceOrder(order));
                v.focus = Some(spawn_pane(
                    &mut commands,
                    &settings,
                    v.workspace,
                    None,
                    None,
                )?);
                v.zoom = false;
            }
            GrowWidth | GrowHeight | ShrinkWidth | ShrinkHeight => {
                let width = matches!(action, GrowWidth | ShrinkWidth);
                let mut child = leaf.ok_or("no focused pane")?;
                while let Ok(parent) = parents.get(child) {
                    let parent = parent.parent();
                    let direction = nodes.get(parent).map_err(|e| e.to_string())?.flex_direction;
                    if width == matches!(direction, FlexDirection::Row | FlexDirection::RowReverse)
                    {
                        let mut node = nodes.get_mut(child).map_err(|e| e.to_string())?;
                        node.flex_grow = (node.flex_grow
                            + if matches!(action, GrowWidth | GrowHeight) {
                                0.25
                            } else {
                                -0.25
                            })
                        .max(0.1);
                        break;
                    }
                    child = parent;
                }
            }
            ReorderPrev | ReorderNext => {
                let leaf = leaf.ok_or("no focused pane")?;
                let parent = parents.get(leaf).map_err(|e| e.to_string())?.parent();
                let siblings = children.get(parent).map_err(|e| e.to_string())?;
                let index = siblings
                    .iter()
                    .position(|e| e == leaf)
                    .ok_or("missing child")?;
                let other = if action == ReorderPrev {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(siblings.len() - 1)
                };
                commands
                    .entity(parent)
                    .entry::<Children>()
                    .and_modify(move |mut children| children.swap(index, other));
            }
            SaveLayout | LoadLayout => {
                let root = event
                    .target
                    .filter(|e| roots.contains(*e))
                    .unwrap_or(v.workspace);
                let path = event.value.clone();
                let save = action == SaveLayout;
                let mapping = event.mapping.clone();
                v.notice = if save {
                    "saving layout..."
                } else {
                    "loading layout..."
                }
                .into();
                // Only native scene serialization needs this World boundary.
                // File I/O runs on the task pool and returns through ECS.
                commands.queue(move |world: &mut World| {
                    let serialized = if save {
                        assets::serialize_layout(world, root).map(Some)
                    } else {
                        Ok(None)
                    };
                    let sender = world.resource::<SceneIo>().sender.clone();
                    let wake = world.resource::<Wake>().clone();
                    bevy_tasks::IoTaskPool::get()
                        .spawn(async move {
                            let result = serialized.and_then(|text| match text {
                                Some(text) => std::fs::write(&path, text)
                                    .map(|_| None)
                                    .map_err(|e| e.to_string()),
                                None => std::fs::read_to_string(&path)
                                    .map(Some)
                                    .map_err(|e| e.to_string()),
                            });
                            let _ = sender
                                .send(SceneDone {
                                    viewer: id,
                                    root,
                                    path,
                                    mapping,
                                    result,
                                })
                                .await;
                            wake.notify();
                        })
                        .detach();
                });
            }
            // Help is the prefix command column itself; there is no second surface.
            Help => {
                v.prefix = true;
                v.help_scroll = 0;
            }
            other => return Err(format!("unknown action {other}")),
        }
        Ok(())
    })();
    if let Err(error) = result {
        v.notify(error, true);
    }
    wake.notify();
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

#[expect(
    clippy::too_many_arguments,
    reason = "Bevy injects each declared ECS access"
)]
fn input_event(
    event: On<RoutedInput>,
    mut commands: Commands,
    mut viewers: Query<&mut Viewer>,
    panes: Query<&PaneView>,
    settings: Res<Settings>,
    terminals: Query<&Terminal>,
    mut views: NonSendMut<Views>,
    wake: Res<Wake>,
) {
    let id = event.viewer;
    let Ok(mut v) = viewers.get_mut(id) else {
        return;
    };
    let result = (|| -> Result<(), String> {
        match &event.input {
            Input::PasteBegin => {}
            Input::Resize { rows, cols } => {
                v.rows = (*rows).min(4096);
                v.cols = (*cols).min(4096);
            }
            Input::Key {
                key,
                ctrl,
                alt,
                shift,
            } => {
                let token = event.input.token().unwrap_or_default();
                if v.prefix {
                    if key == "escape" {
                        v.prefix = false;
                        v.notice.clear();
                        return Ok(());
                    }
                    // A doubled prefix is literal even if also listed as a binding.
                    if token != settings.prefix
                        && let Some(binding) = settings.bindings.iter().find(|b| b.key == token)
                    {
                        v.prefix = false;
                        commands.trigger(Control {
                            viewer: id,
                            action: binding.action.clone(),
                            value: String::new(),
                            target: None,
                            mapping: Vec::new(),
                        });
                        return Ok(());
                    }
                    if token != settings.prefix {
                        v.notify(format!("unbound prefix key {token}"), false);
                        return Ok(());
                    }
                    v.prefix = false;
                } else if token == settings.prefix {
                    v.prefix = true;
                    v.help_scroll = 0;
                    v.notice.clear();
                    return Ok(());
                }
                let pane = panes
                    .get(v.focus.ok_or("no focused pane")?)
                    .map_err(|e| e.to_string())?
                    .pane;
                let terminal = terminals.get(pane).map_err(|_| "terminal not found")?;
                let application = terminal.screen().application_cursor();
                terminal.input(&crate::encode::key_bytes(
                    key,
                    *ctrl,
                    *alt,
                    *shift,
                    application,
                )?)?;
                v.scrollback = 0;
                v.notice.clear();
            }
            Input::Paste { text } => {
                if v.prefix {
                    return Ok(());
                }
                let pane = panes
                    .get(v.focus.ok_or("no focused pane")?)
                    .map_err(|e| e.to_string())?
                    .pane;
                let terminal = terminals.get(pane).map_err(|_| "terminal not found")?;
                if terminal.screen().bracketed_paste() {
                    terminal.input(format!("\x1b[200~{text}\x1b[201~").as_bytes())?;
                } else {
                    terminal.input(text.as_bytes())?;
                }
                v.scrollback = 0;
                v.notice.clear();
            }
            Input::Mouse {
                action,
                button,
                x,
                y,
                ctrl,
                alt,
                shift,
            } => {
                // Pick against the last painted native layout, not a second
                // rectangle hit-test implementation.
                let context = views.get_mut(&id).ok_or("presentation not initialized")?;
                if v.prefix {
                    return Ok(());
                }
                let hit = context.presentation.pointer(*x, *y, action == "press");
                if let Some(hit) = hit {
                    if action == "press" {
                        v.focus = Some(hit);
                        v.scrollback = 0;
                    }
                    let rect = context
                        .presentation
                        .rects()
                        .iter()
                        .find(|r| r.leaf == hit)
                        .ok_or("picked pane has no rectangle")?;
                    let terminal = terminals.get(rect.pane).map_err(|_| "terminal not found")?;
                    let screen = terminal.screen();
                    let mode = screen.mouse_protocol_mode();
                    use vt100::MouseProtocolMode as MouseMode;
                    if mode == MouseMode::None || *shift {
                        if action == "scrollup" || action == "scrolldown" {
                            if v.focus != Some(hit) {
                                v.focus = Some(hit);
                                v.scrollback = 0;
                            }
                            commands.trigger(Control {
                                viewer: id,
                                action: if action == "scrollup" {
                                    Action::ScrollUp
                                } else {
                                    Action::ScrollDown
                                }
                                .to_string(),
                                value: String::new(),
                                target: None,
                                mapping: Vec::new(),
                            });
                        }
                    } else if *x >= rect.x
                        && *x < rect.x + rect.width
                        && *y >= rect.y
                        && *y < rect.y + rect.height
                        && let Some(bytes) = crate::encode::mouse_bytes(
                            screen,
                            action,
                            *button,
                            x - rect.x + 1,
                            y - rect.y + 1,
                            *ctrl,
                            *alt,
                            *shift,
                        )
                    {
                        terminal.input(&bytes)?;
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        v.notify(error, true);
    }
    wake.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[derive(Component, bevy_reflect::Reflect)]
    #[reflect(Component)]
    struct Extra(u32);

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
