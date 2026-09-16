//! A harness-owned controlling terminal: a viewer runs on the slave side of a pty as the
//! leader of its own session; the master side is drained by a thread into a bounded buffer so
//! the child never blocks on output, and keys are written to it verbatim.

use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use super::Result;

/// Output retained per terminal; a viewer repaints a few KiB per frame.
const OUTPUT_BOUND: usize = 4 * 1024 * 1024;

pub struct Terminal {
    child: Child,
    master: OwnedFd,
    output: Arc<Mutex<Vec<u8>>>,
}

impl Terminal {
    /// Spawns `command` as a session leader whose controlling terminal is a fresh
    /// `rows`x`cols` pty.
    pub fn spawn(mut command: Command, rows: u16, cols: u16) -> Result<Self> {
        let pty = nix::pty::openpty(
            Some(&nix::pty::Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )?;
        for fd in [&pty.master, &pty.slave] {
            nix::fcntl::fcntl(
                fd,
                nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
            )?;
        }
        command
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave.try_clone()?));
        // Only async-signal-safe calls run between fork and exec.
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
        let child = command.spawn()?;
        drop(pty.slave);
        let output = Arc::new(Mutex::new(Vec::new()));
        // The reader owns a dup of the master; it ends with EIO/EOF once every slave side is
        // gone, which the session kill in `Drop` guarantees.
        {
            let output = Arc::clone(&output);
            let mut master = std::fs::File::from(pty.master.try_clone()?);
            std::thread::spawn(move || {
                let mut buffer = [0u8; 65536];
                loop {
                    match master.read(&mut buffer) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
                            output.extend_from_slice(&buffer[..n]);
                            if output.len() > OUTPUT_BOUND {
                                let excess = output.len() - OUTPUT_BOUND;
                                output.drain(..excess);
                            }
                        }
                    }
                }
            });
        }
        Ok(Self {
            child,
            master: pty.master,
            output,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Writes `bytes` as typed input.
    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = nix::unistd::write(self.master.as_fd(), rest)?;
            rest = &rest[n..];
        }
        Ok(())
    }

    /// Everything the child wrote so far (bounded).
    pub fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// `SIGKILL` to the whole session (the wrapper and the viewer it runs), as an abrupt
    /// terminal death would.
    pub fn kill_session(&mut self) -> Result<()> {
        let pid = Pid::from_raw(i32::try_from(self.child.id())?);
        match killpg(pid, Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(e) => return Err(e.into()),
        }
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.kill_session();
        }
    }
}
