//! Real-process scenarios against the exact fux binary with the deterministic fixture child as
//! the pane program. Each test owns a private HOME/XDG root and every process it starts.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

fn guard() -> MutexGuard<'static, ()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn fux_binary() -> PathBuf {
    let path = std::env::var_os("FUX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("target/debug/fux")
        });
    assert!(
        path.is_file(),
        "FUX_BIN must name the built fux binary at {}",
        path.display()
    );
    path
}

struct Environment {
    root: PathBuf,
    fixture_listener: UnixListener,
}

impl Environment {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fxb-{:x}-{nonce:x}", std::process::id()));
        for directory in [
            root.clone(),
            root.join("run"),
            root.join("state"),
            root.join("config/fux"),
        ] {
            fs::create_dir_all(&directory).expect("private directory");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .expect("private mode");
        }
        let fixture = env!("CARGO_BIN_EXE_fux-fixture-child");
        let document = format!(
            "default-command = {{ argv = [{:?}, {:?}, \"--deadline-ms=30000\"] }}\nclipboard = \"write-only\"\n",
            fixture,
            format!("--control={}", root.join("fixture.sock").display()),
        );
        fs::write(root.join("config/fux/config.toml"), document).expect("private config");
        let fixture_listener =
            UnixListener::bind(root.join("fixture.sock")).expect("fixture socket");
        fixture_listener
            .set_nonblocking(true)
            .expect("nonblocking fixture socket");
        Self {
            root,
            fixture_listener,
        }
    }

    fn variables(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.root.display().to_string()),
            (
                "XDG_RUNTIME_DIR".into(),
                self.root.join("run").display().to_string(),
            ),
            (
                "XDG_STATE_HOME".into(),
                self.root.join("state").display().to_string(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                self.root.join("config").display().to_string(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("SHELL".into(), "/bin/sh".into()),
        ]
    }

    fn manager_socket(&self) -> PathBuf {
        self.root.join("run/fux/manager.sock")
    }
    fn control_socket(&self, workspace: &str) -> PathBuf {
        self.root.join("run/fux").join(format!("{workspace}.sock"))
    }
    fn attach_socket(&self, workspace: &str) -> PathBuf {
        self.root
            .join("run/fux")
            .join(format!("{workspace}.attach.sock"))
    }
    fn descriptor(&self, workspace: &str) -> PathBuf {
        self.root
            .join("run/fux/workspaces")
            .join(format!("{workspace}.json"))
    }

    fn accept_fixture(&self) -> Jsonl {
        Jsonl::new(accept_before(
            &self.fixture_listener,
            Instant::now() + DEADLINE,
        ))
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(fux_binary())
            .args(arguments)
            .env_clear()
            .envs(self.variables())
            .stdin(Stdio::null())
            .output()
            .expect("run fux binary")
    }

    fn daemon_log(&self) -> String {
        fs::read_to_string(self.root.join("state/fux/daemon.log")).unwrap_or_default()
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("daemon log:\n{}", self.daemon_log());
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Server {
    child: Option<Child>,
}

impl Server {
    fn start(environment: &Environment, workspace: &str) -> Self {
        let child = Command::new(fux_binary())
            .args(["serve", "--name", workspace])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fux server");
        wait_for_path(&environment.manager_socket());
        wait_for_path(&environment.control_socket(workspace));
        Self { child: Some(child) }
    }

    fn terminate(&mut self) {
        if let Some(child) = &self.child {
            let _ = kill(
                Pid::from_raw(i32::try_from(child.id()).expect("pid")),
                Signal::SIGTERM,
            );
        }
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        let child = self.child.as_mut().expect("server");
        loop {
            if let Some(status) = child.try_wait().expect("wait server") {
                self.child.take();
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "server shutdown deadline expired"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A real viewer on a PTY, driven byte by byte and observed through a terminal emulator.
struct TerminalViewer {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
    output: std::sync::Arc<Mutex<Vec<u8>>>,
    rows: u16,
    cols: u16,
}

impl TerminalViewer {
    fn spawn(environment: &Environment, workspace: &str, rows: u16, cols: u16) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("client PTY");
        let mut command = CommandBuilder::new(fux_binary());
        command.arg(workspace);
        command.env_clear();
        for (key, value) in environment.variables() {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn fux viewer");
        drop(pair.slave);
        let mut terminal = pair.master.try_clone_reader().expect("client reader");
        let output = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&output);
        let (done_tx, reader_done) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = terminal.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                let mut captured = captured
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let remaining = 4_usize
                    .saturating_mul(1024 * 1024)
                    .saturating_sub(captured.len());
                captured.extend_from_slice(&chunk[..count.min(remaining)]);
            }
            let _ = done_tx.send(());
        });
        let writer = pair.master.take_writer().expect("client writer");
        Self {
            master: pair.master,
            child,
            writer,
            reader: Some(reader),
            reader_done,
            output,
            rows,
            cols,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("terminal input");
        self.writer.flush().expect("flush");
    }

    fn screen(&self) -> String {
        let bytes = self
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut parser = vt100::Parser::new(self.rows, self.cols, 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    fn wait_for_text(&self, needle: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let screen = self.screen();
            if screen.contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "viewer never painted {needle:?}; screen:\n{screen}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn detach(&mut self) -> u32 {
        self.write(b"\x01d");
        self.wait_exit()
    }

    fn disconnect(&mut self) {
        self.child.kill().expect("kill viewer");
        self.wait_exit();
    }

    fn wait_exit(&mut self) -> u32 {
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("wait viewer") {
                break status;
            }
            assert!(Instant::now() < deadline, "viewer exit deadline expired");
            std::thread::sleep(Duration::from_millis(5));
        };
        if let Some(reader) = self.reader.take() {
            self.reader_done
                .recv_timeout(DEADLINE)
                .expect("viewer reader completion");
            reader.join().expect("viewer reader");
        }
        status.exit_code()
    }

    /// The rendered alternate screen just before the viewer restored the primary screen.
    fn final_screen(&self) -> String {
        const RESTORE: &[u8] = b"\x1b[?1049l";
        let bytes = self
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let end = bytes
            .windows(RESTORE.len())
            .rposition(|window| window == RESTORE)
            .expect("finished viewer restores its primary screen");
        let mut parser = vt100::Parser::new(self.rows, self.cols, 0);
        parser.process(&bytes[..end]);
        parser.screen().contents()
    }

    fn raw(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize viewer pty");
    }
}

impl Drop for TerminalViewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct Jsonl {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        // A short-lived descendant may already have closed its end; macOS then rejects timeout
        // changes even though the queued frame is still readable.
        let _ = stream.set_read_timeout(Some(DEADLINE));
        let _ = stream.set_write_timeout(Some(DEADLINE));
        Self {
            reader: BufReader::new(stream.try_clone().expect("clone stream")),
            writer: stream,
        }
    }
    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.writer, &value).expect("request");
        self.writer.write_all(b"\n").expect("newline");
        self.writer.flush().expect("flush");
    }
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("response");
        assert!(!line.is_empty(), "peer closed before a response");
        serde_json::from_str(&line).expect("response JSON")
    }
    fn expect_silence(&mut self, timeout: Duration) {
        let stream = self.reader.get_mut();
        let _ = stream.set_read_timeout(Some(timeout));
        let mut probe = [0_u8; 1];
        match stream.read(&mut probe) {
            Ok(0) => panic!("peer closed while checking for silence"),
            Ok(_) => panic!("unexpected bytes while expecting silence"),
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "silence probe: {error}"
            ),
        }
        let _ = stream.set_read_timeout(Some(DEADLINE));
    }
}

