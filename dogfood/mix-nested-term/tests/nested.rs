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
    let (master, slave) = pty::open(terminal::Size::new(rows_n, cols_n).expect("size")).expect("pty");
    let script = "printf 'hi\\033[31mred\\033[0m\\n'; read x; echo got:$x; read y; stty size; exit 3";
    let argv = [env!("CARGO_BIN_EXE_mix-nested-term"), "sh", "-c", script];
    let (_child, pid) = slave.spawn(&argv, |_| {}, Dispositions::Default).expect("spawn");
    let mut outer = Parser::with_options(fux_vt::Size::new(rows_n, cols_n).expect("size"), 0, Options::default())
        .expect("parser");

    // The program's first line, red where it said, inside the border.
    pump(&master, &mut outer, |s| rows(s).get(1).is_some_and(|r| r.starts_with("│hired")));
    let screen = outer.screen();
    let window = screen.window();
    assert_eq!(window.cell(0, 0).map(|c| c.contents().to_string()), Some("┌".into()));
    assert_eq!(window.cell(10, 39).map(|c| c.contents().to_string()), Some("┘".into()));
    let red = window.cell(1, 3).expect("cell");
    assert_eq!((red.contents(), red.attributes().foreground()), ("r", fux_vt::Color::Idx(1)));
    let status = &rows(screen)[11];
    assert!(status.contains("9x38"), "status: {status}");

    // Keys reach it, and its echo comes back in the box.
    type_keys(&master, b"abc\r");
    pump(&master, &mut outer, |s| rows(s).iter().any(|r| r.starts_with("│got:abc")));

    // The outer terminal grows: the box and the program's PTY follow.
    terminal::set_window_size(&master, terminal::Size::new(14, 50).expect("size"), None).expect("resize");
    outer.resize(fux_vt::Size::new(14, 50).expect("size")).expect("resize");
    pump(&master, &mut outer, |s| rows(s).get(13).is_some_and(|r| r.contains("11x48")));
    type_keys(&master, b"\r");
    pump(&master, &mut outer, |s| rows(s).iter().any(|r| r.starts_with("│11 48")));

    // It exits: the status line says how, and a key quits with its code.
    pump(&master, &mut outer, |s| rows(s).get(13).is_some_and(|r| r.contains("exited 3")));
    type_keys(&master, b"q");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = fuxix::process::reap(pid).expect("reap") {
            break status;
        }
        assert!(Instant::now() < deadline, "the app did not quit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status, Status::Exited(3));
}
