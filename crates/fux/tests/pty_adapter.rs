//! `PtyAdapter` against real processes on a real `IoTaskPool`, and the `TerminalPlugin` ingest
//! path fed from the adapter's channel.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
use std::time::{Duration, Instant};

use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_tasks::{IoTaskPool, TaskPool};
use fux::model::{Effect, Inbound, ModelPlugin, OutputPacing, Pane, PaneSize, Process, Title};
use fux::pty::{PtyAdapter, TerminalPlugin};
use fux::terminal::Terminal;

const DEADLINE: Duration = Duration::from_secs(10);

fn adapter() -> (PtyAdapter, async_channel::Receiver<Inbound>) {
    IoTaskPool::get_or_init(TaskPool::new);
    let (tx, rx) = async_channel::unbounded();
    (PtyAdapter::new(tx), rx)
}

/// The next message, or `None` once `deadline` passes or the channel closes.
fn recv(rx: &async_channel::Receiver<Inbound>, deadline: Duration) -> Option<Inbound> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        match rx.try_recv() {
            Ok(message) => return Some(message),
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(async_channel::TryRecvError::Closed) => return None,
        }
    }
    None
}

fn spawn(pane: Entity, script: &str, rows: u16, cols: u16) -> Effect {
    Effect::SpawnPane {
        pane,
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: None,
        env: Vec::new(),
        rows,
        cols,
    }
}

