//! Runner-side applier of `Effect::RunPlugin`/`KillPlugin`: a plugin process is an ordinary
//! subprocess in its own process group with null stdin; stdout and stderr are appended line by
//! line to the plugin's log file (bounded: the file rotates once to `.1` past
//! [`MAX_LOG_BYTES`]) on `IoTaskPool`; the exit is reported as `Inbound::PluginExited`.
//! `KillPlugin` sends `SIGTERM` to the group, then `SIGKILL` after [`GRACE`].

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::os::unix::fs::OpenOptionsExt;
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
    pgid: Option<Pid>,
    terminate: Arc<AtomicBool>,
}

/// Live plugin processes by `(plugin, run)`.
#[derive(Default, Clone)]
struct Registry(Arc<Mutex<HashMap<(Entity, u64), Live>>>);

impl Registry {
    fn reserve(&self, key: (Entity, u64)) -> Option<Arc<AtomicBool>> {
        let mut map = self.0.lock().ok()?;
        if map.len() >= super::MAX_PLUGINS * super::MAX_RUNS_RETAINED || map.contains_key(&key) {
            return None;
        }
        let terminate = Arc::new(AtomicBool::new(false));
        map.insert(key, Live { pgid: None, terminate: terminate.clone() });
        Some(terminate)
    }

    fn started(&self, key: (Entity, u64), pgid: Pid) {
        if let Ok(mut map) = self.0.lock()
            && let Some(live) = map.get_mut(&key)
        {
            live.pgid = Some(pgid);
            if live.terminate.load(Ordering::SeqCst) {
                let _ = killpg(pgid, Signal::SIGTERM);
            }
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
        if let Some(pgid) = live.pgid {
            let _ = killpg(pgid, Signal::SIGTERM);
        }
        true
    }
}

pub struct PluginAdapter {
    inbound: Sender<Inbound>,
    live: Registry,
    executable: PathBuf,
}

impl PluginAdapter {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self::with_executable(inbound, std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zor")))
    }

