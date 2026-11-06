//! The bridge, run as a program, driven over a Unix socket.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test fails by panicking"
)]
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn socket_path(name: &str) -> PathBuf {
    let dir = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = dir.join(format!("ptybridge-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn bridge(path: &Path, args: &[&str]) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_fuxix-ptybridge"))
        .arg("--once")
        .args(args.iter().take_while(|a| **a != "--"))
        .arg(format!("unix:{}", path.display()))
        .args(args.iter().skip_while(|a| **a != "--"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("bridge starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "bridge never listened");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

fn wait(mut child: Child, within: Duration) -> String {
    let deadline = Instant::now() + within;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("bridge still running after {within:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut log = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut log)
        .expect("log");
    log
}

#[test]
fn a_program_talks_through_the_socket_and_its_exit_closes_it() {
    let path = socket_path("talk");
    let child = bridge(
        &path,
        &[
            "--size",
            "30x100",
            "--",
            "sh",
            "-c",
            "stty size; read x; echo got:$x",
        ],
    );
    let mut conn = UnixStream::connect(&path).expect("connect");
    conn.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    conn.write_all(b"hello\n").expect("write");
    // Read to the end: the program's exit is what closes the connection.
    let mut out = Vec::new();
    conn.read_to_end(&mut out).expect("read to end");
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("30 100\r\n"), "{out:?}");
    assert!(out.contains("got:hello\r\n"), "{out:?}");
    let log = wait(child, Duration::from_secs(5));
    assert!(log.contains("program ended: exit status: 0"), "{log}");
    assert!(!path.exists(), "socket file left behind");
}

#[test]
fn the_client_hanging_up_ends_the_program() {
    let path = socket_path("hangup");
    let child = bridge(&path, &["--raw", "--", "sleep", "30"]);
    drop(UnixStream::connect(&path).expect("connect"));
    let log = wait(child, Duration::from_secs(5));
    assert!(log.contains("SIGHUP"), "{log}");
}
