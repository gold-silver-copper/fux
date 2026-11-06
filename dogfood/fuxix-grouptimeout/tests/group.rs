//! What the program promises, run as a program.
use std::process::Command;
use std::time::{Duration, Instant};

fn grouptimeout(args: &[&str]) -> (Option<i32>, Duration) {
    let start = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_fuxix-grouptimeout"))
        .args(args)
        .status()
        .ok()
        .and_then(|s| s.code());
    (status, start.elapsed())
}

#[test]
fn a_background_process_left_behind_is_timed_out_with_its_job() {
    let script = "start /b ping -n 30 127.0.0.1 > NUL & exit 0";
    let (code, took) = grouptimeout(&["1", "cmd.exe", "/c", script]);
    assert_eq!(code, Some(124), "its job outlived its leader, so it timed out");
    assert!(took >= Duration::from_millis(900) && took < Duration::from_secs(8), "{took:?}");
}

#[test]
fn statuses_pass_through_as_timeout_gives_them() {
    assert_eq!(grouptimeout(&["5", "cmd.exe", "/c", "exit 3"]).0, Some(3));
    assert_eq!(grouptimeout(&["5", r"C:\nonexistent\x.exe"]).0, Some(127));
    assert_eq!(
        grouptimeout(&["--leader-only", "5", "cmd.exe", "/c", "start /b ping -n 30 127.0.0.1 > NUL & exit 4"]).0,
        Some(4)
    );
    assert_eq!(grouptimeout(&["bogus", "cmd.exe"]).0, Some(125));
}
