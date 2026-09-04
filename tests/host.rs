#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test failures should retain their assertion context"
)]

use fux::host::{
    Action, Command, InputRouter, MouseEvent, WorkspaceHost, platform_tool_from, resolve_zor_path,
};
use fux::state::{AgentState, Direction, PaneId, WorkspaceState};
#[cfg(unix)]
use koh::client::{ClientTerminal, IrohConnector, run_client};
#[cfg(unix)]
use koh::predict::{DisplayPreference, Overlay};
use koh::server::{ChangeSignal, ClientId, SessionHost};
#[cfg(unix)]
use koh::server::{Hosts, SharedHost};
#[cfg(unix)]
use koh::transport_iroh::{
    IrohChannel, bind_endpoint_local, bind_endpoint_local_alpns, generate_secret_key, loopback_addr,
};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(unix)]
use tokio::sync::mpsc;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

fn flattened(actions: &[Action]) -> (Vec<u8>, Vec<Command>, Vec<MouseEvent>) {
    let mut bytes = Vec::new();
    let mut commands = Vec::new();
    let mut mouse = Vec::new();
    for action in actions {
        match action {
            Action::Forward(value) => bytes.extend(value),
            Action::Command(value) => commands.push(value.clone()),
            Action::Mouse(value) => mouse.push(*value),
        }
    }
    (bytes, commands, mouse)
}

