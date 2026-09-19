//! Bounded check subprocesses: cancellation is reserved before scheduling, every run owns
//! its process group, and completion remains pending until `CheckDone` is queued.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_channel::Sender;
use async_io::{Async, Timer};
use bevy_ecs::entity::Entity;
use bevy_ecs::error::BevyError;
use bevy_tasks::futures_lite::{AsyncRead, AsyncReadExt, future};
use bevy_tasks::{IoTaskPool, Task};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::model::{Effect, Inbound, MAX_FINAL_OUTPUT_BYTES};
use crate::runner::Adapter;

/// Captured bytes per stream before the run is abandoned (CHECKS.md:47).
pub const MAX_STREAM_BYTES: usize = 256 * 1024;
const POLL: Duration = Duration::from_millis(20);

#[derive(Default)]
struct Process {
    child: Option<Child>,
    cancelled: bool,
}

impl Process {
    fn kill_and_reap(&mut self) -> std::io::Result<ExitStatus> {
        let Some(mut child) = self.child.take() else {
            return Err(std::io::Error::other("exit status unavailable"));
        };
        if let Err(error) = fux::runner::signals::child_exited(child.id())
            && error.raw_os_error() == Some(nix::libc::ECHILD)
        {
            return Err(error);
        }
        // Taking the child under the process lock retires every other signal holder.
        // The unreaped leader reserves its PID until group cleanup completes.
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
        let _ = child.kill();
        child.wait()
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        let _ = self.kill_and_reap();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.kill_and_reap();
    }
}

/// Reserved checks, including those not yet spawned or awaiting completion delivery.
#[derive(Default, Clone)]
pub struct Registry(Arc<Mutex<HashMap<Entity, Arc<Mutex<Process>>>>>);

impl Registry {
    fn reserve(&self, check: Entity) -> Option<Arc<Mutex<Process>>> {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(&check) || map.len() >= crate::model::limits::MAX_CHECKS {
            return None;
        }
        let process = Arc::new(Mutex::new(Process::default()));
        map.insert(check, Arc::clone(&process));
        Some(process)
    }

    fn remove(&self, check: Entity) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&check);
    }

    fn kill(&self, check: Entity) {
        if let Some(process) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&check) {
            process.lock().unwrap_or_else(|e| e.into_inner()).cancel();
        }
    }

    fn cancel_all(&self) {
        for process in self.0.lock().unwrap_or_else(|e| e.into_inner()).values() {
            process.lock().unwrap_or_else(|e| e.into_inner()).cancel();
        }
    }
}

pub struct CheckRunner {
    inbound: Sender<Inbound>,
    live: Registry,
    tasks: HashMap<Entity, Task<()>>,
    executable: PathBuf,
    shutting_down: bool,
}

impl CheckRunner {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self::with_executable(
            inbound,
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zor")),
        )
    }

    pub fn with_executable(inbound: Sender<Inbound>, executable: PathBuf) -> Self {
        Self {
            inbound,
            live: Registry::default(),
            tasks: HashMap::new(),
            executable,
            shutting_down: false,
        }
    }
}

impl Drop for CheckRunner {
    fn drop(&mut self) {
        // Spawn holds this same process lock. No queued task can spawn after teardown,
        // and dropping retained tasks also cancels a blocked completion-channel send.
        self.live.cancel_all();
    }
}

impl Adapter for CheckRunner {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::RunCheck { .. } | Effect::KillCheck { .. })
    }

    fn apply(&mut self, effect: Effect) -> Result<Option<Inbound>, BevyError> {
        match effect {
            Effect::RunCheck {
                check,
                argv,
                cwd,
                timeout_ms,
            } => {
                if self.shutting_down {
                    return Err(BevyError::from("check runner is shutting down"));
                }
                self.tasks.retain(|_, task| !task.is_finished());
                let process = self
                    .live
                    .reserve(check)
                    .ok_or_else(|| BevyError::from("check capacity exhausted or duplicate run"))?;
                let inbound = self.inbound.clone();
                let live = self.live.clone();
                let executable = self.executable.clone();
                let task = IoTaskPool::get().spawn(async move {
                    let outcome =
                        execute_reserved(&argv, &cwd, timeout_ms, &process, Some(&executable))
                            .await;
                    let _ = inbound.send(outcome.into_inbound(check)).await;
                    live.remove(check);
                });
                self.tasks.insert(check, task);
                Ok(None)
            }
            Effect::KillCheck { check } => {
                self.live.kill(check);
                Ok(None)
            }
            other => Err(BevyError::from(format!(
                "check runner: not a check effect: {other:?}"
            ))),
        }
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
        self.live.cancel_all();
    }

    fn pending(&self) -> bool {
        !self
            .live
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

/// What one run produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub problem: Option<String>,
}

