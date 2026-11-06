//! Two clients share one shell and both see what either types; a late
//! joiner is replayed what came before; the socket is this user's alone,
//! and goes when the shell exits.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, reason = "a test")]
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn socket_path(name: &str) -> PathBuf {
    let dir = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = dir.join(format!("shareshell-{name}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn start(path: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_fuxix-shareshell"))
        .args(["serve"])
        .arg(path)
        .args(["--size", "24x80", "--", "/bin/sh"])
        .env("PS1", "$ ")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !path.exists() {
        assert!(start.elapsed() < Duration::from_secs(5), "no socket");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
}

/// Reads from `stream` until what it read contains `want`.
fn read_until(stream: &mut UnixStream, want: &str) -> String {
    stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let start = Instant::now();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains(want) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "never saw {want:?} in {:?}",
            String::from_utf8_lossy(&seen)
        );
        match stream.read(&mut buf) {
            Ok(0) => panic!("closed before {want:?}: {:?}", String::from_utf8_lossy(&seen)),
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(_) => {}
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

#[test]
fn clients_share_one_shell_and_late_joiners_are_replayed() {
    let path = socket_path("share");
    let mut server = start(&path);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "the socket is the user's alone");

    let mut a = UnixStream::connect(&path).unwrap();
    let mut b = UnixStream::connect(&path).unwrap();
    // The product is printed only by the shell, never echoed as typed.
    a.write_all(b"echo from-a-$((6*7))\n").unwrap();
    read_until(&mut a, "from-a-42");
    read_until(&mut b, "from-a-42");
    b.write_all(b"echo from-b-$((7*8))\n").unwrap();
    read_until(&mut a, "from-b-56");
    read_until(&mut b, "from-b-56");

    let mut late = UnixStream::connect(&path).unwrap();
    let replay = read_until(&mut late, "from-b-56");
    assert!(replay.contains("from-a-42"), "replayed: {replay:?}");

    late.write_all(b"exit 3\n").unwrap();
    read_until(&mut a, "exited 3]");
    let status = server.wait().unwrap();
    assert_eq!(status.code(), Some(3));
    assert!(!path.exists(), "the socket is removed");
}