#[test]
fn default_bare_zor_name_resolves_securely_through_path() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = std::env::temp_dir().join(format!("fux-zor-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).expect("path dir");
    let executable = root.join("zor");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("fixture");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("executable");
    assert_eq!(
        resolve_zor_path(std::path::Path::new("zor"), Some(root.as_os_str())),
        Some(executable.clone())
    );
    let link = root.join("linked-zor");
    symlink(&executable, &link).expect("symlink");
    assert_eq!(resolve_zor_path(&link, None), None);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn android_tool_fallback_does_not_assume_usr_bin() {
    assert_eq!(
        platform_tool_from("env", None, None, true),
        "/system/bin/env"
    );
    assert_eq!(platform_tool_from("sh", None, None, true), "/system/bin/sh");
}

#[test]
fn workspace_control_mutates_the_same_shared_session_state() {
    use fux::control::{Axis, CommandResult, FocusTarget, Reply, Request};
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("shared workspace");
    let (reply, opened) = control.dispatch(Request::Split {
        id: 3,
        axis: Axis::Horizontal,
        target: None,
        argv: vec!["/bin/cat".into()],
        env: Default::default(),
    });
    let pane = match reply {
        Reply::Completed {
            id: 3,
            result: CommandResult::Pane { pane },
        } => pane,
        other => panic!("unexpected reply: {other:?}"),
    };
    assert_eq!(opened.len(), 1);
    let (reply, focused) = control.dispatch(Request::Focus {
        id: 4,
        target: FocusTarget::Pane(pane),
    });
    assert!(matches!(reply, Reply::Completed { id: 4, .. }));
    assert_eq!(focused.len(), 1);
    assert_eq!(session.snapshot().panes().len(), 2);
    let (reply, closed) = control.dispatch(Request::Kill { id: 5, pane });
    assert!(matches!(reply, Reply::Completed { id: 5, .. }));
    assert_eq!(closed.len(), 1);
    assert_eq!(session.snapshot().panes().len(), 1);
    session.shutdown();
}

#[cfg(unix)]
#[test]
fn control_kill_waits_for_real_exit_and_shutdown_rejects_new_work() {
    use fux::control::{Reply, Request};
    let (mut session, control) = WorkspaceHost::shared(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "trap 'exit 23' HUP; while :; do sleep 1; done".into(),
        ],
        32,
        None,
    )
    .expect("workspace");
    std::thread::sleep(Duration::from_millis(100));
    let (reply, events) = control.dispatch(Request::Kill { id: 30, pane: 1 });
    assert!(matches!(reply, Reply::Completed { .. }));
    assert!(
        events.iter().any(|event| matches!(
            event,
            fux::control::Event::PaneClosed {
                pane: 1,
                exit_status: Some(23),
                ..
            }
        )),
        "unexpected close events: {events:?}"
    );
    assert_eq!(session.snapshot().metadata().exit_code, Some(23));
    control.shutdown();
    let before = session.snapshot().panes().len();
    session.input(b"\x01c");
    let (reply, _) = control.dispatch(Request::New {
        id: 31,
        cwd: None,
        argv: vec!["/bin/cat".into()],
        env: Default::default(),
    });
    assert!(matches!(reply, Reply::Failed { .. }));
    assert_eq!(session.snapshot().panes().len(), before);
}

#[cfg(unix)]
#[test]
fn popup_is_keyboard_interactive_and_is_removed_after_exit() {
    use fux::control::{CommandResult, Reply, Request};
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("workspace");
    let marker = std::env::temp_dir().join(format!("fux-popup-input-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let script = format!(
        "stty raw -echo; read x; printf '%s' \"$x\" > '{}'; sleep 1",
        marker.display()
    );
    let (reply, _) = control.dispatch(Request::Popup {
        id: 40,
        rows: Some(8),
        cols: Some(30),
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        env: Default::default(),
    });
    let pane = match reply {
        Reply::Completed {
            result: CommandResult::Pane { pane },
            ..
        } => pane,
        other => panic!("unexpected popup reply: {other:?}"),
    };
    session.input(b"q\n");
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut observed = false;
    while Instant::now() < deadline {
        let state = session.snapshot();
        observed |= std::fs::read_to_string(&marker).is_ok_and(|text| text == "q");
        if observed && state.pane(PaneId(pane)).is_none() {
            assert!(state.popups().is_empty());
            let _ = std::fs::remove_file(marker);
            session.shutdown();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("popup did not receive input and clean itself up; observed={observed}");
}

#[cfg(unix)]
#[test]
fn high_output_host_can_shutdown_before_attach_without_deadlock() {
    let started = Instant::now();
    let host = WorkspaceHost::spawn(
        vec!["/bin/sh".into(), "-c".into(), "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done".into()],
        32,
        None,
    ).expect("host");
    host.shutdown();
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn no_attach_high_output_child_is_drained_and_reaches_terminal_state() {
    let (session, control) = WorkspaceHost::shared(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "dd if=/dev/zero bs=8192 count=128 2>/dev/null; exit 6".into(),
        ],
        0,
        None,
    )
    .expect("workspace");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !control.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(control.is_empty());
    assert_eq!(control.terminal_exit_code(), Some(6));
    session.shutdown();
}

#[test]
fn default_history_is_admitted_and_multibyte_capture_stays_byte_bounded() {
    let (mut session, control) = WorkspaceHost::shared(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '界界界界界界界界'; sleep 2".into(),
        ],
        fux::host::DEFAULT_SCROLLBACK,
        None,
    )
    .expect("default history workspace");
    control
        .configure_bindings(
            &fux::config::Config::default(),
            PathBuf::from("/tmp/fux-default.sock"),
        )
        .expect("default config remains admissible");
    session.attach_notify(ChangeSignal::default());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let (reply, _) = control.dispatch(fux::control::Request::Capture {
            id: 991,
            pane: 1,
            attrs: false,
            scrollback: u32::MAX,
            max_bytes: 7,
        });
        if let fux::control::Reply::Completed {
            result: fux::control::CommandResult::Capture { text: value },
            ..
        } = reply
            && !value.is_empty()
        {
            assert!(value.len() <= 7);
            assert!(value.is_char_boundary(value.len()));
            break;
        }
        assert!(Instant::now() < deadline, "capture deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
    session.shutdown();
}

#[test]
fn one_mib_budget_rejects_512_square_geometry_before_pty_resize() {
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 0, None).expect("host");
    let mut config = fux::config::Config::default();
    config.history.scrollback_lines = 0;
    config.resources.max_units = 1024 * 1024;
    control
        .configure_bindings(&config, PathBuf::from("/tmp/fux-small.sock"))
        .expect("small budget");
    session.attach_notify(ChangeSignal::default());
    let before = session.snapshot().pane(PaneId(1)).expect("pane").clone();
    session.resize(ClientId::next(), 512, 512);
    let after_state = session.snapshot();
    let after = after_state.pane(PaneId(1)).expect("pane");
    assert_eq!((after.rows, after.columns), (before.rows, before.columns));
    session.shutdown();
}

#[test]
fn history_and_osc_clipboard_share_the_same_total_byte_budget() {
    let payload = "YQ==".repeat(1024);
    let script = format!(
        "stty raw -echo; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033]52;c;{payload}\\007'; sleep 1"
    );
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/sh".into(), "-c".into(), script], 0, None).expect("host");
    session.attach_notify(ChangeSignal::default());
    let mut config = fux::config::Config::default();
    config.history.scrollback_lines = 0;
    config.resources.max_units = session
        .snapshot()
        .recompute_resource_units()
        .saturating_add(128);
    control
        .configure_bindings(&config, PathBuf::from("/tmp/fux-osc-budget.sock"))
        .expect("tight current-state budget");
    session.input(b"x");
    std::thread::sleep(Duration::from_millis(100));
    assert!(session.snapshot().metadata().clipboard_base64.is_empty());
    session.shutdown();

    let (session, control) = WorkspaceHost::shared(vec!["/bin/cat".into()], 1, None).expect("host");
    let mut config = fux::config::Config::default();
    config.resources.max_units = 1;
    assert!(
        control
            .configure_bindings(&config, PathBuf::from("/tmp/fux-history-budget.sock"))
            .is_err()
    );
    session.shutdown();
}

#[cfg(unix)]
#[test]
fn shutdown_reaps_bare_and_wrapped_descendant_process_groups() {
    let mut wrappers = vec![None];
    if let Some(zor) = std::env::var_os("ZOR_BIN") {
        wrappers.push(Some(PathBuf::from(zor)));
    }
    for zor in wrappers {
        let marker = std::env::temp_dir().join(format!(
            "fux-descendant-{}-{}",
            std::process::id(),
            zor.is_some()
        ));
        let _ = std::fs::remove_file(&marker);
        let script = format!("sleep 60 & printf '%s' $! > '{}'; wait", marker.display());
        let mut host = WorkspaceHost::spawn(vec!["/bin/sh".into(), "-c".into(), script], 32, zor)
            .expect("host");
        host.attach_notify(ChangeSignal::default());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "descendant did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(&marker).expect("pid");
        host.shutdown();
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("probe")
            .success();
        assert!(!alive, "descendant survived workspace shutdown");
        let _ = std::fs::remove_file(marker);
    }
}

#[cfg(unix)]
#[test]
fn natural_tiled_exits_remove_panes_reflow_focus_and_allow_empty_workspace() {
    use fux::control::{CommandResult, Reply, Request};
    let (mut session, control) = WorkspaceHost::shared(
        vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()],
        32,
        None,
    )
    .expect("workspace");
    let (reply, _) = control.dispatch(Request::Split {
        id: 50,
        axis: fux::control::Axis::Horizontal,
        target: None,
        argv: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        env: Default::default(),
    });
    let exited = match reply {
        Reply::Completed {
            result: CommandResult::Pane { pane },
            ..
        } => pane,
        other => panic!("unexpected split: {other:?}"),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while session.snapshot().pane(PaneId(exited)).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let state = session.snapshot();
    assert!(state.pane(PaneId(exited)).is_none());
    assert_eq!(state.tabs().first().map(|tab| tab.focused), Some(PaneId(1)));
    let _ = control.dispatch(Request::Kill { id: 51, pane: 1 });
    assert!(session.snapshot().panes().is_empty());
    assert!(session.snapshot().tabs().is_empty());
    assert!(control.is_empty());
    let (reply, _) = control.dispatch(Request::New {
        id: 52,
        cwd: None,
        argv: vec!["/bin/cat".into()],
        env: Default::default(),
    });
    assert!(matches!(reply, Reply::Completed { .. }));
    assert_eq!(session.snapshot().tabs().len(), 1);
    control.shutdown();
}

#[cfg(unix)]
#[test]
fn final_natural_pane_is_a_durable_non_live_tombstone() {
    let (mut session, control) = WorkspaceHost::shared(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf FINAL_FRAME; exit 9".into(),
        ],
        0,
        None,
    )
    .expect("workspace");
    session.attach_notify(ChangeSignal::default());
    let client = ClientId::next();
    session.resize(client, 24, 80);
    assert_eq!(control.attached_clients(), 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let state = session.snapshot();
        let settled = control.is_empty()
            && state
                .pane(PaneId(1))
                .is_some_and(|pane| pane.exit_status == Some(9));
        if settled {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(control.is_empty());
    for _ in 0..4 {
        let state = session.snapshot();
        let pane = state.pane(PaneId(1)).expect("durable final pane");
        assert_eq!(pane.exit_status, Some(9));
        assert!(
            pane.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("FINAL_FRAME")
        );
        assert_eq!(state.tabs().first().map(|tab| tab.focused), Some(PaneId(1)));
    }
    session.client_detached(client);
    assert_eq!(control.attached_clients(), 0);
    session.shutdown();
}

#[test]
fn workspace_control_shutdown_stops_panes_before_registry_removal() {
    let (session, control) = WorkspaceHost::shared(
        vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        100,
        None,
    )
    .expect("workspace");
    assert!(session.alive());
    control.shutdown();
    assert!(!session.alive());
    session.shutdown();
}

#[cfg(unix)]
#[test]
fn control_honors_spawn_context_limits_capture_attrs_and_truthful_listing() {
    use fux::control::{CommandResult, Reply, Request};
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("workspace");
    let mut config = fux::config::Config::default();
    config.history.capture_bytes = 12;
    config.resources.max_panes = 2;
    control
        .configure_bindings(&config, PathBuf::from("/tmp/fux-test.sock"))
        .expect("configure limits");
    let (reply, _) = control.dispatch(Request::New {
        id: 20,
        cwd: Some(PathBuf::from("/tmp")),
        argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '\\033[31m%s:%s' \"$FUX_TEST\" \"$PWD\"; sleep 5".into(),
        ],
        env: [("FUX_TEST".to_owned(), "ok".to_owned())].into(),
    });
    let pane = match reply {
        Reply::Completed {
            result: CommandResult::Pane { pane },
            ..
        } => pane,
        other => panic!("unexpected reply: {other:?}"),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if session.snapshot().pane(PaneId(pane)).is_some_and(|pane| {
            pane.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("ok")
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (plain, _) = control.dispatch(Request::Capture {
        id: 21,
        pane,
        attrs: false,
        scrollback: 0,
        max_bytes: 100,
    });
    let (styled, _) = control.dispatch(Request::Capture {
        id: 22,
        pane,
        attrs: true,
        scrollback: 0,
        max_bytes: 100,
    });
    let captured = |reply| match reply {
        Reply::Completed {
            result: CommandResult::Capture { text },
            ..
        } => text,
        other => panic!("unexpected capture: {other:?}"),
    };
    let plain = captured(plain);
    let styled = captured(styled);
    assert!(plain.len() <= 12 && styled.len() <= 12);
    assert!(!plain.contains("\x1b[") && styled.contains("\x1b["));
    let (limited, _) = control.dispatch(Request::Split {
        id: 23,
        axis: fux::control::Axis::Horizontal,
        target: None,
        argv: vec!["/bin/cat".into()],
        env: Default::default(),
    });
    assert!(matches!(limited, Reply::Failed { .. }));
    assert!(matches!(
        control
            .dispatch(Request::SetStatus {
                id: 24,
                segment: "ci".into(),
                text: "running".into(),
            })
            .0,
        Reply::Completed { .. }
    ));
    assert!(matches!(
        control
            .dispatch(Request::SetStatus {
                id: 25,
                segment: "ci".into(),
                text: String::new(),
            })
            .0,
        Reply::Completed { .. }
    ));
    assert!(!session.snapshot().metadata().status.contains_key("ci"));
    let summary = control.summary("test".into());
    let listed = summary
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .find(|item| item.id == pane)
        .expect("listed pane");
    assert_eq!(listed.cwd, PathBuf::from("/tmp"));
    assert_eq!(listed.command.first().map(String::as_str), Some("/bin/sh"));
    assert!(listed.pid.is_some());
    session.shutdown();
}

#[test]
fn shared_control_exports_async_pty_output_agent_title_and_close_events() {
    #[derive(Clone)]
    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<fux::control::Event>>>);
    impl fux::host::WorkspaceEventSink for Sink {
        fn publish(&self, event: fux::control::Event) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
        }
    }
    let command = vec!["/bin/sh".into(), "-c".into(), "sleep 0.1; printf '\\033]2;agent-title\\007\\033]7877;state=blocked;agent=codex;seq=1;visible=blocker;exited=0\\033\\\\\\033]7877;state=blocked;agent=codex;seq=1;visible=blocker;exited=0\\033\\\\\\033]7877;state=idle;agent=codex;seq=2;visible=idle;exited=0\\033\\\\'; exit 0".into()];
    let (mut session, control) = WorkspaceHost::shared(command, 32, None).expect("workspace");
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    control.set_event_sink(std::sync::Arc::new(Sink(std::sync::Arc::clone(&events))));
    session.attach_notify(ChangeSignal::default());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output = events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneOutput { .. }));
        let agent = events
            .iter()
            .any(|event| matches!(event, fux::control::Event::AgentState { .. }));
        let closed = events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneClosed { .. }));
        if output && agent && closed {
            break;
        }
        drop(events);
        assert!(Instant::now() < deadline, "async event deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let transitions: Vec<_> = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(|event| match event {
            fux::control::Event::AgentState {
                old_state,
                new_state,
                ..
            } => Some((*old_state, *new_state)),
            _ => None,
        })
        .collect();
    assert_eq!(
        transitions,
        [
            (
                fux::control::AgentStatus::None,
                fux::control::AgentStatus::Blocked
            ),
            (
                fux::control::AgentStatus::Blocked,
                fux::control::AgentStatus::Idle
            )
        ]
    );
    session.shutdown();
}

