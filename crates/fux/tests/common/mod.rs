//! A fux server over a real `RemoteHttpPlugin` for the BRP integration tests.

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

use async_channel::{Receiver, Sender};
use async_io::Timer;
use bevy_app::App;
use bevy_ecs::world::World;
use bevy_tasks::futures_lite::future;
use fux::config::Config;
use fux::model::Inbound;
use fux::remote::client::{self, ClientError, Descriptor};
use fux::remote::{HttpTransport, RemoteControlPlugin, descriptor};
use serde_json::{Value, json};

pub fn build(runtime_dir: &Path, name: &str) -> App {
    let (inbound, _keep) = async_channel::unbounded();
    // The receiver is dropped: `Wake` sends fail harmlessly; the test loop polls instead.
    build_with(runtime_dir, name, inbound)
}

/// [`build`] with the runner's inbound channel supplied by the caller.
pub fn build_with(runtime_dir: &Path, name: &str, inbound: Sender<Inbound>) -> App {
    build_on(runtime_dir, name, inbound, HttpTransport::default())
}

/// [`build_with`] on a chosen HTTP transport.
pub fn build_on(
    runtime_dir: &Path,
    name: &str,
    inbound: Sender<Inbound>,
    transport: HttpTransport,
) -> App {
    let mut app = fux::app::build_headless(&Config::default());
    app.add_plugins(RemoteControlPlugin {
        runtime_dir: runtime_dir.to_path_buf(),
        server_name: name.into(),
        inbound,
        transport,
    });
    app
}

type Steer = Box<dyn FnOnce(&mut World) + Send>;

/// A server on its own thread, stepped every 10 ms; the real runner belongs to `runner.rs`.
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
        Self::start_with(|_| {})
    }

    /// Like [`Server::start`], with `setup` run on the built App before its first update.
    pub fn start_with(setup: impl FnOnce(&mut World) + Send + 'static) -> Self {
        Self::start_inner(setup, false, HttpTransport::default())
    }

    /// [`Server::start`] on a chosen HTTP transport; `woken` makes the loop also step as soon
    /// as an `Inbound::Wake` arrives, as the real runner does, so request latency is the
    /// server's rather than the 10 ms poll.
    pub fn start_on(transport: HttpTransport, woken: bool) -> Self {
        Self::start_inner(|_| {}, woken, transport)
    }

    fn start_inner(
        setup: impl FnOnce(&mut World) + Send + 'static,
        woken: bool,
        transport: HttpTransport,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("run");
        let brp = descriptor::descriptor_path(&runtime, "test");
        let stop = Arc::new(AtomicBool::new(false));
        let (steer, steer_rx) = mpsc::channel::<Steer>();
        let thread = {
            let stop = Arc::clone(&stop);
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let (inbound, wake) = async_channel::unbounded();
                let mut app = build_on(&runtime, "test", inbound, transport);
                // As `fux serve` does through its Startup system: the workspace exists before
                // the first update publishes the descriptor, so a client that reads it never
                // observes an empty server.
                fux::lifecycle::bootstrap(app.world_mut(), "default", &[]).unwrap();
                setup(app.world_mut());
                app.finish();
                app.cleanup();
                app.update();
                while !stop.load(Ordering::Relaxed) {
                    while let Ok(step) = steer_rx.try_recv() {
                        step(app.world_mut());
                    }
                    app.update();
                    if woken {
                        wait_wake(&wake, Duration::from_millis(10));
                    } else {
                        std::thread::sleep(Duration::from_millis(10));
                        while wake.try_recv().is_ok() {}
                    }
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

    /// Runs `f` on the server thread before its next update (what the runner does with the
    /// adapters' messages) and returns its result.
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

    pub fn workspace_names(&self) -> Vec<String> {
        let list = self.call("fux/workspace.list", json!({})).unwrap();
        list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap().to_owned())
            .collect()
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

/// Sleeps until a message arrives on `wake` or `timeout` elapses, draining what queued up.
fn wait_wake(wake: &Receiver<Inbound>, timeout: Duration) {
    bevy_tasks::block_on(future::or(
        async {
            let _ = wake.recv().await;
        },
        async {
            Timer::after(timeout).await;
        },
    ));
    while wake.try_recv().is_ok() {}
}

pub fn code(result: Result<Value, ClientError>) -> i16 {
    match result {
        Err(ClientError::Rpc { code, .. }) => code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}
