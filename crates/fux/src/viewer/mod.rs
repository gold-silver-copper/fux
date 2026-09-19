//! The viewer (prompt 3.11): a second Bevy `App` with the same stack minus PTYs. Its World holds
//! the replicated instance scene plus viewer-local chrome; presentation focus lives here and only
//! here (`bevy_input_focus`); painting is one pass over the local `UiStack` into termina.
//!
//! The World holds no OS handle: [`run`] owns the terminal and the socket, blocks on one wake
//! channel (terminal events, server frames, the next timer deadline), fills [`Inbox`], steps the
//! App once, then writes the painter's bytes to the terminal and the [`Outbox`] to the server.

pub mod choosers;
pub mod chrome;
pub mod connection;
pub mod copy_mode;
pub mod focus;
pub mod input;
pub mod keys;
pub mod paint;
pub mod prompts;
pub mod replicate;
pub mod terminal_io;
pub mod theme;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use serde_json::Value;

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
use crate::remote::client;
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
    Terminal {
        event: termina::Event,
        revision: u64,
    },
    TerminalClosed,
    Frame(ServerFrame),
    Disconnected(String),
    /// `fux.toml` changed or finished loading, or a BRP worker delivered a reply: step once so
    /// the asset systems and [`BrpReply`] readers run.
    Ready,
}

/// What the runner delivers to one update: terminal events and server frames, in arrival order.
#[derive(Resource, Default, Debug)]
pub struct Inbox {
    pub events: Vec<termina::Event>,
    pub frames: Vec<ServerFrame>,
    /// Minimum original paint revision of the admitted terminal batch.
    pub input_revision: Option<u64>,
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
    /// A destructive binding awaits `y` in a confirmation popover.
    Confirm,
    CopyMode,
    /// A tab, workspace or help list owns the keys.
    Chooser,
    /// A text prompt owns the keys.
    Prompt,
}

/// "A modal is open": a viewer-local popup owns presentation focus, so the server's target is
/// not re-imposed until it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Modal;

impl ComputedStates for Modal {
    type SourceStates = Mode;

    fn compute(mode: Mode) -> Option<Self> {
        matches!(mode, Mode::Confirm | Mode::Chooser | Mode::Prompt).then_some(Self)
    }
}

/// A BRP call the viewer made through a worker thread, identified for its reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrpTag {
    WorkspaceList,
    WorkspaceNew(String),
    WorkspaceKill(String),
    RootRename(NodeId),
    Capture(PaneId),
    PluginAction(String),
}

/// A worker's reply, delivered as a message on the update after it arrived.
#[derive(Message, Debug)]
pub struct BrpReply {
    pub tag: BrpTag,
    pub result: Result<Value, String>,
}

/// The viewer's BRP client (prompt 3.11: prompts and choosers commit through
/// `remote::client::call`): every call runs on its own worker thread so the update never
/// blocks on the server; the reply wakes the runner and is read as a [`BrpReply`].
#[derive(Resource)]
pub struct Brp {
    path: PathBuf,
    tx: async_channel::Sender<(BrpReply, Option<PendingCall>)>,
    rx: async_channel::Receiver<(BrpReply, Option<PendingCall>)>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
    wake: assets::Wake,
}

