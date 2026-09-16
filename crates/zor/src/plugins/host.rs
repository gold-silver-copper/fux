//! Runner-side applier of `Effect::RunPlugin`/`KillPlugin`: a plugin process is an ordinary
//! subprocess in its own process group with null stdin; stdout and stderr are appended line by
//! line to the plugin's log file (bounded: the file rotates once to `.1` past
//! [`MAX_LOG_BYTES`]) on `IoTaskPool`; the exit is reported as `Inbound::PluginExited`.
//! `KillPlugin` sends `SIGTERM` to the group, then `SIGKILL` after [`GRACE`].

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_channel::Sender;
use async_io::{Async, Timer};
use bevy_ecs::entity::Entity;
use bevy_ecs::error::BevyError;
use bevy_tasks::IoTaskPool;
use bevy_tasks::futures_lite::{AsyncRead, AsyncReadExt, future};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::model::{Effect, Inbound};
use crate::runner::Adapter;

/// Bytes of the live log file before it rotates to `<log>.1`.
pub const MAX_LOG_BYTES: u64 = 256 * 1024;
/// Longest line kept verbatim; longer ones are clipped with a marker.
pub const MAX_LINE_BYTES: usize = 8 * 1024;
/// After `SIGTERM`, how long the group may take to exit before `SIGKILL`.
pub const GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);

struct Live {
    pgid: Pid,
    terminate: Arc<AtomicBool>,
}

/// Live plugin processes by `(plugin, run)`.
#[derive(Default, Clone)]
struct Registry(Arc<Mutex<HashMap<(Entity, u64), Live>>>);

impl Registry {
    fn insert(&self, key: (Entity, u64), live: Live) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(key, live);
        }
    }

    fn remove(&self, key: (Entity, u64)) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&key);
        }
    }

    fn terminate(&self, key: (Entity, u64)) -> bool {
        let Ok(map) = self.0.lock() else {
            return false;
        };
        let Some(live) = map.get(&key) else {
            return false;
        };
        live.terminate.store(true, Ordering::SeqCst);
        let _ = killpg(live.pgid, Signal::SIGTERM);
        true
    }
}

pub struct PluginAdapter {
    inbound: Sender<Inbound>,
    live: Registry,
}

impl PluginAdapter {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self {
            inbound,
            live: Registry::default(),
        }
    }
}

