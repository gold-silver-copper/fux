//! The viewer (prompt 3.11): a second Bevy `App` with the same stack minus PTYs. Its World holds
//! the replicated instance scene plus viewer-local chrome; presentation focus lives here and only
//! here (`bevy_input_focus`); painting is one pass over the local `UiStack` into termina.
//!
//! The World holds no OS handle: [`run`] owns the terminal and the socket, blocks on one wake
//! channel (terminal events, server frames, the next timer deadline), fills [`Inbox`], steps the
//! App once, then writes the painter's bytes to the terminal and the [`Outbox`] to the server.

pub mod chrome;
pub mod connection;
pub mod focus;
pub mod input;
pub mod keys;
pub mod paint;
pub mod replicate;
pub mod terminal_io;
pub mod theme;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use bevy_app::prelude::*;
use bevy_asset::AssetPlugin;
use bevy_asset::io::{AssetSourceBuilders, AssetSourceId};
use bevy_camera::{Camera, RenderTarget, RenderTargetInfo};
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_input_focus::directional_navigation::DirectionalNavigationPlugin;
use bevy_input_focus::tab_navigation::{TabGroup, TabIndex, TabNavigationPlugin};
use bevy_input_focus::{InputDispatchPlugin, InputFocusPlugin};
use bevy_math::UVec2;
use bevy_picking::pointer::PointerId;
use bevy_state::prelude::*;
use bevy_window::{PrimaryWindow, Window, WindowResolution};

use crate::assets;
use crate::model::{
    Ids, InstanceNode, NodeId, PaneId, ShownBy, Shows, Surface, ViewerRequest, Zoomed,
};
use crate::wire::{ClientFrame, ExactTargetSpec, ServerFrame};

pub use chrome::{ChromeRoots, Text};
pub use paint::{Painter, Screen, ScreenCell};
pub use replicate::{Grid, Replicated, Roots, ShowingRoot, TargetPane};

/// How `fux attach` runs the viewer.
#[derive(Clone, Debug)]
pub struct ViewerOptions {
    pub brp_path: PathBuf,
    pub workspace: String,
    pub exact: Option<ExactTargetSpec>,
    /// Where `fux.toml` lives; the viewer loads and watches it for its theme, bindings and
    /// clipboard policy.
    pub config_dir: PathBuf,
}

/// What wakes the runner.
#[derive(Debug)]
pub enum Wake {
    Terminal(termina::Event),
    TerminalClosed,
    Frame(ServerFrame),
    Disconnected(String),
    /// `fux.toml` changed or finished loading: step once so the asset systems run.
    Asset,
}

/// What the runner delivers to one update: terminal events and server frames, in arrival order.
#[derive(Resource, Default, Debug)]
pub struct Inbox {
    pub events: Vec<termina::Event>,
    pub frames: Vec<ServerFrame>,
}

/// Frames to send after the update.
#[derive(Resource, Default, Debug)]
pub struct Outbox(pub Vec<ClientFrame>);

impl Outbox {
    pub fn push(&mut self, request: ViewerRequest) {
        self.0.push(ClientFrame::Request { request });
    }
}

/// Set when the viewer should stop: exit code and a message printed after the terminal is
/// restored.
#[derive(Resource, Debug, Clone)]
pub struct Exit {
    pub code: i32,
    pub message: Option<String>,
}

/// The outer terminal size in cells; the pane area is one row shorter (status bar).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub cols: u16,
    pub rows: u16,
}

impl Viewport {
    /// The part of the terminal the server lays out into.
    pub fn pane_area(self) -> crate::model::Viewport {
        crate::model::Viewport {
            rows: self.rows.saturating_sub(1).max(1),
            cols: self.cols.max(1),
        }
    }
}

/// The layout camera every node lays out against.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalCamera(pub Entity);

/// The `PrimaryWindow` entity.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalWindow(pub Entity);

