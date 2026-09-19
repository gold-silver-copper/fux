//! Runner-side provider processes. Each attempt owns a process group and a bounded stdin
//! queue; closing stdin starts a deadline independently of blocked pipe reads or writes.

use core::time::Duration;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_channel::{Receiver, Sender};
use async_io::{Async, Timer};
use bevy_ecs::entity::Entity;
use bevy_ecs::error::BevyError;
use bevy_platform::collections::HashMap;
use bevy_tasks::futures_lite::future;
use bevy_tasks::{IoTaskPool, Task};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::model::{Effect, Inbound};
use crate::runner::Adapter;

const CHUNK: usize = 8 * 1024;
const GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const STDIN_QUEUE: usize = 32;
const MAX_WRITE: usize = 1024 * 1024;

#[derive(Default)]
struct Process {
    child: Option<Child>,
    cancelled: bool,
}

impl Process {
    fn kill_and_reap(&mut self) -> i32 {
        let Some(mut child) = self.child.take() else {
            return -1;
        };
        // Only the PID returned by our own spawn is ever used as a process-group ID.
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
        let _ = child.kill();
        child.wait().map_or(-1, exit_code)
    }
}

struct Sidecar {
    stdin: Sender<Vec<u8>>,
    process: Arc<Mutex<Process>>,
    closing: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    // Retain supervision, rather than leaving detached tasks after adapter teardown.
    _task: Task<()>,
}

impl Sidecar {
    fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        self.stdin.close();
    }
}

pub struct ProviderAdapter {
    inbound: Sender<Inbound>,
    sidecars: HashMap<Entity, Sidecar>,
    executable: PathBuf,
    shutting_down: bool,
}

impl ProviderAdapter {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self::with_executable(inbound, std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zor")))
    }

    pub fn with_executable(inbound: Sender<Inbound>, executable: PathBuf) -> Self {
        Self {
            inbound,
            sidecars: HashMap::default(),
            executable,
            shutting_down: false,
        }
    }
}

impl Drop for ProviderAdapter {
    fn drop(&mut self) {
        for sidecar in self.sidecars.values() {
            sidecar.close();
            // Spawn and cancellation use the same lock: queued work cannot spawn after Drop.
            let mut process = sidecar.process.lock().unwrap_or_else(|e| e.into_inner());
            process.cancelled = true;
            process.kill_and_reap();
        }
    }
}

impl Adapter for ProviderAdapter {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::SpawnProvider { .. } | Effect::WriteProvider { .. })
    }

    fn pending(&self) -> bool {
        self.sidecars.values().any(|sidecar| !sidecar.finished.load(Ordering::SeqCst))
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
        for sidecar in self.sidecars.values() {
            sidecar.close();
        }
    }

    fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        match effect {
            Effect::SpawnProvider { attempt, argv, cwd, env } => {
                if self.shutting_down || self.sidecars.contains_key(&attempt) {
                    return Err(BevyError::from(format!(
                        "provider adapter: {attempt} already has a sidecar or adapter is shutting down"
                    )));
                }
                let (stdin, rx) = async_channel::bounded(STDIN_QUEUE);
                let process = Arc::new(Mutex::new(Process::default()));
                let closing = Arc::new(AtomicBool::new(false));
                let finished = Arc::new(AtomicBool::new(false));
                let task = IoTaskPool::get().spawn(run(
                    attempt, argv, cwd, env, rx, self.inbound.clone(),
                    self.executable.clone(), process.clone(), closing.clone(), finished.clone(),
                ));
                self.sidecars.insert(attempt, Sidecar { stdin, process, closing, finished, _task: task });
                Ok(())
            }
            Effect::WriteProvider { attempt, bytes } => {
                let Some(sidecar) = self.sidecars.get(&attempt) else {
                    return Err(BevyError::from(format!("provider adapter: {attempt} has no sidecar")));
                };
                if bytes.is_empty() {
                    sidecar.close();
                    return Ok(());
                }
                if bytes.len() > MAX_WRITE || sidecar.closing.load(Ordering::SeqCst) {
                    return Err(BevyError::from("provider adapter: stdin closed or frame exceeds 1 MiB"));
                }
                sidecar.stdin.try_send(bytes)
                    .map_err(|e| BevyError::from(format!("provider adapter: stdin rejected delivery: {e}")))
            }
            other => Err(BevyError::from(format!("provider adapter: unhandled {other:?}"))),
        }
    }
}