    pub fn with_executable(inbound: Sender<Inbound>, executable: PathBuf) -> Self {
        Self {
            inbound,
            live: Registry::default(),
            executable,
        }
    }
}

impl Drop for PluginAdapter {
    fn drop(&mut self) {
        if let Ok(map) = self.live.0.lock() {
            for live in map.values() {
                live.terminate.store(true, Ordering::SeqCst);
                if let Some(pgid) = live.pgid {
                    let _ = killpg(pgid, Signal::SIGKILL);
                }
            }
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
                let executable = self.executable.clone();
                let terminate = live.reserve((plugin, run))
                    .ok_or_else(|| BevyError::from("plugin process capacity exhausted or duplicate run"))?;
                IoTaskPool::get()
                    .spawn(async move {
                        let code = if terminate.load(Ordering::SeqCst) {
                            None
                        } else {
                            execute(plugin, run, &argv, cwd.as_deref(), &env, &log, &live, &terminate, &executable).await
                        };
                        let _ = inbound
                            .send(Inbound::PluginExited { plugin, run, code })
                            .await;
                        live.remove((plugin, run));
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

    fn pending(&self) -> bool {
        self.live.0.lock().is_ok_and(|map| !map.is_empty())
    }

    fn shutdown(&mut self) {
        if let Ok(map) = self.live.0.lock() {
            for live in map.values() {
                live.terminate.store(true, Ordering::SeqCst);
                if let Some(pgid) = live.pgid {
                    let _ = killpg(pgid, Signal::SIGTERM);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Bounded log file
// ---------------------------------------------------------------------------------------------

/// Appends one line to the plugin log, rotating first when the file is past the bound. The
/// line is clipped to [`MAX_LINE_BYTES`].
pub fn append_line(log: &Path, prefix: &str, line: &[u8]) -> std::io::Result<()> {
    // All runs of a plugin share a log. Serialize rotation and append as one operation.
    static LOG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOG_LOCK.lock().map_err(|_| std::io::Error::other("log lock poisoned"))?;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(log).is_ok_and(|m| m.len() >= MAX_LOG_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        std::fs::rename(log, PathBuf::from(rotated))?;
    }
    let mut file = OpenOptions::new().create(true).append(true).mode(0o600).open(log)?;
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
    let mut lines = std::collections::VecDeque::with_capacity(limit.min(1000));
    let mut rotated = log.as_os_str().to_owned();
    rotated.push(".1");
    for path in [PathBuf::from(rotated), log.to_path_buf()] {
        use std::io::Read;
        let mut bytes = Vec::new();
        if File::open(&path).and_then(|file| file.take(MAX_LOG_BYTES + MAX_LINE_BYTES as u64 + 1024).read_to_end(&mut bytes)).is_ok() {
            for line in String::from_utf8_lossy(&bytes).lines() {
                if limit == 0 { return Vec::new(); }
                if lines.len() == limit.min(1000) { lines.pop_front(); }
                lines.push_back(line.to_owned());
            }
        }
    }
    lines.into_iter().collect()
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
                future::yield_now().await;
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
            Ok(Some(status)) => {
                // The leader may exit while descendants keep its pipes open. They remain
                // owned by this run and must not outlive it or pin the drain forever.
                let _ = killpg(pgid, Signal::SIGKILL);
                return status.code();
            }
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
    terminate: &AtomicBool,
    executable: &Path,
) -> Option<i32> {
    let prefix = format!("[{run}] ");
    let Some((program, _)) = argv.split_first() else {
        let _ = append_line(log, &prefix, b"zor: empty command");
        return None;
    };
    let mut command = Command::new(executable);
    command
        .args(["plugin", "supervise"])
        .env("ZOR_PLUGIN_ARGV", serde_json::to_string(argv).ok()?)
        .env("ZOR_HOST_PID", std::process::id().to_string())
        .env_remove("ZOR_TOKEN")
        .env_remove("ZOR_BRP")
        .env_remove("FUX_TOKEN")
        .env_remove("FUX_BRP")
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
    live.started((plugin, run), pgid);
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
    let (_, code) = future::zip(reads, wait_exit(&mut child, pgid, terminate)).await;
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

/// Internal process-group supervisor, invoked only by the host adapter. A separate process
/// is essential: threads and Drop cannot run after the server receives SIGKILL.
pub fn supervise() -> Result<i32, String> {
    let parent: i32 = std::env::var("ZOR_HOST_PID").map_err(|e| e.to_string())?
        .parse().map_err(|_| "invalid host pid")?;
    let argv: Vec<String> = serde_json::from_str(&std::env::var("ZOR_PLUGIN_ARGV").map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if nix::unistd::getppid().as_raw() != parent {
        return Err("plugin host no longer owns supervisor".into());
    }
    let (program, args) = argv.split_first().ok_or("empty supervised command")?;
    // Optional bounded status channel for adapters that must distinguish signal exit and
    // spawn failure from an ordinary nonzero exit. Default children retain inherited stdin.
    let mut report = if std::env::var("ZOR_CHILD_STATUS").as_deref() == Ok("1") {
        use std::os::fd::AsFd;
        Some(File::from(std::io::stdin().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?))
    } else { None };
    let mut command = Command::new(program);
    command.args(args).env_remove("ZOR_PLUGIN_ARGV").env_remove("ZOR_HOST_PID")
        .env_remove("ZOR_CHILD_STATUS");
    if report.is_some() { command.stdin(Stdio::null()); }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let problem = format!("spawn {program}: {e}");
            report_status(&mut report, None, Some(&problem));
            return Err(problem);
        }
    };
    loop {
        if nix::unistd::getppid().as_raw() != parent {
            let _ = killpg(nix::unistd::getpgrp(), Signal::SIGKILL);
            return Err("plugin host exited".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                report_status(&mut report, status.code(), None);
                return Ok(status.code().unwrap_or(128));
            }
            Ok(None) => {}
            Err(e) => {
                let problem = e.to_string();
                report_status(&mut report, None, Some(&problem));
                return Err(problem);
            }
        }
        std::thread::sleep(POLL);
    }
}

fn report_status(file: &mut Option<File>, code: Option<i32>, problem: Option<&str>) {
    if let Some(mut file) = file.take() {
        let problem = problem.map(|p| p.chars().take(1024).collect::<String>());
        if let Ok(bytes) = serde_json::to_vec(&(code, problem)) {
            let _ = file.write_all(&bytes);
        }
    }
}