/// The latest deadline a chrome timer needs the runner to wake for.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct WakeDeadline(pub Option<Duration>);

/// Command mode (prompt 3.11: modes are `bevy_state` states).
#[derive(States, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    #[default]
    Normal,
    /// The prefix was pressed; the next chord is a binding.
    Prefix,
    /// A destructive binding awaits `y`.
    Confirm,
    CopyMode,
}

#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewerSystems {
    /// `First`: inbox → World.
    Ingest,
    /// `Update`: focus follows the server's target.
    Focus,
    /// `Update`, after focus: chrome projections.
    Chrome,
}

/// The translator holding the last mouse cell.
#[derive(Resource)]
struct TerminalInput(input::Translator);

/// Builds the viewer App for a `cols`×`rows` terminal with no terminal or socket attached and
/// no configuration directory (built-in theme and bindings); tests drive [`Inbox`] directly.
pub fn build(cols: u16, rows: u16) -> App {
    build_in(
        cols,
        rows,
        Path::new(&AssetPlugin::default().file_path),
        Arc::new(|| {}),
    )
}

/// [`build`] over a configuration directory: `fux.toml` there is loaded and watched, and
/// `wake` is called whenever the watcher or a load has something for the next update.
pub fn build_in(cols: u16, rows: u16, config_dir: &Path, wake: assets::Wake) -> App {
    let mut app = App::new();
    let root = assets::asset_root(config_dir);
    app.world_mut()
        .get_resource_or_init::<AssetSourceBuilders>()
        .insert(AssetSourceId::Default, assets::waking_source(&root, wake));
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_state::app::StatesPlugin,
        bevy_time::TimePlugin,
        AssetPlugin {
            file_path: root,
            ..Default::default()
        },
        assets::ConfigAssetPlugin,
    ));
    crate::layout::add_ui_stack(&mut app);
    app.add_plugins((
        InputFocusPlugin,
        InputDispatchPlugin,
        TabNavigationPlugin,
        DirectionalNavigationPlugin,
    ));
    app.register_type::<PaneId>()
        .register_type::<NodeId>()
        .register_type::<Shows>()
        .register_type::<ShownBy>()
        .register_type::<InstanceNode>()
        .register_type::<Surface>()
        .register_type::<Zoomed>()
        .register_type::<bevy_ui::Node>()
        .register_type::<bevy_ui::ComputedNode>()
        .register_type::<bevy_ui::ZIndex>()
        .register_type::<bevy_ui::GlobalZIndex>()
        .register_type::<bevy_ui::BackgroundColor>()
        .register_type::<bevy_ui::BorderColor>()
        .register_type::<bevy_ui::ScrollPosition>()
        .register_type::<Name>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<TabIndex>()
        .register_type::<TabGroup>();
    app.init_resource::<Ids>()
        .init_resource::<Inbox>()
        .init_resource::<Outbox>()
        .init_resource::<WakeDeadline>()
        .insert_resource(Viewport { cols, rows })
        .init_state::<Mode>()
        .configure_sets(
            Update,
            (ViewerSystems::Focus, ViewerSystems::Chrome).chain(),
        )
        .add_systems(First, drain_terminal.in_set(ViewerSystems::Ingest))
        .configure_sets(
            First,
            ViewerSystems::Ingest.after(bevy_ecs::message::MessageUpdateSystems),
        );

    let world = app.world_mut();
    let window = world
        .spawn((
            Window {
                resolution: WindowResolution::new(u32::from(cols), u32::from(rows))
                    .with_scale_factor_override(1.0),
                ..Default::default()
            },
            PrimaryWindow,
        ))
        .id();
    let camera = spawn_camera(world, cols, rows);
    world.insert_resource(LocalWindow(window));
    world.insert_resource(LocalCamera(camera));
    world.insert_resource(TerminalInput(input::Translator::new(window, cols, rows)));
    world.spawn(PointerId::Mouse);
    let chrome = chrome::spawn_chrome(world);
    world.insert_resource(chrome);
    world.insert_resource(Painter::new(cols, rows));

    app.add_plugins((
        replicate::ReplicatePlugin,
        focus::FocusPlugin,
        chrome::ChromePlugin,
        theme::ThemePlugin,
        paint::PaintPlugin,
    ));
    app
}