#[test]
fn output_eof_and_exit_arrive_in_order() {
    let (mut adapter, rx) = adapter();
    let pane = Entity::from_bits(7);
    adapter
        .apply(spawn(pane, "printf hello; exit 3", 24, 80))
        .unwrap();
    assert_eq!(adapter.live_count(), 1);

    let Inbound::PaneSpawned { pane: spawned, pid } =
        recv(&rx, DEADLINE).expect("message before deadline")
    else {
        panic!("expected PaneSpawned first");
    };
    assert_eq!(spawned, pane);
    assert!(pid > 0);

    let mut output = Vec::new();
    let mut saw_eof = false;
    let mut recycled = 0;
    loop {
        match recv(&rx, DEADLINE).expect("message before deadline") {
            Inbound::PaneOutput { pane: p, bytes } => {
                assert_eq!(p, pane);
                assert!(!saw_eof, "output after EOF");
                output.extend_from_slice(&bytes);
                adapter.apply(Effect::RecycleBuffer(bytes)).unwrap();
                recycled += 1;
            }
            Inbound::PaneEof { pane: p } => {
                assert_eq!(p, pane);
                saw_eof = true;
            }
            Inbound::PaneExited { pane: p, code } => {
                assert_eq!(p, pane);
                assert!(saw_eof, "exit before EOF");
                assert_eq!(code, 3);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(recycled >= 1);
    assert!(
        String::from_utf8_lossy(&output).contains("hello"),
        "{output:?}"
    );
    adapter.apply(Effect::ReleasePty { pane }).unwrap();
    assert_eq!(adapter.live_count(), 0);
    // Effects for a released pane are ignored, never fatal.
    adapter
        .apply(Effect::WritePty {
            pane,
            bytes: b"x".to_vec(),
        })
        .unwrap();
    adapter
        .apply(Effect::ResizePty {
            pane,
            rows: 1,
            cols: 1,
        })
        .unwrap();
}

#[test]
fn input_reaches_the_process_and_resize_is_observed() {
    let (mut adapter, rx) = adapter();
    let pane = Entity::from_bits(9);
    adapter
        .apply(spawn(pane, "read line; stty size; echo got:$line", 24, 80))
        .unwrap();
    let Inbound::PaneSpawned { .. } = recv(&rx, DEADLINE).expect("message before deadline") else {
        panic!("expected PaneSpawned");
    };
    adapter
        .apply(Effect::ResizePty {
            pane,
            rows: 30,
            cols: 100,
        })
        .unwrap();
    adapter
        .apply(Effect::WritePty {
            pane,
            bytes: b"ping\n".to_vec(),
        })
        .unwrap();
    let mut output = String::new();
    loop {
        match recv(&rx, DEADLINE).expect("message before deadline") {
            Inbound::PaneOutput { bytes, .. } => output.push_str(&String::from_utf8_lossy(&bytes)),
            Inbound::PaneExited { code, .. } => {
                assert_eq!(code, 0);
                break;
            }
            _ => {}
        }
    }
    assert!(output.contains("30 100"), "{output}");
    assert!(output.contains("got:ping"), "{output}");
}

#[test]
fn terminate_hangs_up_and_then_kills_within_two_seconds() {
    let (mut adapter, rx) = adapter();
    let pane = Entity::from_bits(11);
    // Ignoring SIGHUP forces the SIGKILL escalation; the shell announces once the trap is set.
    adapter
        .apply(spawn(
            pane,
            "trap '' HUP; echo armed; while :; do sleep 1; done",
            24,
            80,
        ))
        .unwrap();
    let Inbound::PaneSpawned { .. } = recv(&rx, DEADLINE).expect("message before deadline") else {
        panic!("expected PaneSpawned");
    };
    loop {
        if let Inbound::PaneOutput { bytes, .. } =
            recv(&rx, DEADLINE).expect("message before deadline")
            && String::from_utf8_lossy(&bytes).contains("armed")
        {
            break;
        }
    }
    let started = Instant::now();
    adapter.apply(Effect::Terminate { pane }).unwrap();
    loop {
        if let Inbound::PaneExited { code, .. } =
            recv(&rx, Duration::from_secs(2)).expect("exit within two seconds")
        {
            assert_eq!(code, 128 + 9, "killed by SIGKILL");
            break;
        }
    }
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "SIGKILL waits the grace"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    // A process that honours SIGHUP exits at once.
    let pane = Entity::from_bits(12);
    adapter.apply(spawn(pane, "sleep 100", 24, 80)).unwrap();
    let Inbound::PaneSpawned { .. } = recv(&rx, DEADLINE).expect("message before deadline") else {
        panic!("expected PaneSpawned");
    };
    let started = Instant::now();
    adapter.apply(Effect::Terminate { pane }).unwrap();
    loop {
        if let Inbound::PaneExited { code, .. } =
            recv(&rx, Duration::from_secs(2)).expect("exit within two seconds")
        {
            assert_eq!(code, 128 + 1, "killed by SIGHUP");
            break;
        }
    }
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_command_that_cannot_start_reports_spawn_failure() {
    let (mut adapter, rx) = adapter();
    let pane = Entity::from_bits(13);
    adapter
        .apply(Effect::SpawnPane {
            pane,
            argv: vec!["/nonexistent/fux-no-such-binary".into()],
            cwd: None,
            env: Vec::new(),
            rows: 24,
            cols: 80,
        })
        .unwrap();
    let Inbound::PaneSpawnFailed {
        pane: failed,
        error,
    } = recv(&rx, DEADLINE).expect("message before deadline")
    else {
        panic!("expected PaneSpawnFailed");
    };
    assert_eq!(failed, pane);
    assert!(!error.is_empty());
    assert_eq!(adapter.live_count(), 0);
}

/// The plugin path: adapter messages written into `Messages<Inbound>` drive `Process` and the
/// `Terminal`, and the buffers come back as `RecycleBuffer` effects.
#[test]
fn terminal_plugin_ingests_output_and_transitions_process() {
    let (mut adapter, rx) = adapter();
    let mut app = App::new();
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_time::TimePlugin,
        bevy_state::app::StatesPlugin,
        ModelPlugin,
        TerminalPlugin,
    ));
    let pane = app
        .world_mut()
        .spawn((
            Pane,
            Process::Starting,
            PaneSize { rows: 24, cols: 80 },
            Title::default(),
            OutputPacing::default(),
            Terminal::new(24, 80, 100),
            Disabled,
        ))
        .id();
    adapter
        .apply(spawn(
            pane,
            "printf '\\033]2;named\\a\\033[6nhello'; exit 3",
            24,
            80,
        ))
        .unwrap();

    let start = Instant::now();
    let mut recycled = 0;
    let mut host_reply = false;
    loop {
        assert!(start.elapsed() < DEADLINE, "pane never exited");
        while let Ok(message) = rx.try_recv() {
            app.world_mut().write_message(message);
        }
        app.update();
        for effect in app.world_mut().resource_mut::<Messages<Effect>>().drain() {
            match effect {
                Effect::RecycleBuffer(_) => recycled += 1,
                Effect::WritePty { bytes, .. } => host_reply |= bytes == b"\x1b[1;1R",
                _ => {}
            }
        }
        let process = *app.world().entity(pane).get::<Process>().unwrap();
        if let Process::Exited { code } = process {
            assert_eq!(code, 3);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    {
        let entity = app.world().entity(pane);
        let terminal = entity.get::<Terminal>().unwrap();
        assert!(terminal.screen_lines()[0].starts_with("hello"));
        assert_eq!(entity.get::<Title>().unwrap().0, "named");
    }
    assert!(recycled >= 1, "output buffers are returned to the adapter");
    assert!(host_reply, "DSR answer emitted as WritePty");

    // A PaneSize change resizes the emulator; the PTY resize goes out only for a live pid.
    app.world_mut()
        .entity_mut(pane)
        .get_mut::<PaneSize>()
        .unwrap()
        .rows = 10;
    app.update();
    let terminal = app.world().entity(pane).get::<Terminal>().unwrap();
    assert_eq!((terminal.rows(), terminal.cols()), (10, 80));
    let world = app.world_mut();
    let resized = world
        .resource_mut::<Messages<Effect>>()
        .drain()
        .any(|effect| matches!(effect, Effect::ResizePty { .. }));
    assert!(!resized, "no PTY resize for an exited pane");
}