fn control(environment: &Environment, workspace: &str) -> Jsonl {
    let mut stream =
        UnixStream::connect(environment.control_socket(workspace)).expect("control socket");
    fux::proto::socket::negotiate_client(&mut stream).expect("control preface");
    Jsonl::new(stream)
}

fn accept_before(listener: &UnixListener, deadline: Instant) -> UnixStream {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).expect("blocking stream");
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept fixture: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "fixture readiness deadline expired"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was not created",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_absent(path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    while path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} leaked after shutdown",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_process_absent(pid: i32) {
    let deadline = Instant::now() + DEADLINE;
    while kill(Pid::from_raw(pid), None).is_ok() {
        assert!(Instant::now() < deadline, "process {pid} survived cleanup");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pid_of(value: &Value) -> i32 {
    i32::try_from(value["pid"].as_u64().expect("pid")).expect("pid range")
}

#[test]
fn natural_last_pane_exit_is_observable_before_workspace_retirement() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    let ready = fixture.receive();
    assert_eq!(ready["event"], "ready");
    let mut viewer = TerminalViewer::spawn(&environment, "binary", 24, 80);
    viewer.wait_for_text("binary │");
    fixture.send(json!({"command":"write","chunks_hex":[hex(b"FINAL_BINARY")]}));
    assert_eq!(fixture.receive()["bytes"], 12);
    viewer.wait_for_text("FINAL_BINARY");
    let mut subscriber = control(&environment, "binary");
    subscriber.send(json!({"command":"subscribe","id":93,"events":["pane.closed"]}));
    assert_eq!(subscriber.receive()["status"], "accepted");
    fixture.send(json!({"command":"exit","status":29}));
    assert_eq!(fixture.receive()["event"], "cleanup");
    let event = subscriber.receive();
    assert_eq!(event["event"], "pane.closed");
    assert_eq!(event["exit_status"], 29);
    assert_eq!(event["id"], 93);
    assert_eq!(
        viewer.wait_exit(),
        29,
        "viewer propagates the pane's exit status"
    );
    assert!(
        viewer.final_screen().contains("FINAL_BINARY"),
        "final output painted before restore"
    );
    assert!(
        viewer.final_screen().contains("(exit 29)"),
        "the bar shows the focused pane's exit status"
    );
    assert!(
        server.wait().success(),
        "server exits once its last workspace retired"
    );
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket("binary"));
    wait_for_absent(&environment.attach_socket("binary"));
    assert!(
        !environment.descriptor("binary").exists(),
        "descriptor removed"
    );
}

