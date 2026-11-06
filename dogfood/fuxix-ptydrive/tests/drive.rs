#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test fails by panicking"
)]
use fuxix::process::Status;
use fuxix_ptydrive::{Driver, Settled};
use std::time::Duration;

fn holds(output: &[u8], text: &str) -> bool {
    output.windows(text.len()).any(|w| w == text.as_bytes())
}

/// Types, resizes, settles and ends a shell, reading what it said.
#[test]
fn drives_a_shell_through_a_resize() {
    let mut drive = Driver::spawn(&["cmd.exe"], 24, 80).unwrap();
    drive.send(b"mode con & echo RE^ADY\r").unwrap();
    assert!(drive.wait_for(0, b"READY", Duration::from_secs(10)).unwrap());
    assert!(holds(drive.output(), "80"));
    drive.resize(30, 100).unwrap();
    let mark = drive.output().len();
    drive.send(b"mode con\r").unwrap();
    let settled = drive
        .settle(Duration::from_millis(500), Duration::from_secs(5))
        .unwrap();
    assert_eq!(settled, Settled::Quiet);
    assert!(holds(&drive.output()[mark..], "100"));
    drive.send(b"exit 7\r").unwrap();
    assert_eq!(
        drive.finish(Duration::from_secs(5)).unwrap(),
        Status::Exited(7)
    );
}

/// A program that floods output while we type a lot neither blocks the
/// driver; one that ignores its end is hung up.
#[test]
fn keeps_up_with_a_flood() {
    let flood = "for /L %i in (1,1,3000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let line = format!("{flood} & echo DO^NE & pause > NUL");
    let mut drive = Driver::spawn(&["cmd.exe", "/c", &line], 24, 80).unwrap();
    drive.send(&vec![b'y'; 2000]).unwrap();
    assert!(drive.wait_for(0, b"DONE", Duration::from_secs(30)).unwrap());
    drive.finish(Duration::from_millis(200)).unwrap();
}
