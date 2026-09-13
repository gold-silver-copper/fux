//! A task-owned controlling terminal with bounded output and child cleanup.
use super::{local::Root, process::Guard};
use anyhow::{Result, ensure};
use std::{
    os::unix::process::CommandExt,
    path::Path,
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub struct Terminal {
    pub child: Guard,
    pty: nix::pty::OpenptyResult,
    raw: Vec<u8>,
    escapes: regex::bytes::Regex,
    visual: Option<super::visual::Capture>,
    failure: Option<usize>,
}
impl Terminal {
    pub fn start(root: &Root, binary: &Path) -> Result<Self> {
        Self::start_with_args(root, binary, &[])
    }
    pub fn start_with_args(root: &Root, binary: &Path, args: &[&str]) -> Result<Self> {
        Self::start_with_size(root, binary, args, 24, 80)
    }
    pub fn start_with_size(
        root: &Root,
        binary: &Path,
        args: &[&str],
        rows: u16,
        columns: u16,
    ) -> Result<Self> {
        let pty = super::pty::open(rows, columns)?;
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        let mut command = root.command(binary);
        command.args(args);
        command
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave.try_clone()?));
        // Only async-signal-safe syscalls execute in the forked child.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "macos")]
                let request = libc::c_ulong::from(libc::TIOCSCTTY);
                #[cfg(not(target_os = "macos"))]
                let request = libc::TIOCSCTTY;
                if libc::ioctl(0, request, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let escapes = regex::bytes::Regex::new(
            r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]",
        )?;
        Ok(Self {
            failure: super::failure::terminal(rows, columns),
            visual: super::visual::Capture::new(binary, args, rows, columns)?,
            child: Guard(command.spawn()?),
            pty,
            raw: Vec::new(),
            escapes,
        })
    }
    pub fn pump(&mut self) -> Result<()> {
        let mut bytes = [0; 65536];
        loop {
            match nix::unistd::read(&self.pty.master, &mut bytes) {
                Ok(0) | Err(nix::errno::Errno::EIO | nix::errno::Errno::EAGAIN) => return Ok(()),
                Ok(count) => {
                    super::failure::output(self.failure, &bytes[..count]);
                    if let Some(visual) = &mut self.visual {
                        visual.feed(&bytes[..count])?;
                    }
                    self.raw.extend_from_slice(&bytes[..count]);
                    ensure!(self.raw.len() <= 16 * 1024 * 1024, "terminal output bound");
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    /// Drain the real PTY throughout a measurement window without retaining output here.
    pub fn pump_for(&mut self, duration: Duration, mut output: impl FnMut(&[u8])) -> Result<()> {
        use std::os::fd::AsFd;
        let deadline = Instant::now() + duration;
        let mut bytes = [0; 65536];
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut polls = [nix::poll::PollFd::new(
                self.pty.master.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            match nix::poll::poll(
                &mut polls,
                u16::try_from(remaining.as_millis().clamp(1, 1000))?,
            ) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
            match nix::unistd::read(&self.pty.master, &mut bytes) {
                Ok(0) | Err(nix::errno::Errno::EIO) => return Ok(()),
                Ok(count) => {
                    super::failure::output(self.failure, &bytes[..count]);
                    if let Some(visual) = &mut self.visual {
                        visual.feed(&bytes[..count])?;
                    }
                    output(&bytes[..count]);
                }
                Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        super::failure::input(self.failure, bytes);
        ensure!(
            nix::unistd::write(&self.pty.master, bytes)? == bytes.len(),
            "terminal input short write"
        );
        Ok(())
    }
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.raw)
    }
    pub fn close(&mut self) -> Result<()> {
        let captured = (|| {
            self.pump()?;
            self.checkpoint("terminal-close")
        })();
        if self.child.0.try_wait()?.is_none() {
            self.child.0.kill()?;
            // Keep servicing the PTY while reaping, as wait() does for natural
            // exits. Killing a viewer does not prove its output is quiescent.
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                self.pump()?;
                if self.child.0.try_wait()?.is_some() {
                    break;
                }
                ensure!(
                    Instant::now() < deadline,
                    "owned terminal child {} wait timed out",
                    self.child.0.id()
                );
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
        captured.map(|_| ())
    }
    pub fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
        use std::os::fd::AsRawFd;
        ensure!(
            self.child.0.try_wait()?.is_none(),
            "resize after viewer exit"
        );
        if let Some(visual) = &mut self.visual {
            visual.resize(rows, columns)?;
        }
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // The live fixture-owned PTY receives an initialized winsize.
        if unsafe { libc::ioctl(self.pty.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        super::failure::resize(self.failure, rows, columns);
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(self.child.0.id())?),
            nix::sys::signal::Signal::SIGWINCH,
        )?;
        Ok(())
    }
    pub fn wait_for(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            self.pump()?;
            let text = self.escapes.replace_all(&self.raw, &b""[..]);
            if text
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
            {
                self.checkpoint(needle)?;
                return Ok(());
            }
            ensure!(
                self.child.0.try_wait()?.is_none() && Instant::now() < deadline,
                "terminal missing {needle:?}: {}",
                String::from_utf8_lossy(&self.raw)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
        super::local::until(timeout, || {
            self.pump()?;
            Ok(self.child.0.try_wait()?)
        })
    }
    pub fn checkpoint(&mut self, label: &str) -> Result<Option<String>> {
        super::failure::checkpoint(self.failure, label);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.frame_pending()? {
            self.pump()?;
            ensure!(
                Instant::now() < deadline,
                "unfinished synchronized frame at {label}"
            );
            if self.frame_pending()? {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        match &mut self.visual {
            Some(visual) => visual.checkpoint(label),
            None => Ok(None),
        }
    }
    pub fn frame_pending(&self) -> Result<bool> {
        self.visual
            .as_ref()
            .map_or(Ok(false), super::visual::Capture::frame_pending)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("terminal cleanup: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_reaps_a_terminal_with_output_in_flight() -> Result<()> {
        let root = Root::new("fterm-reap-", &["/bin/cat".into()])?;
        let mut terminal = Terminal::start_with_args(
            &root,
            Path::new("/bin/sh"),
            &[
                "-c",
                "printf READY; read line; printf '%262144s' x; exec sleep 30",
            ],
        )?;
        terminal.wait_for("READY", Duration::from_secs(3))?;
        terminal.send(b"go\n")?;
        terminal.close()?;
        ensure!(
            terminal.child.0.try_wait()?.is_some(),
            "cleanup left the child running"
        );
        // Repeated owner/drop cleanup must also succeed after the child was reaped.
        terminal.close()?;
        Ok(())
    }
}
