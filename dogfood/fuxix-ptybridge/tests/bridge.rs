//! The bridge, run as a program, driven over a Unix socket.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test fails by panicking"
)]
use std::io::{Read, Write};
use fuxix::socket::Client as UnixStream;
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
    child
}

/// The pipe the bridge at `path` listens on, once it does.
fn connect(path: &Path) -> UnixStream {
    let base = path.to_string_lossy().replace('\\', "/");
    let name = fuxix::socket::name(&format!("fuxix-ptybridge-{base}")).expect("a name");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&name) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => panic!("bridge never listened: {e}"),
        }
    }
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
fn a_program_talks_through_the_pipe_and_its_exit_closes_it() {
    let path = socket_path("talk");
    let child = bridge(
        &path,
        &["--size", "30x100", "--", "cmd.exe", "/v:on", "/c", "set /p x=? & echo got:!x!"],
    );
    let mut conn = connect(&path);
    conn.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    conn.write_all(b"hello\r").expect("write");
    // Read to the end: the program's exit is what closes the connection.
    let mut out = Vec::new();
    conn.read_to_end(&mut out).expect("read to end");
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("got:hello"), "{out:?}");
    let log = wait(child, Duration::from_secs(5));
    assert!(log.contains("program ended"), "{log}");
}

#[test]
fn the_client_hanging_up_ends_the_program() {
    let path = socket_path("hangup");
    let child = bridge(&path, &["--", "ping.exe", "-n", "30", "127.0.0.1"]);
    drop(connect(&path));
    let log = wait(child, Duration::from_secs(8));
    assert!(log.contains("SIGHUP"), "{log}");
}
