//! The agent run as a program: served over its socket, recovering a stale
//! one, and stopping cleanly on TERM.
#![allow(clippy::panic, reason = "a test fails by panicking")]
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fuxix-agent-socket");

fn dir(name: &str) -> PathBuf {
    let base = fuxix::file::scratch().unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join(format!("agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The agent, killed if a test panics before stopping it.
struct Agent(Child);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn serve(socket: &Path) -> Agent {
    let child = Command::new(BIN)
        .args(["--socket"])
        .arg(socket)
        .arg("serve")
        .stderr(Stdio::piped())
        .spawn();
    let Ok(child) = child else { panic!("spawning the agent") };
    let child = Agent(child);
    let start = Instant::now();
    while UnixStream::connect(socket).is_err() {
        assert!(start.elapsed() < Duration::from_secs(10), "the agent never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn client(socket: &Path, args: &[&str], stdin: &str) -> (bool, String) {
    let child = Command::new(BIN)
        .arg("--socket")
        .arg(socket)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { panic!("spawning a client") };
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(stdin.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else { panic!("client output") };
    (out.status.success(), String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

fn term(agent: &mut Agent) -> String {
    let child = &mut agent.0;
    let Some(pid) = fuxix::process::Pid::of(child) else { panic!("no pid") };
    assert_eq!(fuxix::process::kill(pid, fuxix::process::Signal::Term), Ok(()));
    let mut log = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut stderr, &mut log);
    }
    assert!(child.wait().is_ok_and(|s| s.success()), "TERM is a clean exit: {log}");
    log
}

#[test]
fn secrets_are_served_privately_and_term_cleans_up() {
    let dir = dir("serve");
    let socket = dir.join("agent.sock");
    let mut agent = serve(&socket);
    // Its memory is not this user's to read.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let mem = std::fs::metadata(format!("/proc/{}/mem", agent.0.id())).map(|m| m.uid());
        if fuxix::process::geteuid() != 0 {
            assert_eq!(mem.ok(), Some(0));
        }
    }
    let mode = |p: &Path| std::fs::metadata(p).map(|m| m.permissions().mode() & 0o777).ok();
    assert_eq!(mode(&socket), Some(0o600));
    assert_eq!(mode(&dir), Some(0o700));
    assert_eq!(client(&socket, &["add", "gh"], "s3cret token\n"), (true, "added".to_owned()));
    assert_eq!(client(&socket, &["add", "aws"], "k\n"), (true, "added".to_owned()));
    assert_eq!(client(&socket, &["list"], ""), (true, "aws gh".to_owned()));
    assert_eq!(client(&socket, &["get", "gh"], ""), (true, "s3cret token".to_owned()));
    assert_eq!(client(&socket, &["remove", "gh"], ""), (true, "removed".to_owned()));
    assert!(!client(&socket, &["get", "gh"], "").0);
    // Pipelined requests on one connection, and a hang-up straight after.
    let Ok(mut raw) = UnixStream::connect(&socket) else { panic!("connect") };
    assert!(raw.write_all(b"LIST\r\nBOGUS\nQUIT\nLIST\n").is_ok());
    let lines: Vec<String> = BufReader::new(raw).lines().map_while(Result::ok).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines.first().map(String::as_str), Some("OK aws"));
    assert_eq!(lines.get(2).map(String::as_str), Some("OK bye"));
    // A second agent on the same socket is refused while the first runs.
    let second = Command::new(BIN).arg("--socket").arg(&socket).arg("serve").output();
    assert!(second.is_ok_and(|o| !o.status.success()));
    let log = term(&mut agent);
    assert!(log.contains("TERM received; forgetting 1 secret(s)"), "{log}");
    assert!(std::fs::symlink_metadata(&socket).is_err(), "the socket is removed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_socket_is_replaced_and_a_foreign_file_is_not() {
    let dir = dir("stale");
    assert!(std::fs::create_dir_all(&dir).is_ok());
    let socket = dir.join("agent.sock");
    // A crashed agent's socket: bound, then its listener gone.
    drop(UnixListener::bind(&socket));
    assert!(std::fs::symlink_metadata(&socket).is_ok());
    let mut agent = serve(&socket);
    assert_eq!(client(&socket, &["list"], ""), (true, String::new()));
    let log = term(&mut agent);
    assert!(log.contains("removing stale socket"), "{log}");
    // A regular file at the path is not the agent's to remove.
    assert!(std::fs::write(&socket, b"mine").is_ok());
    let refused = Command::new(BIN).arg("--socket").arg(&socket).arg("serve").output();
    assert!(refused.is_ok_and(|o| !o.status.success()));
    assert_eq!(std::fs::read(&socket).ok().as_deref(), Some(&b"mine"[..]));
    let _ = std::fs::remove_dir_all(&dir);
}
