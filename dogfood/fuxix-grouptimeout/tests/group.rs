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

/// Whether `pid` is a running (not zombie) process.
fn running(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with('Z'))
    })
}

#[test]
fn a_background_process_left_behind_is_timed_out_with_its_group() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("grouptimeout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pidfile = dir.join("bg");
    let script = format!("sleep 30 & echo $! > {}; exit 0", pidfile.display());
    let (code, took) = grouptimeout(&["0.5", "sh", "-c", &script]);
    let bg = std::fs::read_to_string(&pidfile).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        code,
        Some(124),
        "the group outlived its leader, so it timed out"
    );
    assert!(
        took >= Duration::from_millis(450) && took < Duration::from_secs(5),
        "{took:?}"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !running(bg.trim()),
        "the background sleep {} was killed",
        bg.trim()
    );
    Ok(())
}

#[test]
fn a_group_ignoring_term_is_killed_after_the_grace() {
    let (code, took) = grouptimeout(&[
        "-k",
        "0.3",
        "0.3",
        "sh",
        "-c",
        "trap '' TERM; sleep 30 & wait",
    ]);
    assert_eq!(code, Some(137));
    assert!(
        took >= Duration::from_millis(550) && took < Duration::from_secs(5),
        "{took:?}"
    );
}

#[test]
fn statuses_pass_through_as_timeout_gives_them() {
    assert_eq!(grouptimeout(&["5", "sh", "-c", "exit 3"]).0, Some(3));
    assert_eq!(
        grouptimeout(&["5", "sh", "-c", "kill -INT $$"]).0,
        Some(130)
    );
    assert_eq!(grouptimeout(&["5", "/nonexistent/x"]).0, Some(127));
    assert_eq!(
        grouptimeout(&["--leader-only", "5", "sh", "-c", "sleep 30 & exit 4"]).0,
        Some(4)
    );
    assert_eq!(grouptimeout(&["bogus", "true"]).0, Some(125));
}
