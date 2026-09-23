use crate::{
    actions::Action,
    assets::{self, Settings},
    control::{Axis, Chooser, Command, Control, Order, Scope, Shutdown, Subject, UserInput},
    execute::execute,
    frame::{self, sync_view},
    interaction::Prefix,
    layout::{collapse_layout, invalidate_layouts, reload_layouts, scene_completions},
    model::*,
    presentation::{self, Presentation},
    protocol::{Input, MouseAction, MouseButton, Token},
    terminal::{Terminal, TerminalPlugin},
};
use bevy_app::{App, AppExit, Plugin, PostUpdate, Update};
use bevy_ecs::prelude::*;

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
                        Command::Select {
                            scope: Scope::Tab,
                            entity: hit,
                        }
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
                                t.screen().mouse_protocol_mode() == fux_vt::MouseProtocolMode::None
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
                        t.screen().mouse_protocol_mode() == fux_vt::MouseProtocolMode::None
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

pub struct ServerPlugin;
impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        // A command that fails reports it; it does not end the process. Bevy's
        // default routes a failed command to `panic`, and this server owns real
        // PTYs and other people's sessions behind an API that exposes the whole
        // ECS to any same-user caller, so one bad request could end every
        // session at once. That is the same reason this crate forbids `unwrap`,
        // `expect` and `panic!` in its own code: fux degrades and says what went
        // wrong rather than stopping. The error is logged, not swallowed.
        //
        // This covers errors that reach the handler. A direct `panic!` does not
        // pass through it, which is why hunt 6's finding 005 had to be fixed
        // where it was raised rather than contained here.
        app.insert_resource(bevy_ecs::error::FallbackErrorHandler(
            bevy_ecs::error::error,
        ))
        .register_type::<Workspace>()
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
        .register_type::<crate::actions::Target>()
        .register_type::<crate::interaction::Prefix>()
        .register_type::<crate::interaction::Overlay>()
        .register_type::<crate::interaction::Mode>()
        .register_type::<crate::interaction::Entry>()
        .register_type::<crate::interaction::Run>()
        .register_type::<Command>()
        .register_type::<Subject>()
        .register_type::<Axis>()
        .register_type::<Order>()
        .register_type::<Chooser>()
        .register_type::<Scope>()
        .register_type::<crate::interaction::MoveTo>()
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
            .add_observer(|removed: On<Remove<Viewer>>, mut commands: Commands| {
                commands.entity(removed.entity).try_remove::<Presentation>();
            })
            .add_observer(|_: On<Shutdown>, mut exits: MessageWriter<AppExit>| {
                exits.write(AppExit::Success);
            })
            .add_systems(
                PostUpdate,
                initialize.after(assets::SettingsApplied).run_if(
                    assets::initial_settings_settled
                        .and_then(bevy_ecs::schedule::common_conditions::run_once),
                ),
            )
            // Do not advertise BRP readiness or accept attach before the initial
            // workspace exists. Subsequent reloads leave this gate open.
            .configure_sets(
                bevy_remote::RemoteLast,
                bevy_remote::RemoteSystems::ProcessRequests
                    .run_if(assets::initial_settings_settled),
            )
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
                settle_remote_requests
                    .before(bevy_remote::RemoteSystems::ProcessRequests)
                    .run_if(assets::initial_settings_settled),
            );
    }
}

/// Creates the initial workspace, tab and configured shell. Runs once at
/// startup and again if an attach finds no workspace left to join.
pub(crate) fn initialize(world: &mut World) {
    let settings = world.resource::<Settings>().clone();
    let root = workspace(world, "main");
    let tab = world.spawn((Tab, Name::new("main"), ChildOf(root))).id();
    if let Err(error) = spawn_pane(world, &settings, tab, None, None) {
        bevy_log::error!("initial terminal: {error}");
    }
    // Launch is created after Update; settle its native PTY lifecycle next turn,
    // including the missing/invalid-config fallback without another request.
    world.resource::<Wake>().notify();
}

