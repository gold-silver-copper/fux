//! git adapter (WORKTREES.md:62-68): `Effect::RunGit { op }` runs `git` through argv without a
//! shell or PTY, noninteractive, hooks disabled, system/global configuration excluded, each
//! stream capped at 256 KiB and the whole command bounded by a ten-second deadline. The result
//! comes back as `Inbound::GitDone { op }`; `code: None` means the outcome is unknown (spawn
//! failure, deadline, output overflow), which consumers must treat as *uncertain*, never as
//! failure. Op ids come from [`GitOps`]; consumers remember the id they committed under and
//! collect the reply with [`take_reply`], so a lost reply can never be mistaken for another
//! command's.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use async_channel::Sender;
use bevy_app::prelude::*;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_tasks::IoTaskPool;

use crate::model::{Effect, Inbound, Phase};
use crate::runner::Adapter;

/// Per-stream retention bound (WORKTREES.md:64).
pub const MAX_STREAM_BYTES: usize = 256 * 1024;
/// Whole-command budget (WORKTREES.md:65).
pub const DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

/// A finished git command as `Inbound::GitDone` carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDone {
    pub op: u64,
    /// `None`: outcome unknown (never spawned, killed at the deadline, or output overflow).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitDone {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// Op id allocation and the replies not yet collected. Never persisted: an op id is meaningful
/// only within one server incarnation; after a restart every pending op is a lost reply.
#[derive(Resource, Default, Debug)]
pub struct GitOps {
    next: u64,
    replies: HashMap<u64, GitDone>,
}

impl GitOps {
    pub fn allocate(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    pub fn record(&mut self, done: GitDone) {
        self.replies.insert(done.op, done);
    }

    pub fn take(&mut self, op: u64) -> Option<GitDone> {
        self.replies.remove(&op)
    }

    pub fn pending_replies(&self) -> usize {
        self.replies.len()
    }
}

/// Allocates an op id and emits `Effect::RunGit` for it. The caller commits the id with its
/// intent in the same update, so the effect is journaled before the runner sees it.
pub fn request(world: &mut World, argv: Vec<String>, cwd: impl Into<String>) -> u64 {
    let op = world.resource_mut::<GitOps>().allocate();
    world.write_message(Effect::RunGit {
        op,
        argv,
        cwd: cwd.into(),
    });
    op
}

/// The reply to `op`, once; `None` while it is still running or when it was lost.
pub fn take_reply(world: &mut World, op: u64) -> Option<GitDone> {
    world.resource_mut::<GitOps>().take(op)
}

/// `Inbound::GitDone` → the replies table, in `Phase::Ingest`.
fn ingest(mut inbound: MessageReader<Inbound>, mut ops: ResMut<GitOps>) {
    for message in inbound.read() {
        if let Inbound::GitDone {
            op,
            code,
            stdout,
            stderr,
        } = message
        {
            ops.record(GitDone {
                op: *op,
                code: *code,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
            });
        }
    }
}

pub struct GitPlugin;

impl Plugin for GitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GitOps>()
            .add_systems(First, ingest.in_set(Phase::Ingest));
    }
}

// ---------------------------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------------------------

/// Runner-side applier of `Effect::RunGit`: one blocking task per command on `IoTaskPool`.
pub struct GitAdapter {
    inbound: Sender<Inbound>,
    stopping: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
    jobs: Vec<bevy_tasks::Task<()>>,
}

impl GitAdapter {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self {
            inbound,
            stopping: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicUsize::new(0)),
            jobs: Vec::new(),
        }
    }
}

