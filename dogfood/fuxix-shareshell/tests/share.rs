//! Two clients share one shell and both see what either types; a late
//! joiner is replayed what came before; the socket is this user's alone,
//! and goes when the shell exits.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, reason = "a test")]
use std::io::{Read, Write};
use fuxix::socket::Client as UnixStream;
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
        .args(["--size", "24x80", "--", "cmd.exe"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    child
}

/// The pipe the server at `path` listens on, once it does.
fn connect(path: &Path) -> UnixStream {
    let base = path.to_string_lossy().replace('\\', "/");
    let name = fuxix::socket::name(&format!("fuxix-shareshell-{base}")).unwrap();
    let start = Instant::now();
    loop {
        match UnixStream::connect(&name) {
            Ok(stream) => return stream,
            Err(_) if start.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("no pipe: {e}"),
        }
    }
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
    let mut a = connect(&path);
    let mut b = connect(&path);
    // The carets go as cmd reads the line, so what is typed never matches.
    a.write_all(b"echo from^-a\r").unwrap();
    read_until(&mut a, "from-a");
    read_until(&mut b, "from-a");
    b.write_all(b"echo from^-b\r").unwrap();
    read_until(&mut a, "from-b");
    read_until(&mut b, "from-b");
    let mut late = connect(&path);
    let replay = read_until(&mut late, "from-b");
    assert!(replay.contains("from-a"), "replayed: {replay:?}");
    late.write_all(b"exit 3\r").unwrap();
    read_until(&mut a, "exited 3]");
    let status = server.wait().unwrap();
    assert_eq!(status.code(), Some(3));
}
