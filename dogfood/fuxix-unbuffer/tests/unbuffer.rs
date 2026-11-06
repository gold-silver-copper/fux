//! What unbuffer promises, run as a user runs it: from a pipeline, with no
//! terminal of its own.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "tests fail by panicking"
)]
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn unbuffer() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fuxix-unbuffer"))
}

/// stdout and stderr are both terminals, their output arrives on our
/// stdout as written ("\n", not "\r\n"), and the exit status passes through.
#[test]
fn output_is_a_terminal_and_the_status_passes_through() {
    let script =
        "[ -t 1 ] && echo out-tty; [ -t 2 ] && echo err-tty >&2; [ -t 0 ] || echo in-pipe; exit 7";
    let out = unbuffer()
        .args(["sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "out-tty\nerr-tty\nin-pipe\n"
    );
    assert_eq!(out.status.code(), Some(7));
    assert!(out.stderr.is_empty());
}

/// grep, reading a pipe that stays open, writes each match at once, as it
/// does to a terminal; to a pipe it would hold them until its input ended.
#[test]
fn a_filter_in_a_pipeline_writes_line_by_line() {
    let mut child = unbuffer()
        .args(["grep", "keep"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin.write_all(b"drop\nkeep 1\n").unwrap();
    stdin.flush().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buffer = [0u8; 64];
        let n = stdout.read(&mut buffer).unwrap();
        tx.send(buffer[..n].to_vec()).unwrap();
        let mut rest = Vec::new();
        stdout.read_to_end(&mut rest).unwrap();
        rest
    });
    let first = rx.recv_timeout(Duration::from_secs(5));
    drop(stdin);
    let rest = reader.join().unwrap();
    assert_eq!(
        first.as_deref(),
        Ok(&b"keep 1\n"[..]),
        "grep held its output back"
    );
    assert!(rest.is_empty());
    assert_eq!(child.wait().unwrap().code(), Some(0));
}

/// A command killed by a signal is told as 128 + the signal; one that
/// can't be found as 127.
#[test]
fn deaths_and_missing_commands_are_told_by_status() {
    let killed = unbuffer()
        .args(["sh", "-c", "kill -TERM $$"])
        .output()
        .unwrap();
    assert_eq!(killed.status.code(), Some(128 + 15));
    let missing = unbuffer().arg("no-such-command-a5").output().unwrap();
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no-such-command-a5"));
}
