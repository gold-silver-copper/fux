//! Runner-side applier of `Effect::RunCheck`/`KillCheck` (CHECKS.md:46-52): the command runs
//! through argv without a shell or PTY in its own process group with null stdin; both streams
//! are captured nonblocking on `IoTaskPool` up to 256 KiB each; the timeout, an output overflow
//! or a spawn failure kills the group and reports an uncertain outcome; the retained tail of
//! each stream is at most 4096 UTF-8 bytes. The reply is `Inbound::CheckDone`.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_channel::Sender;
use async_io::{Async, Timer};
use bevy_ecs::entity::Entity;
use bevy_ecs::error::BevyError;
use bevy_tasks::IoTaskPool;
use bevy_tasks::futures_lite::{AsyncRead, AsyncReadExt, future};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::model::{Effect, Inbound, MAX_FINAL_OUTPUT_BYTES};
use crate::runner::Adapter;

/// Captured bytes per stream before the run is abandoned (CHECKS.md:47).
pub const MAX_STREAM_BYTES: usize = 256 * 1024;
const POLL: Duration = Duration::from_millis(20);

/// A live check: its process group and whether `KillCheck` asked for it.
struct Live {
    pgid: Pid,
    cancelled: Arc<AtomicBool>,
}

/// Live checks by entity, shared between the adapter and its tasks.
#[derive(Default, Clone)]
pub struct Registry(Arc<Mutex<HashMap<Entity, Live>>>);

impl Registry {
    fn insert(&self, check: Entity, live: Live) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(check, live);
        }
    }

    fn remove(&self, check: Entity) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&check);
        }
    }

    fn kill(&self, check: Entity) -> bool {
        let Ok(map) = self.0.lock() else {
            return false;
        };
        let Some(live) = map.get(&check) else {
            return false;
        };
        live.cancelled.store(true, Ordering::SeqCst);
        let _ = killpg(live.pgid, Signal::SIGKILL);
        true
    }
}

pub struct CheckRunner {
    inbound: Sender<Inbound>,
    live: Registry,
}

impl CheckRunner {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self {
            inbound,
            live: Registry::default(),
        }
    }
}

impl Adapter for CheckRunner {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::RunCheck { .. } | Effect::KillCheck { .. })
    }

    fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        match effect {
            Effect::RunCheck {
                check,
                argv,
                cwd,
                timeout_ms,
            } => {
                let inbound = self.inbound.clone();
                let live = self.live.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        let outcome = execute(check, &argv, &cwd, timeout_ms, &live).await;
                        let _ = inbound.send(outcome.into_inbound(check)).await;
                    })
                    .detach();
                Ok(())
            }
            Effect::KillCheck { check } => {
                // A check that is not live any more already answered; nothing to kill.
                self.live.kill(check);
                Ok(())
            }
            other => Err(BevyError::from(format!(
                "check runner: not a check effect: {other:?}"
            ))),
        }
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

enum Read {
    Eof,
    Overflow,
    Failed(String),
}

/// Drains one pipe into `buffer` until EOF or the stream bound.
async fn drain<R: AsyncRead + Unpin>(mut pipe: R, capture: &Mutex<Capture>, stderr: bool) -> Read {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => return Read::Eof,
            Ok(n) => {
                let Ok(mut capture) = capture.lock() else {
                    return Read::Failed("capture buffer poisoned".into());
                };
                let target = if stderr {
                    &mut capture.stderr
                } else {
                    &mut capture.stdout
                };
                if target.len() + n > MAX_STREAM_BYTES {
                    return Read::Overflow;
                }
                target.extend_from_slice(chunk.get(..n).unwrap_or_default());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Read::Failed(e.to_string()),
        }
    }
}

async fn wait_exit(child: &mut Child) -> Result<ExitStatus, String> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                Timer::after(POLL).await;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn kill_group(child: &mut Child, pgid: Pid) {
    let _ = killpg(pgid, Signal::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs the command to completion, timeout or failure. Public so tests can exercise the runner
/// without an app.
pub async fn execute(
    check: Entity,
    argv: &[String],
    cwd: &str,
    timeout_ms: u64,
    live: &Registry,
) -> Outcome {
    let uncertain = |problem: String| Outcome {
        problem: Some(problem),
        ..Default::default()
    };
    let Some((program, args)) = argv.split_first() else {
        return uncertain("empty argv".into());
    };
    let mut child = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return uncertain(format!("spawn {program}: {e}")),
    };
    let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    live.insert(
        check,
        Live {
            pgid,
            cancelled: Arc::clone(&cancelled),
        },
    );
    let outcome = run(&mut child, pgid, timeout_ms).await;
    live.remove(check);
    if cancelled.load(Ordering::SeqCst) {
        return Outcome {
            problem: Some("cancelled by zor/check.cancel".into()),
            ..outcome
        };
    }
    outcome
}

async fn run(child: &mut Child, pgid: Pid, timeout_ms: u64) -> Outcome {
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_group(child, pgid);
        return Outcome {
            problem: Some("subprocess pipes missing".into()),
            ..Default::default()
        };
    };
    let (stdout, stderr) = match (Async::new(stdout), Async::new(stderr)) {
        (Ok(o), Ok(e)) => (o, e),
        (Err(e), _) | (_, Err(e)) => {
            kill_group(child, pgid);
            return Outcome {
                problem: Some(format!("nonblocking pipes: {e}")),
                ..Default::default()
            };
        }
    };
    let capture = Mutex::new(Capture::default());
    let timeout = Timer::after(Duration::from_millis(timeout_ms));
    let reads = async {
        let (out, err) = future::zip(
            drain(stdout, &capture, false),
            drain(stderr, &capture, true),
        )
        .await;
        match (out, err) {
            (Read::Eof, Read::Eof) => None,
            (Read::Overflow, _) | (_, Read::Overflow) => {
                Some("subprocess output exceeded 256 KiB".to_owned())
            }
            (Read::Failed(e), _) | (_, Read::Failed(e)) => Some(format!("read output: {e}")),
        }
    };
    let timed_out = async {
        timeout.await;
        Some(format!(
            "timed out after {timeout_ms} ms; outcome uncertain"
        ))
    };
    // Both pipes closed and the leader reaped, or one of the failures above.
    let problem = future::or(
        async {
            match reads.await {
                Some(problem) => Some(problem),
                None => wait_exit(child).await.err().map(|e| format!("wait: {e}")),
            }
        },
        timed_out,
    )
    .await;
    let (stdout, stderr) = {
        let capture = capture.lock().map(|c| (c.stdout.clone(), c.stderr.clone()));
        capture.unwrap_or_default()
    };
    let (stdout, out_truncated) = tail(&stdout);
    let (stderr, err_truncated) = tail(&stderr);
    if let Some(problem) = problem {
        kill_group(child, pgid);
        return Outcome {
            code: None,
            stdout,
            stderr,
            truncated: out_truncated || err_truncated,
            problem: Some(problem),
        };
    }
    // `wait_exit` reaped the leader: the status is cached by `Child`.
    let Ok(Some(status)) = child.try_wait() else {
        kill_group(child, pgid);
        return Outcome {
            stdout,
            stderr,
            truncated: out_truncated || err_truncated,
            problem: Some("exit status unavailable".into()),
            ..Default::default()
        };
    };
    // A signal exit is an observed nonzero outcome (CHECKS.md:28): `code` stays `None`.
    Outcome {
        code: status.code(),
        stdout,
        stderr,
        truncated: out_truncated || err_truncated,
        problem: None,
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