#[test]
fn prefix_mutations_publish_authoritative_open_focus_and_close_events() {
    #[derive(Clone)]
    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<fux::control::Event>>>);
    impl fux::host::WorkspaceEventSink for Sink {
        fn publish(&self, event: fux::control::Event) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
        }
    }
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 0, None).expect("host");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    control.set_event_sink(Arc::new(Sink(Arc::clone(&events))));
    session.attach_notify(ChangeSignal::default());
    session.input(b"\x01|");
    session.input(b"\x01h");
    session.input(b"\x01x");
    let events = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneOpened { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneFocused { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneClosed { .. }))
    );
    drop(events);
    session.shutdown();
}

#[test]
fn fast_control_child_events_are_opened_then_output_agent_then_closed_once() {
    #[derive(Clone)]
    struct Sink(Arc<std::sync::Mutex<Vec<fux::control::Event>>>);
    impl fux::host::WorkspaceEventSink for Sink {
        fn publish(&self, event: fux::control::Event) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
        }
    }
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 0, None).expect("host");
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    control.set_event_sink(Arc::new(Sink(Arc::clone(&events))));
    session.attach_notify(ChangeSignal::default());
    let report = "\\033]7877;state=blocked;agent=fast;seq=1;visible=blocker;exited=0\\033\\\\";
    let script = format!("printf 'OUT{report}{report}X'; exit 4");
    let (reply, _) = control.dispatch(fux::control::Request::New {
        id: 700,
        cwd: None,
        argv: vec!["/bin/sh".into(), "-c".into(), script],
        env: Default::default(),
    });
    let pane = match reply {
        fux::control::Reply::Completed {
            result: fux::control::CommandResult::Pane { pane },
            ..
        } => pane,
        other => panic!("new pane failed: {other:?}"),
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|event| matches!(event, fux::control::Event::PaneClosed { pane: id, .. } if *id == pane)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "event deadline: {:?}",
            events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let events: Vec<_> = events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|event| match event {
            fux::control::Event::PaneOpened { pane: id, .. }
            | fux::control::Event::PaneOutput { pane: id, .. }
            | fux::control::Event::AgentState { pane: id, .. }
            | fux::control::Event::PaneClosed { pane: id, .. } => *id == pane,
            _ => false,
        })
        .cloned()
        .collect();
    assert!(matches!(
        events.first(),
        Some(fux::control::Event::PaneOpened { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(fux::control::Event::PaneClosed { .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, fux::control::Event::PaneOpened { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, fux::control::Event::PaneClosed { .. }))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, fux::control::Event::PaneOutput { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, fux::control::Event::AgentState { .. })),
        "events: {events:?}"
    );
    session.shutdown();
}

#[test]
fn successful_control_mutation_pulses_the_change_signal() {
    // Phase F2 control integration: successful mutations wake attached transport loops.
    use fux::control::{Reply, Request};
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("workspace");
    let changed = ChangeSignal::default();
    let receiver = changed.subscribe();
    session.attach_notify(changed);
    let (reply, _) = control.dispatch(Request::SetStatus {
        id: 91,
        segment: "test".into(),
        text: "ready".into(),
    });
    assert!(matches!(reply, Reply::Completed { id: 91, .. }));
    assert!(receiver.has_changed().expect("change sender alive"));
    session.shutdown();
}

#[test]
fn configured_external_binding_receives_context_with_secret_environment_scrubbed() {
    use std::collections::BTreeMap;
    let output = std::path::Path::new("/tmp").join(format!("fux-binding-{}", std::process::id()));
    let script = format!(
        "printf '%s|%s|%s|%s|%s' \"$FUX_PANE\" \"$FUX_SOCKET\" \"$FUX_CWD\" \"${{FUX_SECRET-unset}}\" \"${{KOH_SECRET-unset}}\" > {}",
        output.display()
    );
    let mut config = fux::config::Config::default();
    config.prefix = "C-b".into();
    config.bindings = BTreeMap::from([(
        "e".into(),
        fux::config::Binding::External {
            external: fux::config::Command::new(vec!["/bin/sh".into(), "-c".into(), script])
                .expect("command"),
        },
    )]);
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("workspace");
    control
        .configure_bindings(&config, "/tmp/work.sock".into())
        .expect("bindings");
    let _ = control.dispatch(fux::control::Request::New {
        id: 77,
        cwd: Some(PathBuf::from("/tmp")),
        argv: vec!["/bin/cat".into()],
        env: Default::default(),
    });
    // SAFETY is unnecessary: avoid mutating process env; inherited fux/koh variables are tested by
    // the supervisor's pure scrub test, while the child verifies they are absent here.
    session.input(b"\x02e");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.exists() {
        assert!(Instant::now() < deadline, "binding deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let value = std::fs::read_to_string(&output).expect("binding output");
    let fields: Vec<_> = value.split('|').collect();
    assert_eq!(fields.first().copied(), Some("2"));
    assert_eq!(fields.get(1).copied(), Some("/tmp/work.sock"));
    assert_eq!(fields.get(2).copied(), Some("/tmp"));
    assert_eq!(fields.get(3).copied(), Some("unset"));
    assert_eq!(fields.get(4).copied(), Some("unset"));
    std::fs::remove_file(output).expect("cleanup");
    session.shutdown();
}

#[cfg(unix)]
#[test]
fn shutdown_kills_and_joins_hung_external_binding_group() {
    use std::collections::BTreeMap;
    let marker = std::env::temp_dir().join(format!("fux-binding-pid-{}", std::process::id()));
    let script = format!("printf '%s' $$ > '{}'; sleep 60 & wait", marker.display());
    let mut config = fux::config::Config::default();
    config.bindings = BTreeMap::from([(
        "e".into(),
        fux::config::Binding::External {
            external: fux::config::Command::new(vec!["/bin/sh".into(), "-c".into(), script])
                .expect("command"),
        },
    )]);
    let (mut session, control) =
        WorkspaceHost::shared(vec!["/bin/cat".into()], 32, None).expect("workspace");
    control
        .configure_bindings(&config, "/tmp/work.sock".into())
        .expect("bindings");
    session.input(b"\x01e");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "binding did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(&marker).expect("pid");
    control.shutdown();
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .status()
            .expect("kill probe")
            .success()
    );
    let _ = std::fs::remove_file(marker);
}

#[test]
fn router_command_table_and_unknown_sequences_are_lossless() {
    let mut router = InputRouter::new(0x01, 25);
    let actions = router.feed(b"a\x01|b\x01h\x01\x01\x1b[A\x01!", 0);
    let actions = [actions, router.flush_timeout(30)].concat();
    let (bytes, commands, mouse) = flattened(&actions);
    assert_eq!(bytes, b"ab\x01\x1b[A\x01!");
    assert_eq!(
        commands,
        vec![Command::SplitHorizontal, Command::Focus(Direction::Left)]
    );
    assert!(mouse.is_empty());
}

#[test]
fn router_is_streaming_at_every_byte_boundary() {
    let input = b"pre\x1b[200~paste\x01|\x1b[201~post\x1b[<0;12;7M";
    let mut expected_router = InputRouter::new(0x01, 25);
    let expected = expected_router.feed(input, 0);
    for boundary in 0..=input.len() {
        let mut router = InputRouter::new(0x01, 25);
        let first = input.get(..boundary).unwrap_or_default();
        let second = input.get(boundary..).unwrap_or_default();
        let actions = [router.feed(first, 0), router.feed(second, 1)].concat();
        assert_eq!(
            flattened(&actions),
            flattened(&expected),
            "boundary {boundary}"
        );
    }
    let (_, commands, mouse) = flattened(&expected);
    assert!(commands.is_empty(), "prefix inside paste is verbatim");
    assert_eq!(
        mouse,
        vec![MouseEvent {
            code: 0,
            column: 12,
            row: 7,
            release: false
        }]
    );
}

#[test]
fn router_timeout_releases_partial_escape() {
    let mut router = InputRouter::new(0x01, 10);
    assert!(router.feed(b"\x1b[", 4).is_empty());
    assert!(router.flush_timeout(13).is_empty());
    assert_eq!(flattened(&router.flush_timeout(14)).0, b"\x1b[");
}

#[cfg(unix)]
#[test]
fn host_timer_forwards_ambiguous_escape_without_another_input_chunk() {
    // Phase F2 input routing: the host owns the ambiguity deadline and wakes without new input.
    let mut host = host_for_script(
        "stty raw -echo; dd bs=1 count=2 2>/dev/null | od -An -tx1; sleep 1",
        32,
    );
    host.input(b"\x1b[");
    wait_until(&mut host, Duration::from_secs(2), |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            pane.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == ["1b", "5b"])
        })
    });
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn prefix_tab_and_overlay_commands_mutate_shared_state() {
    // Phase F2 prefix table: tab navigation and picker/help commands are observable behavior.
    let mut host = host_for_script("cat", 32);
    host.input(b"\x01t");
    assert_eq!(host.snapshot().tabs().len(), 2);
    assert_eq!(host.snapshot().active_tab(), Some(fux::state::TabId(2)));
    host.input(b"\x01n");
    assert_eq!(host.snapshot().active_tab(), Some(fux::state::TabId(1)));
    host.input(b"\x01p");
    assert_eq!(host.snapshot().active_tab(), Some(fux::state::TabId(2)));
    host.input(b"\x01?");
    assert!(host.snapshot().metadata().status.contains_key("help"));
    host.input(b"\x01s");
    assert!(host.snapshot().metadata().status.contains_key("picker"));
    host.shutdown();
}