impl Outcome {
    fn into_inbound(self, check: Entity) -> Inbound {
        Inbound::CheckDone {
            check,
            code: self.code,
            stdout: self.stdout,
            stderr: self.stderr,
            truncated: self.truncated,
            problem: self.problem,
        }
    }
}

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Drains one pipe into `buffer` until EOF or the stream bound.
async fn drain<R: AsyncRead + Unpin>(
    mut pipe: R,
    capture: &Mutex<Capture>,
    stderr: bool,
) -> Result<(), String> {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let mut capture = capture.lock().map_err(|_| "capture buffer poisoned")?;
                let target = if stderr {
                    &mut capture.stderr
                } else {
                    &mut capture.stdout
                };
                if target.len() + n > MAX_STREAM_BYTES {
                    return Err("subprocess output exceeded 256 KiB".into());
                }
                target.extend_from_slice(chunk.get(..n).unwrap_or_default());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read output: {e}")),
        }
    }
}

async fn wait_exit(process: &Mutex<Process>) -> Result<ExitStatus, String> {
    loop {
        {
            let mut process = process.lock().unwrap_or_else(|e| e.into_inner());
            if process.cancelled {
                return Err("cancelled by zor/check.cancel; outcome uncertain".into());
            }
            let child = process.child.as_ref().ok_or("exit status unavailable")?;
            match fux::runner::signals::child_exited(child.id()) {
                Ok(true) => {
                    // Kill pipe-holding descendants while the unreaped leader pins its PID,
                    // then collect the original native status and drain the buffered tail.
                    return process.kill_and_reap().map_err(|e| format!("wait: {e}"));
                }
                Ok(false) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if e.raw_os_error() == Some(nix::libc::ECHILD) {
                        process.child.take();
                    }
                    return Err(format!("wait: {e}"));
                }
            }
        }
        Timer::after(POLL).await;
    }
}

/// Runs a direct command without an app or a guardian executable. The returned future owns
/// cleanup too: dropping it terminates and reaps its process group.
pub async fn execute(
    check: Entity,
    argv: &[String],
    cwd: &str,
    timeout_ms: u64,
    live: &Registry,
) -> Outcome {
    let Some(process) = live.reserve(check) else {
        return Outcome {
            problem: Some("check capacity exhausted or duplicate run".into()),
            ..Default::default()
        };
    };
    struct Reservation<'a> {
        check: Entity,
        live: &'a Registry,
        process: Arc<Mutex<Process>>,
    }
    impl Drop for Reservation<'_> {
        fn drop(&mut self) {
            let _ = self
                .process
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .kill_and_reap();
            self.live.remove(self.check);
        }
    }
    let reservation = Reservation {
        check,
        live,
        process,
    };
    execute_reserved(argv, cwd, timeout_ms, &reservation.process, None).await
}

async fn execute_reserved(
    argv: &[String],
    cwd: &str,
    timeout_ms: u64,
    process: &Mutex<Process>,
    executable: Option<&Path>,
) -> Outcome {
    let spawn = || -> Result<_, String> {
        let mut owned = process.lock().unwrap_or_else(|e| e.into_inner());
        if owned.cancelled {
            return Err("cancelled before spawn; outcome uncertain".into());
        }
        let (program, args) = argv.split_first().ok_or("empty argv")?;
        let mut status_pipe = None;
        let mut command = if let Some(executable) = executable {
            // The existing guardian survives host SIGKILL and kills the owned group.
            let mut command = Command::new(executable);
            command
                .args(["plugin", "supervise"])
                .env(
                    "ZOR_PLUGIN_ARGV",
                    serde_json::to_string(argv).map_err(|e| e.to_string())?,
                )
                .env("ZOR_HOST_PID", std::process::id().to_string())
                .env("ZOR_CHILD_STATUS", "1");
            let (reader, writer) =
                std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
            status_pipe =
                Some(Async::new(File::from(OwnedFd::from(reader))).map_err(|e| e.to_string())?);
            command.stdin(Stdio::from(OwnedFd::from(writer)));
            command
        } else {
            let mut command = Command::new(program);
            command.args(args).stdin(Stdio::null());
            command
        };
        let mut child = command
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("spawn {program}: {e}"))?;
        let pipes = (child.stdout.take(), child.stderr.take(), status_pipe);
        owned.child = Some(child);
        Ok(pipes)
    };
    let result = match spawn() {
        Err(problem) => Outcome {
            problem: Some(problem),
            ..Default::default()
        },
        Ok((Some(stdout), Some(stderr), status_pipe)) => {
            match (
                Async::new(File::from(OwnedFd::from(stdout))),
                Async::new(File::from(OwnedFd::from(stderr))),
            ) {
                (Ok(stdout), Ok(stderr)) => {
                    run(process, stdout, stderr, status_pipe, timeout_ms).await
                }
                (Err(e), _) | (_, Err(e)) => Outcome {
                    problem: Some(format!("nonblocking pipes: {e}")),
                    ..Default::default()
                },
            }
        }
        _ => Outcome {
            problem: Some("subprocess pipes missing".into()),
            ..Default::default()
        },
    };
    let mut process = process.lock().unwrap_or_else(|e| e.into_inner());
    let _ = process.kill_and_reap();
    if process.cancelled {
        Outcome {
            code: None,
            problem: Some("cancelled by zor/check.cancel; outcome uncertain".into()),
            ..result
        }
    } else {
        result
    }
}

