#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "tests")]
//! Runs mix-nested-term on a PTY with a fux-vt screen as its outer
//! terminal, and checks the box, the program inside, keys, resize and exit.
use std::time::{Duration, Instant};

use fux_vt::{Options, Parser, Screen};
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Dispositions, Status};
use fuxix::{pty, terminal};

fn rows(screen: &Screen) -> Vec<String> {
    screen.window().iter().map(|(_, row)| row.text()).collect()
}

/// Feeds the outer screen what the app writes until `done` holds.
fn pump(master: &pty::Master, outer: &mut Parser, done: impl Fn(&Screen) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = [0u8; 65536];
    while !done(outer.screen()) {
        assert!(Instant::now() < deadline, "timed out; screen:\n{}", rows(outer.screen()).join("\n"));
        let mut fds = [PollFd::new(master, Events::IN)];
        let _ = fuxix::poll::poll(&mut fds, Some(Duration::from_millis(100)));
        match fuxix::io::read(master, &mut buf) {
            Ok(n) if n > 0 => {
                let _ = outer.process_all(&buf[..n], &mut ());
            }
            _ => {}
        }
    }
}

fn type_keys(master: &pty::Master, bytes: &[u8]) {
    assert_eq!(fuxix::io::write_what_fits(master, bytes).expect("write"), bytes.len());
}

#[test]
fn runs_a_program_in_the_box() {
    let (rows_n, cols_n) = (12, 40);
    let _ = pty::load(std::path::Path::new("no-pinned-conpty"));
    let (master, slave) = pty::open(terminal::Size::new(rows_n, cols_n).expect("size")).expect("pty");
    let script = "echo hi& set /p x=? & echo got:!x!& set /p y=? & exit 3";
    let argv = [env!("CARGO_BIN_EXE_mix-nested-term"), "cmd.exe", "/v:on", "/c", script];
    let job = slave.spawn(&argv, |_| {}, Dispositions::Default).expect("spawn");
    let mut outer = Parser::with_options(fux_vt::Size::new(rows_n, cols_n).expect("size"), 0, Options::default())
        .expect("parser");

    // The program's first line inside the border.
    pump(&master, &mut outer, |s| rows(s).iter().any(|r| r.starts_with("│hi")));
    let screen = outer.screen();
    let window = screen.window();
    assert_eq!(window.cell(0, 0).map(|c| c.contents().to_string()), Some("┌".into()));
    assert_eq!(window.cell(10, 39).map(|c| c.contents().to_string()), Some("┘".into()));
    let status = &rows(screen)[11];
    assert!(status.contains("9x38"), "status: {status}");

    // Keys reach it, and its echo comes back in the box.
    type_keys(&master, b"abc\r");
    pump(&master, &mut outer, |s| rows(s).iter().any(|r| r.starts_with("│got:abc")));

    // The outer terminal grows: the box follows.
    terminal::set_window_size(&master, terminal::Size::new(14, 50).expect("size"), None).expect("resize");
    outer.resize(fux_vt::Size::new(14, 50).expect("size")).expect("resize");
    pump(&master, &mut outer, |s| rows(s).get(13).is_some_and(|r| r.contains("11x48")));
    type_keys(&master, b"\r");

    // It exits: the status line says how, and a key quits with its code.
    pump(&master, &mut outer, |s| rows(s).get(13).is_some_and(|r| r.contains("exited 3")));
    type_keys(&master, b"q");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = job.ended().expect("ended") {
            break status;
        }
        assert!(Instant::now() < deadline, "the app did not quit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status, Status::Exited(3));
}
