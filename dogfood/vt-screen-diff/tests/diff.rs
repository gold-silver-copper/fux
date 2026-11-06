//! What vt-screen-diff promises, run on recordings as a user runs it.
#![allow(clippy::unwrap_used, reason = "tests fail by panicking")]
use std::path::PathBuf;
use std::process::Command;

fn file(name: &str, bytes: &[u8]) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn diff(args: &[&std::ffi::OsStr]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_vt-screen-diff"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Two raw recordings that paint the same text, one word in another colour
/// and drawn by other bytes (a cursor move, not spaces): the report names
/// the row and the cells by attribute, the bytes that differ but draw the
/// same are no difference, and the status says they differ.
#[test]
fn final_screens_differ_by_cell_and_attribute() {
    let left = file("left.raw", b"hello world\r\nsame line\r\n\x1b]2;one\x07");
    let right = file(
        "right.raw",
        b"hello \x1b[1;31mworld\x1b[m\r\nsame\x1b[Cline\r\n\x1b]2;two\x07",
    );
    let (code, out) = diff(&[
        "--size".as_ref(),
        "4x20".as_ref(),
        left.as_os_str(),
        right.as_os_str(),
    ]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("title: \"one\" | \"two\""), "{out}");
    assert!(out.contains("row 0:\n  - hello world\n  + hello world\n  cols 6-10: \"world\" | \"world\" [bold fg=1]\n"), "{out}");
    assert!(!out.contains("row 1"), "{out}");
    let (same, out) = diff(&[left.as_os_str(), left.as_os_str()]);
    assert_eq!(
        (same, out.contains("the final screens are the same")),
        (0, true),
        "{out}"
    );
}

/// Two casts whose final screens match but which differed on the way:
/// `--first` finds the event after which they first differ, with its time.
#[test]
fn first_finds_where_screens_diverge_even_if_they_meet_again() {
    let header = "{\"version\": 2, \"width\": 10, \"height\": 2}\n";
    let left = format!(
        "{header}[0.1, \"o\", \"ab\"]\n[0.2, \"o\", \"cd\"]\n[0.3, \"o\", \"\\u001b[2J\\u001b[Hx\"]\n"
    );
    let right = format!(
        "{header}[0.1, \"o\", \"ab\"]\n[0.25, \"o\", \"cX\"]\n[0.3, \"o\", \"\\u001b[2J\\u001b[Hx\"]\n"
    );
    let (left, right) = (
        file("left.cast", left.as_bytes()),
        file("right.cast", right.as_bytes()),
    );
    let (code, out) = diff(&["--first".as_ref(), left.as_os_str(), right.as_os_str()]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(
            "first differ after step 2 (left: 0.200s, byte 2; right: 0.250s, byte 2):\nrow 0:"
        ),
        "{out}"
    );
    assert!(out.contains("col 3: \"d\" | \"X\""), "{out}");
    assert!(out.contains("the final screens are the same"), "{out}");
}
