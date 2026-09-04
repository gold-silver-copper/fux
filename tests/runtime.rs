#![allow(dead_code, clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/runtime/mod.rs"]
mod runtime;

use fux::control::{CommandResult, Event, EventKind, Reply, Request};
use runtime::{ControlHandler, EventHub};
use std::fs;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[test]
fn terminal_workspace_retirement_waits_for_clients_with_a_bounded_fallback() {
    let empty_since = Instant::now();
    assert!(!runtime::terminal_workspace_retirement_due(
        empty_since,
        0,
        empty_since + runtime::FINAL_STATE_MIN_GRACE - Duration::from_millis(1),
    ));
    assert!(runtime::terminal_workspace_retirement_due(
        empty_since,
        0,
        empty_since + runtime::FINAL_STATE_MIN_GRACE,
    ));
    assert!(!runtime::terminal_workspace_retirement_due(
        empty_since,
        1,
        empty_since + runtime::FINAL_STATE_MIN_GRACE,
    ));
    assert!(runtime::terminal_workspace_retirement_due(
        empty_since,
        1,
        empty_since + runtime::FINAL_STATE_MAX_GRACE,
    ));
}

struct Echo;
impl ControlHandler for Echo {
    fn handle(&self, request: Request) -> Reply {
        Reply::Completed {
            id: request.id(),
            result: CommandResult::Unit,
        }
    }
}

struct OversizedReply;
impl ControlHandler for OversizedReply {
    fn handle(&self, request: Request) -> Reply {
        Reply::Completed {
            id: request.id(),
            result: CommandResult::Capture {
                text: "x".repeat(fux::control::MAX_FRAME_BYTES + 1),
            },
        }
    }
}

