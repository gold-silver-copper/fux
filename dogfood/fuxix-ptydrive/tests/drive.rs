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
    let mut drive = Driver::spawn(&["sh"], 24, 80).unwrap();
    drive.send(b"stty size; echo RE''ADY\r").unwrap();
    assert!(
        drive
            .wait_for(0, b"READY\r\n", Duration::from_secs(5))
            .unwrap()
    );
    assert!(holds(drive.output(), "24 80"));
    drive.resize(30, 100).unwrap();
    let mark = drive.output().len();
    drive.send(b"stty size\r").unwrap();
    let settled = drive
        .settle(Duration::from_millis(200), Duration::from_secs(5))
        .unwrap();
    assert_eq!(settled, Settled::Quiet);
    assert!(holds(&drive.output()[mark..], "30 100"));
    drive.send(b"exit 7\r").unwrap();
    assert_eq!(
        drive.finish(Duration::from_secs(5)).unwrap(),
        Status::Exited(7)
    );
}

/// A program that floods output while we type a lot neither blocks the
/// driver nor loses a byte; one that ignores its end is hung up.
#[test]
fn keeps_every_byte_of_a_flood() {
    let mut drive = Driver::spawn(&["sh", "-c", "stty raw -echo; head -c 3000000 /dev/zero | tr '\\0' x; echo; echo DONE; cat >/dev/null"], 24, 80).unwrap();
    drive.send(&vec![b'y'; 200_000]).unwrap();
    assert!(drive.wait_for(0, b"DONE", Duration::from_secs(20)).unwrap());
    let xs = drive.output().iter().filter(|&&b| b == b'x').count();
    assert_eq!(xs, 3_000_000);
    // `cat` waits for input forever: finish hangs it up.
    let status = drive.finish(Duration::from_millis(200)).unwrap();
    assert_eq!(status, Status::Signalled { signal: fuxix::process::Signal::Hup.raw(), core_dumped: false });
}