#[test]
fn every_recognized_router_shape_survives_every_chunk_boundary() {
    let cases: &[&[u8]] = &[
        b"\x01|",
        b"\x01-",
        b"\x01h",
        b"\x01j",
        b"\x01k",
        b"\x01l",
        b"\x01x",
        b"\x01c",
        b"\x01t",
        b"\x01n",
        b"\x01p",
        b"\x01z",
        b"\x01[",
        b"\x01d",
        b"\x01s",
        b"\x01?",
        b"\x1b[200~literal\x01x\x1b[201~",
        b"\x1b[<64;9;4M",
        b"\x1b[<4;2;3M",
        b"\x1b[<4;5;3m",
        b"\x1b[<0;2;3m",
        b"\x1b[unknown",
    ];
    for input in cases {
        let mut whole = InputRouter::new(0x01, 25);
        let expected = flattened(&whole.feed(input, 0));
        for boundary in 0..=input.len() {
            let mut split = InputRouter::new(0x01, 25);
            let actions = [
                split.feed(input.get(..boundary).unwrap_or_default(), 0),
                split.feed(input.get(boundary..).unwrap_or_default(), 1),
                split.flush_timeout(30),
            ]
            .concat();
            let mut expected_actions = whole.clone().flush_timeout(30);
            let mut expected_combined = expected.clone();
            let flushed = flattened(&expected_actions);
            expected_combined.0.extend(flushed.0);
            expected_combined.1.extend(flushed.1);
            expected_combined.2.extend(flushed.2);
            assert_eq!(
                flattened(&actions),
                expected_combined,
                "input={input:?} boundary={boundary}"
            );
            expected_actions.clear();
        }
    }
}