#[test]
fn oversized_capture_or_listing_returns_a_structured_error_without_disconnect() {
    let root = temp_root("bounded-reply");
    fs::create_dir(&root).expect("runtime root");
    let socket = fux::control::bind_control_socket(&root, "work").expect("bind");
    let path = socket.path().to_owned();
    let stop = Arc::new(AtomicBool::new(false));
    let task = runtime::serve_control(
        socket,
        Arc::new(OversizedReply),
        EventHub::default(),
        Arc::clone(&stop),
    )
    .expect("serve");
    let reply = runtime::request(&path, &Request::List { id: 88 }).expect("bounded error");
    assert!(matches!(
        reply,
        Reply::Failed {
            id: 88,
            error: fux::control::ReplyError {
                code: fux::control::ErrorCode::FrameTooLarge,
                ..
            }
        }
    ));
    stop.store(true, std::sync::atomic::Ordering::Release);
    task.join().expect("join");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn control_server_echoes_ids_and_streams_filtered_events_in_order() {
    let root = temp_root("control");
    fs::create_dir(&root).expect("runtime root");
    let socket = fux::control::bind_control_socket(&root, "work").expect("bind");
    let path = socket.path().to_owned();
    let events = EventHub::default();
    let stop = Arc::new(AtomicBool::new(false));
    let task = runtime::serve_control(socket, Arc::new(Echo), events.clone(), Arc::clone(&stop))
        .expect("serve");
    let reply = runtime::request(&path, &Request::List { id: 41 }).expect("request");
    assert_eq!(reply.id(), 41);

    let mut stream = UnixStream::connect(&path).expect("subscribe");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("timeout");
    fux::control::write_frame(
        &mut stream,
        &Request::Subscribe {
            id: 7,
            events: vec![EventKind::PaneFocused],
        },
    )
    .expect("write");
    let accepted: serde_json::Value = read_frame(&mut stream);
    assert_eq!(accepted.get("id"), Some(&serde_json::json!(7)));
    events.publish(Event::PaneOutput { id: 0, pane: 2 });
    events.publish(Event::PaneFocused { id: 0, pane: 3 });
    let event: serde_json::Value = read_frame(&mut stream);
    assert_eq!(event.get("id"), Some(&serde_json::json!(7)));
    assert_eq!(event.get("pane"), Some(&serde_json::json!(3)));
    drop(stream);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while events.subscriber_count() != 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(events.subscriber_count(), 0);
    stop.store(true, std::sync::atomic::Ordering::Release);
    task.join().expect("join");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn reload_is_transactional_and_environment_scrubs_secrets() {
    let root = temp_root("config");
    fs::create_dir(&root).expect("root");
    let path = root.join("config.toml");
    fs::write(&path, "prefix = 'C-b'\n").expect("config");
    let live = runtime::LiveConfig::load(path.clone()).expect("load");
    assert_eq!(live.snapshot().prefix, "C-b");
    fs::write(&path, "unknown = true\n").expect("bad config");
    assert!(live.reload().is_err());
    assert_eq!(live.snapshot().prefix, "C-b");
    fs::write(
        &path,
        "prefix = 'C-b'\n[notifications]\nenabled = false\nnotify-blocked = true\nnotify-idle = true\nremote-clients = true\n",
    )
    .expect("mutable config");
    assert!(!live.reload().expect("mutable reload").notifications.enabled);
    fs::write(
        &path,
        "prefix = 'C-b'\ndefault-command = { argv = ['/bin/false'] }\n",
    )
    .expect("restart config");
    assert!(
        live.reload()
            .expect_err("restart required")
            .to_string()
            .contains("restart")
    );
    assert_eq!(live.snapshot().prefix, "C-b");
    let clean = runtime::scrub_environment([
        ("PATH".into(), "/bin".into()),
        ("FUX_TOKEN".into(), "secret".into()),
        ("KOH_KEY_PASSPHRASE".into(), "secret".into()),
    ]);
    assert!(clean.contains_key(std::ffi::OsStr::new("PATH")));
    assert_eq!(clean.len(), 1);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn top_level_cli_alias_uses_the_control_socket_without_a_tty() {
    let root = temp_root("cli");
    fs::create_dir(&root).expect("root");
    let path = root.join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("listener");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let request = fux::control::read_request(&mut stream)
            .expect("request")
            .expect("frame");
        assert!(matches!(request, Request::List { id: 1 }));
        fux::control::write_frame(
            &mut stream,
            &Reply::Completed {
                id: 1,
                result: CommandResult::Unit,
            },
        )
        .expect("reply");
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fux"))
        .arg("list")
        .env("FUX_SOCKET", &path)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run fux");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Reply = serde_json::from_slice(&output.stdout).expect("CLI JSON reply");
    assert_eq!(reply.id(), 1);
    server.join().expect("server join");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn one_shot_control_rpc_has_a_bounded_read_deadline() {
    let root = temp_root("rpc-deadline");
    fs::create_dir(&root).expect("root");
    let path = root.join("hung.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("listener");
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept");
        std::thread::sleep(Duration::from_secs(3));
    });
    let started = std::time::Instant::now();
    assert!(runtime::request(&path, &Request::List { id: 4 }).is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
    server.join().expect("join");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn cli_aliases_forward_optional_control_arguments_and_empty_default_command() {
    type RequestCheck = Box<dyn Fn(Request) + Send>;
    let cases: Vec<(&[&str], RequestCheck)> = vec![
        (
            &["new", "--cwd", "/tmp"],
            Box::new(|request| match request {
                Request::New { cwd, argv, .. } => {
                    assert_eq!(cwd, Some("/tmp".into()));
                    assert!(argv.is_empty());
                }
                other => panic!("unexpected request: {other:?}"),
            }),
        ),
        (
            &["split", "h", "--target", "7", "--", "/bin/sh"],
            Box::new(|request| match request {
                Request::Split { target, argv, .. } => {
                    assert_eq!(target, Some(7));
                    assert_eq!(argv, ["/bin/sh"]);
                }
                other => panic!("unexpected request: {other:?}"),
            }),
        ),
        (
            &["capture", "3", "--attrs", "--scrollback", "42"],
            Box::new(|request| match request {
                Request::Capture {
                    attrs, scrollback, ..
                } => {
                    assert!(attrs);
                    assert_eq!(scrollback, 42);
                }
                other => panic!("unexpected request: {other:?}"),
            }),
        ),
        (
            &["popup", "--size", "90x30", "--", "/bin/true"],
            Box::new(|request| match request {
                Request::Popup {
                    rows, cols, argv, ..
                } => {
                    assert_eq!((cols, rows), (Some(90), Some(30)));
                    assert_eq!(argv, ["/bin/true"]);
                }
                other => panic!("unexpected request: {other:?}"),
            }),
        ),
    ];
    for (index, (arguments, check)) in cases.into_iter().enumerate() {
        let root = temp_root(&format!("cli-options-{index}"));
        fs::create_dir(&root).expect("root");
        let path = root.join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = fux::control::read_request(&mut stream)
                .expect("request")
                .expect("frame");
            check(request);
            fux::control::write_frame(
                &mut stream,
                &Reply::Completed {
                    id: 1,
                    result: CommandResult::Unit,
                },
            )
            .expect("reply");
        });
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_fux"))
            .args(arguments)
            .env("FUX_SOCKET", &path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run alias");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        server.join().expect("server");
        fs::remove_dir_all(root).expect("cleanup");
    }
}

#[test]
fn raw_ctl_workspace_request_routes_to_the_manager_socket() {
    let root = temp_root("manager-cli");
    let runtime_dir = root.join("run/fux");
    let state = root.join("state");
    fs::create_dir_all(&runtime_dir).expect("runtime");
    fs::create_dir_all(&state).expect("state");
    let socket = runtime_dir.join("manager.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("manager");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        assert!(matches!(
            runtime::read_manager_request(&mut stream).expect("request"),
            runtime::ManagerRequest::List
        ));
        runtime::write_manager_reply(
            &mut stream,
            &runtime::ManagerReply::Pick {
                names: vec!["one".into(), "two".into()],
            },
        )
        .expect("reply");
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fux"))
        .args([
            "ctl",
            r#"{"command":"workspace","id":9,"action":{"list":{}}}"#,
        ])
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", state)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run ctl");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().expect("join");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_picker_selects_between_two_manager_workspace_names() {
    let names = vec!["alpha".to_owned(), "beta".to_owned()];
    assert_eq!(
        runtime::select_workspace(&names, "2\n").expect("pick"),
        "beta"
    );
    assert!(runtime::select_workspace(&names, "3").is_err());
}

#[test]
fn notifier_fires_only_on_meaningful_transitions_and_selects_platform_tools() {
    let policy = fux::config::NotificationPolicy::default();
    let mut gate = runtime::NotificationGate::default();
    use fux::control::AgentStatus::{Blocked, Idle, Working};
    assert!(!gate.observe(1, Idle, &policy));
    assert!(!gate.observe(1, Working, &policy));
    assert!(gate.observe(1, Blocked, &policy));
    assert!(!gate.observe(1, Blocked, &policy));
    assert!(gate.observe(1, Idle, &policy));
    assert!(gate.observe(2, Blocked, &policy));
    gate.remove(1);
    gate.remove(2);
    assert_eq!(gate.tracked_count(), 0);
    let command = runtime::notification_command("fux", "blocked", false, true, false, |name| {
        name == "termux-notification"
    })
    .expect("notifier");
    assert_eq!(
        command.first().map(String::as_str),
        Some("termux-notification")
    );
    assert!(runtime::notification_command("fux", "idle", false, false, false, |_| true).is_none());
}

#[test]
fn hung_notifier_is_killed_and_reaped_during_shutdown() {
    use std::os::unix::process::CommandExt as _;
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .process_group(0)
        .spawn()
        .expect("notifier fixture");
    let stop = AtomicBool::new(true);
    let started = std::time::Instant::now();
    runtime::reap_notification(child, &stop);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn hook_supervisor_uses_injected_backoff_and_terminates_on_shutdown() {
    #[derive(Default)]
    struct Clock {
        now: std::sync::Mutex<Duration>,
        sleeps: std::sync::Mutex<Vec<Duration>>,
    }
    impl runtime::HookClock for Clock {
        fn now(&self) -> Duration {
            *self
                .now
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
        fn sleep(&self, stop: &AtomicBool, duration: Duration) {
            self.sleeps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(duration);
            *self
                .now
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += duration;
            if self
                .sleeps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
                >= 3
            {
                stop.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }
    struct Process;
    impl runtime::HookProcess for Process {
        fn exited(&mut self) -> bool {
            true
        }
        fn terminate(&mut self) {}
    }
    #[derive(Default)]
    struct Runner {
        spawns: std::sync::Mutex<usize>,
    }
    impl runtime::HookCommand for Runner {
        fn spawn(
            &self,
            _hook: &fux::config::Hook,
            _environment: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
        ) -> anyhow::Result<Box<dyn runtime::HookProcess>> {
            *self
                .spawns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            Ok(Box::new(Process))
        }
    }
    let clock = Arc::new(Clock::default());
    let runner = Arc::new(Runner::default());
    let hook = fux::config::Hook {
        name: "test".into(),
        command: fux::config::Command::new(vec!["ignored".into()]).expect("command"),
    };
    let supervisor =
        runtime::HookSupervisor::start_with_clock(&[hook], [], runner.clone(), clock.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while clock
        .sleeps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len()
        < 3
    {
        assert!(std::time::Instant::now() < deadline, "hook deadline");
        std::thread::yield_now();
    }
    supervisor.shutdown();
    assert!(
        *runner
            .spawns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            >= 2
    );
    let sleeps = clock
        .sleeps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(sleeps.first().copied(), Some(Duration::from_millis(200)));
    assert!(
        sleeps
            .windows(2)
            .all(|pair| matches!(pair, [first, second] if second >= first))
    );

    struct BlockingProcess(Arc<std::sync::atomic::AtomicBool>);
    impl runtime::HookProcess for BlockingProcess {
        fn exited(&mut self) -> bool {
            false
        }
        fn terminate(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    struct BlockingRunner {
        spawned: Arc<std::sync::atomic::AtomicBool>,
        terminated: Arc<std::sync::atomic::AtomicBool>,
    }
    impl runtime::HookCommand for BlockingRunner {
        fn spawn(
            &self,
            _hook: &fux::config::Hook,
            _environment: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
        ) -> anyhow::Result<Box<dyn runtime::HookProcess>> {
            self.spawned
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(Box::new(BlockingProcess(Arc::clone(&self.terminated))))
        }
    }
    let spawned = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let runner = Arc::new(BlockingRunner {
        spawned: Arc::clone(&spawned),
        terminated: Arc::clone(&terminated),
    });
    let hook = fux::config::Hook {
        name: "stop".into(),
        command: fux::config::Command::new(vec!["ignored".into()]).expect("command"),
    };
    let supervisor = runtime::HookSupervisor::start(&[hook], [], runner);
    while !spawned.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::yield_now();
    }
    supervisor.shutdown();
    assert!(terminated.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn production_hook_termination_kills_the_entire_process_group() {
    use runtime::HookCommand as _;
    let root = temp_root("hook-process-group");
    fs::create_dir(&root).expect("root");
    let pid_file = root.join("descendant.pid");
    let hook = fux::config::Hook {
        name: "descendants".into(),
        command: fux::config::Command::new(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
        ])
        .expect("command"),
    };
    let mut process = runtime::ProcessHookCommand
        .spawn(&hook, &Default::default())
        .expect("spawn hook");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !pid_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let descendant: i32 = fs::read_to_string(&pid_file)
        .expect("descendant pid")
        .trim()
        .parse()
        .expect("pid");
    process.terminate();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while nix::sys::signal::kill(nix::unistd::Pid::from_raw(descendant), None).is_ok()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(descendant), None).is_err());
    fs::remove_dir_all(root).expect("cleanup");
}

fn read_frame(stream: &mut UnixStream) -> serde_json::Value {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        assert_eq!(stream.read(&mut byte).expect("read"), 1);
        if byte[0] == b'\n' {
            break;
        }
        bytes.push(byte[0]);
    }
    serde_json::from_slice(&bytes).expect("json")
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let unique = format!("fr-{label}-{}", std::process::id());
    std::path::Path::new("/tmp").join(unique)
}
