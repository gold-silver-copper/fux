#![allow(clippy::expect_used, clippy::panic)]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(10);

// Binary boundary: real fux manager/control sockets, daemon process, Zor wrapper, and PTY child.
#[test]
fn real_binaries_publish_agent_state_and_remove_every_private_runtime_artifact() {
    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("binary");
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");

    let id = run(&fux, ["id"], &environment);
    assert!(
        id.status.success(),
        "id failed: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let allow = String::from_utf8(id.stdout)
        .expect("endpoint id")
        .trim()
        .to_owned();
    assert_eq!(allow.len(), 64);

    environment.write_config(&fixture, &zor);
    let mut server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--allow", &allow, "--name", "binary"])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    wait_for_path(&environment.manager_socket());
    wait_for_path(&environment.control_socket());

    let mut fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(fixture.receive()["event"], "ready");
    let listed = run(&fux, ["binary", "list"], &environment);
    assert!(
        listed.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains("fux-fixture-child"));

    let split = run(&fux, ["binary", "split", "horizontal"], &environment);
    assert!(
        split.status.success(),
        "split failed: {}",
        String::from_utf8_lossy(&split.stderr)
    );
    let mut second_fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(second_fixture.receive()["event"], "ready");
    let mut client = TerminalChild::spawn(&fux, &environment, 40, 120);
    wait_for_list(&fux, &environment, "\"width\":60");
    // Zor samples an outer resize on its bounded 50 ms coordinator cadence.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fixture_size(&mut fixture), (37, 58));
    assert_eq!(fixture_size(&mut second_fixture), (37, 58));

    let popup = run(&fux, ["binary", "popup", "--size", "30x8"], &environment);
    assert!(
        popup.status.success(),
        "popup failed: {}",
        String::from_utf8_lossy(&popup.stderr)
    );
    let mut popup_fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(popup_fixture.receive()["event"], "ready");
    wait_for_list(&fux, &environment, "\"name\":\"popups\"");
    popup_fixture.send(json!({"command":"read_exact", "bytes":1}));
    client.write(b"q");
    assert_eq!(popup_fixture.receive()["bytes_hex"], "71");
    popup_fixture.send(json!({"command":"exit", "status":0}));
    assert_eq!(popup_fixture.receive()["event"], "cleanup");
    wait_for_list_absent(&fux, &environment, "\"name\":\"popups\"");
    client.detach();

    for (sequence, state) in [(1, "working"), (2, "blocked"), (3, "idle")] {
        fixture.send(json!({
            "command":"agent",
            "payload":format!("state={state};agent=fixture;seq={sequence}")
        }));
        wait_for_list(&fux, &environment, state);
    }
    fixture.send(json!({"command":"write", "chunks_hex":["62696e617279"]}));
    assert_eq!(fixture.receive()["bytes"], 6);
    let captured = run(&fux, ["binary", "capture", "1"], &environment);
    assert!(captured.status.success());
    assert!(String::from_utf8_lossy(&captured.stdout).contains("binary"));

    server.terminate(Signal::SIGTERM);
    server.wait();
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket());
    assert!(
        !environment.descriptor().exists(),
        "workspace descriptor leaked"
    );
}

struct PrivateEnvironment {
    root: PathBuf,
}

impl PrivateEnvironment {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fux-verify-{label}-{}-{nonce}", std::process::id()));
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
        Self { root }
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
            (
                "KOH_KEY_NEW_PASSPHRASE".into(),
                "verification-only-passphrase".into(),
            ),
            (
                "KOH_KEY_PASSPHRASE".into(),
                "verification-only-passphrase".into(),
            ),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
        ]
    }

    fn write_config(&self, fixture: &Path, zor: &Path) {
        let document = format!(
            "default-command = {{ argv = [{:?}, {:?}, \"--deadline-ms=30000\"] }}\nzor-path = {:?}\n[notifications]\nenabled = false\n",
            fixture.display().to_string(),
            format!("--control={}", self.fixture_socket().display()),
            zor.display().to_string(),
        );
        fs::write(self.root.join("config/fux/config.toml"), document).expect("private config");
    }

    fn fixture_socket(&self) -> PathBuf {
        self.root.join("fixture.sock")
    }
    fn manager_socket(&self) -> PathBuf {
        self.root.join("run/fux/manager.sock")
    }
    fn control_socket(&self) -> PathBuf {
        self.root.join("run/fux/binary.sock")
    }
    fn descriptor(&self) -> PathBuf {
        self.root.join("run/fux/workspaces/binary.json")
    }
}

