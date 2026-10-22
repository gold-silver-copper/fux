//! A pane's journal and its readiness, through fux's library alone: no
//! server, at most a `sleep` on a terminal of its own.
use fux::id::PaneId;
use fux::log::Seq;
use fux::pane::{self, Pane, Process, Runs};
use fux::ready::{self, State};
use std::time::{Duration, Instant};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A 5x20 pane running nothing, as a shell's.
fn pane() -> Result<Pane> {
    let id: PaneId = "%1".parse()?;
    Ok(Pane::new(
        id,
        "sh".into(),
        Runs::Shell,
        pane::size(5, 20),
        1 << 10,
    )?)
}

/// The newest piece of output in `pane`'s journal: each run's text and the
/// characters it changed.
fn last_runs(pane: &Pane) -> Vec<(String, (usize, usize))> {
    (pane.record.printed_after(Seq(0)).last())
        .map(|(_, printed)| {
            (printed.runs())
                .map(|(print, _, fresh)| (print.text.to_owned(), fresh))
                .collect()
        })
        .unwrap_or_default()
}

/// Ends the program `process` holds, if any, and reaps it.
fn end(process: Process) {
    let Process::Reading(child) = process else {
        return;
    };
    let fux::process::Child { mut leader, master } = child;
    leader.closed();
    leader.hang_up();
    drop(master);
    let Err(pid) = leader.finish() else {
        return;
    };
    for _ in 0..200 {
        if fux::process::reaped(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Within REDRAWS a line drawn again the same changed nothing, and no
/// piece lost prints.
#[test]
fn a_line_drawn_again_the_same_changes_nothing() -> Result {
    let mut pane = pane()?;
    pane.output(b"ab");
    assert_eq!(last_runs(&pane), [("ab".to_owned(), (0, 2))]);
    // Erased, which prints nothing, then drawn again as it read.
    pane.output(b"\r\x1b[K");
    pane.output(b"\rab");
    assert_eq!(last_runs(&pane), [("ab".to_owned(), (0, 0))]);
    assert!(!pane.record.prints_lost_after(Seq(0)));
    Ok(())
}

/// Past REDRAWS (1 MiB printed since the last input) every run is whole,
/// a line drawn again the same included.
#[test]
fn past_a_flood_every_run_is_whole() -> Result {
    let mut pane = pane()?;
    let flood: Vec<u8> = std::iter::repeat_n(b'y', 1 << 20).collect();
    pane.output(&flood);
    for _ in 0..1 << 16 {
        if !pane.has_unread() {
            break;
        }
        pane.read_unread();
    }
    if pane.has_unread() {
        return Err("the flood is never all read".into());
    }
    pane.output(b"\r\nab");
    pane.output(b"\r\x1b[K");
    pane.output(b"\rab");
    assert_eq!(last_runs(&pane), [("ab".to_owned(), (0, 2))]);
    Ok(())
}

/// A command typed, begun on the main screen, whose program is blocked
/// elsewhere (a relay's report: waiting) is at work, busy; not typed, it
/// waits.
#[test]
fn a_command_typed_blocked_elsewhere_is_busy() -> Result {
    let mut pane = pane()?;
    let argv = ["sleep".to_owned(), "30".to_owned()];
    let child = fux::process::spawn(&argv, std::path::Path::new("/"), (&[], &[]), pane.size())?;
    pane.process = Process::Reading(child);
    pane.output(b"\x1b]133;C\x07\x1b]133;S;activity=waiting\x07");
    let later = Instant::now().checked_add(Duration::from_secs(10));
    let typed = later.map(|now| ready::state(&pane, now, Some(0)));
    let alone = later.map(|now| ready::state(&pane, now, None));
    end(std::mem::replace(&mut pane.process, Process::Absent));
    assert_eq!(typed, Some(State::Busy));
    assert_eq!(alone, Some(State::Waiting));
    Ok(())
}
