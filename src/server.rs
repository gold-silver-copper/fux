use crate::{
    assets::{self, Settings},
    chrome::{self, at, fit},
    control::{Control, Shutdown, UserInput},
    model::*,
    presentation::{self, Presentation},
    protocol::{Frame, Input},
    terminal::{Terminal, TerminalPlugin},
};
use base64::Engine;
use bevy_app::{App, AppExit, Plugin, Startup, Update};
use bevy_ecs::{
    entity::EntityHashMap, prelude::*, system::SystemChangeTick, world::EntityRefExcept,
};
use bevy_remote::{BrpError, BrpResult, RemotePlugin};
use bevy_ui::{FlexDirection, Node, Val};
use bevy_world_serialization::DynamicWorld;
use serde_json::{Value, json};
use std::{
    fmt::Write,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Event)]
struct RoutedControl(Control);
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
        if let Some(entity) = event.target {
            if world.get_entity(entity).is_err() {
                notice(world, event.viewer, "target no longer exists", true);
                return;
            }
            let valid_kind = match event.action.as_str() {
                "tab_close"
                | "rename_tab"
                | "tab_menu"
                | "tab_select"
                | "tab_reorder_previous"
                | "tab_reorder_next"
                | "move_tab" => world.get::<Tab>(entity).is_some(),
                "workspace_close"
                | "rename_workspace"
                | "workspace_menu"
                | "workspace_select"
                | "workspace_reorder_previous"
                | "workspace_reorder_next"
                | "move_workspace"
                | "save_layout"
                | "load_layout" => world.get::<Workspace>(entity).is_some(),
                "close" | "rename_pane" | "terminate" | "focus" | "zoom" | "copy" | "copy_mode"
                | "swap" => world.get::<PaneView>(entity).is_some(),
                _ => true,
            };
            if !valid_kind {
                notice(
                    world,
                    event.viewer,
                    "target has the wrong kind for this action",
                    true,
                );
                return;
            }
            if world.get::<PaneView>(entity).is_some()
                && !matches!(event.action.as_str(), "swap" | "focus")
            {
                target.leaf = Some(entity);
            }
            if world.get::<Tab>(entity).is_some()
                && matches!(
                    event.action.as_str(),
                    "tab_close"
                        | "rename_tab"
                        | "tab_menu"
                        | "tab_reorder_previous"
                        | "tab_reorder_next"
                )
            {
                target.tab = Some(entity);
                target.leaf = None;
            }
            if world.get::<Workspace>(entity).is_some()
                && matches!(
                    event.action.as_str(),
                    "workspace_close"
                        | "rename_workspace"
                        | "workspace_menu"
                        | "workspace_reorder_previous"
                        | "workspace_reorder_next"
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
            &event.action,
            event.target,
            &event.value,
            false,
        ) {
            Ok(true) => {}
            Ok(false) => world.trigger(RoutedControl(event)),
            Err(error) => notice(world, event.viewer, &error, true),
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
        if let Input::Key {
            key,
            ctrl,
            alt,
            shift,
        } = &event.input
            && v.prefix
            && v.prompt.is_none()
        {
            let token = format!(
                "{}{}{}{}",
                if *ctrl { "ctrl-" } else { "" },
                if *alt { "alt-" } else { "" },
                if *shift && key.chars().count() != 1 {
                    "shift-"
                } else {
                    ""
                },
                key
            );
            let settings = world.resource::<Settings>();
            if token != settings.prefix
                && let Some(binding) = settings.bindings.iter().find(|b| b.key == token)
            {
                let action = binding.action.clone();
                let target = crate::actions::Target::viewer(v);
                crate::interaction::dispatch(world, event.viewer, target, &action, None, "", true);
                return;
            }
        }
        let Some(v) = world.get::<Viewer>(event.viewer) else {
            return;
        };
        if !v.prefix
            && v.prompt.is_none()
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
                        "tab_menu"
                    } else {
                        "tab_select"
                    })
                } else if world.get::<Workspace>(hit).is_some() {
                    target.workspace = hit;
                    target.tab = None;
                    target.leaf = None;
                    Some("workspace_menu")
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
                    Some("pane_menu")
                } else {
                    None
                };
                if let Some(action) = action {
                    let destination = (action == "tab_select").then_some(hit);
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
            && world
                .get::<Viewer>(event.viewer)
                .is_some_and(|v| !v.prefix && v.prompt.is_none())
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
                        notice(world, event.viewer, &error, true);
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

type Views = EntityHashMap<View>;
struct View {
    presentation: Presentation,
    last: String,
    clipboard: Vec<String>,
    next_paint: Instant,
    paint_wake_pending: bool,
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
            notice(
                world,
                done.viewer,
                &result.unwrap_or_else(|error| error),
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
        .with_method_main("fux.attach", attach)
        .with_method_main("fux.frame", frame)
        .with_watching_method_main("fux.frame+watch", frame_watch)
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
fn first_leaf(world: &World, root: Entity) -> Option<Entity> {
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

fn invalidate_layouts(
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
fn scene(world: &mut World, root: Entity) -> Result<(u32, Arc<DynamicWorld>), String> {
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
fn with_views<T>(world: &mut World, f: impl FnOnce(&mut World, &mut Views) -> T) -> T {
    // Views is an unreflected non-send resource installed by ServerPlugin; no
    // remote request can remove it.
    #[expect(
        clippy::expect_used,
        reason = "installed once, never remotely removable"
    )]
    let mut views = world
        .remove_non_send::<Views>()
        .expect("presentation contexts installed");
    let result = f(world, &mut views);
    world.insert_non_send(views);
    result
}
const DETACHED: &str = "viewer no longer attached";
fn sync_view(world: &mut World, views: &mut Views, id: Entity) -> Result<(), String> {
    crate::navigation::repair(world);
    // Controls can arrive before the next Update; run the same native change-
    // tracking system at this synchronous input/presentation boundary too.
    world
        .run_system_cached(invalidate_layouts)
        .map_err(|e| e.to_string())?;
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let root = v.workspace;
    let scroll = v
        .help_scroll
        .min(chrome::help_limit(world.resource::<Settings>(), v.rows));
    if scroll != v.help_scroll {
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.help_scroll = scroll;
    }
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    if v.focus.is_none_or(|e| world.get::<PaneView>(e).is_none()) {
        let focus = first_leaf(world, root);
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.focus = focus;
    }
    let (revision, scene) = scene(world, root)?;
    let registry = world.resource::<AppTypeRegistry>().clone();
    let context = views.entry(id).or_insert_with(|| View {
        presentation: Presentation::new(registry),
        last: String::new(),
        clipboard: Vec::new(),
        next_paint: Instant::now(),
        paint_wake_pending: false,
    });
    context.presentation.sync(
        &scene,
        revision,
        root,
        world.get::<Viewer>(id).ok_or(DETACHED)?,
    )?;
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    if v.rows > 1
        && v.cols > 0
        && !context
            .presentation
            .rects()
            .iter()
            .any(|r| Some(r.leaf) == v.focus)
    {
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.focus =
            context.presentation.rects().first().map(|r| r.leaf);
        context.presentation.sync(
            &scene,
            revision,
            root,
            world.get::<Viewer>(id).ok_or(DETACHED)?,
        )?;
    }
    Ok(())
}
fn size_terminals(world: &mut World, views: &Views) {
    let mut sizes = EntityHashMap::<(u16, u16)>::default();
    for context in views.values() {
        for rect in context.presentation.rects() {
            let rows = rect.height;
            let cols = rect.width;
            sizes
                .entry(rect.pane)
                .and_modify(|s| {
                    s.0 = s.0.min(rows);
                    s.1 = s.1.min(cols)
                })
                .or_insert((rows, cols));
        }
    }
    for (pane, (rows, cols)) in sizes {
        if let Some(mut terminal) = world.get_mut::<Terminal>(pane) {
            let _ = terminal.resize(rows, cols);
        }
    }
}
fn attach(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params = params.unwrap_or_default();
    let requested = params.get("workspace").and_then(Value::as_str);
    let root = crate::navigation::workspaces(world)
        .into_iter()
        .find(|e| {
            requested
                .is_none_or(|wanted| world.get::<Name>(*e).is_some_and(|n| n.as_str() == wanted))
        })
        .ok_or_else(|| BrpError::internal("workspace not found"))?;
    let rows = params
        .get("rows")
        .and_then(Value::as_u64)
        .unwrap_or(24)
        .min(4096) as u16;
    let cols = params
        .get("cols")
        .and_then(Value::as_u64)
        .unwrap_or(80)
        .min(4096) as u16;
    let focused = first_leaf(world, root);
    let id = world
        .spawn(Viewer {
            workspace: root,
            tab: None,
            focus: focused,
            rows,
            cols,
            zoom: false,
            scrollback: 0,
            notice: String::new(),
            notice_error: false,
            help_scroll: 0,
            prefix: false,
            prompt: None,
            buffer: String::new(),
        })
        .id();
    with_views(world, |world, views| sync_view(world, views, id)).map_err(BrpError::internal)?;
    Ok(json!({"viewer":id.to_bits()}))
}
fn request_viewer(params: Option<Value>) -> Result<Entity, BrpError> {
    let id = params
        .and_then(|p| p.get("viewer").and_then(Value::as_u64))
        .ok_or_else(|| BrpError::internal("viewer entity required"))?;
    Ok(Entity::from_bits(id))
}
fn frame(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    make_frame(world, request_viewer(params)?)
        .map(|f| json!(f))
        .map_err(BrpError::internal)
}
fn frame_watch(In(params): In<Option<Value>>, world: &mut World) -> BrpResult<Option<Value>> {
    let id = request_viewer(params)?;
    let now = Instant::now();
    let deferred = world.non_send_mut::<Views>().get_mut(&id).and_then(|view| {
        if now >= view.next_paint {
            view.paint_wake_pending = false;
            None
        } else {
            let needs_wake = !view.paint_wake_pending;
            view.paint_wake_pending = true;
            Some((view.next_paint, needs_wake))
        }
    });
    if let Some((deadline, needs_wake)) = deferred {
        if needs_wake {
            // Coalesce hot output before painting, without a periodic idle tick.
            let wake = world.resource::<Wake>().clone();
            bevy_tasks::IoTaskPool::get()
                .spawn(async move {
                    async_io::Timer::at(deadline).await;
                    wake.notify();
                })
                .detach();
        }
        return Ok(None);
    }
    let has_effect = world
        .non_send::<Views>()
        .get(&id)
        .is_some_and(|view| !view.clipboard.is_empty());
    let frame = make_frame(world, id).map_err(BrpError::internal)?;
    if frame.detach {
        return Ok(Some(json!(frame)));
    }
    let mut views = world.non_send_mut::<Views>();
    let Some(view) = views.get_mut(&id) else {
        return Ok(None);
    };
    if !has_effect && view.last == frame.paint {
        return Ok(None);
    }
    view.last.clone_from(&frame.paint);
    view.next_paint = Instant::now() + Duration::from_millis(16);
    Ok(Some(json!(frame)))
}
fn make_frame(world: &mut World, id: Entity) -> Result<Frame, String> {
    if world.get::<Viewer>(id).is_none() {
        return Ok(Frame {
            paint: String::new(),
            detach: true,
        });
    }
    with_views(world, |world, views| {
        // Size negotiation must never count another viewer's stale, now-hidden
        // tab projection merely because that viewer has not requested a frame.
        let viewers: Vec<_> = world
            .query_filtered::<Entity, With<Viewer>>()
            .iter(world)
            .collect();
        for viewer in viewers {
            sync_view(world, views, viewer)?;
        }
        size_terminals(world, views);
        if let Some(selection) = world.get::<crate::selection::Selection>(id) {
            let visible = views
                .get(&id)
                .and_then(|v| {
                    v.presentation
                        .rects()
                        .iter()
                        .find(|r| r.leaf == selection.leaf)
                })
                .map_or((0, 0), |r| (r.height, r.width));
            crate::selection::refresh_visible(world, id, visible);
        }
        paint(world, views, id)
    })
}

pub(crate) fn content_size(world: &World, viewer: Entity, leaf: Entity) -> Option<(u16, u16)> {
    world
        .get_non_send::<Views>()?
        .get(&viewer)?
        .presentation
        .rects()
        .iter()
        .find(|r| r.leaf == leaf)
        .map(|r| (r.height, r.width))
}

pub(crate) fn visible_leaf(world: &World, viewer: Entity, leaf: Entity) -> bool {
    world.get_non_send::<Views>().is_none_or(|views| {
        views
            .get(&viewer)
            .is_some_and(|view| view.presentation.rects().iter().any(|r| r.leaf == leaf))
    })
}

pub(crate) fn neighbor(
    world: &World,
    viewer: Entity,
    leaf: Entity,
    direction: &str,
) -> Option<Entity> {
    world
        .non_send::<Views>()
        .get(&viewer)?
        .presentation
        .neighbor(leaf, direction)
}

pub(crate) fn directional_neighbor(
    rects: &[crate::protocol::PaneRect],
    leaf: Entity,
    direction: &str,
) -> Option<Entity> {
    let here = rects.iter().find(|r| r.leaf == leaf)?;
    let cx = i32::from(here.x) * 2 + i32::from(here.width);
    let cy = i32::from(here.y) * 2 + i32::from(here.height);
    rects
        .iter()
        .filter_map(|r| {
            let dx = i32::from(r.x) * 2 + i32::from(r.width) - cx;
            let dy = i32::from(r.y) * 2 + i32::from(r.height) - cy;
            let (forward, cross) = match direction {
                "left" => (-dx, dy.abs()),
                "right" => (dx, dy.abs()),
                "up" => (-dy, dx.abs()),
                _ => (dy, dx.abs()),
            };
            (forward > 0).then_some(((cross, forward, r.leaf.to_bits()), r.leaf))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, leaf)| leaf)
}

fn name(world: &World, entity: Entity) -> String {
    world
        .get::<Name>(entity)
        .map_or_else(|| entity.to_bits().to_string(), |n| n.as_str().to_owned())
}

pub(crate) fn clipboard(world: &mut World, id: Entity, text: String) -> Result<(), String> {
    crate::selection::validate_clipboard(world.resource::<Settings>(), &text)?;
    let mut views = world.non_send_mut::<Views>();
    let view = views.get_mut(&id).ok_or("presentation not initialized")?;
    if view.clipboard.len() == 16 {
        return Err("clipboard delivery queue is full".into());
    }
    view.clipboard.push(text);
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.notice = "selection copied via OSC52".into();
        v.notice_error = false;
    }
    Ok(())
}

fn process_status(world: &World, pane: Entity) -> String {
    world
        .get::<ProcessState>(pane)
        .map_or_else(String::new, |s| {
            s.exit
                .map(|code| format!(" [exit:{code}]"))
                .or_else(|| s.error.as_ref().map(|error| format!(" [{error}]")))
                .unwrap_or_default()
        })
}

fn paint(world: &mut World, views: &mut Views, id: Entity) -> Result<Frame, String> {
    let v = world.get::<Viewer>(id).ok_or("viewer no longer attached")?;
    let focus = v.focus;
    let scrollback = v.scrollback;
    let modal = v.prefix
        || v.prompt.is_some()
        || world.get::<crate::interaction::Overlay>(id).is_some()
        || world.get::<crate::selection::Selection>(id).is_some();
    let view = views.get_mut(&id).ok_or("missing presentation")?;
    let mut out = String::from("\x1b[?2026h\x1b[?7l\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    let focused = focus
        .and_then(|leaf| world.get::<PaneView>(leaf))
        .map_or_else(String::new, |view| {
            format!(
                "{}: {}{}",
                view.pane.to_bits(),
                name(world, view.pane),
                process_status(world, view.pane)
            )
        });
    let mut cursor = None;
    for rect in view.presentation.rects() {
        let selected = focus == Some(rect.leaf);
        let status = process_status(world, rect.pane);
        let exited = !status.is_empty();
        match world.get_mut::<Terminal>(rect.pane) {
            Some(mut terminal) => {
                let (lines, screen) =
                    terminal.snapshot(if selected { scrollback } else { 0 }, rect.width);
                for (row, line) in lines.iter().take(usize::from(rect.height)).enumerate() {
                    // Snapshot rows already reset style at both ends.
                    at(&mut out, rect.x, rect.y + row as u16, line);
                }
                let (row, col) = screen.cursor_position();
                if selected
                    && !screen.hide_cursor()
                    && scrollback == 0
                    && !exited
                    && row < rect.height
                    && col < rect.width
                {
                    cursor = Some((rect.x + col, rect.y + row));
                }
            }
            None => at(
                &mut out,
                rect.x,
                rect.y,
                fit("terminal not found", rect.width, false),
            ),
        }
        if !selected && exited {
            let label = fit(&status, rect.width, false);
            at(
                &mut out,
                rect.x + rect.width - chrome::width(&label),
                rect.y + rect.height - 1,
                format_args!("\x1b[0;2;7m{label}\x1b[0m"),
            );
        }
    }
    if let Some(selection) = world.get::<crate::selection::Selection>(id)
        && let Some(rect) = view
            .presentation
            .rects()
            .iter()
            .find(|r| r.leaf == selection.leaf)
    {
        crate::selection::paint(&mut out, selection, rect);
    }
    view.presentation.paint_separators(&mut out, focus);
    let v = world.get::<Viewer>(id).ok_or("viewer no longer attached")?;
    let tabs: Vec<_> = crate::navigation::tabs(world, v.workspace)
        .into_iter()
        .map(|tab| (tab, name(world, tab)))
        .collect();
    let hits = chrome::tab_bar(&mut out, v, &name(world, v.workspace), &tabs, &focused);
    view.presentation.chrome(hits);
    if let Some(overlay) = world.get::<crate::interaction::Overlay>(id) {
        chrome::surface(
            &mut out,
            v,
            &crate::interaction::lines(world, overlay, v.rows),
        )
    } else {
        chrome::panel_context(&mut out, v, world.resource::<Settings>(), |action| {
            crate::actions::unavailable(world, crate::actions::Target::viewer(v), action).is_some()
        })
    };
    if world.resource::<Settings>().clipboard == crate::assets::ClipboardPolicy::Disabled {
        view.clipboard.clear();
    }
    for text in view.clipboard.drain(..) {
        let _ = write!(
            out,
            "\x1b]52;c;{}\x07",
            base64::engine::general_purpose::STANDARD.encode(text)
        );
    }
    if !modal && let Some((x, y)) = cursor {
        at(&mut out, x, y, "\x1b[?25h");
    }
    out.push_str("\x1b[0m\x1b[?2026l");
    Ok(Frame {
        paint: out,
        detach: false,
    })
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
    if crate::navigation::handles(&event.action) {
        let action = event.action.clone();
        let target = event.target;
        let value = event.value.clone();
        commands.queue(move |world: &mut World| {
            let result = crate::navigation::control(world, id, &action, target, &value);
            if let Err(error) = result {
                notice(world, id, &error, true);
            }
        });
        wake.notify();
        return;
    }
    if event.action == "detach" {
        commands.entity(id).try_despawn();
        return;
    }
    let Ok(mut v) = viewers.get_mut(id) else {
        return;
    };
    v.notice.clear();
    v.notice_error = false;
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
        match event.action.as_str() {
            "split_horizontal" | "split_vertical" => {
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
                    node.flex_direction = if event.action == "split_vertical" {
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
            "terminate" => terminals
                .get_mut(pane.ok_or("no focused process")?)
                .map_err(|_| "terminal not found")?
                .stop()?,
            "focus_previous" => {
                v.focus = views
                    .get_mut(&id)
                    .ok_or("presentation not initialized")?
                    .presentation
                    .focus_step(true);
                v.scrollback = 0;
            }
            "focus_left" | "focus_right" | "focus_up" | "focus_down" => {
                let presentation = &views
                    .get(&id)
                    .ok_or("presentation not initialized")?
                    .presentation;
                let next = presentation.neighbor(
                    v.focus.ok_or("no visible focus")?,
                    event.action.rsplit('_').next().unwrap_or_default(),
                );
                if let Some(next) = next {
                    v.focus = Some(next);
                    v.scrollback = 0;
                }
            }
            "focus_next" => {
                v.focus = views
                    .get_mut(&id)
                    .ok_or("presentation not initialized")?
                    .presentation
                    .focus_next();
                v.scrollback = 0;
            }
            "focus" => {
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
            "zoom" => {
                let target = leaf.ok_or("no pane")?;
                v.zoom = v.focus != Some(target) || !v.zoom;
                v.focus = Some(target);
            }
            "scroll_up" => {
                v.scrollback = v.scrollback.saturating_add(usize::from(v.rows / 2).max(1))
            }
            "scroll_down" => {
                v.scrollback = v.scrollback.saturating_sub(usize::from(v.rows / 2).max(1))
            }
            "copy" => {
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
            "workspace_new" => {
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
            "grow_width" | "grow_height" | "shrink_width" | "shrink_height" => {
                let width = event.action.ends_with("width");
                let mut child = leaf.ok_or("no focused pane")?;
                while let Ok(parent) = parents.get(child) {
                    let parent = parent.parent();
                    let direction = nodes.get(parent).map_err(|e| e.to_string())?.flex_direction;
                    if width == matches!(direction, FlexDirection::Row | FlexDirection::RowReverse)
                    {
                        let mut node = nodes.get_mut(child).map_err(|e| e.to_string())?;
                        node.flex_grow = (node.flex_grow
                            + if event.action.starts_with("grow") {
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
            "reorder_prev" | "reorder_next" => {
                let leaf = leaf.ok_or("no focused pane")?;
                let parent = parents.get(leaf).map_err(|e| e.to_string())?.parent();
                let siblings = children.get(parent).map_err(|e| e.to_string())?;
                let index = siblings
                    .iter()
                    .position(|e| e == leaf)
                    .ok_or("missing child")?;
                let other = if event.action == "reorder_prev" {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(siblings.len() - 1)
                };
                commands
                    .entity(parent)
                    .entry::<Children>()
                    .and_modify(move |mut children| children.swap(index, other));
            }
            "save_layout" | "load_layout" => {
                let root = event
                    .target
                    .filter(|e| roots.contains(*e))
                    .unwrap_or(v.workspace);
                let path = event.value.clone();
                let save = event.action == "save_layout";
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
            "help" => {
                v.prompt = Some("help".into());
                v.help_scroll = 0;
                v.buffer.clear();
            }
            other => return Err(format!("unknown action {other}")),
        }
        Ok(())
    })();
    if let Err(error) = result {
        v.notice = error;
        v.notice_error = true;
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
                if v.prompt.is_some() {
                    if key == "escape" {
                        v.prompt = None;
                        v.buffer.clear();
                    } else if key == "enter" {
                        let action = v.prompt.take().ok_or("prompt disappeared")?;
                        let value = std::mem::take(&mut v.buffer);
                        commands.trigger(Control {
                            viewer: id,
                            action,
                            value,
                            target: None,
                            mapping: Vec::new(),
                        });
                    } else if key == "backspace" {
                        v.buffer.pop();
                    } else if key.chars().count() == 1 && !ctrl && !alt {
                        v.buffer.push_str(key);
                    }
                    return Ok(());
                }
                let token = format!(
                    "{}{}{}{}",
                    if *ctrl { "ctrl-" } else { "" },
                    if *alt { "alt-" } else { "" },
                    if *shift && key.chars().count() != 1 {
                        "shift-"
                    } else {
                        ""
                    },
                    key
                );
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
                        v.notice = format!("unbound prefix key {token}");
                        v.notice_error = false;
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
                terminal.input(&key_bytes(key, *ctrl, *alt, *shift, application)?)?;
                v.scrollback = 0;
                v.notice.clear();
            }
            Input::Paste { text } => {
                if v.prefix {
                    return Ok(());
                }
                if let Some(prompt) = &v.prompt {
                    if prompt != "help" {
                        v.buffer.push_str(text);
                    }
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
                if v.prefix || v.prompt.is_some() {
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
                                    "scroll_up"
                                } else {
                                    "scroll_down"
                                }
                                .into(),
                                value: String::new(),
                                target: None,
                                mapping: Vec::new(),
                            });
                        }
                    } else if *x >= rect.x
                        && *x < rect.x + rect.width
                        && *y >= rect.y
                        && *y < rect.y + rect.height
                    {
                        let release = action == "release";
                        let motion = action == "move";
                        if release && mode == MouseMode::Press
                            || motion
                                && (matches!(mode, MouseMode::Press | MouseMode::PressRelease)
                                    || mode == MouseMode::ButtonMotion && *button == 3)
                        {
                            return Ok(());
                        }
                        let mut code = if action == "scrollup" {
                            64
                        } else if action == "scrolldown" {
                            65
                        } else {
                            u16::from((*button).min(3))
                        };
                        if motion {
                            code += 32;
                        }
                        if *shift {
                            code += 4;
                        }
                        if *alt {
                            code += 8;
                        }
                        if *ctrl {
                            code += 16;
                        }
                        let col = x - rect.x + 1;
                        let row = y - rect.y + 1;
                        let (rows, cols) = screen.size();
                        if row > rows || col > cols {
                            return Ok(());
                        }
                        if screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
                            terminal.input(
                                format!(
                                    "\x1b[<{code};{col};{row}{}",
                                    if release { 'm' } else { 'M' }
                                )
                                .as_bytes(),
                            )?;
                        } else if col <= 223 && row <= 223 {
                            terminal.input(&[
                                27,
                                b'[',
                                b'M',
                                (if release { 3 } else { code }) as u8 + 32,
                                col as u8 + 32,
                                row as u8 + 32,
                            ])?;
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        v.notice = error;
        v.notice_error = true;
    }
    wake.notify();
}

fn notice(world: &mut World, id: Entity, message: &str, error: bool) {
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.notice = message.to_owned();
        v.notice_error = error;
    }
}
fn key_bytes(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    application: bool,
) -> Result<Vec<u8>, String> {
    let modifier = 1 + usize::from(shift) + 2 * usize::from(alt) + 4 * usize::from(ctrl);
    let csi = |code, final_byte| {
        if modifier > 1 {
            format!("\x1b[{code};{modifier}{final_byte}")
        } else {
            format!("\x1b[{code}{final_byte}")
        }
        .into_bytes()
    };
    let function = key.strip_prefix('f').and_then(|n| n.parse::<usize>().ok());
    let cursor = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => function
            .filter(|n| (1..=4).contains(n))
            .map(|n| char::from(b'P' + n as u8 - 1)),
    };
    if let Some(final_byte) = cursor {
        return Ok(if modifier > 1 {
            csi(1, final_byte)
        } else {
            let prefix = if application || function.is_some() {
                'O'
            } else {
                '['
            };
            format!("\x1b{prefix}{final_byte}").into_bytes()
        });
    }
    let mut bytes = match key {
        "enter" => vec![13],
        "tab" if shift => b"\x1b[Z".to_vec(),
        "tab" => vec![9],
        "escape" => vec![27],
        "backspace" => vec![127],
        "insert" | "delete" | "pageup" | "pagedown" => {
            let code = match key {
                "insert" => 2,
                "delete" => 3,
                "pageup" => 5,
                _ => 6,
            };
            csi(code, '~')
        }
        _ if let Some(n) = function => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            let code = codes
                .get(n.wrapping_sub(5))
                .ok_or("unsupported function key")?;
            csi(*code, '~')
        }
        _ if key.chars().count() == 1 => {
            let c = key.chars().next().ok_or("empty key")?;
            if ctrl && c.is_ascii() {
                vec![(c.to_ascii_uppercase() as u8) & 0x1f]
            } else {
                key.as_bytes().to_vec()
            }
        }
        _ => return Err(format!("unsupported key {key}")),
    };
    if alt && !bytes.starts_with(&[27]) {
        bytes.insert(0, 27);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Component, bevy_reflect::Reflect)]
    #[reflect(Component)]
    struct Extra(u32);

    #[test]
    fn arbitrary_layout_changes_invalidate_only_the_owning_workspace() {
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
        let untouched = scene(app.world_mut(), right).unwrap().1;
        let initial = scene(app.world_mut(), left).unwrap().1;
        app.update();
        assert!(Arc::ptr_eq(
            &initial,
            &scene(app.world_mut(), left).unwrap().1
        ));
        // The descendant deliberately has no Node. Newly inserted, previously
        // absent types and ordinary in-place writes must still reach the scene.
        app.world_mut().entity_mut(leaf).insert(Extra(7));
        // A synchronous control can observe this mutation before another Update.
        app.world_mut()
            .run_system_cached(invalidate_layouts)
            .unwrap();
        let inserted = scene(app.world_mut(), left).unwrap().1;
        assert!(!Arc::ptr_eq(&initial, &inserted));
        assert!(Arc::ptr_eq(
            &untouched,
            &scene(app.world_mut(), right).unwrap().1
        ));
        app.world_mut().get_mut::<Extra>(leaf).unwrap().0 = 9;
        app.update();
        let modified = scene(app.world_mut(), left).unwrap().1;
        assert!(!Arc::ptr_eq(&inserted, &modified));
        app.world_mut().entity_mut(leaf).remove::<Extra>();
        app.update();
        let removed = scene(app.world_mut(), left).unwrap().1;
        assert!(!Arc::ptr_eq(&modified, &removed));
        assert!(Arc::ptr_eq(
            &untouched,
            &scene(app.world_mut(), right).unwrap().1
        ));
        app.world_mut().entity_mut(nested).insert(ChildOf(right));
        app.update();
        let emptied = scene(app.world_mut(), left).unwrap().1;
        let moved = scene(app.world_mut(), right).unwrap().1;
        assert!(!Arc::ptr_eq(&removed, &emptied));
        assert!(!Arc::ptr_eq(&untouched, &moved));
        assert!(!emptied.entities.iter().any(|entity| entity.entity == leaf));
        assert!(moved.entities.iter().any(|entity| entity.entity == leaf));
        app.world_mut().despawn(nested);
        app.update();
        let despawned = scene(app.world_mut(), right).unwrap().1;
        assert!(
            !despawned
                .entities
                .iter()
                .any(|entity| entity.entity == leaf)
        );
        assert!(Arc::ptr_eq(
            &emptied,
            &scene(app.world_mut(), left).unwrap().1
        ));
        app.world_mut().entity_mut(right).remove::<Workspace>();
        assert!(scene(app.world_mut(), right).is_err());
        app.world_mut().despawn(right);
        let replacement = app.world_mut().spawn(Workspace).id();
        app.update();
        assert_eq!(
            scene(app.world_mut(), replacement)
                .unwrap()
                .1
                .entities
                .len(),
            1
        );
    }

    #[test]
    fn modified_keys_preserve_xterm_protocol_semantics() {
        for (key, plain, modified) in [
            ("f1", "\x1bOP", "\x1b[1;8P"),
            ("f4", "\x1bOS", "\x1b[1;8S"),
            ("f5", "\x1b[15~", "\x1b[15;8~"),
            ("insert", "\x1b[2~", "\x1b[2;8~"),
            ("home", "\x1b[H", "\x1b[1;8H"),
        ] {
            assert_eq!(
                key_bytes(key, false, false, false, false).unwrap(),
                plain.as_bytes()
            );
            assert_eq!(
                key_bytes(key, true, true, true, false).unwrap(),
                modified.as_bytes()
            );
        }
        for key in ["f0", "f13", "f999", "fno", ""] {
            assert!(key_bytes(key, false, false, false, false).is_err());
        }
        assert_eq!(
            key_bytes("left", false, false, false, true).unwrap(),
            b"\x1bOD"
        );
        assert_eq!(
            key_bytes("left", true, false, false, true).unwrap(),
            b"\x1b[1;5D"
        );
        assert_eq!(
            key_bytes("f1", true, false, true, false).unwrap(),
            b"\x1b[1;6P"
        );
        assert_eq!(
            key_bytes("f12", false, true, false, false).unwrap(),
            b"\x1b[24;3~"
        );
    }
}
