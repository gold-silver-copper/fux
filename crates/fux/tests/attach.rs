//! Attachment stream (prompt 3.10) against a real loopback listener: a server App steps on a
//! thread while std `TcpStream` clients speak the wire protocol. The lifecycle plugin is not
//! part of this test; a stand-in system applies the few viewer requests the scenarios need.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ui::Node;
use serde::Serialize;

use fux::attach::{AttachAdapter, AttachEndpoint, AttachPlugin, AttachToken, ProjectionPlugin};
use fux::layout::{LayoutPlugin, ops};
use fux::model::invariants::check_invariants;
use fux::model::{
    Detaching, Effect, Inbound, Limits, ModelPlugin, PaneId, PaneTemplate, Places, Process,
    ServerInstance, Viewer, ViewerRequest, Viewport,
};
use fux::terminal::Terminal;
use fux::wire::{self, ByeReason, ClientFrame, ExactTargetSpec, Hello, SceneFrame, ServerFrame};

const PID: u32 = 4242;
const STEP: Duration = Duration::from_millis(2);
const PATIENCE: Duration = Duration::from_secs(5);

type Probe = Box<dyn FnOnce(&mut World) + Send>;

struct Server {
    port: u16,
    token: String,
    nonce: String,
    panes: Vec<PaneId>,
    probes: mpsc::Sender<Probe>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        Self::start_with(Limits::default())
    }

    fn start_with(limits: Limits) -> Self {
        let (inbound_tx, inbound_rx) = async_channel::unbounded::<Inbound>();
        let (probes, probe_rx) = mpsc::channel::<Probe>();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut app = App::new();
            app.add_plugins((
                bevy_app::TaskPoolPlugin::default(),
                bevy_state::app::StatesPlugin,
                bevy_time::TimePlugin,
                bevy_asset::AssetPlugin::default(),
                ModelPlugin,
                LayoutPlugin,
                ProjectionPlugin,
                AttachPlugin {
                    inbound: inbound_tx,
                },
            ))
            .insert_resource(ServerInstance {
                name: "test".into(),
                nonce: "nonce-test".into(),
                pid: std::process::id(),
                started_ms: 0,
            })
            .insert_resource(limits)
            .add_systems(PreUpdate, lifecycle_stand_in);
            app.finish();
            app.cleanup();
            let mut adapter = AttachAdapter::from_app(&app).unwrap();
            // Startup binds the listener.
            app.update();
            let world = app.world_mut();
            let ws = ops::new_workspace(world, "default").unwrap();
            let root = ops::new_root(world, ws, "main").unwrap();
            let mut panes = Vec::new();
            for _ in 0..2 {
                let leaf = ops::spawn_node(
                    world,
                    root,
                    None,
                    Node {
                        flex_grow: 1.0,
                        ..Default::default()
                    },
                    Some(PaneTemplate::default()),
                )
                .unwrap();
                let pane = world.get::<Places>(leaf).unwrap().0;
                let mut terminal = Terminal::new(24, 40, 100);
                terminal.feed(b"hello from a pane\r\n");
                world
                    .entity_mut(pane)
                    .insert((Process::Live { pid: PID }, terminal))
                    .remove::<Disabled>();
                panes.push(*world.get::<PaneId>(pane).unwrap());
            }
            let endpoint = world.resource::<AttachEndpoint>().clone();
            let token = world.resource::<AttachToken>().0.clone();
            ready_tx.send((endpoint.port, token, panes)).unwrap();
            while !stopping.load(Ordering::Relaxed) {
                while let Ok(probe) = probe_rx.try_recv() {
                    probe(app.world_mut());
                }
                while let Ok(message) = inbound_rx.try_recv() {
                    app.world_mut().write_message(message);
                }
                app.update();
                // The runner's job: drain effects into the adapter after every update.
                let effects: Vec<Effect> = app
                    .world_mut()
                    .resource_mut::<Messages<Effect>>()
                    .drain()
                    .collect();
                for effect in effects {
                    if AttachAdapter::handles(&effect) {
                        assert!(adapter.apply(effect));
                    }
                }
                thread::sleep(STEP);
            }
        });
        let (port, token, panes) = ready_rx.recv().unwrap();
        Self {
            port,
            token,
            nonce: "nonce-test".into(),
            panes,
            probes,
            stop,
            thread: Some(thread),
        }
    }

    fn with_world<T: Send + 'static>(&self, f: impl FnOnce(&mut World) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.probes
            .send(Box::new(move |world| {
                let _ = tx.send(f(world));
            }))
            .unwrap();
        rx.recv_timeout(PATIENCE).expect("server thread answers")
    }

    fn viewer_count(&self) -> usize {
        self.with_world(|world| world.query::<&Viewer>().iter(world).count())
    }

    fn wait_for_viewers(&self, count: usize) {
        let deadline = Instant::now() + PATIENCE;
        while self.viewer_count() != count {
            assert!(
                Instant::now() < deadline,
                "viewer count never reached {count}"
            );
            thread::sleep(STEP);
        }
    }

    fn hello(&self, rows: u16, cols: u16) -> Hello {
        Hello {
            token: self.token.clone(),
            instance: self.nonce.clone(),
            workspace: "default".into(),
            stream: String::new(),
            viewport: Viewport { rows, cols },
            exact_target: None,
        }
    }

    fn connect(&self) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        stream.set_nodelay(true).unwrap();
        Client {
            stream,
            buf: Vec::new(),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What `LifecyclePlugin` would do with the requests these scenarios send.
fn lifecycle_stand_in(world: &mut World) {
    let mut requests = Vec::new();
    let mut gone = Vec::new();
    for message in world.resource_mut::<Messages<Inbound>>().drain() {
        match message {
            Inbound::ViewerRequest { viewer, request } => requests.push((viewer, request)),
            Inbound::ViewerGone { viewer } => gone.push(viewer),
            _ => {}
        }
    }
    for (viewer, request) in requests {
        match request {
            ViewerRequest::Resize { rows, cols } => {
                ops::resize_viewer(world, viewer, Viewport { rows, cols }).unwrap();
            }
            ViewerRequest::Detach => {
                world.entity_mut(viewer).insert(Detaching);
            }
            _ => {}
        }
    }
    for viewer in gone {
        ops::detach_viewer(world, viewer).unwrap();
    }
}

struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Client {
    fn send<T: Serialize>(&mut self, frame: &T) {
        wire::encode(frame, &mut self.buf).unwrap();
        self.stream.write_all(&self.buf).unwrap();
    }

    /// `None` on EOF.
    fn recv(&mut self) -> Option<ServerFrame> {
        let mut prefix = [0u8; wire::FRAME_PREFIX_BYTES];
        match self.stream.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(error) => panic!("read failed: {error}"),
        }
        let len = wire::payload_len(&prefix).unwrap().unwrap();
        self.buf.clear();
        self.buf.resize(len, 0);
        self.stream.read_exact(&mut self.buf).unwrap();
        Some(serde_json::from_slice(&self.buf).unwrap())
    }

    fn expect_welcome(&mut self) {
        match self.recv() {
            Some(ServerFrame::Welcome(welcome)) => {
                assert_eq!(welcome.workspace, "default");
                assert_eq!(welcome.instance, "nonce-test");
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    fn expect_scene(&mut self) -> SceneFrame {
        match self.recv() {
            Some(ServerFrame::Scene(scene)) => scene,
            other => panic!("expected Scene, got {other:?}"),
        }
    }

    fn expect_bye(&mut self, reason: ByeReason) {
        match self.recv() {
            Some(ServerFrame::Bye { reason: got, .. }) => assert_eq!(got, reason),
            other => panic!("expected Bye({reason:?}), got {other:?}"),
        }
    }

    /// Reads scenes until one satisfies `pred`.
    fn scene_where(&mut self, pred: impl Fn(&SceneFrame) -> bool) -> SceneFrame {
        for _ in 0..16 {
            let scene = self.expect_scene();
            if pred(&scene) {
                return scene;
            }
        }
        panic!("no matching scene frame arrived");
    }
}

/// Entity ids of a serialized `DynamicWorld`: the keys of its `entities` map.
fn entity_keys(ron: &str) -> Vec<u64> {
    ron.lines()
        .filter_map(|line| line.trim().strip_suffix(": ("))
        .filter_map(|key| key.parse().ok())
        .collect()
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/attach/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
    name: &str,
) -> T {
    let text = fixture(name);
    let value: T = serde_json::from_str(&text).unwrap();
    let expected: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_value(&value).unwrap(), expected, "{name}");
    value
}

#[test]
fn fixtures_pin_the_wire_shapes() {
    let hello: Hello = round_trip("hello.json");
    assert_eq!(
        hello.exact_target,
        Some(ExactTargetSpec {
            pane: PaneId(3),
            pid: Some(4242)
        })
    );
    let client: Vec<ClientFrame> = round_trip("client_frames.json");
    assert!(matches!(
        client.first(),
        Some(ClientFrame::Request {
            request: ViewerRequest::Input(_)
        })
    ));
    assert!(matches!(
        client.last(),
        Some(ClientFrame::Ack { revision: 7 })
    ));
    let server: Vec<ServerFrame> = round_trip("server_frames.json");
    assert!(matches!(
        server.last(),
        Some(ServerFrame::Bye {
            reason: ByeReason::ExactTargetLost,
            ..
        })
    ));
    // Unknown fields are refused on every message.
    assert!(serde_json::from_str::<Hello>(r#"{"token":"t","instance":"i","workspace":"w","viewport":{"rows":1,"cols":1},"extra":1}"#).is_err());
}

#[test]
fn wrong_token_or_instance_is_refused() {
    let server = Server::start();
    let mut client = server.connect();
    let mut hello = server.hello(24, 80);
    hello.token = "0".repeat(64);
    client.send(&hello);
    client.expect_bye(ByeReason::Refused);
    assert!(client.recv().is_none());

    let mut client = server.connect();
    let mut hello = server.hello(24, 80);
    hello.instance = "stale".into();
    client.send(&hello);
    client.expect_bye(ByeReason::Refused);

    let mut client = server.connect();
    let mut hello = server.hello(24, 80);
    hello.workspace = "nowhere".into();
    client.send(&hello);
    client.expect_bye(ByeReason::Refused);
    assert!(client.recv().is_none());
    assert_eq!(server.viewer_count(), 0);
}

/// Idle pre-auth connections count against the cap (`2 * viewers`): the connection past it is
/// not served until one of them closes.
#[test]
fn connections_past_the_cap_wait_for_a_slot() {
    let server = Server::start_with(Limits {
        viewers: 1,
        ..Limits::default()
    });
    let idle: Vec<Client> = (0..2).map(|_| server.connect()).collect();
    let mut third = server.connect();
    third.send(&server.hello(24, 80));
    third
        .stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut prefix = [0u8; wire::FRAME_PREFIX_BYTES];
    let blocked = third.stream.read_exact(&mut prefix).unwrap_err();
    assert!(
        matches!(
            blocked.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "the third connection was served while two idle ones held the cap: {blocked}"
    );
    assert_eq!(server.viewer_count(), 0);

    drop(idle);
    third.stream.set_read_timeout(Some(PATIENCE)).unwrap();
    third.expect_welcome();
    server.wait_for_viewers(1);
}

#[test]
fn attachment_streams_a_full_scene_then_deltas() {
    let server = Server::start();
    let mut client = server.connect();
    client.send(&server.hello(24, 80));
    client.expect_welcome();
    let first = client.expect_scene();
    assert!(first.full);
    assert_eq!(first.revision, 1);
    assert!(first.scene.contains("ComputedNode"), "{}", first.scene);
    assert!(first.scene.contains("Shows"), "{}", first.scene);
    assert_eq!(first.terminals.len(), 2);
    assert!(
        first
            .terminals
            .iter()
            .all(|t| t.full && !t.lines.is_empty())
    );
    assert_eq!(first.terminals[0].rows, 24);
    let roots = first.roots.as_ref().expect("roots on the full frame");
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].name, "main");
    assert_eq!(first.target, Some(server.panes[0]));
    assert!(first.showing.is_some());
    let first_keys = entity_keys(&first.scene);
    assert!(!first_keys.is_empty());

    client.send(&ClientFrame::Request {
        request: ViewerRequest::Resize {
            rows: 30,
            cols: 100,
        },
    });
    let resized = client.scene_where(|s| s.scene.contains("ComputedNode"));
    assert!(!resized.full);
    assert!(resized.revision > 1);
    assert!(resized.scene.contains("100.0"), "{}", resized.scene);
    assert!(resized.despawned.is_empty());

    // A second viewer on the same root gets its own instance entities; only the two shared
    // pane entities (carrying `PaneId` for the leaves' `Shows`) appear in both scenes.
    let mut second = server.connect();
    second.send(&server.hello(10, 40));
    second.expect_welcome();
    let other = second.expect_scene();
    assert!(other.full);
    let other_keys = entity_keys(&other.scene);
    let shared = other_keys.iter().filter(|k| first_keys.contains(k)).count();
    assert_eq!(shared, 2, "{}", other.scene);
    assert!(other_keys.len() > 2 + 1, "root plus leaves expected");
    assert_eq!(other.terminals.len(), 2);
    server.wait_for_viewers(2);
    assert_eq!(server.with_world(check_invariants), Ok(()));
}

#[test]
fn exact_attachment_checks_the_pid() {
    let server = Server::start();
    let mut client = server.connect();
    let mut hello = server.hello(24, 80);
    hello.exact_target = Some(ExactTargetSpec {
        pane: server.panes[1],
        pid: Some(PID + 1),
    });
    client.send(&hello);
    client.expect_bye(ByeReason::Refused);

    let mut client = server.connect();
    let mut hello = server.hello(24, 80);
    hello.exact_target = Some(ExactTargetSpec {
        pane: server.panes[1],
        pid: Some(PID),
    });
    client.send(&hello);
    client.expect_welcome();
    let scene = client.expect_scene();
    assert_eq!(scene.target, Some(server.panes[1]));
    let exact = server.with_world(|world| {
        world
            .query_filtered::<(), (With<Viewer>, With<fux::model::ExactTarget>)>()
            .iter(world)
            .count()
    });
    assert_eq!(exact, 1);
}

#[test]
fn closing_the_socket_detaches_the_viewer() {
    let server = Server::start();
    let mut client = server.connect();
    client.send(&server.hello(24, 80));
    client.expect_welcome();
    let _ = client.expect_scene();
    server.wait_for_viewers(1);
    drop(client);
    server.wait_for_viewers(0);
    assert_eq!(server.with_world(check_invariants), Ok(()));
}

#[test]
fn detach_request_ends_with_bye() {
    let server = Server::start();
    let mut client = server.connect();
    client.send(&server.hello(24, 80));
    client.expect_welcome();
    let _ = client.expect_scene();
    client.send(&ClientFrame::Request {
        request: ViewerRequest::Detach,
    });
    loop {
        match client.recv() {
            Some(ServerFrame::Scene(_)) => continue,
            Some(ServerFrame::Bye { reason, .. }) => {
                assert_eq!(reason, ByeReason::Detached);
                break;
            }
            other => panic!("expected Bye, got {other:?}"),
        }
    }
    assert!(client.recv().is_none());
    server.wait_for_viewers(0);
}

#[test]
fn oversized_hello_prefix_is_a_protocol_error() {
    let server = Server::start();
    let mut client = server.connect();
    client
        .stream
        .write_all(&(wire::MAX_FRAME_BYTES as u32 + 1).to_be_bytes())
        .unwrap();
    client.expect_bye(ByeReason::Protocol);
    assert!(client.recv().is_none());
}
