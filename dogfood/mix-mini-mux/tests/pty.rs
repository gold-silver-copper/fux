//! Runs the mux on a PTY, as a person's terminal would, and reads what it
//! paints through a fux-vt screen.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]

use fux_vt::{Mode, Parser, Size};
use fuxix::poll::{Events, PollFd};
use fuxix::process::{Dispositions, Pid, Status};
use fuxix::pty::Master;
use std::io::Write;
use std::time::{Duration, Instant};

struct Run {
    master: Master,
    pid: Pid,
    parser: Parser,
}

impl Run {
    fn start(rows: u16, cols: u16, args: &[&str]) -> Run {
        let (master, slave) = fuxix::pty::open(fuxix::terminal::Size::new(rows, cols).unwrap()).unwrap();
        let mut argv = vec![env!("CARGO_BIN_EXE_mix-mini-mux")];
        argv.extend_from_slice(args);
        let (_child, pid) = slave
            .spawn(&argv, |c| { c.env("SHELL", "/bin/sh"); }, Dispositions::Default)
            .unwrap();
        let parser = Parser::with_options(Size::new(rows, cols).unwrap(), 0, Default::default()).unwrap();
        Run { master, pid, parser }
    }

    /// Reads what the mux paints until `done` holds of the screen, or 10 s.
    fn until(&mut self, what: &str, done: impl Fn(&Parser) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        let mut buffer = [0u8; 4096];
        while !done(&self.parser) {
            assert!(Instant::now() < end, "never saw {what}; screen:\n{}", self.text());
            let mut fds = [PollFd::new(&self.master, Events::IN)];
            let _ = fuxix::poll::poll(&mut fds, Some(Duration::from_millis(50)));
            while let Ok(n @ 1..) = fuxix::io::read(&self.master, &mut buffer) {
                let _ = self.parser.process_all(&buffer[..n], &mut ());
            }
        }
    }

    fn row(&self, row: u16) -> String {
        self.parser.screen().window().row(row).map(|r| r.text()).unwrap_or_default()
    }

    fn text(&self) -> String {
        (0..self.parser.screen().size().rows()).map(|r| self.row(r) + "\n").collect()
    }

    fn keys(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }

    fn ended(&mut self) -> Status {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = fuxix::process::reap(self.pid).unwrap() {
                return status;
            }
            assert!(Instant::now() < end, "the mux did not end");
            self.until("nothing", |_| true);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn two_programs_side_by_side_and_the_terminal_given_back() {
    let mut run = Run::start(10, 41, &["echo LEFT-OK; exec sleep 30", "echo RIGHT-OK; exec sleep 30"]);
    run.until("both panes", |p| {
        let row = p.screen().window().row(0).map(|r| r.text()).unwrap_or_default();
        let row = row.trim_end();
        row.starts_with("LEFT-OK") && row.contains("RIGHT-OK")
    });
    let top = run.row(0);
    assert_eq!(top.find("RIGHT-OK"), Some(top.find('│').unwrap() + '│'.len_utf8()), "{top:?}");
    assert!(run.row(9).contains("C-a o: switch"), "{}", run.row(9));
    assert!(run.parser.screen().mode(Mode::AlternateScreen));
    run.keys(b"\x01q");
    assert_eq!(run.ended(), Status::Exited(0));
    run.until("the main screen back", |p| !p.screen().mode(Mode::AlternateScreen));
    assert!(fuxix::terminal::attributes(&run.master).unwrap().edits_lines(), "the terminal left raw");
}

#[test]
fn keys_go_to_the_focused_pane_and_a_resize_reaches_both() {
    let watcher = "trap 'stty size' WINCH; stty size; while :; do sleep 0.05; done";
    let mut run = Run::start(10, 41, &["exec cat", watcher]);
    run.until("the right pane's size", |p| p.screen().window().row(0).is_some_and(|r| r.text().trim_end().ends_with("9 20")));
    run.keys(b"hello\r");
    run.until("cat's echo", |p| p.screen().window().row(1).is_some_and(|r| r.text().starts_with("hello")));
    // Focus moves right: what is typed now goes to the watcher's pane.
    run.keys(b"\x01o");
    run.keys(b"xyz");
    run.until("typing in the right pane", |p| p.screen().window().row(1).is_some_and(|r| r.text().trim_end().ends_with("xyz")));
    assert!(!run.row(2).contains("xyz"), "{}", run.text());
    fuxix::terminal::set_window_size(&run.master, fuxix::terminal::Size::new(12, 61).unwrap(), None).unwrap();
    run.parser.resize(Size::new(12, 61).unwrap()).unwrap();
    run.until("the new size in the right pane", |p| (0..11).any(|r| p.screen().window().row(r).is_some_and(|r| r.text().trim_end().ends_with("11 30"))));
    run.keys(b"\x01q");
    assert_eq!(run.ended(), Status::Exited(0));
}
