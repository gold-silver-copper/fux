//! Bounded, optional desktop delivery. Notification policy belongs to the dashboard.
use anyhow::{Context, Result};
use std::{
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Running {
    child: Child,
    deadline: Instant,
}
impl Drop for Running {
    fn drop(&mut self) {
        // Only signal an unreaped child, whose PID cannot have been reused.
        if matches!(self.child.try_wait(), Ok(None)) {
            if let Ok(pid) = i32::try_from(self.child.id()) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(crate) struct Delivery {
    program: Option<PathBuf>,
    running: Option<Running>,
}
impl Delivery {
    pub(crate) fn new(program: Option<PathBuf>) -> Self {
        Self {
            program,
            running: None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.running.is_some()
    }

    pub(crate) fn start(&mut self, title: &str, body: &str) -> Result<()> {
        anyhow::ensure!(!self.busy(), "notification delivery already running");
        anyhow::ensure!(
            title.len() <= 128 && body.len() <= 512,
            "notification text exceeds limit"
        );
        let mut command = if let Some(program) = &self.program {
            let mut command = Command::new(program);
            command.arg(title).arg(body);
            command
        } else {
            native_command(title, body)?
        };
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .context("start notification command")?;
        self.running = Some(Running {
            child,
            deadline: Instant::now() + Duration::from_secs(2),
        });
        Ok(())
    }

    pub(crate) fn poll(&mut self) -> Option<Result<()>> {
        let running = self.running.as_mut()?;
        let result = match running.child.try_wait() {
            Ok(Some(status)) if status.success() => Ok(()),
            Ok(Some(status)) => Err(anyhow::anyhow!("notification command exited with {status}")),
            Ok(None) if Instant::now() < running.deadline => return None,
            Ok(None) => Err(anyhow::anyhow!("notification command timed out")),
            Err(error) => Err(error.into()),
        };
        self.running = None;
        Some(result)
    }
}

#[cfg(target_os = "macos")]
fn native_command(title: &str, body: &str) -> Result<Command> {
    let mut command = Command::new("/usr/bin/osascript");
    command.args([
        "-e",
        "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run",
        "--",
        title,
        body,
    ]);
    Ok(command)
}

#[cfg(target_os = "linux")]
fn native_command(title: &str, body: &str) -> Result<Command> {
    anyhow::ensure!(
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "desktop notification requires a desktop session"
    );
    let mut command = Command::new("notify-send");
    command.args(["--", title, body]);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn externally_reaped_leader_does_not_authorize_group_signals() -> Result<()> {
        struct Root(PathBuf);
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "zor-notifier-wait-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let root = Root(path);
        let end = root.0.join("end");
        let result = root.0.join("result");
        // This bounded descendant survives its leader. Reap the leader outside
        // std::Child so try_wait returns ECHILD, without relying on PID reuse.
        let child = Command::new("/bin/sh").args(["-c",
            "(i=0; while [ ! -e \"$NOTICE_END\" ] && [ $i -lt 200 ]; do i=$((i+1)); sleep 0.01; done; if [ -e \"$NOTICE_END\" ]; then printf alive > \"$NOTICE_RESULT\"; fi) & exit 0"])
            .env("NOTICE_END", &end).env("NOTICE_RESULT", &result)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .process_group(0).spawn()?;
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id())?);
        let mut running = Running {
            child,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        loop {
            match nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))? {
                nix::sys::wait::WaitStatus::Exited(_, 0) => break,
                nix::sys::wait::WaitStatus::StillAlive => {
                    anyhow::ensure!(
                        Instant::now() < running.deadline,
                        "fixture leader did not exit"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                status => anyhow::bail!("unexpected fixture leader status: {status:?}"),
            }
        }
        assert!(running.child.try_wait().is_err());
        drop(running);
        std::fs::write(end, b"finish")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::read(&result).ok().as_deref() != Some(b"alive") {
            anyhow::ensure!(
                Instant::now() < deadline,
                "unknown ownership signalled the surviving group"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
