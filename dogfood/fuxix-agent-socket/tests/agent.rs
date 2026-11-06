//! The agent run as a program: served over its socket, recovering a stale
//! one, and stopping cleanly on TERM.
#![allow(clippy::panic, reason = "a test fails by panicking")]
use std::io::Write;
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
    while !client(socket, &["list"], "").0 {
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

#[test]
fn secrets_are_served_privately() {
    let dir = dir("serve");
    let socket = dir.join("agent.sock");
    let agent = serve(&socket);
    assert_eq!(fuxix::file::is_private_dir(&dir).ok(), Some(true));
    assert_eq!(client(&socket, &["add", "gh"], "s3cret token\n"), (true, "added".to_owned()));
    assert_eq!(client(&socket, &["add", "aws"], "k\n"), (true, "added".to_owned()));
    assert_eq!(client(&socket, &["list"], ""), (true, "aws gh".to_owned()));
    assert_eq!(client(&socket, &["get", "gh"], ""), (true, "s3cret token".to_owned()));
    assert_eq!(client(&socket, &["remove", "gh"], ""), (true, "removed".to_owned()));
    assert!(!client(&socket, &["get", "gh"], "").0);
    // A second agent on the same path is refused while the first runs.
    let second = Command::new(BIN).arg("--socket").arg(&socket).arg("serve").output();
    assert!(second.is_ok_and(|o| !o.status.success()));
    drop(agent);
    let _ = std::fs::remove_dir_all(&dir);
}