impl Adapter for PluginAdapter {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(effect, Effect::RunPlugin { .. } | Effect::KillPlugin { .. })
    }

    fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        match effect {
            Effect::RunPlugin {
                plugin,
                run,
                argv,
                cwd,
                env,
                log,
            } => {
                let inbound = self.inbound.clone();
                let live = self.live.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        let code = execute(plugin, run, &argv, cwd.as_deref(), &env, &log, &live).await;
                        let _ = inbound
                            .send(Inbound::PluginExited { plugin, run, code })
                            .await;
                    })
                    .detach();
                Ok(())
            }
            Effect::KillPlugin { plugin, run } => {
                // A run that is not live any more already answered; nothing to kill.
                self.live.terminate((plugin, run));
                Ok(())
            }
            other => Err(BevyError::from(format!(
                "plugin adapter: not a plugin effect: {other:?}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Bounded log file
// ---------------------------------------------------------------------------------------------

/// Appends one line to the plugin log, rotating first when the file is past the bound. The
/// line is clipped to [`MAX_LINE_BYTES`].
pub fn append_line(log: &Path, prefix: &str, line: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(log).is_ok_and(|m| m.len() >= MAX_LOG_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        std::fs::rename(log, PathBuf::from(rotated))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    let mut buf = Vec::with_capacity(prefix.len() + line.len().min(MAX_LINE_BYTES) + 32);
    buf.extend_from_slice(prefix.as_bytes());
    if line.len() > MAX_LINE_BYTES {
        let mut end = MAX_LINE_BYTES;
        // Never cut inside a UTF-8 sequence (continuation bytes are `10xxxxxx`).
        while end > 0 && line.get(end).is_some_and(|b| b & 0xC0 == 0x80) {
            end -= 1;
        }
        buf.extend_from_slice(line.get(..end).unwrap_or_default());
        buf.extend_from_slice(b" [clipped]");
    } else {
        buf.extend_from_slice(line);
    }
    buf.push(b'\n');
    file.write_all(&buf)
}

/// The last `limit` lines of the log (the rotated file first when the live one is short).
pub fn read_tail(log: &Path, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut rotated = log.as_os_str().to_owned();
    rotated.push(".1");
    for path in [PathBuf::from(rotated), log.to_path_buf()] {
        if let Ok(text) = std::fs::read_to_string(&path) {
            lines.extend(text.lines().map(str::to_owned));
        }
    }
    let skip = lines.len().saturating_sub(limit);
    lines.drain(..skip);
    lines
}

// ---------------------------------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------------------------------

/// Copies one pipe line by line into the log with `prefix` until EOF.
async fn drain<R: AsyncRead + Unpin>(mut pipe: R, log: &Path, prefix: &str) {
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(chunk.get(..n).unwrap_or_default());
                while let Some(nl) = pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=nl).collect();
                    let line = line.strip_suffix(b"\n").unwrap_or(&line);
                    let _ = append_line(log, prefix, line);
                }
                if pending.len() > MAX_LINE_BYTES {
                    let line = core::mem::take(&mut pending);
                    let _ = append_line(log, prefix, &line);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let _ = append_line(log, prefix, &pending);
    }
}

/// Reaps the leader; after a terminate request the group gets [`GRACE`] before `SIGKILL`.
async fn wait_exit(child: &mut Child, pgid: Pid, terminate: &AtomicBool) -> Option<i32> {
    let mut asked_at: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if terminate.load(Ordering::SeqCst) {
            let at = *asked_at.get_or_insert_with(Instant::now);
            if at.elapsed() >= GRACE {
                let _ = killpg(pgid, Signal::SIGKILL);
                let _ = child.kill();
                return child.wait().ok().and_then(|s| s.code());
            }
        }
        Timer::after(POLL).await;
    }
}

/// Runs the process to completion, logging both streams; `None` on a signal exit or a spawn
/// failure (which is logged).
async fn execute(
    plugin: Entity,
    run: u64,
    argv: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    log: &Path,
    live: &Registry,
) -> Option<i32> {
    let prefix = format!("[{run}] ");
    let Some((program, args)) = argv.split_first() else {
        let _ = append_line(log, &prefix, b"zor: empty command");
        return None;
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = append_line(log, &prefix, format!("zor: spawn {program}: {e}").as_bytes());
            return None;
        }
    };
    let _ = append_line(
        log,
        &prefix,
        format!("zor: started pid {} {}", child.id(), argv.join(" ")).as_bytes(),
    );
    let pgid = Pid::from_raw(i32::try_from(child.id()).unwrap_or(0));
    let terminate = Arc::new(AtomicBool::new(false));
    live.insert(
        (plugin, run),
        Live {
            pgid,
            terminate: Arc::clone(&terminate),
        },
    );
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let reads = async {
        match (stdout.and_then(|p| Async::new(p).ok()), stderr.and_then(|p| Async::new(p).ok())) {
            (Some(out), Some(err)) => {
                future::zip(drain(out, log, &prefix), drain(err, log, &prefix)).await;
            }
            (Some(out), None) => drain(out, log, &prefix).await,
            (None, Some(err)) => drain(err, log, &prefix).await,
            (None, None) => {}
        }
    };
    let (_, code) = future::zip(reads, wait_exit(&mut child, pgid, &terminate)).await;
    live.remove((plugin, run));
    let _ = append_line(
        log,
        &prefix,
        match code {
            Some(code) => format!("zor: exited {code}"),
            None => "zor: exited by signal".to_owned(),
        }
        .as_bytes(),
    );
    code
}
