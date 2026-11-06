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

/// Its output comes through a console, and the exit status passes through.
#[test]
fn output_comes_through_and_the_status_passes_through() {
    let out = unbuffer()
        .args(["cmd.exe", "/c", "echo out-tty& exit 7"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("out-tty"));
    assert_eq!(out.status.code(), Some(7));
}

/// One that can't be found is told as 127.
#[test]
fn a_missing_command_is_told_by_status() {
    let missing = unbuffer().arg("no-such-command-a5").output().unwrap();
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no-such-command-a5"));
}