fn spawn_camera(world: &mut World, cols: u16, rows: u16) -> Entity {
    let size = UVec2::new(u32::from(cols), u32::from(rows));
    let mut camera = Camera::default();
    camera.computed.target_info = Some(RenderTargetInfo {
        physical_size: size,
        scale_factor: 1.0,
    });
    world.spawn((camera, RenderTarget::None { size })).id()
}

/// Resizes the window, the camera target and the painter to a new terminal size.
pub fn resize(world: &mut World, cols: u16, rows: u16) {
    let previous = *world.resource::<Viewport>();
    let viewport = Viewport { cols, rows };
    if previous == viewport {
        return;
    }
    world.insert_resource(viewport);
    let size = UVec2::new(u32::from(cols), u32::from(rows));
    let camera = world.resource::<LocalCamera>().0;
    if let Some(mut camera) = world.get_mut::<Camera>(camera) {
        camera.computed.target_info = Some(RenderTargetInfo {
            physical_size: size,
            scale_factor: 1.0,
        });
    }
    if let Some(mut target) = world.get_mut::<RenderTarget>(camera) {
        *target = RenderTarget::None { size };
    }
    let window = world.resource::<LocalWindow>().0;
    if let Some(mut window) = world.get_mut::<Window>(window) {
        window
            .resolution
            .set_physical_resolution(u32::from(cols), u32::from(rows));
    }
    world.resource_mut::<Painter>().resize(cols, rows);
    let mut translator = world.resource_mut::<TerminalInput>();
    translator.0.cols = cols;
    translator.0.rows = rows;
    if previous.pane_area() != viewport.pane_area() {
        let area = viewport.pane_area();
        world.resource_mut::<Outbox>().push(ViewerRequest::Resize {
            rows: area.rows,
            cols: area.cols,
        });
    }
}

/// `First`: terminal events → `bevy_input` messages, pointer input, paste, resize; mouse events
/// inside the pane area are forwarded to the server, which owns pane mouse policy.
fn drain_terminal(world: &mut World) {
    let events = core::mem::take(&mut world.resource_mut::<Inbox>().events);
    for event in &events {
        let translated = world.resource_mut::<TerminalInput>().0.translate(event);
        let Some(translated) = translated else {
            continue;
        };
        match translated {
            input::Input::Key(press) => {
                let mut release = press.clone();
                release.state = bevy_input::ButtonState::Released;
                world.write_message(press);
                world.write_message(release);
            }
            input::Input::Mouse(mouse) => {
                if let Some(button) = mouse.button {
                    world.write_message(button);
                }
                if let Some(motion) = mouse.motion {
                    world.write_message(motion);
                }
                if let Some(wheel) = mouse.wheel {
                    world.write_message(wheel);
                }
                if let Some(pointer) = mouse.pointer_move {
                    world.write_message(pointer);
                }
                if let Some(pointer) = mouse.pointer_action {
                    world.write_message(pointer);
                }
                let area = world.resource::<Viewport>().pane_area();
                let normal = *world.resource::<State<Mode>>().get() == Mode::Normal;
                if normal && mouse.event.row < area.rows && mouse.event.col < area.cols {
                    world
                        .resource_mut::<Outbox>()
                        .push(ViewerRequest::Pointer(mouse.event));
                }
            }
            input::Input::Paste(ime) => {
                world.write_message(ime);
            }
            input::Input::Resize { cols, rows } => resize(world, cols, rows),
            input::Input::Focus(_) => {}
        }
    }
}

