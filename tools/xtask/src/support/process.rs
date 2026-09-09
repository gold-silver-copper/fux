//! Bounded cleanup for fixture-owned children, retaining failures across owners.
use anyhow::{Result, ensure};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use std::{
    process::{Child, ExitStatus},
    time::{Duration, Instant},
};

pub trait OwnedProcess {
    fn running(&mut self) -> Result<bool>;
    fn terminate(&mut self) -> Result<()>;
    fn wait_bounded(&mut self, timeout: Duration) -> Result<()>;
    fn kill(&mut self) -> Result<()>;
}

pub fn wait(child: &mut Child, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "owned child {} wait timed out",
            child.id()
        );
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}
impl OwnedProcess for Child {
    fn running(&mut self) -> Result<bool> {
        Ok(self.try_wait()?.is_none())
    }
    fn terminate(&mut self) -> Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        kill(Pid::from_raw(i32::try_from(self.id())?), Signal::SIGTERM)?;
        Ok(())
    }
    fn wait_bounded(&mut self, timeout: Duration) -> Result<()> {
        wait(self, timeout)?;
        Ok(())
    }
    fn kill(&mut self) -> Result<()> {
        Child::kill(self)?;
        Ok(())
    }
}

pub fn stop_owned(children: &mut [Option<&mut dyn OwnedProcess>]) -> Vec<String> {
    let mut failures = Vec::new();
    for child in children.iter_mut().flatten() {
        let primary = (|| -> Result<()> {
            if child.running()? {
                child.terminate()?;
            }
            child.wait_bounded(Duration::from_secs(10))
        })();
        if let Err(error) = primary {
            failures.push(format!("{error:#}"));
            let fallback = child
                .kill()
                .and_then(|()| child.wait_bounded(Duration::from_secs(3)));
            if let Err(error) = fallback {
                failures.push(format!("{error:#}"));
            }
        }
    }
    failures
}

/// Scope cleanup for fixture clients. Scenario assertions still verify normal exit explicitly.
pub struct Guard(pub Child);
impl Drop for Guard {
    fn drop(&mut self) {
        for error in stop_owned(&mut [Some(&mut self.0)]) {
            eprintln!("fixture child cleanup: {error}");
        }
    }
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Bounded external CLI call using files so descendants cannot hold a capture pipe open.
pub fn output(
    mut command: std::process::Command,
    timeout: Duration,
    maximum: u64,
) -> Result<Output> {
    use std::{
        io::{Read, Seek, SeekFrom},
        process::Stdio,
    };
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Guard(
        command
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()?,
    );
    let deadline = Instant::now() + timeout;
    let status = (|| -> Result<ExitStatus> {
        loop {
            ensure!(
                stdout.metadata()?.len() <= maximum && stderr.metadata()?.len() <= maximum,
                "CLI output limit exceeded"
            );
            if let Some(status) = child.0.try_wait()? {
                return Ok(status);
            }
            ensure!(Instant::now() < deadline, "CLI deadline exceeded");
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = child.0.kill();
            wait(&mut child.0, Duration::from_secs(3))?;
            return Err(error);
        }
    };
    let read = |file: &mut std::fs::File| -> Result<Vec<u8>> {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(maximum + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= maximum, "CLI output limit exceeded");
        Ok(bytes)
    };
    Ok(Output {
        status,
        stdout: read(&mut stdout)?,
        stderr: read(&mut stderr)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminate_of_a_reaped_child_does_not_signal_its_old_pid() -> Result<()> {
        let mut child = std::process::Command::new("/usr/bin/true").spawn()?;
        assert!(wait(&mut child, Duration::from_secs(3))?.success());
        child.terminate()?;
        assert!(child.try_wait()?.is_some());
        Ok(())
    }
    #[derive(Default)]
    struct Fixture {
        wait_failure: bool,
        kill_failure: bool,
        calls: Vec<String>,
    }
    impl OwnedProcess for Fixture {
        fn running(&mut self) -> Result<bool> {
            Ok(true)
        }
        fn terminate(&mut self) -> Result<()> {
            self.calls.push("terminate".into());
            Ok(())
        }
        fn wait_bounded(&mut self, timeout: Duration) -> Result<()> {
            self.calls.push(format!("wait:{}", timeout.as_secs()));
            if std::mem::take(&mut self.wait_failure) {
                anyhow::bail!("wait failed");
            }
            Ok(())
        }
        fn kill(&mut self) -> Result<()> {
            self.calls.push("kill".into());
            ensure!(!self.kill_failure, "cannot kill");
            Ok(())
        }
    }
    #[test]
    fn timeout_falls_back_and_reaps_every_remaining_owner() {
        let mut watcher = Fixture {
            wait_failure: true,
            ..Fixture::default()
        };
        let mut observer = Fixture::default();
        let mut server = Fixture::default();
        let errors = stop_owned(&mut [Some(&mut watcher), Some(&mut observer), Some(&mut server)]);
        assert_eq!(errors.len(), 1);
        assert_eq!(watcher.calls, ["terminate", "wait:10", "kill", "wait:3"]);
        assert_eq!(observer.calls, server.calls);
        assert_eq!(server.calls, ["terminate", "wait:10"]);
    }
    #[test]
    fn failed_kill_still_reaches_other_owners() {
        let mut watcher = Fixture {
            wait_failure: true,
            kill_failure: true,
            ..Fixture::default()
        };
        let mut server = Fixture::default();
        assert_eq!(
            stop_owned(&mut [Some(&mut watcher), None, Some(&mut server)]).len(),
            2
        );
        assert_eq!(server.calls, ["terminate", "wait:10"]);
    }
}