#[cfg(unix)]
fn host_for_script(script: &str, scrollback: usize) -> WorkspaceHost {
    let mut host = WorkspaceHost::spawn(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        scrollback,
        Some(PathBuf::from("/definitely/not/zor")),
    )
    .expect("spawn host");
    host.attach_notify(ChangeSignal::default());
    host
}

#[cfg(unix)]
fn wait_until(
    host: &mut WorkspaceHost,
    timeout: Duration,
    predicate: impl Fn(&fux::state::WorkspaceState) -> bool,
) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let state = host.snapshot();
        if predicate(&state) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let state = host.snapshot();
    let text = state.pane(PaneId(1)).map(|pane| {
        pane.cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
    });
    assert!(
        predicate(&state),
        "host state did not converge before timeout; pane={text:?}"
    );
}

#[cfg(unix)]
#[test]
fn real_child_receives_da_and_dsr_replies() {
    let mut host = host_for_script(
        r"stty raw -echo; printf '\033[6n\033[c'; od -An -N16 -tx1; sleep 1",
        32,
    );
    wait_until(&mut host, Duration::from_secs(3), |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            let text: String = pane.cells.iter().map(|cell| cell.text.as_str()).collect();
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            normalized.contains("1b 5b 31 3b 31 52")
                && normalized.contains("1b 5b 3f 36 32 3b 31 3b 36 63")
        })
    });
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn real_child_osc_updates_agent_and_duplicate_is_idempotent() {
    let report = "\\033]7877;state=blocked;agent=test;seq=7;visible=blocker;exited=0\\033\\\\";
    let changed = "\\033]7877;state=idle;agent=test;seq=7;visible=idle;exited=0\\033\\\\";
    let script = format!("printf '{report}{report}{changed}'; sleep 0.2");
    let mut host = host_for_script(&script, 32);
    wait_until(&mut host, Duration::from_secs(3), |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            pane.agent.state == AgentState::Idle
                && pane.agent.sequence == 7
                && pane.agent.flags.idle
        })
    });
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn copy_mode_command_is_shared_and_yanks_into_workspace_clipboard() {
    let mut host = host_for_script("printf abc; sleep 2", 32);
    wait_until(&mut host, Duration::from_secs(2), |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            pane.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("abc")
        })
    });
    host.input(b"\x01[");
    host.input(b"u");
    assert_eq!(
        host.snapshot()
            .pane(PaneId(1))
            .map(|pane| pane.viewport_offset),
        Some(3)
    );
    host.input(b" ");
    host.input(b"h");
    host.input(b"y");
    assert!(!host.snapshot().metadata().clipboard_base64.is_empty());
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn viewport_offset_rebuilds_cells_from_bounded_terminal_scrollback() {
    // Phase F2 copy mode: synchronized viewport cells come from the cloned history screen.
    let mut host = host_for_script("seq 1 30; sleep 2", 32);
    host.resize(ClientId::next(), 6, 20);
    wait_until(&mut host, Duration::from_secs(2), |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            pane.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("30")
        })
    });
    let live = host.snapshot().pane(PaneId(1)).cloned().expect("live pane");
    host.input(b"\x01[");
    host.input(b"u");
    let history = host
        .snapshot()
        .pane(PaneId(1))
        .cloned()
        .expect("history pane");
    assert_eq!(history.viewport_offset, 3);
    assert_ne!(history.cells, live.cells);
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn bell_clipboard_and_scrollback_survive_child_exit_and_clamp() {
    let mut host = host_for_script(r"printf '\a\033]52;c;aGVsbG8=\a'; seq 1 80; sleep 1", 10);
    wait_until(&mut host, Duration::from_secs(3), |state| {
        state.metadata().bell_count == 1 && state.metadata().clipboard_base64 == "aGVsbG8="
    });
    let capture = host.capture(PaneId(1), usize::MAX);
    assert!(capture.is_some());
    let state = host.snapshot();
    assert_eq!(state.metadata().bell_count, 1);
    assert_eq!(state.metadata().clipboard_base64, "aGVsbG8=");
    host.input(b"\x01x");
    let closed = host.snapshot();
    assert!(closed.pane(PaneId(1)).is_none());
    assert_eq!(closed.metadata().bell_count, 1);
    assert_eq!(closed.metadata().clipboard_base64, "aGVsbG8=");
    host.shutdown();
}

