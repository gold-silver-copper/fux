//! Bounded execution used by the real verification gate, not a simulated runner.
use anyhow::{Context, Result, ensure};
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Passed,
    Failed,
    Interrupted,
    TimedOut,
    OutputLimit,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub state: State,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u128,
}
pub struct Cancellation {
    pub flag: Arc<AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
}
impl Cancellation {
    pub fn install() -> Result<Self> {
        let mut value = Self {
            flag: Arc::new(AtomicBool::new(false)),
            registrations: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            value
                .registrations
                .push(signal_hook::flag::register(signal, value.flag.clone())?);
        }
        Ok(value)
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
struct Owner {
    child: Child,
    pid: Pid,
    reaped: bool,
}
impl Owner {
    fn exited(&self) -> Result<bool> {
        // nix does not expose waitid on macOS. POSIX WNOWAIT lets the gate
        // observe termination without releasing the process-group leader identity.
        // SAFETY: siginfo_t is a C output record valid when zero initialized;
        // waitid receives its writable address and the retained owned child PID.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid.as_raw() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(error.into())
            };
        }
        // SAFETY: successful waitid initialized the SIGCHLD record, or left the
        // zero PID when no child has exited under WNOHANG.
        Ok(unsafe { info.si_pid() } != 0)
    }
    fn signal(&self, signal: Signal) -> Result<()> {
        match killpg(self.pid, signal) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            // macOS returns EPERM for a group containing only zombies. Verify
            // that case rather than interpreting a permission error as cleanup.
            Err(nix::errno::Errno::EPERM) if self.exited()? && !self.has_live_group_member()? => {
                Ok(())
            }
            Err(e) => Err(anyhow::Error::new(e)
                .context(format!("signal owned group {} with {signal:?}", self.pid))),
        }
    }
    fn has_live_group_member(&self) -> Result<bool> {
        let mut command = Command::new("/bin/ps");
        command
            .env_clear()
            .env("LC_ALL", "C")
            .args(["-axo", "pgid=,stat="]);
        let output =
            fux_xtask::support::process::output(command, Duration::from_secs(3), 4 * 1024 * 1024)?;
        ensure!(output.status.success(), "cannot verify owned group cleanup");
        for line in std::str::from_utf8(&output.stdout)?.lines() {
            let mut fields = line.split_whitespace();
            let group = fields
                .next()
                .context("process group field")?
                .parse::<i32>()?;
            let state = fields.next().context("process state field")?;
            if group == self.pid.as_raw() && !state.starts_with('Z') {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn finish(&mut self, graceful: bool) -> Result<std::process::ExitStatus> {
        // WNOWAIT keeps the leader PID reserved until the owned group has been
        // signalled. Never address an old process group after reaping its leader.
        if graceful {
            self.signal(Signal::SIGTERM)?;
            let end = Instant::now() + Duration::from_secs(2);
            while !self.exited()? && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.signal(Signal::SIGKILL)?;
        let status = fux_xtask::support::process::wait(&mut self.child, Duration::from_secs(3))?;
        self.reaped = true;
        Ok(status)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish(true);
        }
    }
}
struct Capture<R> {
    reader: R,
    file: File,
    bytes: u64,
    eof: bool,
}
impl<R: Read + AsFd> Capture<R> {
    fn new(reader: R, path: &Path) -> Result<Self> {
        let flags = OFlag::from_bits_truncate(fcntl(&reader, FcntlArg::F_GETFL)?);
        fcntl(&reader, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            reader,
            file,
            bytes: 0,
            eof: false,
        })
    }
    fn drain(&mut self, limit: u64) -> Result<bool> {
        // Finite work per stream keeps deadlines and stderr responsive during floods.
        for _ in 0..8 {
            let mut buffer = [0; 8192];
            let n = match self.reader.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(false);
                }
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            let allowed = (limit - self.bytes).min(n as u64) as usize;
            self.file.write_all(&buffer[..allowed])?;
            self.bytes += allowed as u64;
            if allowed < n {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub fn run(
    mut command: Command,
    logs: &Path,
    timeout: Duration,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<Outcome> {
    ensure!(
        !timeout.is_zero() && limit > 0,
        "invalid gate execution bounds"
    );
    fs::create_dir_all(logs)?;
    ensure!(
        !cancel.load(Ordering::SeqCst),
        "verification interrupted before child spawn"
    );
    let start = Instant::now();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("spawn verification check")?;
    let pid = Pid::from_raw(i32::try_from(child.id())?);
    let mut owner = Owner {
        child,
        pid,
        reaped: false,
    };
    let mut stdout = Capture::new(
        owner.child.stdout.take().context("check stdout")?,
        &logs.join("stdout.log"),
    )?;
    let mut stderr = Capture::new(
        owner.child.stderr.take().context("check stderr")?,
        &logs.join("stderr.log"),
    )?;
    let reason = loop {
        if cancel.load(Ordering::SeqCst) {
            break Some(State::Interrupted);
        }
        if start.elapsed() >= timeout {
            break Some(State::TimedOut);
        }
        if stdout.drain(limit)? | stderr.drain(limit)? {
            break Some(State::OutputLimit);
        }
        if owner.exited()? {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = owner.finish(reason.is_some())?;
    let mut state = reason.unwrap_or(if status.success() {
        State::Passed
    } else {
        State::Failed
    });
    let end = Instant::now() + Duration::from_secs(2);
    while !stdout.eof || !stderr.eof {
        if stdout.drain(limit)? | stderr.drain(limit)? {
            state = State::OutputLimit;
            break;
        }
        ensure!(
            Instant::now() < end,
            "check output pipes remain open after owned group cleanup"
        );
        if !stdout.eof || !stderr.eof {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    stdout.file.sync_all()?;
    stderr.file.sync_all()?;
    Ok(Outcome {
        state,
        exit_code: status.code(),
        elapsed_ms: start.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shell(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]);
        c
    }
    #[test]
    fn actual_check_gets_eof_instead_of_callers_input_and_failure_is_retained() -> Result<()> {
        let root = tempfile::tempdir()?;
        let input = root.path().join("input");
        fs::write(&input, b"must not become child input\n")?;
        let mut c = shell(
            "if read line; then echo contaminated; exit 7; fi; echo isolated; echo diagnostic >&2; exit 23",
        );
        c.stdin(File::open(input)?);
        let result = run(
            c,
            &root.path().join("logs"),
            Duration::from_secs(3),
            1024,
            &AtomicBool::new(false),
        )?;
        assert_eq!(result.state, State::Failed);
        assert_eq!(result.exit_code, Some(23));
        assert_eq!(
            fs::read(root.path().join("logs/stdout.log"))?,
            b"isolated\n"
        );
        assert_eq!(
            fs::read(root.path().join("logs/stderr.log"))?,
            b"diagnostic\n"
        );
        Ok(())
    }
    #[test]
    fn success_cleans_leftover_group_while_leader_identity_is_reserved() -> Result<()> {
        let root = tempfile::tempdir()?;
        let result = run(
            shell("sleep 30 & echo complete"),
            root.path(),
            Duration::from_secs(3),
            1024,
            &AtomicBool::new(false),
        )?;
        assert_eq!(result.state, State::Passed);
        assert!(result.elapsed_ms < 2000);
        Ok(())
    }
    #[test]
    fn timeout_and_interrupt_stop_the_actual_command_and_descendants() -> Result<()> {
        for interrupted in [false, true] {
            let root = tempfile::tempdir()?;
            let flag = Arc::new(AtomicBool::new(false));
            let shared = flag.clone();
            let thread = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                if interrupted {
                    shared.store(true, Ordering::SeqCst);
                }
            });
            let result = run(
                shell("sleep 30 & wait"),
                root.path(),
                if interrupted {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(100)
                },
                1024,
                &flag,
            );
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("cancellation thread"))?;
            let result = result?;
            assert_eq!(
                result.state,
                if interrupted {
                    State::Interrupted
                } else {
                    State::TimedOut
                }
            );
            assert!(result.elapsed_ms < 3500);
        }
        Ok(())
    }
    #[test]
    fn output_limit_is_a_failure_with_bounded_diagnostics() -> Result<()> {
        let root = tempfile::tempdir()?;
        let result = run(
            shell("while :; do printf 1234567890; done"),
            root.path(),
            Duration::from_secs(3),
            1000,
            &AtomicBool::new(false),
        )?;
        assert_eq!(result.state, State::OutputLimit);
        assert_eq!(fs::metadata(root.path().join("stdout.log"))?.len(), 1000);
        Ok(())
    }
}