#[test]
fn detach_and_reattach_preserve_the_pane_process_and_its_history() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    let ready = fixture.receive();
    let pid = pid_of(&ready);
    let mut viewer = TerminalViewer::spawn(&environment, "binary", 24, 80);
    viewer.wait_for_text("binary │");
    fixture.send(json!({"command":"write","chunks_hex":[hex(b"BEFORE_DETACH\r\n")]}));
    fixture.receive();
    viewer.wait_for_text("BEFORE_DETACH");
    // Input reaches the fixture byte-exactly.
    fixture.send(json!({"command":"read_exact","bytes":3}));
    viewer.write(b"abc");
    assert_eq!(fixture.receive()["bytes_hex"], "616263");
    assert_eq!(viewer.detach(), 0);
    // Output while detached is retained in the server.
    fixture.send(json!({"command":"write","chunks_hex":[hex(b"WHILE_DETACHED\r\n")]}));
    fixture.receive();
    let listing = environment.run(&["binary", "list"]);
    let value: Value = serde_json::from_slice(&listing.stdout).expect("listing");
    let pane = &value["result"]["value"]["workspaces"][0]["tabs"][0]["panes"][0];
    assert_eq!(pid_of(pane), pid, "the same process keeps running");
    let mut again = TerminalViewer::spawn(&environment, "binary", 30, 100);
    again.wait_for_text("BEFORE_DETACH");
    again.wait_for_text("WHILE_DETACHED");
    // The fixture observes the renegotiated PTY size for the new viewer.
    fixture.send(json!({"command":"size"}));
    let size = fixture.receive();
    assert_eq!(size["rows"], 29);
    assert_eq!(size["columns"], 100);
    // A hard disconnect (no detach) leaves the pane alive too.
    again.disconnect();
    let listing = environment.run(&["binary", "list"]);
    let value: Value = serde_json::from_slice(&listing.stdout).expect("listing");
    assert_eq!(
        pid_of(&value["result"]["value"]["workspaces"][0]["tabs"][0]["panes"][0]),
        pid
    );
    fixture.send(json!({"command":"quit"}));
    assert_eq!(fixture.receive()["event"], "cleanup");
    assert!(server.wait().success());
}

#[test]
fn forced_close_terminates_descendants_and_reports_the_status() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    let ready = fixture.receive();
    let primary = pid_of(&ready);
    fixture.send(json!({"command":"spawn","mode":"ignore_hup","exit_status":23}));
    let spawned = fixture.receive();
    assert_eq!(spawned["event"], "spawned");
    let descendant = pid_of(&spawned);
    let mut announced = environment.accept_fixture();
    assert_eq!(announced.receive()["event"], "descendant_ready");
    // Open a second pane so the workspace survives the close.
    let split = environment.run(&["binary", "split", "horizontal"]);
    assert!(
        split.status.success(),
        "{}",
        String::from_utf8_lossy(&split.stderr)
    );
    let mut second = environment.accept_fixture();
    assert_eq!(second.receive()["event"], "ready");
    let mut subscriber = control(&environment, "binary");
    subscriber.send(json!({"command":"subscribe","id":95,"events":["pane.closed"]}));
    assert_eq!(subscriber.receive()["status"], "accepted");
    let killed = environment.run(&["binary", "kill", "1"]);
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stdout)
    );
    let event = subscriber.receive();
    assert_eq!(event["event"], "pane.closed");
    assert_eq!(event["pane"], 1);
    assert!(event["exit_status"].as_i64().is_some(), "{event}");
    wait_for_process_absent(primary);
    wait_for_process_absent(descendant);
    let listing = environment.run(&["binary", "list"]);
    let value: Value = serde_json::from_slice(&listing.stdout).expect("listing");
    let panes = value["result"]["value"]["workspaces"][0]["tabs"][0]["panes"]
        .as_array()
        .expect("panes");
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["id"], 2);
    subscriber.expect_silence(Duration::from_millis(200));
    second.send(json!({"command":"quit"}));
    assert_eq!(second.receive()["event"], "cleanup");
    assert!(server.wait().success());
}

