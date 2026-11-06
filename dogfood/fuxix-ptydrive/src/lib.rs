//! Drive a terminal program on a PTY: start it at a size, type at it,
//! resize it, wait for its output to settle, and keep every byte it wrote.
//!
//! ```no_run
//! use fuxix_ptydrive::Driver;
//! use std::time::Duration;
//! let mut drive = Driver::spawn(&["sh"], 24, 80)?;
//! drive.send(b"echo hi\r")?;
//! drive.settle(Duration::from_millis(100), Duration::from_secs(5))?;
//! assert!(drive.output().windows(2).any(|w| w == b"hi"));
//! # Ok::<(), std::io::Error>(())
//! ```
use fuxix::Errno;
use fuxix::io;
use fuxix::poll::{Events, PollFd};
use fuxix::process::{self, Dispositions, Pid, Signal, Status};
use fuxix::pty::Master;
use fuxix::terminal::Size;
use std::ffi::OsStr;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub mod script;

/// A program running on a PTY this driver holds the master of.
pub struct Driver {
    master: Master,
    child: Child,
    pid: Pid,
    output: Vec<u8>,
    /// The master read its end: every slave is closed.
    hung_up: bool,
    status: Option<Status>,
}

/// Why [`Driver::settle`] returned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Settled {
    /// No output for the quiet time.
    Quiet,
    /// Output kept coming until the limit.
    TimedOut,
    /// The program ended and its output is all read.
    Ended,
}

/// A size of `rows` by `cols`, neither zero.
pub fn size(rows: u16, cols: u16) -> std::io::Result<Size> {
    Size::new(rows, cols).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("a terminal of {rows}x{cols} has no cells"),
        )
    })
}

impl Driver {
    /// Starts `argv` on a new PTY of `rows` by `cols`, with this process's
    /// environment and `TERM=xterm-256color`.
    pub fn spawn<S: AsRef<OsStr>>(argv: &[S], rows: u16, cols: u16) -> std::io::Result<Driver> {
        Driver::spawn_with(argv, rows, cols, |command| {
            command.env("TERM", "xterm-256color");
        })
    }

    /// Starts `argv` on a new PTY of `rows` by `cols`; `setup` sets its
    /// directory and environment.
    pub fn spawn_with<S: AsRef<OsStr>>(
        argv: &[S],
        rows: u16,
        cols: u16,
        setup: impl FnOnce(&mut Command),
    ) -> std::io::Result<Driver> {
        let (master, slave) = fuxix::pty::open(size(rows, cols)?).map_err(std::io::Error::other)?;
        let (child, pid) = slave.spawn(argv, Dispositions::Default, setup)?;
        Ok(Driver {
            master,
            child,
            pid,
            output: Vec::new(),
            hung_up: false,
            status: None,
        })
    }

    /// The program's pid, which is also its session and process group.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Every byte the program wrote so far.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Takes the output read so far, leaving none.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// How the program ended, once it has and was reaped.
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// Whether the program last asked for application cursor keys
    /// (`CSI ? 1 h`), so arrows should be sent as `SS3 A` rather than
    /// `CSI A`.
    pub fn application_cursor(&self) -> bool {
        let set = rfind(&self.output, b"\x1b[?1h");
        let reset = rfind(&self.output, b"\x1b[?1l");
        set > reset
    }