async fn run(
    process: &Mutex<Process>,
    stdout: Async<File>,
    stderr: Async<File>,
    status_pipe: Option<Async<File>>,
    timeout_ms: u64,
) -> Outcome {
    let capture = Mutex::new(Capture::default());
    let result = future::or(
        async {
            let reads = future::try_zip(
                drain(stdout, &capture, false),
                drain(stderr, &capture, true),
            );
            let (_, status) = future::try_zip(reads, wait_exit(process)).await?;
            if let Some(pipe) = status_pipe {
                let mut bytes = Vec::new();
                pipe.take(8193)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|e| format!("guardian status: {e}"))?;
                if bytes.len() > 8192 {
                    return Err("guardian status exceeded 8192 bytes; outcome uncertain".into());
                }
                serde_json::from_slice::<(Option<i32>, Option<String>)>(&bytes)
                    .map_err(|e| format!("guardian status unavailable: {e}; outcome uncertain"))
            } else {
                Ok((status.code(), None))
            }
        },
        async {
            Timer::after(Duration::from_millis(timeout_ms)).await;
            Err(format!(
                "timed out after {timeout_ms} ms; outcome uncertain"
            ))
        },
    )
    .await;
    let capture = capture.into_inner().unwrap_or_else(|e| e.into_inner());
    let (stdout, out_truncated) = tail(&capture.stdout);
    let (stderr, err_truncated) = tail(&capture.stderr);
    let (code, problem) = match result {
        Ok(outcome) => outcome,
        Err(problem) => (None, Some(problem)),
    };
    Outcome {
        code,
        stdout,
        stderr,
        truncated: out_truncated || err_truncated,
        problem,
    }
}

/// The last `MAX_FINAL_OUTPUT_BYTES` of a stream as UTF-8 (lossy), on a character boundary.
pub fn tail(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= MAX_FINAL_OUTPUT_BYTES {
        return (text.into_owned(), false);
    }
    let mut start = text.len() - MAX_FINAL_OUTPUT_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text.get(start..).unwrap_or_default().to_owned(), true)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_cancellation_prevents_process_creation() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let check = bevy_ecs::world::World::new().spawn_empty().id();
        let live = Registry::default();
        let process = live.reserve(check).expect("reservation");
        // Exercise the exact gap between apply's reservation and its task's first poll.
        live.kill(check);
        let outcome = future::block_on(execute_reserved(
            &[
                "/bin/sh".into(),
                "-c".into(),
                "touch spawned; exec sleep 60".into(),
            ],
            dir.path().to_str().expect("fixture path"),
            60_000,
            &process,
            None,
        ));
        assert_eq!(outcome.code, None);
        assert!(outcome.problem.is_some());
        assert!(!dir.path().join("spawned").exists());
        live.remove(check);
    }

    #[test]
    fn released_leader_does_not_authorize_signalling_surviving_group_members() {
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut process = Process {
            child: Some(
                Command::new("/bin/sh")
                    .args(["-c", "read line; exit 0"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .expect("leader"),
            ),
            cancelled: false,
        };
        let leader = process.child.as_mut().expect("owned leader");
        let pgid = i32::try_from(leader.id()).expect("process group");
        let mut survivor = OwnedChild(
            Command::new("/bin/sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(pgid)
                .spawn()
                .expect("group member"),
        );
        // Deliberately release the leader first. A surviving member keeps the old group
        // signalable, but the adapter no longer owns the leader's numeric identity.
        drop(leader.stdin.take());
        assert!(leader.wait().expect("leader exit").success());
        assert_eq!(
            process
                .kill_and_reap()
                .expect_err("lost child")
                .raw_os_error(),
            Some(nix::libc::ECHILD)
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(survivor.0.try_wait().expect("survivor status").is_none());
    }
}
