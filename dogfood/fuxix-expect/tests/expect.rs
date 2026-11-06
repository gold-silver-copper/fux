//! fuxix-expect against real programs on a real PTY.
#![allow(clippy::unwrap_used, reason = "tests fail by panicking")]
use std::process::Command;
use std::time::{Duration, Instant};

fn expect(script: &str, options: &[&str], argv: &[&str]) -> (Option<i32>, String, String, Duration) {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| ".".into());
    let path = format!("{dir}/fuxix-expect-test-{}-{}.exp", std::process::id(), script.len());
    std::fs::write(&path, script).unwrap();
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_fuxix-expect"))
        .args(options)
        .arg(&path)
        .arg("--")
        .args(argv)
        .output()
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code(), text(&out.stdout), text(&out.stderr), start.elapsed())
}

#[test]
fn a_dialogue_runs_on_a_terminal_and_the_programs_status_is_the_exit_status() {
    let program = r#"[ -t 0 ] && printf 'tty %s\n' "$(stty size)"; printf 'name? '; read n; echo "hi $n"; exit 3"#;
    let (code, out, err, _) = expect(
        "expect tty 7 33\nexpect name? \nsend bob\\r\nexpect hi bob\n",
        &["--rows", "7", "--cols", "33"],
        &["sh", "-c", program],
    );
    assert_eq!(code, Some(3), "{out}{err}");
    // The PTY echoes what was typed, as a terminal does, and maps \n to \r\n.
    assert!(out.contains("name? bob\r\nhi bob\r\n"), "{out:?}");
}

#[test]
fn a_wait_that_times_out_hangs_the_program_up() {
    let (code, _, err, took) = expect("timeout 0.5\nexpect never\n", &[], &["sleep", "30"]);
    assert_eq!(code, Some(124), "{err}");
    assert!(err.contains("timed out waiting for \"never\""), "{err}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    let (code, _, err, _) = expect("expect never\n", &["-q"], &["true"]);
    assert_eq!(code, Some(125), "{err}");
}
