//! Bounded noninteractive child-process effects. Callers own argv, environment and policy.
use anyhow::{Context, Result};
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const MAX_OUTPUT: usize = 262144;
pub(crate) struct Running {
    child: Option<Child>,
    reaped: bool,
    cleanup_deadline: Option<Instant>,
}
impl Running {
    pub(crate) fn new(child: Child) -> Self {
        Self {
            child: Some(child),
            reaped: false,
            cleanup_deadline: None,
        }
    }

    pub(crate) fn child_mut(&mut self) -> Result<&mut Child> {
        self.child
            .as_mut()
            .context("owned subprocess handle missing")
    }

    /// Signal while the unreaped group leader still reserves its identity.
    pub(crate) fn stop(&mut self, deadline: Instant) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        self.cleanup_deadline = Some(deadline);
        anyhow::ensure!(
            Instant::now() < deadline,
            "owned subprocess cleanup deadline exceeded"
        );
        let group = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(i32::try_from(self.child_mut()?.id())?),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = self.child_mut()?.kill();
        loop {
            if self.child_mut()?.try_wait()?.is_some() {
                self.reaped = true;
                // A missing group is already gone. Other group errors cannot be
                // promoted to successful descendant-cleanup evidence.
                match group {
                    Ok(()) | Err(nix::errno::Errno::ESRCH) => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "owned subprocess cleanup deadline exceeded"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        // The process group was created by this command. Include its descendants.
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = child.kill();
        let deadline = self
            .cleanup_deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(3));
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // A native wait can outlast the caller's cleanup deadline. Keep the owned
        // handle solely for reaping; the caller has already received a cleanup
        // error, never a successful termination claim. No later PID signaling.
        let _ = std::thread::Builder::new()
            .name("zor-child-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    }
}
fn drain(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                anyhow::ensure!(
                    output.len() + count <= MAX_OUTPUT,
                    "subprocess output limit exceeded"
                );
                output.extend_from_slice(
                    buffer
                        .get(..count)
                        .context("invalid subprocess output length")?,
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
pub(crate) struct Output {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub(crate) fn run_command(command: &mut Command, deadline: Instant) -> Result<Output> {
    anyhow::ensure!(Instant::now() < deadline, "subprocess deadline exceeded");
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("start subprocess")?;
    let mut child = Running::new(child);
    let mut stdout = child
        .child_mut()?
        .stdout
        .take()
        .context("subprocess stdout missing")?;
    let mut stderr = child
        .child_mut()?
        .stderr
        .take()
        .context("subprocess stderr missing")?;
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        nix::fcntl::fcntl(
            fd,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
    }
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        anyhow::ensure!(
            Instant::now() < deadline,
            "subprocess deadline exceeded; outcome may be uncertain"
        );
        let out_done = drain(&mut stdout, &mut out)?;
        let err_done = drain(&mut stderr, &mut err)?;
        if out_done
            && err_done
            && let Some(status) = child.child_mut()?.try_wait()?
        {
            child.reaped = true;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    struct Input(u8);
    impl Read for Input {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0 += 1;
            match self.0 {
                1 => {
                    buffer
                        .get_mut(..3)
                        .expect("drain buffer")
                        .copy_from_slice(b"abc");
                    Ok(3)
                }
                2 => Err(std::io::ErrorKind::Interrupted.into()),
                3 => Err(std::io::ErrorKind::WouldBlock.into()),
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn interrupted_and_partial_reads_preserve_bytes_until_eof() {
        let mut input = Input(0);
        let mut output = Vec::new();
        assert!(!drain(&mut input, &mut output).expect("temporarily blocked"));
        assert_eq!(output, b"abc");
        assert!(drain(&mut input, &mut output).expect("EOF"));
        assert_eq!(output, b"abc");
        let mut output = vec![0; MAX_OUTPUT - 1];
        assert!(drain(&mut Input(0), &mut output).is_err());
        assert_eq!(output.len(), MAX_OUTPUT - 1);
    }

    #[test]
    fn expired_caller_deadline_prevents_process_creation() {
        let root = tempfile::tempdir().expect("private fixture");
        let marker = root.path().join("must-not-exist");
        let mut command = Command::new("/usr/bin/touch");
        command.arg(&marker);
        assert!(run_command(&mut command, Instant::now()).is_err());
        assert!(!marker.exists());
    }
}