struct PendingGit(Arc<AtomicUsize>);
impl Drop for PendingGit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for GitAdapter {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.pending.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
    }
}
impl Adapter for GitAdapter {
    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    fn pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst) != 0
    }
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::RunGit { .. })
    }

    fn apply(&mut self, effect: Effect) -> Result<Option<Inbound>, BevyError> {
        let Effect::RunGit { op, argv, cwd } = effect else {
            return Err(BevyError::from("git adapter: not a RunGit"));
        };
        let rejection = if self.stopping.load(Ordering::SeqCst) {
            Some("git adapter is shutting down")
        } else if self.pending.load(Ordering::SeqCst) >= 64 {
            Some("git adapter concurrency limit reached")
        } else {
            None
        };
        if let Some(reason) = rejection {
            // This request is already committed. Return its completion directly to the
            // runner: the shared inbound queue may be full, and effects have no batch limit.
            return Ok(Some(Inbound::GitDone {
                op,
                code: None,
                stdout: String::new(),
                stderr: format!("{reason}; git was not spawned"),
            }));
        }
        self.jobs.retain(|job| !job.is_finished());
        self.pending.fetch_add(1, Ordering::SeqCst);
        let pending = PendingGit(Arc::clone(&self.pending));
        let stopping = Arc::clone(&self.stopping);
        let inbound = self.inbound.clone();
        self.jobs.push(IoTaskPool::get().spawn(async move {
            let _pending = pending;
            let done = run_with_cancel(op, &argv, Path::new(&cwd), Some(&stopping));
            let _ = inbound
                .send(Inbound::GitDone {
                    op: done.op,
                    code: done.code,
                    stdout: done.stdout,
                    stderr: done.stderr,
                })
                .await;
        }));
        Ok(None)
    }
}

/// Runs `git <argv>` in `cwd` under the WORKTREES.md:62-66 regime and blocks until it finished
/// or the deadline killed it. Public so tests and the worktree reconciler can call git directly
/// with the same environment.
pub fn run(op: u64, argv: &[String], cwd: &Path) -> GitDone {
    run_with_cancel(op, argv, cwd, None)
}

fn run_with_cancel(op: u64, argv: &[String], cwd: &Path, stopping: Option<&AtomicBool>) -> GitDone {
    let unknown = |stderr: String| GitDone {
        op,
        code: None,
        stdout: String::new(),
        stderr,
    };
    if stopping.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
        return unknown("git cancelled before spawn".into());
    }
    let mut child = match Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(argv)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return unknown(format!("spawn: {e}")),
    };
    let started = Instant::now();
    let read_deadline = started + DEADLINE + Duration::from_secs(2);
    let stdout = child
        .stdout
        .take()
        .map(|pipe| std::thread::spawn(move || read_bounded(pipe, read_deadline)));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| std::thread::spawn(move || read_bounded(pipe, read_deadline)));
    let (exited, owned) = loop {
        match fux::runner::signals::child_exited(child.id()) {
            Ok(true) => break (true, true),
            Ok(false)
                if started.elapsed() < DEADLINE
                    && !stopping.is_some_and(|stop| stop.load(Ordering::SeqCst)) =>
            {
                std::thread::sleep(POLL);
            }
            Ok(false) => break (false, true),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => break (false, error.raw_os_error() != Some(nix::libc::ECHILD)),
        }
    };
    let status = if owned {
        // This thread is the only signal holder/reaper. The unreaped leader pins its PID
        // through group cleanup on normal exit, cancellation, deadline, and wait errors.
        if let Ok(pgid) = i32::try_from(child.id()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        let _ = child.kill();
        let status = child.wait().ok();
        if exited { status } else { None }
    } else {
        // ECHILD has revoked signal authority, even if descendants still hold the pipes.
        None
    };
    let join = |reader: Option<std::thread::JoinHandle<Option<String>>>| {
        reader.and_then(|r| r.join().ok()).flatten()
    };
    let out = join(stdout);
    let err = join(stderr);
    match (status, out, err) {
        (Some(status), Some(stdout), Some(stderr)) => GitDone {
            op,
            code: status.code(),
            stdout,
            stderr,
        },
        (None, _, err) => unknown(format!(
            "git did not finish within {}s{}",
            DEADLINE.as_secs(),
            err.map(|e| format!(": {e}")).unwrap_or_default()
        )),
        (Some(_), _, _) => {
            unknown("git output was incomplete or exceeded the retention bound".into())
        }
    }
}

/// Retains at most `MAX_STREAM_BYTES`, draining overflow until EOF. Nonblocking reads also
/// bound cleanup when signal authority is lost and a descendant still holds a pipe.
fn read_bounded(mut pipe: impl Read + AsFd, deadline: Instant) -> Option<String> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL).ok()?);
    fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut overflow = false;
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
            }
            Err(_) => return None,
            Ok(n) => {
                if overflow {
                    continue;
                }
                if buffer.len() + n > MAX_STREAM_BYTES {
                    overflow = true;
                    continue;
                }
                buffer.extend_from_slice(chunk.get(..n).unwrap_or_default());
            }
        }
    }
    (!overflow).then(|| String::from_utf8_lossy(&buffer).into_owned())
}