impl Drop for PrivateEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct OwnedChild {
    child: Option<Child>,
}

struct TerminalChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl TerminalChild {
    fn spawn(fux: &Path, environment: &PrivateEnvironment, rows: u16, columns: u16) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("client PTY");
        let mut command = CommandBuilder::new(fux);
        command.arg("binary");
        for (key, value) in environment.variables() {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn fux client");
        drop(pair.slave);
        let mut output = pair.master.try_clone_reader().expect("client reader");
        let reader = std::thread::spawn(move || {
            let mut sink = std::io::sink();
            let _ = std::io::copy(&mut output, &mut sink);
        });
        let writer = pair.master.take_writer().expect("client writer");
        Self {
            child,
            writer,
            reader: Some(reader),
        }
    }

    fn detach(&mut self) {
        self.write(&[1, b'd']);
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait client") {
                assert!(status.success(), "client detach status {status}");
                break;
            }
            assert!(Instant::now() < deadline, "client detach deadline expired");
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().expect("client reader");
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("terminal input");
        self.writer.flush().expect("detach flush");
    }
}

impl Drop for TerminalChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl OwnedChild {
    fn spawn(command: &mut Command) -> Self {
        Self {
            child: Some(command.spawn().expect("spawn fux server")),
        }
    }
    fn terminate(&mut self, signal: Signal) {
        if let Some(child) = &self.child {
            kill(
                Pid::from_raw(i32::try_from(child.id()).expect("pid")),
                signal,
            )
            .expect("signal server");
        }
    }
    fn wait(&mut self) {
        let deadline = Instant::now() + DEADLINE;
        let child = self.child.as_mut().expect("owned server");
        loop {
            if let Some(status) = child.try_wait().expect("wait server") {
                assert!(status.success(), "server exited with {status}");
                self.child.take();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server shutdown deadline expired"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Jsonl {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}
impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream.try_clone().expect("clone fixture control")),
            writer: stream,
        }
    }
    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.writer, &value).expect("fixture request");
        self.writer.write_all(b"\n").expect("fixture newline");
        self.writer.flush().expect("fixture flush");
    }
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let deadline = Instant::now() + DEADLINE;
        loop {
            match self.reader.read_line(&mut line) {
                Ok(_) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("fixture response: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "fixture response deadline expired"
            );
        }
        assert!(!line.is_empty(), "fixture closed unexpectedly");
        serde_json::from_str(&line).expect("fixture JSON")
    }
}

fn run<const N: usize>(
    program: &Path,
    arguments: [&str; N],
    environment: &PrivateEnvironment,
) -> Output {
    Command::new(program)
        .args(arguments)
        .env_clear()
        .envs(environment.variables())
        .stdin(Stdio::null())
        .output()
        .expect("run fux binary")
}

fn wait_for_list(fux: &Path, environment: &PrivateEnvironment, state: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(fux, ["binary", "list"], environment);
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "agent state `{state}` did not reach binary list"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_list_absent(fux: &Path, environment: &PrivateEnvironment, value: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(fux, ["binary", "list"], environment);
        if output.status.success() && !String::from_utf8_lossy(&output.stdout).contains(value) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "`{value}` remained in binary list"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture_size(fixture: &mut Jsonl) -> (u16, u16) {
    fixture.send(json!({"command":"size"}));
    let response = fixture.receive();
    let rows = response["rows"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok());
    let columns = response["columns"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok());
    (
        rows.expect("fixture rows"),
        columns.expect("fixture columns"),
    )
}

fn accept_with_deadline(listener: &UnixListener) -> UnixStream {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let deadline = Instant::now() + DEADLINE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .expect("fixture timeout");
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

fn binary(variable: &str, relative: &str) -> PathBuf {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join(relative)
        });
    assert!(
        path.is_file(),
        "{variable} must name the built binary at {}",
        path.display()
    );
    path
}