    /// Types `bytes` at the program, reading its output while the PTY is
    /// too full to take more, so a program that echoes never blocks us.
    pub fn send(&mut self, mut bytes: &[u8]) -> std::io::Result<()> {
        loop {
            bytes = bytes.get(io::write_what_fits(&self.master, bytes)?..).unwrap_or_default();
            if bytes.is_empty() {
                return Ok(());
            }
            let mut fds = [PollFd::new(&self.master, Events::IN | Events::OUT)];
            retry(|| fuxix::poll::poll(&mut fds, Some(Duration::from_secs(1))))?;
            if fds[0].revents().intersects(Events::IN | Events::HUP) {
                self.drain()?;
            }
            if self.hung_up {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
        }
    }

    /// Resizes the PTY; the kernel tells the program with `SIGWINCH`.
    pub fn resize(&mut self, rows: u16, cols: u16) -> std::io::Result<()> {
        fuxix::terminal::set_window_size(&self.master, size(rows, cols)?, None)?;
        Ok(())
    }

    /// Reads output until none comes for `quiet`, the program ends, or
    /// `limit` passes.
    pub fn settle(&mut self, quiet: Duration, limit: Duration) -> std::io::Result<Settled> {
        let start = Instant::now();
        let mut last = start;
        loop {
            let now = Instant::now();
            let until_quiet = quiet.saturating_sub(now.saturating_duration_since(last));
            let until_limit = limit.saturating_sub(now.saturating_duration_since(start));
            if self.ended()? {
                return Ok(Settled::Ended);
            }
            if until_quiet.is_zero() {
                return Ok(Settled::Quiet);
            }
            if until_limit.is_zero() {
                return Ok(Settled::TimedOut);
            }
            if self.pump(until_quiet.min(until_limit))? {
                last = Instant::now();
            }
        }
    }

    /// Reads output until it holds `needle` after the first `from` bytes,
    /// the program ends, or `limit` passes: whether it was found.
    pub fn wait_for(
        &mut self,
        from: usize,
        needle: &[u8],
        limit: Duration,
    ) -> std::io::Result<bool> {
        let start = Instant::now();
        loop {
            let tail = self.output.get(from..).unwrap_or_default();
            if needle.is_empty() || tail.windows(needle.len()).any(|w| w == needle) {
                return Ok(true);
            }
            let left = limit.saturating_sub(start.elapsed());
            if left.is_zero() || self.ended()? {
                return Ok(false);
            }
            self.pump(left)?;
        }
    }

    /// Waits up to `limit` for the program to end, reading its output; then
    /// hangs it up (`SIGHUP` to its group, as a closed terminal would) and,
    /// if that is not enough within a second, kills it. How it ended.
    pub fn finish(&mut self, limit: Duration) -> std::io::Result<Status> {
        let _ = self.settle(limit, limit)?;
        for signal in [Signal::Hup, Signal::Kill] {
            if let Some(status) = self.reaped()? {
                return Ok(status);
            }
            // ESRCH: the group is gone already; the reap below says how.
            match process::kill_group(self.pid, signal) {
                Ok(()) | Err(Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
            let _ = self.settle(Duration::from_secs(1), Duration::from_secs(1))?;
        }
        // Killed: it ends once the kernel has torn it down.
        for _ in 0..100 {
            if let Some(status) = self.reaped()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(std::io::Error::other("the program outlived SIGKILL"))
    }

    /// Whether the program has ended with its output all read: once it
    /// ended, what the master has left is all it wrote.
    fn ended(&mut self) -> std::io::Result<bool> {
        if self.hung_up {
            return Ok(true);
        }
        if self.status.is_none() && process::ended(self.pid)?.is_none() {
            return Ok(false);
        }
        // Exited; what it wrote before may still be in the PTY.
        self.drain()?;
        Ok(true)
    }

    /// Reaps the program if it has ended.
    fn reaped(&mut self) -> std::io::Result<Option<Status>> {
        if self.status.is_none() {
            self.status = process::reap(self.pid)?;
        }
        Ok(self.status)
    }

    /// Waits up to `timeout` for output and reads all there is: whether any
    /// came.
    fn pump(&mut self, timeout: Duration) -> std::io::Result<bool> {
        let mut fds = [PollFd::new(&self.master, Events::IN)];
        retry(|| fuxix::poll::poll(&mut fds, Some(timeout)))?;
        if fds[0].revents().is_empty() {
            return Ok(false);
        }
        self.drain()
    }

    /// Reads what the master has without waiting: whether any came.
    fn drain(&mut self) -> std::io::Result<bool> {
        let mut buffer = [0u8; 16384];
        let mut any = false;
        loop {
            match io::read(&self.master, &mut buffer) {
                Ok(0) | Err(Errno::IO) => {
                    self.hung_up = true;
                    return Ok(any);
                }
                Ok(count) => {
                    self.output
                        .extend_from_slice(buffer.get(..count).unwrap_or_default());
                    any = true;
                }
                Err(Errno::AGAIN) => return Ok(any),
                Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// Killed and reaped, so no test leaves a program behind.
impl Drop for Driver {
    fn drop(&mut self) {
        if self.status.is_none() && process::kill_group(self.pid, Signal::Kill).is_ok() {
            let _ = self.child.wait();
        }
    }
}

/// `call` again while a signal interrupts it.
fn retry<T>(mut call: impl FnMut() -> fuxix::Result<T>) -> fuxix::Result<T> {
    loop {
        match call() {
            Err(Errno::INTR) => {}
            result => return result,
        }
    }
}

/// Where `needle` last starts in `haystack`, plus one; 0 if nowhere.
fn rfind(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .rposition(|w| w == needle)
        .map_or(0, |at| at.saturating_add(1))
}