struct PendingCall(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PendingCall {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

impl Brp {
    /// A client over `path`; `wake` is called when a reply is ready to be read.
    pub fn new(path: PathBuf, wake: assets::Wake) -> Self {
        let (tx, rx) = async_channel::bounded(32);
        Self {
            path,
            tx,
            rx,
            wake,
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// The `brp.json` calls read (empty when the viewer is headless).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Calls `method` on a worker thread; the reply arrives as a [`BrpReply`] tagged `tag`.
    /// Without a descriptor (headless) nothing is called and no reply comes.
    pub fn call(&self, tag: BrpTag, method: &'static str, params: Value) {
        if self.path.as_os_str().is_empty() {
            bevy_log::debug!("no brp.json: {method} not called");
            return;
        }
        if let Err(error) = self.call_at(self.path.clone(), tag, method, params) {
            bevy_log::warn!("BRP worker: {error}");
        }
    }

    fn call_at(
        &self,
        path: PathBuf,
        tag: BrpTag,
        method: &'static str,
        params: Value,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |count| {
                (count < 32).then_some(count + 1)
            })
            .map_err(|_| "viewer has 32 outstanding BRP calls".to_owned())?;
        let permit = PendingCall(Arc::clone(&self.pending));
        let tx = self.tx.clone();
        let wake = Arc::clone(&self.wake);
        std::thread::Builder::new()
            .name("fux-viewer-brp".into())
            .spawn(move || {
                let result = client::call(&path, method, params).map_err(|e| e.to_string());
                if tx
                    .try_send((BrpReply { tag, result }, Some(permit)))
                    .is_ok()
                {
                    wake();
                }
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Delivers a reply as if a worker had produced it (tests).
    pub fn deliver(&self, reply: BrpReply) {
        let _ = self.tx.try_send((reply, None));
    }
}

/// `First`: worker replies → [`BrpReply`] messages.
fn drain_brp(brp: Res<Brp>, mut replies: MessageWriter<BrpReply>) {
    while let Ok((reply, _permit)) = brp.rx.try_recv() {
        replies.write(reply);
    }
}

/// Set by the workspace chooser or a created workspace: the runner closes the attachment and
/// sends a new `Hello` for this workspace (no `ViewerRequest` is involved).
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Reconnect(pub String);

/// Test hook: `FUX_VIEWER_PANIC_AT=<n>` panics inside an `Update` system on the nth update, so
/// the pseudo-terminal tests can prove the panic hook restores the outer terminal.
#[derive(Resource, Debug)]
struct PanicAt(u32);

fn panic_at(mut hook: ResMut<PanicAt>) {
    hook.0 = hook.0.saturating_sub(1);
    assert!(
        hook.0 > 0,
        "FUX_VIEWER_PANIC_AT: deliberate panic inside a viewer system"
    );
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
        .insert(
            AssetSourceId::Default,
            assets::waking_source(&root, Arc::clone(&wake)),
        );
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
        .insert_resource(Brp::new(PathBuf::new(), wake))
        .add_message::<BrpReply>()
        .init_state::<Mode>()
        .add_computed_state::<Modal>()
        .configure_sets(
            Update,
            (ViewerSystems::Focus, ViewerSystems::Chrome).chain(),
        )
        .add_systems(
            First,
            (drain_terminal, drain_brp).in_set(ViewerSystems::Ingest),
        )
        .configure_sets(
            First,
            ViewerSystems::Ingest.after(bevy_ecs::message::MessageUpdateSystems),
        );
    if let Some(n) = std::env::var("FUX_VIEWER_PANIC_AT")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        app.insert_resource(PanicAt(n.max(1)))
            .add_systems(Update, panic_at);
    }

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
        choosers::ChoosersPlugin,
        prompts::PromptsPlugin,
        copy_mode::CopyModePlugin,
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
    let painted = world.resource::<replicate::PaintedSceneRevision>().0;
    let revision = world
        .resource_mut::<Inbox>()
        .input_revision
        .take()
        .unwrap_or(painted);
    world.resource_mut::<replicate::InputRevision>().0 = revision;
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
            input::Input::Mouse(mut mouse) => {
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
                    mouse.event.revision = revision;
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

/// Admits a terminal-reader event with its original paint provenance. Output-only frames
/// do not invalidate typing; changed layout/focus does, rather than redirecting old keys.
pub fn queue_terminal(world: &mut World, event: termina::Event, revision: u64) {
    let current = world.resource::<replicate::PaintedSceneRevision>().0;
    let oldest = world.resource::<replicate::InputSceneRevision>().0;
    if matches!(
        &event,
        termina::Event::WindowResized(_) | termina::Event::FocusIn | termina::Event::FocusOut
    ) {
        world.resource_mut::<Inbox>().events.push(event);
        return;
    }
    if revision < oldest || revision > current {
        // Releases cancel held gestures without picking the new scene, locally or remotely.
        if let termina::Event::Mouse(mouse) = &event
            && matches!(mouse.kind, termina::event::MouseEventKind::Up(_))
            && let Some(input::Input::Mouse(mut mouse)) =
                world.resource_mut::<TerminalInput>().0.translate(&event)
        {
            if let Some(button) = mouse.button {
                world.write_message(button);
            }
            for (id, mut press) in world
                .query::<(&PointerId, &mut bevy_picking::pointer::PointerPress)>()
                .iter_mut(world)
            {
                if *id == PointerId::Mouse {
                    *press = Default::default();
                }
            }
            if let Some(mut action) = mouse.pointer_action {
                action.action = bevy_picking::pointer::PointerAction::Cancel;
                world.write_message(action);
            }
            mouse.event.revision = revision;
            world
                .resource_mut::<Outbox>()
                .push(ViewerRequest::Pointer(mouse.event));
        }
        return;
    }
    let mut inbox = world.resource_mut::<Inbox>();
    inbox.input_revision = Some(
        inbox
            .input_revision
            .map_or(revision, |old| old.min(revision)),
    );
    inbox.events.push(event);
}

/// Dispatch already-read terminal input against the completed paint before applying any
/// newly received scene. Both halves run in this wake, even under continuous input.
pub fn update(app: &mut App) {
    let frames = {
        let mut inbox = app.world_mut().resource_mut::<Inbox>();
        if inbox.events.is_empty() {
            Vec::new()
        } else {
            core::mem::take(&mut inbox.frames)
        }
    };
    app.update();
    if !frames.is_empty() {
        let mut painted = core::mem::take(&mut app.world_mut().resource_mut::<Painter>().out);
        app.world_mut().resource_mut::<Inbox>().frames = frames;
        app.update();
        let mut painter = app.world_mut().resource_mut::<Painter>();
        painted.extend_from_slice(&painter.out);
        painter.out = painted;
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
    let ready_wake = wake_tx.clone();
    let painted = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let _reader = term.spawn_reader(wake_tx.clone(), Arc::clone(&painted))?;

    let wake: assets::Wake = Arc::new(move || {
        let _ = ready_wake.send(Wake::Ready);
    });
    let mut app = build_in(cols, rows, &opts.config_dir, Arc::clone(&wake));
    app.world_mut()
        .insert_resource(Brp::new(opts.brp_path.clone(), wake));
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
            let mut count = 0;
            let mut pending = first;
            while let Some(wake) = pending.take() {
                match wake {
                    Wake::Terminal { event, revision } => {
                        queue_terminal(app.world_mut(), event, revision);
                    }
                    Wake::Frame(frame) => {
                        app.world_mut().resource_mut::<Inbox>().frames.push(frame);
                    }
                    Wake::Ready => {}
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
        update(&mut app);
        let world = app.world_mut();
        let out = core::mem::take(&mut world.resource_mut::<Painter>().out);
        if !out.is_empty() {
            term.write_all(&out)?;
        }
        painted.store(
            world.resource::<replicate::PaintedSceneRevision>().0,
            std::sync::atomic::Ordering::Release,
        );
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
        if let Some(Reconnect(workspace)) = world.remove_resource::<Reconnect>() {
            // Attach to another workspace: the old stream is closed and drained, the replicated
            // scene is dropped, and a fresh `Hello` starts the next one.
            painted.store(0, std::sync::atomic::Ordering::Release);
            connection.close();
            let mut kept = Vec::new();
            while let Ok(wake) = wake_rx.try_recv() {
                match wake {
                    Wake::Frame(_) | Wake::Disconnected(_) | Wake::Terminal { .. } => {}
                    other => kept.push(other),
                }
            }
            for wake in kept {
                let _ = wake_tx.send(wake);
            }
            replicate::reset(world);
            world.resource_mut::<focus::FocusRing>().clear();
            let viewport = world.resource::<Viewport>().pane_area();
            connection = match connection::Connection::connect(
                &opts.brp_path,
                &workspace,
                viewport,
                None,
                wake_tx.clone(),
            ) {
                Ok(connection) => connection,
                Err(e) => {
                    break Exit {
                        code: 1,
                        message: Some(format!("fux: {e}")),
                    };
                }
            };
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