#[allow(clippy::too_many_arguments, reason = "one supervision task owns one provider attempt")]
async fn run(
    attempt: Entity,
    argv: Vec<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    rx: Receiver<Vec<u8>>,
    inbound: Sender<Inbound>,
    executable: PathBuf,
    process: Arc<Mutex<Process>>,
    closing: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
) {
    let spawn = || -> std::io::Result<_> {
        let mut owned = process.lock().unwrap_or_else(|e| e.into_inner());
        if owned.cancelled || closing.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("provider cancelled before spawn"));
        }
        if argv.is_empty() {
            return Err(std::io::Error::other("empty provider command"));
        }
        // Reuse the plugin guardian: unlike an in-process task it survives host SIGKILL
        // long enough to kill this group. Provider stdio is inherited by its child.
        let mut command = Command::new(executable);
        command.args(["plugin", "supervise"])
            .envs(env)
            .env("ZOR_PLUGIN_ARGV", serde_json::to_string(&argv)?)
            .env("ZOR_HOST_PID", std::process::id().to_string())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .process_group(0);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        let pipes = (child.id(), child.stdin.take(), child.stdout.take());
        owned.child = Some(child);
        Ok(pipes)
    };
    let code = match spawn() {
        Err(error) => {
            bevy_log::warn!("provider {attempt}: spawn failed: {error}");
            127
        }
        Ok((pid, stdin, stdout)) => {
            let (exited, exit_rx) = async_channel::bounded::<()>(1);
            let writer_exit = exit_rx.clone();
            let writes = async {
                future::race(async {
                    if let Some(stdin) = stdin {
                        write_loop(stdin, rx).await;
                    }
                    closing.store(true, Ordering::SeqCst);
                }, async { let _ = writer_exit.recv().await; }).await;
            };
            let reads = async {
                // Started always precedes output, and output is drained before Exited.
                let _ = inbound.send(Inbound::ProviderStarted { attempt, pid }).await;
                future::race(async {
                    if let Some(stdout) = stdout {
                        read_loop(attempt, stdout, &inbound).await;
                    }
                }, async {
                    let _ = exit_rx.recv().await;
                    Timer::after(GRACE).await;
                }).await;
            };
            let wait = async {
                let code = wait(&process, &closing).await;
                exited.close();
                code
            };
            let (_, code) = future::zip(future::zip(writes, reads), wait).await;
            code
        }
    };
    let _ = inbound.send(Inbound::ProviderExited { attempt, code }).await;
    finished.store(true, Ordering::SeqCst);
}

/// Pipes as `File`s: `&File` implements `Read`/`Write` for safe async-io operations.
async fn write_loop(stdin: std::process::ChildStdin, rx: Receiver<Vec<u8>>) {
    let Ok(stdin) = Async::new(File::from(OwnedFd::from(stdin))) else { return; };
    while let Ok(bytes) = rx.recv().await {
        let mut offset = 0;
        while offset < bytes.len() {
            match stdin.write_with(|mut s| s.write(bytes.get(offset..).unwrap_or_default())).await {
                Ok(0) | Err(_) => return,
                Ok(n) => offset += n,
            }
        }
    }
}

async fn read_loop(attempt: Entity, stdout: std::process::ChildStdout, inbound: &Sender<Inbound>) {
    let Ok(stdout) = Async::new(File::from(OwnedFd::from(stdout))) else { return; };
    let mut buf = vec![0u8; CHUNK];
    loop {
        match stdout.read_with(|mut s| s.read(&mut buf)).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or_default().to_vec();
                if inbound.send(Inbound::ProviderOutput { attempt, bytes }).await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn wait(process: &Mutex<Process>, closing: &AtomicBool) -> i32 {
    let mut closed_at: Option<Instant> = None;
    loop {
        {
            let mut owned = process.lock().unwrap_or_else(|e| e.into_inner());
            let Some(child) = owned.child.as_mut() else { return -1; };
            match child.try_wait() {
                Ok(Some(status)) => {
                    // The leader has exited, but descendants may still own stdout.
                    owned.kill_and_reap();
                    return exit_code(status);
                }
                Err(_) => return owned.kill_and_reap(),
                Ok(None) => {}
            }
            if closing.load(Ordering::SeqCst)
                && closed_at.get_or_insert_with(Instant::now).elapsed() >= GRACE
            {
                return owned.kill_and_reap();
            }
        }
        Timer::after(POLL).await;
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(-1)
}
