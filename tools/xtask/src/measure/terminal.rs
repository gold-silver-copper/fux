//! A blocking poll driver, not a periodically sampled capture RPC. Observation ends at the
//! viewer's terminal output; physical display/compositor latency is deliberately not claimed.
use super::{Process, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, poll};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RETAIN: usize = 4 * 1024 * 1024;
pub(super) struct Terminal {
    pub process: Process,
    master: OwnedFd,
    pub screen: Screen,
    pub spawned_at: Instant,
    pub raw: Vec<u8>,
    pub bytes: u64,
}
impl Terminal {
    pub fn spawn(mut command: Command, rows: u16, cols: u16) -> Result<Self> {
        let pair = nix::pty::openpty(
            Some(&nix::pty::Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )?;
        for fd in [&pair.master, &pair.slave] {
            fcntl(fd, FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC))?;
        }
        fcntl(&pair.master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
        command
            .stdin(Stdio::from(pair.slave.try_clone()?))
            .stdout(Stdio::from(pair.slave.try_clone()?))
            .stderr(Stdio::from(pair.slave.try_clone()?));
        // Only async-signal-safe calls occur after fork.
        unsafe {
            command.pre_exec(|| {
                if nix::libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "macos")]
                let request = nix::libc::c_ulong::from(nix::libc::TIOCSCTTY);
                #[cfg(not(target_os = "macos"))]
                let request = nix::libc::TIOCSCTTY;
                if nix::libc::ioctl(0, request, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawned_at = Instant::now();
        let process = Process(command.spawn()?);
        drop(pair.slave);
        Ok(Self {
            process,
            master: pair.master,
            screen: Screen::new(rows, cols),
            raw: Vec::new(),
            bytes: 0,
            spawned_at,
        })
    }
    pub fn send(&self, bytes: &[u8], timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut rest = bytes;
        while !rest.is_empty() {
            super::check_cancelled()?;
            if Instant::now() >= deadline {
                return Err("PTY input deadline".into());
            }
            match nix::unistd::write(&self.master, rest) {
                Ok(0) => return Err("PTY input closed".into()),
                Ok(n) => rest = &rest[n..],
                Err(nix::errno::Errno::EINTR) => {}
                Err(nix::errno::Errno::EAGAIN) => {
                    poll(
                        &mut [PollFd::new(self.master.as_fd(), PollFlags::POLLOUT)],
                        10u16,
                    )?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn drain(&mut self) -> Result<()> {
        let mut bytes = [0; 65536];
        // Bound each viewer's turn so a hot writer cannot starve other viewers/deadlines.
        for _ in 0..16 {
            match nix::unistd::read(&self.master, &mut bytes) {
                Ok(0) => return Err("viewer PTY EOF".into()),
                Ok(n) => {
                    self.bytes += n as u64;
                    self.screen.feed(&bytes[..n]);
                    let retain = (RETAIN - self.raw.len()).min(n);
                    self.raw.extend_from_slice(&bytes[..retain]);
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

pub(super) fn drive(
    terminals: &mut [Terminal],
    timeout: Duration,
    marker: Option<&str>,
) -> Result<Vec<f64>> {
    let start = Instant::now();
    let mut observed = vec![None; terminals.len()];
    loop {
        super::check_cancelled()?;
        for (i, terminal) in terminals.iter_mut().enumerate() {
            terminal.process.alive()?;
            terminal.drain()?;
            if observed[i].is_none() && marker.is_some_and(|m| terminal.screen.contains(m)) {
                observed[i] = Some(start.elapsed().as_secs_f64());
            }
        }
        if marker.is_some() && observed.iter().all(Option::is_some) {
            return Ok(observed.into_iter().flatten().collect());
        }
        let Some(left) = timeout.checked_sub(start.elapsed()) else {
            if let Some(marker) = marker {
                return Err(
                    format!("rendered marker {marker:?} deadline; observed {observed:?}").into(),
                );
            }
            return Ok(Vec::new());
        };
        let mut fds: Vec<_> = terminals
            .iter()
            .map(|t| PollFd::new(t.master.as_fd(), PollFlags::POLLIN))
            .collect();
        let ms = u16::try_from(left.as_millis().clamp(1, 1000))?;
        match poll(&mut fds, ms) {
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// Minimal ASCII probe-screen decoder for the CUP/SGR/erase repertoire emitted by both
/// viewers. Markers are ASCII, and matching is row-local in the rendered cell grid, never
/// raw command echo or OSC/title text. Non-ASCII glyphs occupy a single placeholder cell;
/// the workload deliberately uses ASCII only (this is not a general terminal emulator).
pub(super) struct Screen {
    cells: Vec<u8>,
    rows: usize,
    cols: usize,
    row: usize,
    col: usize,
    state: u8,
    escape: Vec<u8>,
    synchronized: bool,
}
impl Screen {
    fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = (usize::from(rows), usize::from(cols));
        Self {
            cells: vec![b' '; rows * cols],
            rows,
            cols,
            row: 0,
            col: 0,
            state: 0,
            escape: Vec::with_capacity(64),
            synchronized: false,
        }
    }
    pub fn contains(&self, marker: &str) -> bool {
        !self.synchronized
            && self
                .cells
                .chunks(self.cols)
                .any(|row| row.windows(marker.len()).any(|w| w == marker.as_bytes()))
    }
    pub fn text(&self) -> String {
        self.cells
            .chunks(self.cols)
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                1 => match b {
                    b'[' => {
                        self.state = 2;
                        self.escape.clear();
                    }
                    b']' | b'P' | b'_' | b'^' => self.state = 3,
                    b'(' | b')' => self.state = 5,
                    _ => self.state = 0,
                },
                2 => {
                    if (0x40..=0x7e).contains(&b) {
                        self.csi(b);
                        self.state = 0;
                    } else if self.escape.len() < 128 {
                        self.escape.push(b);
                    } else {
                        self.state = 0;
                    }
                }
                3 => {
                    if b == 7 {
                        self.state = 0;
                    } else if b == 27 {
                        self.state = 4;
                    }
                }
                4 => {
                    self.state = if b == b'\\' { 0 } else { 3 };
                }
                5 => self.state = 0,
                _ => match b {
                    27 => self.state = 1,
                    b'\r' => self.col = 0,
                    b'\n' => self.newline(),
                    8 => self.col = self.col.saturating_sub(1),
                    b'\t' => self.col = ((self.col / 8 + 1) * 8).min(self.cols - 1),
                    0x20..=0x7e | 0xc0..=0xf7 => {
                        if self.col >= self.cols {
                            self.col = 0;
                            self.newline();
                        }
                        self.cells[self.row * self.cols + self.col] =
                            if b < 128 { b } else { b'?' };
                        self.col += 1;
                    }
                    _ => {}
                },
            }
        }
    }
    fn newline(&mut self) {
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.cells.copy_within(self.cols.., 0);
            self.cells[(self.rows - 1) * self.cols..].fill(b' ');
        }
    }
    fn csi(&mut self, final_byte: u8) {
        let private = self.escape.first() == Some(&b'?');
        let text = String::from_utf8_lossy(&self.escape);
        let params: Vec<usize> = text
            .trim_start_matches('?')
            .split(';')
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        let p = |i: usize, default: usize| {
            params
                .get(i)
                .copied()
                .filter(|&v| v != 0)
                .unwrap_or(default)
        };
        if private {
            if params.contains(&2026) {
                self.synchronized = final_byte == b'h';
            }
            if params.contains(&1049) && final_byte == b'h' {
                self.cells.fill(b' ');
                self.row = 0;
                self.col = 0;
            }
            return;
        }
        match final_byte {
            b'H' | b'f' => {
                self.row = (p(0, 1) - 1).min(self.rows - 1);
                self.col = (p(1, 1) - 1).min(self.cols - 1);
            }
            b'A' => self.row = self.row.saturating_sub(p(0, 1)),
            b'B' => self.row = (self.row + p(0, 1)).min(self.rows - 1),
            b'C' => self.col = (self.col + p(0, 1)).min(self.cols - 1),
            b'D' => self.col = self.col.saturating_sub(p(0, 1)),
            b'G' => self.col = (p(0, 1) - 1).min(self.cols - 1),
            b'd' => self.row = (p(0, 1) - 1).min(self.rows - 1),
            b'J' => {
                let at = (self.row * self.cols + self.col).min(self.cells.len() - 1);
                match p(0, 0) {
                    0 => self.cells[at..].fill(b' '),
                    1 => self.cells[..=at].fill(b' '),
                    2 | 3 => self.cells.fill(b' '),
                    _ => {}
                }
            }
            b'K' => {
                let start = self.row * self.cols;
                let at = self.col.min(self.cols - 1);
                match p(0, 0) {
                    0 => self.cells[start + at..start + self.cols].fill(b' '),
                    1 => self.cells[start..=start + at].fill(b' '),
                    2 => self.cells[start..start + self.cols].fill(b' '),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