#[cfg(unix)]
#[test]
fn resize_and_detach_keep_safe_pty_geometry() {
    let mut host = host_for_script("trap 'stty size' WINCH; sleep 1", 0);
    host.input(b"\x01|");
    wait_until(&mut host, Duration::from_secs(2), |state| {
        state.panes().len() == 2
    });
    host.resize(ClientId::next(), 0, 0);
    let latest = ClientId::next();
    host.resize(latest, 40, 100);
    host.client_detached(latest);
    std::thread::sleep(Duration::from_millis(100));
    let state = host.snapshot();
    assert_eq!(state.panes().len(), 2);
    assert!(
        state
            .panes()
            .values()
            .all(|pane| pane.rows == 2 && pane.columns == 2)
    );
    host.shutdown();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_host_round_trips_through_hosts_and_run_client() {
    let provider = SharedHost::new(|| {
        WorkspaceHost::spawn(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf LOOPBACK_FUX; sleep 0.2; exit 7".into(),
            ],
            32,
            None,
        )
    });
    let hosts = Arc::new(Hosts::new().with(fux::FUX_ALPN, provider));
    let server = match bind_endpoint_local_alpns(generate_secret_key(), hosts.alpns()).await {
        Ok(server) => server,
        Err(error) if format!("{error:#}").contains("Operation not permitted") => return,
        Err(error) => panic!("bind server: {error:#}"),
    };
    let address = loopback_addr(&server);
    let accept = {
        let hosts = hosts.clone();
        tokio::spawn(async move {
            while let Some(incoming) = server.accept().await {
                let hosts = hosts.clone();
                tokio::spawn(async move {
                    if let Ok(connection) = incoming.await {
                        hosts.serve_connection(connection).await;
                    }
                });
            }
        })
    };
    let endpoint = bind_endpoint_local(generate_secret_key(), false)
        .await
        .expect("bind client");
    let connector = IrohConnector::with_alpn(endpoint, address, fux::FUX_ALPN);
    let channel = connector.connect().await.expect("connect workspace");
    let (_input_tx, input_rx) = mpsc::channel::<Vec<u8>>(4);
    let (_resize_tx, resize_rx) = mpsc::channel::<()>(1);
    let terminal =
        fux::client::WorkspaceTerminal::enter(fux::client::CaptureBackend::new(30, 100), false)
            .expect("terminal");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run_client(
            channel,
            connector,
            DisplayPreference::Never,
            (30, 100),
            input_rx,
            resize_rx,
            terminal,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("client timeout")
    .expect("run client");
    assert_eq!(result, Some(7));
    accept.abort();
}

#[cfg(unix)]
struct SemanticTerminal {
    latest: Arc<std::sync::Mutex<WorkspaceState>>,
}

#[cfg(unix)]
impl ClientTerminal<WorkspaceState> for SemanticTerminal {
    fn render(
        &mut self,
        state: &WorkspaceState,
        _overlay: &Overlay,
        _status: Option<&str>,
    ) -> std::io::Result<()> {
        *self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state.clone();
        Ok(())
    }

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok((24, 80))
    }
}