/// Runs the viewer against the real terminal and the server named by `brp.json`; returns the
/// process exit code after the terminal is restored.
pub fn run(opts: ViewerOptions) -> Result<i32, BevyError> {
    let mut term = terminal_io::TerminalIo::open()?;
    let (cols, rows) = term.size()?;
    let (wake_tx, wake_rx) = mpsc::channel();
    let viewport = Viewport { cols, rows };
    let mut connection = match connection::Connection::connect(
        &opts.brp_path,
        &opts.workspace,
        viewport.pane_area(),
        opts.exact.clone(),
        wake_tx.clone(),
    ) {
        Ok(connection) => connection,
        Err(e) => {
            term.restore()?;
            return Err(e.into());
        }
    };
    let asset_wake = wake_tx.clone();
    let _reader = term.spawn_reader(wake_tx)?;

    let mut app = build_in(
        cols,
        rows,
        &opts.config_dir,
        Arc::new(move || {
            let _ = asset_wake.send(Wake::Asset);
        }),
    );
    if opts.exact.is_some() {
        app.world_mut().insert_resource(focus::ExactAttachment);
    }
    app.finish();
    app.cleanup();

    let exit = loop {
        // Block until something happens or the next chrome deadline.
        let deadline = app.world().resource::<WakeDeadline>().0;
        let first = match deadline {
            Some(d) => match wake_rx.recv_timeout(d) {
                Ok(wake) => Some(wake),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    break Exit {
                        code: 1,
                        message: Some("fux: input sources closed".into()),
                    };
                }
            },
            None => match wake_rx.recv() {
                Ok(wake) => Some(wake),
                Err(_) => {
                    break Exit {
                        code: 1,
                        message: Some("fux: input sources closed".into()),
                    };
                }
            },
        };
        // A disconnect is only fatal once every frame that preceded it (a `Bye`, typically) has
        // been applied by an update: the batch stops at the disconnect and it is re-checked below.
        let mut fatal = None;
        {
            let mut inbox = app.world_mut().resource_mut::<Inbox>();
            let mut count = 0;
            let mut pending = first;
            while let Some(wake) = pending.take() {
                match wake {
                    Wake::Terminal(event) => inbox.events.push(event),
                    Wake::Frame(frame) => inbox.frames.push(frame),
                    Wake::Asset => {}
                    Wake::TerminalClosed => {
                        fatal = Some(Exit {
                            code: 1,
                            message: Some("fux: terminal closed".into()),
                        });
                        break;
                    }
                    Wake::Disconnected(why) => {
                        fatal = Some(Exit {
                            code: 1,
                            message: Some(format!("fux: {why}")),
                        });
                        break;
                    }
                }
                count += 1;
                if count < MAX_BATCH {
                    pending = wake_rx.try_recv().ok();
                }
            }
        }
        if app.world().resource::<Inbox>().frames.is_empty()
            && let Some(exit) = fatal.take()
        {
            break exit;
        }
        let deferred_exit = fatal;
        app.update();
        let world = app.world_mut();
        let out = core::mem::take(&mut world.resource_mut::<Painter>().out);
        if !out.is_empty() {
            term.write_all(&out)?;
        }
        // Hand the buffer back so its capacity is reused.
        world.resource_mut::<Painter>().out = out;
        let frames = core::mem::take(&mut world.resource_mut::<Outbox>().0);
        for frame in &frames {
            if let Err(e) = connection.send(frame) {
                world.insert_resource(Exit {
                    code: 1,
                    message: Some(format!("fux: {e}")),
                });
                break;
            }
        }
        if let Some(exit) = world.get_resource::<Exit>() {
            break exit.clone();
        }
        if let Some(exit) = deferred_exit {
            break exit;
        }
    };
    connection.close();
    term.restore()?;
    if let Some(message) = exit.message {
        // The viewer's only stdout use: the farewell after the terminal is back to normal.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("{message}");
        }
    }
    Ok(exit.code)
}

/// Wake messages folded into one update.
const MAX_BATCH: usize = 1024;
