#![allow(clippy::expect_used, clippy::panic)]

use fux::host::WorkspaceHost;
use fux::state::PaneId;
use koh::server::{ChangeSignal, ClientId, SessionHost as _};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);

struct PrivateControl {
    root: PathBuf,
    listener: UnixListener,
}

impl PrivateControl {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "fux-verify-{label}-{}-{}",
            std::process::id(),
            monotonic_nonce()
        ));
        fs::create_dir(&root).expect("private control directory");
        let mut permissions = fs::metadata(&root).expect("control metadata").permissions();
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o700);
        fs::set_permissions(&root, permissions).expect("private permissions");
        let listener = UnixListener::bind(root.join("fixture.sock")).expect("control socket");
        Self { root, listener }
    }

    fn path(&self) -> PathBuf {
        self.root.join("fixture.sock")
    }

    fn accept(&self) -> UnixStream {
        let stream = self
            .listener
            .accept()
            .expect("fixture readiness connection")
            .0;
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("read deadline");
        stream
            .set_write_timeout(Some(DEADLINE))
            .expect("write deadline");
        stream
    }
}

impl Drop for PrivateControl {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Jsonl {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        let reader = BufReader::new(stream.try_clone().expect("clone control"));
        Self {
            writer: stream,
            reader,
        }
    }

    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.writer, &value).expect("control request");
        self.writer.write_all(b"\n").expect("request newline");
        self.writer.flush().expect("flush request");
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("control response");
        assert!(!line.is_empty(), "fixture closed before response");
        serde_json::from_str(&line).expect("response JSON")
    }
}

// Golden path 1: attach, type, render, detach, and reattach retain the authoritative frame.
#[test]
fn deterministic_child_drives_a_real_workspace_session_without_ambient_state() {
    let control_socket = PrivateControl::new("reattach");
    let fixture = env!("CARGO_BIN_EXE_fux-fixture-child");
    let (mut session, control) = WorkspaceHost::shared(
        vec![
            fixture.into(),
            format!("--control={}", control_socket.path().display()),
            "--deadline-ms=5000".into(),
        ],
        32,
        None,
    )
    .expect("real workspace");
    let mut fixture = Jsonl::new(control_socket.accept());
    let ready = fixture.receive();
    assert_eq!(ready["event"], "ready");
    assert_eq!(ready["version"], 1);

    session.attach_notify(ChangeSignal::default());
    let alice = ClientId::next();
    session.resize(alice, 24, 80);
    fixture.send(json!({"command":"write", "chunks_hex":["68656c", "6c6f"]}));
    assert_eq!(fixture.receive()["bytes"], 5);
    let rendered = wait_snapshot(&mut session, |state| {
        state.pane(PaneId(1)).is_some_and(|pane| {
            pane.cells.iter().any(|cell| cell.text == "h")
                && pane.cells.iter().any(|cell| cell.text == "o")
        })
    });
    let rendered_text = visible_text(&rendered);

    fixture.send(json!({"command":"read_exact", "bytes":3}));
    session.input(b"abc");
    assert_eq!(fixture.receive()["bytes_hex"], "616263");
    session.client_detached(alice);
    assert_eq!(control.attached_clients(), 0);
    session.resize(alice, 30, 100);
    let reattached = session.snapshot();
    assert_eq!(visible_text(&reattached), rendered_text);
    session.client_detached(alice);

    fixture.send(json!({"command":"quit"}));
    assert_eq!(fixture.receive()["event"], "cleanup");
    control.shutdown();
    assert_eq!(control.attached_clients(), 0);
}

fn visible_text(state: &fux::state::WorkspaceState) -> String {
    state
        .pane(PaneId(1))
        .expect("fixture pane")
        .cells
        .iter()
        .filter(|cell| !cell.text.is_empty())
        .map(|cell| cell.text.as_str())
        .collect()
}

fn wait_snapshot(
    session: &mut fux::host::WorkspaceSession,
    predicate: impl Fn(&fux::state::WorkspaceState) -> bool,
) -> fux::state::WorkspaceState {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = session.snapshot();
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "workspace snapshot deadline expired"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn monotonic_nonce() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    u128::from(NEXT.fetch_add(1, Ordering::Relaxed))
}
