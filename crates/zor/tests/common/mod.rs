//! A zor server over a real `RemoteHttpPlugin` for the BRP integration tests, plus the typed
//! graph every test builds.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test helpers; each test binary uses a subset"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bevy_app::App;
use bevy_ecs::world::World;
use serde_json::{Value, json};
use zor::config::Config;
use zor::model::*;
use zor::remote::client::{self, ClientError, Descriptor};
use zor::remote::{RemoteHostPlugin, descriptor};

pub fn build(runtime_dir: &Path, state_dir: &Path, name: &str) -> App {
    let (inbound, _keep) = async_channel::unbounded();
    // The receiver is dropped: `Wake` sends fail harmlessly; the test loop polls instead.
    let mut app = zor::app::build_headless(&Config::default(), state_dir);
    app.add_plugins(RemoteHostPlugin {
        runtime_dir: runtime_dir.to_path_buf(),
        server_name: name.into(),
        inbound,
    });
    app
}

type Steer = Box<dyn FnOnce(&mut World) + Send>;

/// A server on its own thread, stepped every 10 ms.
pub struct Server {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    steer: mpsc::Sender<Steer>,
    pub brp: PathBuf,
    pub descriptor: Descriptor,
    _dir: tempfile::TempDir,
}

impl Server {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("run");
        let state = dir.path().join("state");
        let brp = descriptor::descriptor_path(&runtime, "test");
        let stop = Arc::new(AtomicBool::new(false));
        let (steer, steer_rx) = mpsc::channel::<Steer>();
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut app = build(&runtime, &state, "test");
                app.finish();
                app.cleanup();
                app.update();
                while !stop.load(Ordering::Relaxed) {
                    while let Ok(step) = steer_rx.try_recv() {
                        step(app.world_mut());
                    }
                    app.update();
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let descriptor = loop {
            if let Ok(d) = client::read_descriptor(&brp) {
                break d;
            }
            assert!(Instant::now() < deadline, "brp.json never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };
        Self {
            stop,
            thread: Some(thread),
            steer,
            brp,
            descriptor,
            _dir: dir,
        }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        client::call(&self.brp, method, params)
    }

    /// Runs `f` on the server thread before its next update and returns its result.
    pub fn with_world<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut World) -> R + Send + 'static,
    ) -> R {
        let (tx, rx) = mpsc::channel();
        self.steer
            .send(Box::new(move |world: &mut World| {
                let _ = tx.send(f(world));
            }))
            .unwrap();
        rx.recv_timeout(Duration::from_secs(20)).unwrap()
    }

