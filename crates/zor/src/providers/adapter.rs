//! Runner-side applier of `Effect::SpawnProvider`/`WriteProvider`: one subprocess per attempt
//! with piped stdio, read on `IoTaskPool` in bounded chunks into `Inbound::ProviderOutput`,
//! its exit reported as `Inbound::ProviderExited`. An empty `WriteProvider` closes the
//! sidecar's stdin and, if it is still alive after the grace period, kills it.

use core::time::Duration;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::{Receiver, Sender};
use async_io::{Async, Timer};
use bevy_ecs::entity::Entity;
use bevy_ecs::error::BevyError;
use bevy_platform::collections::HashMap;
use bevy_tasks::IoTaskPool;

use crate::model::{Effect, Inbound};
use crate::runner::Adapter;

/// Bytes per `ProviderOutput` message.
const CHUNK: usize = 8 * 1024;
/// After stdin closes, how long the sidecar may take to exit on its own.
const GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

pub struct ProviderAdapter {
    inbound: Sender<Inbound>,
    /// Per attempt: the stdin writer channel; dropping it closes the pipe.
    stdin: HashMap<Entity, Sender<Vec<u8>>>,
}

impl ProviderAdapter {
    pub fn new(inbound: Sender<Inbound>) -> Self {
        Self {
            inbound,
            stdin: HashMap::default(),
        }
    }
}

impl Adapter for ProviderAdapter {
    fn handles(&self, effect: &Effect) -> bool {
        matches!(
            effect,
            Effect::SpawnProvider { .. } | Effect::WriteProvider { .. }
        )
    }

    fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        match effect {
            Effect::SpawnProvider {
                attempt,
                argv,
                cwd,
                env,
            } => {
                if self.stdin.contains_key(&attempt) {
                    return Err(BevyError::from(format!(
                        "provider adapter: {attempt} already has a sidecar"
                    )));
                }
                let (tx, rx) = async_channel::unbounded();
                self.stdin.insert(attempt, tx);
                let inbound = self.inbound.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        run(attempt, argv, cwd, env, rx, inbound).await;
                    })
                    .detach();
                Ok(())
            }
            Effect::WriteProvider { attempt, bytes } => {
                let Some(tx) = self.stdin.get(&attempt) else {
                    return Err(BevyError::from(format!(
                        "provider adapter: {attempt} has no sidecar"
                    )));
                };
                if bytes.is_empty() {
                    // Close request: dropping the sender ends the writer task.
                    self.stdin.remove(&attempt);
                    return Ok(());
                }
                tx.try_send(bytes)
                    .map_err(|e| BevyError::from(format!("provider adapter: {e}")))
            }
            other => Err(BevyError::from(format!(
                "provider adapter: unhandled {other:?}"
            ))),
        }
    }
}

async fn run(
    attempt: Entity,
    argv: Vec<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    rx: Receiver<Vec<u8>>,
    inbound: Sender<Inbound>,
) {
    let Some((program, args)) = argv.split_first() else {
        let _ = inbound
            .send(Inbound::ProviderExited { attempt, code: 127 })
            .await;
        return;
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            bevy_log::warn!("provider {attempt}: spawn failed: {e}");
            let _ = inbound
                .send(Inbound::ProviderExited { attempt, code: 127 })
                .await;
            return;
        }
    };
    let pid = child.id();
    let _ = inbound
        .send(Inbound::ProviderStarted { attempt, pid })
        .await;
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(stdin) = child.stdin.take() {
        let stop = Arc::clone(&stop);
        IoTaskPool::get()
            .spawn(async move {
                write_loop(stdin, rx).await;
                stop.store(true, Ordering::Relaxed);
            })
            .detach();
    }
    if let Some(stdout) = child.stdout.take() {
        read_loop(attempt, stdout, &inbound).await;
    }
    let code = wait(&mut child, &stop).await;
    let _ = inbound
        .send(Inbound::ProviderExited { attempt, code })
        .await;
}

/// Pipes as `File`s: `&File` is `Read`/`Write`, so the safe `Async::{read,write}_with` apply.
async fn write_loop(stdin: std::process::ChildStdin, rx: Receiver<Vec<u8>>) {
    let Ok(stdin) = Async::new(File::from(OwnedFd::from(stdin))) else {
        return;
    };
    while let Ok(bytes) = rx.recv().await {
        let mut offset = 0;
        while offset < bytes.len() {
            let rest = bytes.get(offset..).unwrap_or_default();
            match stdin.write_with(|mut s| s.write(rest)).await {
                Ok(0) | Err(_) => return,
                Ok(n) => offset += n,
            }
        }
    }
}

async fn read_loop(attempt: Entity, stdout: std::process::ChildStdout, inbound: &Sender<Inbound>) {
    let Ok(stdout) = Async::new(File::from(OwnedFd::from(stdout))) else {
        return;
    };
    let mut buf = vec![0u8; CHUNK];
    loop {
        match stdout.read_with(|mut s| s.read(&mut buf)).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let bytes = buf.get(..n).unwrap_or_default().to_vec();
                if inbound
                    .send(Inbound::ProviderOutput { attempt, bytes })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Polls the child; after a close request it gets [`GRACE`] before `kill`.
async fn wait(child: &mut Child, stop: &AtomicBool) -> i32 {
    let mut closed_at: Option<std::time::Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return exit_code(status),
            Ok(None) => {}
            Err(_) => return -1,
        }
        if stop.load(Ordering::Relaxed) {
            let at = *closed_at.get_or_insert_with(std::time::Instant::now);
            if at.elapsed() >= GRACE {
                let _ = child.kill();
                return child.wait().map_or(-1, exit_code);
            }
        }
        Timer::after(POLL).await;
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1)
}
