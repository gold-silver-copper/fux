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
    job: fuxix::process::Job,
    parser: Parser,
}

impl Run {
    fn start(rows: u16, cols: u16, args: &[&str]) -> Run {
        let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
        let (master, slave) = fuxix::pty::open(fuxix::terminal::Size::new(rows, cols).unwrap()).unwrap();
        let mut argv = vec![env!("CARGO_BIN_EXE_mix-mini-mux")];
        argv.extend_from_slice(args);
        let job = slave.spawn(&argv, |_| {}, Dispositions::Default).unwrap();
        let parser = Parser::with_options(Size::new(rows, cols).unwrap(), 0, Default::default()).unwrap();
        Run { master, job, parser }
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
            if let Some(status) = self.job.ended().unwrap() {
                return status;
            }
            assert!(Instant::now() < end, "the mux did not end");
            self.until("nothing", |_| true);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn two_programs_side_by_side() {
    let hold = "ping -n 30 127.0.0.1 > NUL";
    let (left, right) = (format!("echo LEFT-OK& {hold}"), format!("echo RIGHT-OK& {hold}"));
    let mut run = Run::start(10, 41, &[&left, &right]);
    run.until("both panes", |p| {
        let row = p.screen().window().row(0).map(|r| r.text()).unwrap_or_default();
        let row = row.trim_end();
        row.starts_with("LEFT-OK") && row.contains("RIGHT-OK")
    });
    assert!(run.row(9).contains("C-a o: switch"), "{}", run.row(9));
    assert!(run.parser.screen().mode(Mode::AlternateScreen));
    run.keys(b"\x01q");
    assert_eq!(run.ended(), Status::Exited(0));
}

#[test]
fn keys_go_to_the_focused_pane() {
    let left = "set /p x=? & call echo typed:%^x% & ping -n 30 127.0.0.1 > NUL".to_owned();
    let right = "ping -n 30 127.0.0.1 > NUL".to_owned();
    let mut run = Run::start(10, 41, &[&left, &right]);
    run.until("a prompt", |p| p.screen().window().row(0).is_some_and(|r| r.text().contains('?')));
    run.keys(b"hello\r");
    run.until("the echo", |p| (0..8).any(|r| p.screen().window().row(r).is_some_and(|r| r.text().contains("typed:hello"))));
    run.keys(b"\x01q");
    assert_eq!(run.ended(), Status::Exited(0));
}