    /// Sends `params` verbatim: no token/instance injection.
    pub fn raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        client::request(
            &self.descriptor.http.host,
            self.descriptor.http.port,
            method,
            params,
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

pub fn code(result: Result<Value, ClientError>) -> i16 {
    match result {
        Err(ClientError::Rpc { code, .. }) => code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

pub fn handle(instance: &str, pane: u64, pid: Option<u32>) -> PaneHandle {
    PaneHandle {
        instance: instance.into(),
        workspace: "default".into(),
        stream: String::new(),
        pane,
        pid,
    }
}

/// Every entity kind through the typed helpers: one task with a managed attempt, a prompt, a
/// launch operation, a source, a passed check with its result, an artifact, a worktree, a
/// second task whose stop operation is a group member, a machine with a service and an
/// observed agent, a plugin with one action.
pub struct Graph {
    pub task: bevy_ecs::entity::Entity,
    pub attempt: bevy_ecs::entity::Entity,
    pub prompt: bevy_ecs::entity::Entity,
    pub launch: bevy_ecs::entity::Entity,
    pub source: bevy_ecs::entity::Entity,
    pub check: bevy_ecs::entity::Entity,
    pub result: bevy_ecs::entity::Entity,
    pub artifact: bevy_ecs::entity::Entity,
    pub worktree: bevy_ecs::entity::Entity,
    pub other_task: bevy_ecs::entity::Entity,
    pub other_attempt: bevy_ecs::entity::Entity,
    pub stop: bevy_ecs::entity::Entity,
    pub group: bevy_ecs::entity::Entity,
    pub machine: bevy_ecs::entity::Entity,
    pub service: bevy_ecs::entity::Entity,
    pub agent: bevy_ecs::entity::Entity,
    pub plugin: bevy_ecs::entity::Entity,
    pub action: bevy_ecs::entity::Entity,
}

pub fn spawn_graph(world: &mut World) -> Graph {
    let task = spawn_task(
        world,
        TaskSpec {
            id: "t1",
            title: "first",
            location: Location::Cwd("/tmp".into()),
            created_ms: 1_000,
        },
    )
    .unwrap();
    let attempt = spawn_attempt(
        world,
        AttemptSpec {
            task,
            ownership: Ownership::Managed,
            handle: handle("nonce-a", 7, Some(4242)),
        },
    )
    .unwrap();
    let prompt = spawn_prompt(
        world,
        PromptSpec {
            id: "p1",
            attempt,
            text: "hello",
            deadline_ms: 5_000,
            report_token: "deadbeef",
        },
    )
    .unwrap();
    let launch = spawn_operation(world, "launch-1", attempt, OperationKind::Launch).unwrap();
    let source = spawn_source(
        world,
        "s1",
        task,
        SourceRevision {
            expression: "HEAD".into(),
            commit: "abc".into(),
        },
    )
    .unwrap();
    let check = spawn_check(
        world,
        CheckSpec {
            id: "c1",
            task,
            source: Some(source),
            command: CheckCommand {
                argv: vec!["true".into()],
                timeout_ms: 1_000,
            },
            requirement: Some("build"),
            generation: 1,
        },
    )
    .unwrap();
    world.entity_mut(check).insert(CheckState::Passed);
    let result = spawn_result(world, check, Verdict::Passed, OutputTail::default()).unwrap();
    let artifact = spawn_artifact(
        world,
        ArtifactSpec {
            id: "a1",
            attempt,
            path: "out/report.txt",
            requirement: None,
        },
    )
    .unwrap();
    let worktree = spawn_worktree(
        world,
        "w1",
        task,
        WorktreeSpec {
            repo: "/repo".into(),
            branch: "feature".into(),
            base: "main".into(),
            parent: "/state/wt-1".into(),
        },
    )
    .unwrap();
    let other_task = spawn_task(
        world,
        TaskSpec {
            id: "t2",
            title: "second",
            location: Location::Worktree("w1".into()),
            created_ms: 2_000,
        },
    )
    .unwrap();
    let other_attempt = spawn_attempt(
        world,
        AttemptSpec {
            task: other_task,
            ownership: Ownership::Adopted,
            handle: handle("nonce-a", 8, None),
        },
    )
    .unwrap();
    let stop = spawn_operation(world, "stop-2", other_attempt, OperationKind::Stop).unwrap();
    let group = spawn_group(world, "g1", &[launch, stop], 1, &[(1, 0)]).unwrap();
    let machine = spawn_machine(world, "m1", "laptop").unwrap();
    let service = spawn_service(world, "fux-default", Some(machine)).unwrap();
    let agent = spawn_observed_agent(world, handle("nonce-a", 9, Some(77)), Some(machine)).unwrap();
    let plugin = spawn_plugin(
        world,
        "herdr",
        PluginManifest {
            path: "/plugins/herdr".into(),
            version: "1".into(),
        },
    )
    .unwrap();
    let action = spawn_plugin_action(world, "open", plugin).unwrap();
    Graph {
        task,
        attempt,
        prompt,
        launch,
        source,
        check,
        result,
        artifact,
        worktree,
        other_task,
        other_attempt,
        stop,
        group,
        machine,
        service,
        agent,
        plugin,
        action,
    }
}

pub fn no_params() -> Value {
    json!({})
}