#[test]
fn server_shutdown_signal_reaps_owned_processes_and_sockets() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    let primary = pid_of(&fixture.receive());
    fixture.send(json!({"command":"spawn","mode":"hold_pty","exit_status":0}));
    let held = pid_of(&fixture.receive());
    let mut announced = environment.accept_fixture();
    assert_eq!(announced.receive()["event"], "descendant_ready");
    let mut viewer = TerminalViewer::spawn(&environment, "binary", 24, 80);
    viewer.wait_for_text("binary │");
    server.terminate();
    assert!(server.wait().success(), "SIGTERM shutdown exits cleanly");
    assert_eq!(
        viewer.wait_exit(),
        0,
        "viewers exit with the retirement status"
    );
    wait_for_process_absent(primary);
    wait_for_process_absent(held);
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket("binary"));
    wait_for_absent(&environment.attach_socket("binary"));
    assert!(!environment.descriptor("binary").exists());
}

#[test]
fn concurrent_first_clients_elect_exactly_one_server_and_workspace() {
    let _guard = guard();
    let environment = Environment::new();
    let mut viewers: Vec<TerminalViewer> = (0..3)
        .map(|_| TerminalViewer::spawn(&environment, "shared", 24, 80))
        .collect();
    let mut fixtures = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    // Exactly one pane starts; every viewer attaches to it.
    let mut fixture = Jsonl::new(accept_before(&environment.fixture_listener, deadline));
    assert_eq!(fixture.receive()["event"], "ready");
    fixtures.push(fixture);
    for viewer in &viewers {
        viewer.wait_for_text("shared │");
    }
    std::thread::sleep(Duration::from_millis(300));
    environment
        .fixture_listener
        .set_nonblocking(true)
        .expect("nonblocking");
    assert!(
        matches!(environment.fixture_listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a second pane process was started"
    );
    let listing = environment.run(&["shared", "list"]);
    let value: Value = serde_json::from_slice(&listing.stdout).expect("listing");
    assert_eq!(value["result"]["value"]["workspaces"][0]["viewers"], 3);
    let names: Value =
        serde_json::from_slice(&environment.run(&["workspace", "list"]).stdout).expect("names");
    assert_eq!(names["names"], json!(["shared"]));
    let descriptors: Vec<_> = fs::read_dir(environment.root.join("run/fux/workspaces"))
        .expect("descriptors")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(descriptors.len(), 1);
    for viewer in &mut viewers {
        assert_eq!(viewer.detach(), 0);
    }
    let killed = environment.run(&["workspace", "kill", "shared"]);
    assert!(killed.status.success());
    wait_for_absent(&environment.manager_socket());
}

#[test]
fn control_protocol_lists_captures_and_streams_events_without_touching_viewers() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    fixture.receive();
    let mut viewer = TerminalViewer::spawn(&environment, "binary", 24, 80);
    viewer.wait_for_text("binary │");
    let mut subscriber = control(&environment, "binary");
    subscriber.send(
        json!({"command":"subscribe","id":7,"events":["pane.opened","tab.opened","pane.title"]}),
    );
    assert_eq!(subscriber.receive()["status"], "accepted");
    fixture.send(json!({"command":"title","value":"fixture title"}));
    fixture.send(json!({"command":"write","chunks_hex":[hex(b"CAPTURE_ME")]}));
    fixture.receive();
    let title = subscriber.receive();
    assert_eq!(title["event"], "pane.title");
    assert_eq!(title["title"], "fixture title");
    viewer.wait_for_text("CAPTURE_ME");
    let before = viewer.raw().len();
    let capture = environment.run(&["binary", "capture", "1"]);
    let value: Value = serde_json::from_slice(&capture.stdout).expect("capture");
    assert!(
        value["result"]["value"]["text"]
            .as_str()
            .expect("text")
            .contains("CAPTURE_ME")
    );
    let listing = environment.run(&["binary", "list"]);
    let value: Value = serde_json::from_slice(&listing.stdout).expect("listing");
    let pane = &value["result"]["value"]["workspaces"][0]["tabs"][0]["panes"][0];
    assert_eq!(pane["title"], "fixture title");
    assert_eq!(pane["geometry"]["width"], 80);
    assert_eq!(pane["geometry"]["height"], 23);
    assert!(pane["focused"].as_bool().expect("focused"));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        viewer.raw().len(),
        before,
        "reads did not repaint the viewer"
    );
    let tab = environment.run(&["binary", "tab", "new", "work"]);
    assert!(
        tab.status.success(),
        "{}",
        String::from_utf8_lossy(&tab.stdout)
    );
    let mut second = environment.accept_fixture();
    assert_eq!(second.receive()["event"], "ready");
    let mut kinds = Vec::new();
    for _ in 0..2 {
        kinds.push(
            subscriber.receive()["event"]
                .as_str()
                .expect("event")
                .to_owned(),
        );
    }
    kinds.sort();
    assert_eq!(kinds, vec!["pane.opened", "tab.opened"]);
    viewer.wait_for_text(" main ");
    viewer.wait_for_text("work");
    // Removed commands fail clearly instead of doing something else.
    let popup = environment.run(&[
        "binary",
        "ctl",
        "{\"command\":\"popup\",\"id\":1,\"argv\":[\"true\"]}",
    ]);
    assert!(!popup.status.success());
    let stale = environment.run(&["binary", "kill", "99"]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stdout).contains("not-found"));
    assert_eq!(viewer.detach(), 0);
    fixture.send(json!({"command":"quit"}));
    fixture.receive();
    second.send(json!({"command":"quit"}));
    second.receive();
    assert!(server.wait().success());
}

