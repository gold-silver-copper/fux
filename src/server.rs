//! The server plugin: the reflected types BRP can name, the observers and
//! systems that run the session, startup's first workspace, and detaching a
//! viewer whose frame watch closed. Commands, input routing, layouts and the
//! method registry live in `execute`, `routing`, `layout` and `remote`.
#[cfg(test)]
mod tests;
use crate::{
    actions::Action,
    assets::{self, Settings},
    control::{Axis, Chooser, Command, Control, Order, Scope, Shutdown, Subject, UserInput},
    layout::{collapse_layout, invalidate_layouts, reload_layouts, scene_completions},
    model::*,
    presentation::{self, Presentation},
    protocol::Input,
    routing::{route_control, route_input},
    terminal::TerminalPlugin,
};
use bevy_app::{App, AppExit, Plugin, PostUpdate, Update};
use bevy_ecs::prelude::*;

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
