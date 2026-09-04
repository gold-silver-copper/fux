#![allow(clippy::expect_used, clippy::panic)]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn descendant_modes_report_readiness_signals_and_cleanup() {
    let mut fixture = Fixture::spawn(5_000, true);
    assert_eq!(fixture.control.receive()["event"], "ready");

    fixture.control.send(json!({
        "command":"spawn", "mode":"exit", "exit_status":17
    }));
    assert_eq!(fixture.control.receive()["status"], 17);
    assert_eq!(fixture.descendant_ready(), fixture.last_descendant_pid());

    let ignore_hup = fixture.spawn_descendant("ignore_hup", 23);
    kill(ignore_hup, Signal::SIGHUP).expect("send ignored HUP");
    assert!(
        kill(ignore_hup, None).is_ok(),
        "HUP killed ignore_hup child"
    );
    kill(ignore_hup, Signal::SIGTERM).expect("terminate ignore_hup child");
    fixture.control.send(json!({"command":"wait_descendant"}));
    assert_eq!(fixture.control.receive()["status"], 23);

    let wait_signal = fixture.spawn_descendant("wait_signal", 24);
    kill(wait_signal, Signal::SIGINT).expect("signal wait_signal child");
    fixture.control.send(json!({"command":"wait_descendant"}));
    assert_eq!(fixture.control.receive()["status"], 24);

    let held = fixture.spawn_descendant("hold_pty", 0);
    fixture.control.send(json!({"command":"quit"}));
    assert_eq!(fixture.control.receive()["event"], "cleanup");
    fixture.wait_success();
    assert!(
        kill(held, None).is_err(),
        "hold_pty descendant survived cleanup"
    );
}

#[test]
fn stdin_refusal_is_explicit_and_output_backpressure_hits_the_hard_deadline() {
    let mut fixture = Fixture::spawn(5_000, true);
    assert_eq!(fixture.control.receive()["event"], "ready");
    fixture.control.send(json!({"command":"refuse_stdin"}));
    assert_eq!(fixture.control.receive()["event"], "stdin_refused");
    fixture.control.send(json!({"command":"quit"}));
    assert_eq!(fixture.control.receive()["event"], "cleanup");
    fixture.wait_success();

    let mut blocked = Fixture::spawn(200, false);
    assert_eq!(blocked.control.receive()["event"], "ready");
    blocked.control.send(json!({
        "command":"fill_stdout", "bytes":1048576, "byte":120
    }));
    let status = blocked.wait();
    assert!(!status.success(), "blocked fixture unexpectedly succeeded");
}

struct Fixture {
    root: PathBuf,
    listener: UnixListener,
    control: Jsonl,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    _writer: Box<dyn Write + Send>,
    reader: Option<std::thread::JoinHandle<()>>,
    last_pid: Option<Pid>,
}

impl Fixture {
    fn spawn(deadline_ms: u64, drain_output: bool) -> Self {
        let root = private_root();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).expect("control listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("fixture PTY");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux-fixture-child"));
        command.arg(format!("--control={}", socket.display()));
        command.arg(format!("--deadline-ms={deadline_ms}"));
        let child = pair.slave.spawn_command(command).expect("spawn fixture");
        drop(pair.slave);
        let writer = pair.master.take_writer().expect("fixture writer");
        let reader = if drain_output {
            let mut output = pair.master.try_clone_reader().expect("fixture reader");
            Some(std::thread::spawn(move || {
                let mut sink = std::io::sink();
                let _ = std::io::copy(&mut output, &mut sink);
            }))
        } else {
            None
        };
        let control = Jsonl::new(accept(&listener));
        Self {
            root,
            listener,
            control,
            child: Some(child),
            _writer: writer,
            reader,
            last_pid: None,
        }
    }

    fn spawn_descendant(&mut self, mode: &str, status: i32) -> Pid {
        self.control.send(json!({
            "command":"spawn", "mode":mode, "exit_status":status
        }));
        let spawned = self.control.receive();
        assert_eq!(spawned["event"], "spawned");
        let announced = self.descendant_ready();
        let pid = Pid::from_raw(
            i32::try_from(spawned["pid"].as_u64().expect("spawn pid")).expect("pid range"),
        );
        assert_eq!(announced, pid);
        pid
    }

    fn descendant_ready(&mut self) -> Pid {
        let mut descendant = Jsonl::new(accept(&self.listener));
        let ready = descendant.receive();
        assert_eq!(ready["event"], "descendant_ready");
        let pid = Pid::from_raw(
            i32::try_from(ready["pid"].as_u64().expect("ready pid")).expect("pid range"),
        );
        self.last_pid = Some(pid);
        pid
    }

    fn last_descendant_pid(&self) -> Pid {
        self.last_pid.expect("descendant readiness")
    }

    fn wait(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .expect("fixture child")
                .try_wait()
                .expect("wait fixture")
            {
                self.child.take();
                return status;
            }
            assert!(Instant::now() < deadline, "fixture exit deadline expired");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_success(&mut self) {
        let status = self.wait();
        assert!(status.success(), "fixture exited with {status}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Jsonl {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        // A short-lived descendant can close after writing readiness but before accept;
        // macOS then rejects timeout changes even though the queued frame is readable.
        let _ = stream.set_read_timeout(Some(DEADLINE));
        Self {
            reader: BufReader::new(stream.try_clone().expect("clone control")),
            writer: stream,
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

fn accept(listener: &UnixListener) -> UnixStream {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).expect("blocking control");
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept control: {error}"),
        }
        assert!(Instant::now() < deadline, "control accept deadline expired");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn private_root() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!(
        "fux-fixture-lifecycle-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("private root");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private permissions");
    root
}