pub(crate) fn workspace(world: &mut World, name: &str) -> Entity {
    world
        .spawn((Workspace, WorkspaceOrder(0), Name::new(name.to_owned())))
        .id()
}
pub(crate) fn spawn_pane(
    world: &mut World,
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
    let pane = world.spawn((launch, Name::new(name))).id();
    Ok(world.spawn((PaneView { pane }, ChildOf(parent))).id())
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

/// A closed `fux.frame+watch` connection detaches the viewer it was streaming.
/// The entity came from that request's params, so it names whatever the caller
/// chose; this despawns it only if it is in fact a viewer. The check belongs
/// here rather than only where the watch was registered, because the world can
/// change in between: the id may have been despawned and its index reused by an
/// entity of another kind.
fn disconnected(mut commands: Commands, closed: Res<Disconnected>, viewers: Query<(), IsViewer>) {
    while let Ok(entity) = closed.0.try_recv() {
        if viewers.contains(entity) {
            commands.entity(entity).try_despawn();
        } else {
            bevy_log::debug!(
                "A frame-watch connection for entity {entity} closed, but that entity is not a viewer; nothing was detached."
            );
        }
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
                terminal.input(&crate::paste::bracketed(text))?;
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
            use fux_vt::MouseProtocolMode as MouseMode;
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

    #[derive(Component, bevy_reflect::Reflect)]
    #[reflect(Component)]
    struct Extra(u32);

    /// A command that fails must report it and leave the server running.
    /// Bevy's default routes a failed command to `panic`, which would let one
    /// request end every session; `ServerPlugin` replaces that with logging.
    #[test]
    fn a_failing_command_is_reported_rather_than_fatal() -> crate::testing::Outcome {
        let mut app = App::new();
        app.insert_resource(Wake(std::thread::current()));
        app.add_plugins((
            bevy_app::TaskPoolPlugin::default(),
            bevy_asset::AssetPlugin::default(),
            ServerPlugin,
        ));
        let configured: bevy_ecs::error::ErrorHandler = app
            .world()
            .resource::<bevy_ecs::error::FallbackErrorHandler>()
            .0;
        assert!(
            std::ptr::fn_addr_eq(
                configured,
                bevy_ecs::error::error as bevy_ecs::error::ErrorHandler
            ),
            "a failed command must be logged, not a panic"
        );
        // A command against an entity that is gone is the ordinary shape of
        // this: it fails, it is reported, and the next update still runs.
        let gone = app.world_mut().spawn_empty().id();
        app.world_mut().despawn(gone);
        app.world_mut().commands().entity(gone).insert(Extra(1));
        app.update();
        app.update();
        assert!(app.world().get_entity(gone).is_err());
        Ok(())
    }

    /// A BRP insert builds the component through `from_reflect_with_fallback`,
    /// which panics on a partial payload unless the registration carries a
    /// serde `Deserialize` (rejects it), a `Default` or a `FromWorld`. Every
    /// reflected component must carry one, or one request kills the server.
    #[test]
    fn every_reflected_component_survives_a_partial_payload() -> crate::testing::Outcome {
        use bevy_ecs::reflect::{ReflectComponent, ReflectFromWorld};
        use bevy_reflect::{ReflectDeserialize, std_traits::ReflectDefault};
        let mut app = App::new();
        app.insert_resource(Wake(std::thread::current()));
        app.add_plugins((
            bevy_app::TaskPoolPlugin::default(),
            bevy_asset::AssetPlugin::default(),
            ServerPlugin,
        ));
        let registry = app.world().resource::<AppTypeRegistry>().read();
        let unguarded: Vec<&str> = registry
            .iter()
            .filter(|registration| registration.data::<ReflectComponent>().is_some())
            .filter(|registration| {
                registration.data::<ReflectDeserialize>().is_none()
                    && registration.data::<ReflectDefault>().is_none()
                    && registration.data::<ReflectFromWorld>().is_none()
            })
            .map(|registration| registration.type_info().type_path())
            .collect();
        assert!(
            unguarded.is_empty(),
            "reflected components without Deserialize, Default or FromWorld: {unguarded:?}"
        );
        Ok(())
    }

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
}