// Golden path 10: force the first real QUIC connection down while retaining the
// authoritative WorkspaceHost, then prove the reconnect converges to that same state.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_viewer_reconnects_after_forced_loopback_loss_without_state_reset() {
    let provider = SharedHost::new(|| WorkspaceHost::spawn(vec!["/bin/cat".into()], 32, None));
    let hosts = Arc::new(Hosts::new().with(fux::FUX_ALPN, provider));
    let server = match bind_endpoint_local_alpns(generate_secret_key(), hosts.alpns()).await {
        Ok(server) => server,
        Err(error) if format!("{error:#}").contains("Operation not permitted") => return,
        Err(error) => panic!("bind server: {error:#}"),
    };
    let address = loopback_addr(&server);
    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
    let accept = {
        let hosts = Arc::clone(&hosts);
        tokio::spawn(async move {
            let mut first_drop = Some(drop_rx);
            while let Some(incoming) = server.accept().await {
                let hosts = Arc::clone(&hosts);
                let drop = first_drop.take();
                tokio::spawn(async move {
                    if let Ok(connection) = incoming.await {
                        if let Some(drop) = drop {
                            let victim = connection.clone();
                            tokio::spawn(async move {
                                if drop.await.is_ok() {
                                    IrohChannel::new(victim).close(0, b"deterministic loss");
                                }
                            });
                        }
                        hosts.serve_connection(connection).await;
                    }
                });
            }
        })
    };

    let endpoint = bind_endpoint_local(generate_secret_key(), false)
        .await
        .expect("bind reconnecting client");
    let connector = IrohConnector::with_alpn(endpoint, address, fux::FUX_ALPN);
    let channel = connector.connect().await.expect("initial connection");
    let latest = Arc::new(std::sync::Mutex::new(WorkspaceState::default()));
    let terminal = SemanticTerminal {
        latest: Arc::clone(&latest),
    };
    let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(16);
    let (_resize_tx, resize_rx) = mpsc::channel::<()>(2);
    let shutdown = CancellationToken::new();
    let client_shutdown = shutdown.clone();
    let client = tokio::spawn(async move {
        run_client(
            channel,
            connector,
            DisplayPreference::Never,
            (24, 80),
            input_rx,
            resize_rx,
            terminal,
            client_shutdown,
        )
        .await
    });

    input_tx
        .send(b"MARK_ONE".to_vec())
        .await
        .expect("first input");
    wait_for_semantic_text(&latest, "MARK_ONE").await;
    let before = latest
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    drop_tx.send(()).expect("drop first connection");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let _ = input_tx.send(b"MARK_TWO".to_vec()).await;
        let current = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if visible_workspace_text(&current).contains("MARK_TWO") {
            assert!(
                visible_workspace_text(&current).contains("MARK_ONE"),
                "reconnect replaced rather than retained the authoritative workspace"
            );
            assert_eq!(current.tabs(), before.tabs());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reconnect deadline expired"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    shutdown.cancel();
    drop(input_tx);
    tokio::time::timeout(Duration::from_secs(3), client)
        .await
        .expect("client cleanup deadline")
        .expect("client task")
        .expect("client loop");
    accept.abort();
}

#[cfg(unix)]
async fn wait_for_semantic_text(latest: &Arc<std::sync::Mutex<WorkspaceState>>, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let state = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if visible_workspace_text(&state).contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "semantic frame deadline expired"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
fn visible_workspace_text(state: &WorkspaceState) -> String {
    state
        .panes()
        .values()
        .flat_map(|pane| pane.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect()
}