#[test]
fn tiny_viewer_and_resize_keep_the_pane_size_negotiated_over_the_smallest_viewer() {
    let _guard = guard();
    let environment = Environment::new();
    let mut server = Server::start(&environment, "binary");
    let mut fixture = environment.accept_fixture();
    fixture.receive();
    let mut large = TerminalViewer::spawn(&environment, "binary", 40, 120);
    large.wait_for_text("binary │");
    fixture.send(json!({"command":"size"}));
    let size = fixture.receive();
    assert_eq!(
        (size["rows"].as_u64(), size["columns"].as_u64()),
        (Some(39), Some(120))
    );
    let mut small = TerminalViewer::spawn(&environment, "binary", 12, 40);
    small.wait_for_text("binary │");
    let deadline = Instant::now() + DEADLINE;
    loop {
        fixture.send(json!({"command":"size"}));
        let size = fixture.receive();
        if (size["rows"].as_u64(), size["columns"].as_u64()) == (Some(11), Some(40)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pane did not shrink to the smallest viewer: {size}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    small.resize(1, 1);
    std::thread::sleep(Duration::from_millis(300));
    small.write(b"\x01");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        small.child.try_wait().expect("wait").is_none(),
        "one-cell viewer crashed"
    );
    small.write(b"\x1b");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(small.detach(), 0);
    let deadline = Instant::now() + DEADLINE;
    loop {
        fixture.send(json!({"command":"size"}));
        let size = fixture.receive();
        if (size["rows"].as_u64(), size["columns"].as_u64()) == (Some(39), Some(120)) {
            break;
        }
        assert!(Instant::now() < deadline, "pane did not grow back: {size}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(large.detach(), 0);
    fixture.send(json!({"command":"quit"}));
    fixture.receive();
    assert!(server.wait().success());
}

#[test]
fn startup_failure_rolls_back_and_reports_an_error() {
    let _guard = guard();
    let environment = Environment::new();
    fs::write(
        environment.root.join("config/fux/config.toml"),
        "default-command = { argv = [\"/nonexistent/fux-program\"] }\n",
    )
    .expect("config");
    let output = Command::new(fux_binary())
        .args(["serve", "--name", "broken"])
        .env_clear()
        .envs(environment.variables())
        .stdin(Stdio::null())
        .output()
        .expect("run server");
    assert!(
        !output.status.success(),
        "server started without a runnable pane"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("could not start the pane") || stderr.contains("initial workspace"),
        "{stderr}"
    );
    assert!(
        !environment.manager_socket().exists(),
        "manager socket leaked"
    );
    assert!(
        !environment.attach_socket("broken").exists(),
        "attach socket leaked"
    );
    assert!(
        !environment.descriptor("broken").exists(),
        "descriptor leaked"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
