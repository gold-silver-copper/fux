//! zor as a client of a real `fux serve` (prompt 4.2) on disposable XDG directories:
//! `fux/server.info`, `fux/pane.new`, the `events+watch` consumer yielding `PaneSpawned`, the
//! `Effect::FuxCall` adapter, and pane identity validation refusing a wrong pid or instance.
//! Skips with a message when `target/debug/fux` is missing (`cargo build -p fux --bin fux`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bevy_tasks::{IoTaskPool, TaskPool};
use fux::model::SplitDirection;
use fux::remote::methods::{PaneNewParams, TemplateSpec};
use serde_json::json;
use zor::fux_client::{FuxAdapter, FuxClient, IdentityError, spawn_events_consumer};
use zor::model::{Effect, Inbound, PaneHandle};
use zor::runner::Adapter;

const WAIT: Duration = Duration::from_secs(20);

fn fux_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug = exe.parent()?.parent()?;
    let binary = debug.join("fux");
    binary.is_file().then_some(binary)
}

/// A `fux serve` on private XDG directories, terminated with the test.
struct Fux {
    child: Child,
    brp: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fux {
    fn start(binary: &PathBuf) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("run");
        let config = dir.path().join("config");
        let state = dir.path().join("state");
        for d in [&runtime, &config, &state] {
            std::fs::create_dir_all(d).unwrap();
        }
        let child = Command::new(binary)
            .args(["serve", "--name", "zt"])
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("HOME", dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let brp = runtime.join("fux").join("zt.brp.json");
        let deadline = Instant::now() + WAIT;
        while fux::remote::client::read_descriptor(&brp).is_err() {
            assert!(
                Instant::now() < deadline,
                "fux never published {}",
                brp.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            child,
            brp,
            _dir: dir,
        }
    }
}

impl Drop for Fux {
    fn drop(&mut self) {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.child.id()).unwrap());
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn next_matching(
    rx: &async_channel::Receiver<Inbound>,
    mut accept: impl FnMut(&Inbound) -> bool,
) -> Inbound {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "no matching inbound message");
        match rx.try_recv() {
            Ok(message) if accept(&message) => return message,
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn talks_to_a_real_fux_and_validates_pane_identity() {
    let Some(binary) = fux_binary() else {
        eprintln!("skipping: target/debug/fux is missing; run `cargo build -p fux --bin fux`");
        return;
    };
    IoTaskPool::get_or_init(TaskPool::new);
    let fux = Fux::start(&binary);
    let client = FuxClient::open(&fux.brp).unwrap();

    let info = client.server_info().unwrap();
    assert_eq!(info.name, "zt");
    assert_eq!(info.nonce, client.instance());
    assert_eq!(info.workspaces, 1);

    let list = client.workspace_list().unwrap();
    let root = &list.workspaces[0].roots[0];
    let first_pane = root.panes[0].id;

    // The consumer connects and reports the link before anything happens.
    let (tx, rx) = async_channel::unbounded::<Inbound>();
    let cursor = spawn_events_consumer(fux.brp.clone(), 0, tx.clone());
    let link = next_matching(&rx, |m| matches!(m, Inbound::FuxLink { .. }));
    assert!(matches!(&link, Inbound::FuxLink { instance: Some(i) } if i == client.instance()));

    let created = client
        .pane_new(&PaneNewParams {
            generation: Some(root.generation),
            parent: None,
            index: None,
            split: Some(first_pane),
            direction: Some(SplitDirection::Below),
            template: Some(TemplateSpec {
                argv: Some(vec!["/bin/sh".into()]),
                cwd: None,
                env: None,
                stream: None,
            }),
        })
        .unwrap();
    assert_ne!(created.pane, first_pane);

    let spawned = next_matching(&rx, |m| {
        matches!(m, Inbound::FuxEvent { name, body, .. }
            if name == "PaneSpawned" && body["pane"] == json!(created.pane))
    });
    let Inbound::FuxEvent {
        cursor: at, body, ..
    } = spawned
    else {
        panic!("matched message has another shape")
    };
    assert!(at >= 1 && cursor.get() >= at);
    let pid = u32::try_from(body["pid"].as_u64().unwrap()).unwrap();

    // Identity validation: the recorded handle passes; a wrong pid or instance is refused.
    let handle = PaneHandle {
        instance: client.instance().to_owned(),
        workspace: "default".into(),
        stream: String::new(),
        pane: created.pane,
        pid: Some(pid),
    };
    let entry = client.ensure_pane_identity(&handle).unwrap();
    assert_eq!(entry.pid, Some(pid));
    let wrong_pid = PaneHandle {
        pid: Some(pid + 1),
        ..handle.clone()
    };
    assert!(matches!(
        client.ensure_pane_identity(&wrong_pid),
        Err(IdentityError::Pid { .. })
    ));
    let wrong_instance = PaneHandle {
        instance: "stale".into(),
        ..handle.clone()
    };
    assert!(matches!(
        client.ensure_pane_identity(&wrong_instance),
        Err(IdentityError::Instance { .. })
    ));
    let missing = PaneHandle {
        pane: 9_999,
        ..handle.clone()
    };
    assert!(matches!(
        client.ensure_pane_identity(&missing),
        Err(IdentityError::PaneMissing { .. })
    ));

    // The effect adapter answers a call through the same channel.
    let mut adapter = FuxAdapter::new(fux.brp.clone(), tx.clone());
    let effect = Effect::FuxCall {
        call: 7,
        method: "fux/server.info".into(),
        params: json!({}),
    };
    assert!(adapter.handles(&effect));
    assert!(adapter.apply(effect).unwrap().is_none());
    let reply = next_matching(&rx, |m| matches!(m, Inbound::FuxReply { call: 7, .. }));
    let Inbound::FuxReply { result, .. } = reply else {
        panic!("matched message has another shape")
    };
    assert_eq!(result.unwrap()["name"], "zt");
    let mut bad = FuxAdapter::new(fux.brp.clone(), tx.clone());
    assert!(
        bad.apply(Effect::FuxCall {
            call: 8,
            method: "fux/nope".into(),
            params: json!({}),
        })
        .unwrap()
        .is_none()
    );
    let reply = next_matching(&rx, |m| matches!(m, Inbound::FuxReply { call: 8, .. }));
    let Inbound::FuxReply { result, .. } = reply else {
        panic!("matched message has another shape")
    };
    assert!(result.is_err());

    // Validated destructive call: closing the pane reaches the stream as `PaneExited`/`PaneClosed`.
    client.ensure_pane_identity(&handle).unwrap();
    client.pane_close(created.pane).unwrap();
    next_matching(&rx, |m| {
        matches!(m, Inbound::FuxEvent { name, body, .. }
            if (name == "PaneClosed" || name == "PaneExited") && body["pane"] == json!(created.pane))
    });
    drop(rx);
}
