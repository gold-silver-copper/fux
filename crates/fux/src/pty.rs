//! PTY ownership outside the World (prompt 3.6) and the terminal systems inside it.
//!
//! [`PtyAdapter`] applies the PTY [`Effect`]s the runner drains and converts OS activity into
//! [`Inbound`] messages. Every PTY is read by an `IoTaskPool` task over `async_io::Async<File>`
//! on a duplicate of the master descriptor (the duplicate is made with `filedescriptor`, the
//! only safe route from portable-pty's raw descriptor to an owned `File`), so no OS thread is
//! parked per pane and output buffers come from a shared pool refilled by
//! [`Effect::RecycleBuffer`]. Input is written by a second task fed from a bounded queue, so the
//! runner never blocks on an application that stopped reading.
//!
//! [`TerminalPlugin`] owns the ingest side: PTY bytes are grouped per pane and fed to the
//! [`Terminal`] components in parallel, process events become [`Process`] transitions, and a
//! changed [`PaneSize`] resizes the emulator and the kernel PTY.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_io::{Async, Timer};
use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_platform::collections::HashMap;
use bevy_tasks::futures_lite::{AsyncReadExt, AsyncWriteExt};
use bevy_tasks::{IoTaskPool, Task};
use bevy_time::Time;
use nix::sys::signal::Signal;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::layout::LayoutSystems;
use crate::model::{Effect, Inbound, Limits, OutputPacing, Pane, PaneSize, Phase, Process, Title};
use crate::terminal::{MAX_TITLE_CHARS, Terminal, printable};

/// One PTY read; also the size of every pooled output buffer.
const READ_CHUNK: usize = 64 * 1024;
/// Buffers kept for reuse after a burst; beyond this they are freed.
const MAX_POOLED_BUFFERS: usize = 64;
/// Input chunks queued per pane before further input is dropped with a warning.
const INPUT_QUEUE_DEPTH: usize = 1024;
/// Grace between SIGHUP and SIGKILL on [`Effect::Terminate`].
const KILL_GRACE: Duration = Duration::from_secs(1);
/// Exit code reported when the process could not be started.
const SPAWN_FAILED_CODE: i32 = 127;
/// Exit code reported when the exit status could not be collected.
const UNKNOWN_EXIT_CODE: i32 = -1;

/// Output buffers shared by every reader task. A buffer is `READ_CHUNK` long when handed to a
/// reader; the zero-fill on refill is the one per-read cost that replaces a per-read allocation.
#[derive(Default)]
struct BufferPool(Mutex<Vec<Vec<u8>>>);

impl BufferPool {
    fn take(&self) -> Vec<u8> {
        let mut buffer = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(READ_CHUNK));
        buffer.resize(READ_CHUNK, 0);
        buffer
    }

    fn give(&self, mut buffer: Vec<u8>) {
        if buffer.capacity() < READ_CHUNK {
            return;
        }
        buffer.clear();
        let mut pool = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.len() < MAX_POOLED_BUFFERS {
            pool.push(buffer);
        }
    }
}

/// Signal-safe view of one pane's process shared with its termination and reaper tasks.
struct ProcessState {
    pid: u32,
    reaped: AtomicBool,
}

impl ProcessState {
    fn signal_group(&self, signal: Signal) -> Result<(), BevyError> {
        let pid = i32::try_from(self.pid)?;
        nix::sys::signal::killpg(Pid::from_raw(pid), signal)?;
        Ok(())
    }
}

struct PanePty {
    master: Box<dyn MasterPty + Send>,
    input: async_channel::Sender<Vec<u8>>,
    process: Arc<ProcessState>,
    /// Cancelled on drop: a released pane sends nothing further.
    _reader: Task<()>,
    _writer: Task<()>,
}

/// Owns every live PTY; lives in the runner, never in the World.
pub struct PtyAdapter {
    inbound: async_channel::Sender<Inbound>,
    panes: HashMap<Entity, PanePty>,
    buffers: Arc<BufferPool>,
}

impl PtyAdapter {
    pub fn new(inbound: async_channel::Sender<Inbound>) -> Self {
        Self {
            inbound,
            panes: HashMap::default(),
            buffers: Arc::default(),
        }
    }

    /// Whether [`Self::apply`] handles this effect; the runner routes the rest elsewhere.
    pub fn handles(effect: &Effect) -> bool {
        matches!(
            effect,
            Effect::SpawnPane { .. }
                | Effect::WritePty { .. }
                | Effect::ResizePty { .. }
                | Effect::Terminate { .. }
                | Effect::ReleasePty { .. }
                | Effect::RecycleBuffer(_)
        )
    }

    /// PTYs spawned and not yet released.
    pub fn live_count(&self) -> usize {
        self.panes.len()
    }

    /// Applies one PTY effect. Effects naming an unknown pane are logged and ignored; a spawn
    /// that fails is reported as [`Inbound::PaneSpawnFailed`]. Errors are returned only for
    /// runner-level faults (no task pool, a descriptor that cannot be duplicated).
    pub fn apply(&mut self, effect: Effect) -> Result<(), BevyError> {
        match effect {
            Effect::SpawnPane {
                pane,
                argv,
                cwd,
                env,
                rows,
                cols,
            } => self.spawn(pane, &argv, cwd.as_deref(), &env, rows, cols),
            Effect::WritePty { pane, bytes } => {
                let Some(entry) = self.panes.get(&pane) else {
                    bevy_log::warn!("input for unknown pane {pane}");
                    return Ok(());
                };
                match entry.input.try_send(bytes) {
                    Ok(()) => {}
                    Err(async_channel::TrySendError::Full(_)) => {
                        bevy_log::warn!("pane {pane} is not reading input; chunk dropped");
                    }
                    Err(async_channel::TrySendError::Closed(_)) => {
                        bevy_log::warn!("pane {pane} input writer closed; chunk dropped");
                    }
                }
                Ok(())
            }
            Effect::ResizePty { pane, rows, cols } => {
                let Some(entry) = self.panes.get(&pane) else {
                    bevy_log::warn!("resize for unknown pane {pane}");
                    return Ok(());
                };
                if let Err(error) = entry.master.resize(pty_size(rows, cols)) {
                    bevy_log::warn!("resizing pane {pane}: {error}");
                }
                Ok(())
            }
            Effect::Terminate { pane } => {
                let Some(entry) = self.panes.get(&pane) else {
                    bevy_log::warn!("terminate for unknown pane {pane}");
                    return Ok(());
                };
                let process = Arc::clone(&entry.process);
                if let Err(error) = process.signal_group(Signal::SIGHUP) {
                    bevy_log::warn!("SIGHUP to pane {pane}: {error}");
                }
                pool()?
                    .spawn(async move {
                        Timer::after(KILL_GRACE).await;
                        // A reaped leader means the group id may already belong to someone
                        // else; the remaining members (if any) were the leader's to hang up.
                        if !process.reaped.load(Ordering::Acquire) {
                            let _ = process.signal_group(Signal::SIGKILL);
                        }
                    })
                    .detach();
                Ok(())
            }
            Effect::ReleasePty { pane } => {
                let Some(entry) = self.panes.remove(&pane) else {
                    bevy_log::warn!("release for unknown pane {pane}");
                    return Ok(());
                };
                // Released before it exited: nothing will read the PTY again, so the group
                // must not linger; the detached reaper still collects the status.
                if !entry.process.reaped.load(Ordering::Acquire) {
                    let _ = entry.process.signal_group(Signal::SIGKILL);
                }
                drop(entry);
                Ok(())
            }
            Effect::RecycleBuffer(buffer) => {
                self.buffers.give(buffer);
                Ok(())
            }
            other => {
                bevy_log::warn!("effect not handled by the PTY adapter: {other:?}");
                Ok(())
            }
        }
    }

    fn spawn(
        &mut self,
        pane: Entity,
        argv: &[String],
        cwd: Option<&str>,
        env: &[(String, String)],
        rows: u16,
        cols: u16,
    ) -> Result<(), BevyError> {
        let pool = pool()?;
        if self.panes.contains_key(&pane) {
            bevy_log::warn!("pane {pane} already has a PTY; spawn ignored");
            return Ok(());
        }
        let started = match start_process(argv, cwd, env, rows, cols) {
            Ok(started) => started,
            Err(error) => {
                let inbound = self.inbound.clone();
                let error = error.to_string();
                bevy_log::warn!("spawning pane {pane}: {error}");
                pool.spawn(async move {
                    let _ = inbound.send(Inbound::PaneSpawnFailed { pane, error }).await;
                })
                .detach();
                return Ok(());
            }
        };
        let Started {
            master,
            reader,
            writer,
            pid,
        } = started;
        let process = Arc::new(ProcessState {
            pid,
            reaped: AtomicBool::new(false),
        });
        let (eof_tx, eof_rx) = async_channel::bounded::<()>(1);
        let (input_tx, input_rx) = async_channel::bounded::<Vec<u8>>(INPUT_QUEUE_DEPTH);
        let reader_task = pool.spawn(read_pump(
            pane,
            pid,
            reader,
            self.inbound.clone(),
            Arc::clone(&self.buffers),
            eof_tx,
        ));
        let writer_task = pool.spawn(write_pump(writer, input_rx));
        pool.spawn(reap(
            pane,
            Arc::clone(&process),
            eof_rx,
            self.inbound.clone(),
        ))
        .detach();
        self.panes.insert(
            pane,
            PanePty {
                master,
                input: input_tx,
                process,
                _reader: reader_task,
                _writer: writer_task,
            },
        );
        Ok(())
    }
}

fn pool() -> Result<&'static IoTaskPool, BevyError> {
    IoTaskPool::try_get().ok_or_else(|| BevyError::from("IoTaskPool is not initialised"))
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

struct Started {
    master: Box<dyn MasterPty + Send>,
    reader: Async<File>,
    writer: Async<File>,
    pid: u32,
}

/// Duplicates the master descriptor into an owned, non-blocking `File`. The owned file closes
/// without portable-pty's implicit EOT write, which can itself block at teardown.
fn clone_master(master: &dyn MasterPty) -> Result<Async<File>, BevyError> {
    struct MasterFd(i32);
    impl filedescriptor::AsRawFileDescriptor for MasterFd {
        fn as_raw_file_descriptor(&self) -> filedescriptor::RawFileDescriptor {
            self.0
        }
    }
    let fd = MasterFd(
        master
            .as_raw_fd()
            .ok_or_else(|| BevyError::from("PTY has no descriptor"))?,
    );
    let file = filedescriptor::FileDescriptor::dup(&fd)?.as_file()?;
    Ok(Async::new(file)?)
}

fn start_process(
    argv: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    rows: u16,
    cols: u16,
) -> Result<Started, BevyError> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| BevyError::from("empty pane command"))?;
    let pair = native_pty_system().openpty(pty_size(rows, cols))?;
    let mut command = CommandBuilder::new(program);
    command.args(arguments);
    command.env("TERM", "xterm-256color");
    if let Some(cwd) = cwd {
        command.cwd(cwd);
    }
    // Caller-supplied environment is applied last, so a pane can override TERM if it must.
    for (name, value) in env {
        command.env(name, value);
    }
    // portable-pty's spawn calls `setsid` in the child, so `pid` is also the group id.
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let Some(pid) = child.process_id() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(BevyError::from("pane process has no pid"));
    };
    // A pump that cannot start leaves no orphan: the group is killed and the leader reaped.
    let master = pair.master.as_ref();
    let (reader, writer) =
        match clone_master(master).and_then(|reader| Ok((reader, clone_master(master)?))) {
            Ok(pumps) => pumps,
            Err(error) => {
                if let Ok(pid) = i32::try_from(pid) {
                    let _ = nix::sys::signal::killpg(Pid::from_raw(pid), Signal::SIGKILL);
                }
                let _ = child.wait();
                return Err(error);
            }
        };
    // The reaper collects the status through `waitpid`; the handle itself is not needed.
    drop(child);
    Ok(Started {
        master: pair.master,
        reader,
        writer,
        pid,
    })
}

/// Announces the spawn, pumps output until EOF, then announces EOF and releases the reaper.
async fn read_pump(
    pane: Entity,
    pid: u32,
    mut reader: Async<File>,
    inbound: async_channel::Sender<Inbound>,
    buffers: Arc<BufferPool>,
    eof: async_channel::Sender<()>,
) {
    if inbound
        .send(Inbound::PaneSpawned { pane, pid })
        .await
        .is_err()
    {
        return;
    }
    loop {
        let mut buffer = buffers.take();
        match reader.read(buffer.as_mut_slice()).await {
            Ok(0) => {
                buffers.give(buffer);
                break;
            }
            Ok(count) => {
                buffer.truncate(count);
                if inbound
                    .send(Inbound::PaneOutput {
                        pane,
                        bytes: buffer,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                buffers.give(buffer);
            }
            // EIO is how Linux reports the slave side closing; every other error also ends
            // the stream.
            Err(_) => {
                buffers.give(buffer);
                break;
            }
        }
    }
    // Retaining the descriptor could keep the controlling terminal open while the child is
    // trying to finish exiting.
    drop(reader);
    if inbound.send(Inbound::PaneEof { pane }).await.is_err() {
        return;
    }
    let _ = eof.send(()).await;
}

/// Delivers queued input in order; ends when the queue closes or the PTY rejects a write.
async fn write_pump(mut writer: Async<File>, input: async_channel::Receiver<Vec<u8>>) {
    while let Ok(bytes) = input.recv().await {
        if writer.write_all(&bytes).await.is_err() {
            break;
        }
    }
}

/// Collects the exit status after the reader announced EOF (so `PaneExited` follows
/// `PaneEof`), polling `waitpid` with backoff instead of parking a pool thread. Runs detached:
/// a released pane is still reaped, it just reports nothing.
async fn reap(
    pane: Entity,
    process: Arc<ProcessState>,
    eof: async_channel::Receiver<()>,
    inbound: async_channel::Sender<Inbound>,
) {
    let announced = eof.recv().await.is_ok();
    let Ok(pid) = i32::try_from(process.pid) else {
        return;
    };
    let mut interval = Duration::from_millis(5);
    let code = loop {
        match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                Timer::after(interval).await;
                interval = (interval * 2).min(Duration::from_millis(250));
            }
            Ok(WaitStatus::Exited(_, code)) => break code,
            Ok(WaitStatus::Signaled(_, signal, _)) => break 128_i32.saturating_add(signal as i32),
            Ok(_) => {
                Timer::after(interval).await;
            }
            Err(_) => break UNKNOWN_EXIT_CODE,
        }
    };
    process.reaped.store(true, Ordering::Release);
    if announced {
        let _ = inbound.send(Inbound::PaneExited { pane, code }).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------------------------

/// Sets other plugins order against.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerminalSystems {
    /// `PostUpdate`, inside [`Phase::Projection`] after [`LayoutSystems::SizeFold`]: every
    /// emulator matches its [`PaneSize`] after this set.
    Resize,
}

pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            PostUpdate,
            TerminalSystems::Resize
                .in_set(Phase::Projection)
                .after(LayoutSystems::SizeFold),
        )
        .add_systems(
            First,
            (apply_process_events, ingest_output)
                .chain()
                .in_set(Phase::Ingest),
        )
        .add_systems(PostUpdate, resize_terminals.in_set(TerminalSystems::Resize));
    }
}

/// Panes are `Disabled` while starting, so every pane query must opt back in.
type AnyPane = (With<Pane>, Allow<Disabled>);

/// Process events become [`Process`] transitions. A spawned pane is also resized to its
/// current [`PaneSize`] in case the size moved while it was starting.
fn apply_process_events(
    mut inbound: MessageReader<Inbound>,
    mut panes: Query<(&mut Process, &mut Title, &PaneSize), AnyPane>,
    mut effects: MessageWriter<Effect>,
) {
    for message in inbound.read() {
        let pane = match message {
            Inbound::PaneSpawned { pane, .. }
            | Inbound::PaneEof { pane }
            | Inbound::PaneExited { pane, .. }
            | Inbound::PaneSpawnFailed { pane, .. } => *pane,
            _ => continue,
        };
        let Ok((mut process, mut title, size)) = panes.get_mut(pane) else {
            bevy_log::warn!("process event for unknown pane {pane}: {message:?}");
            continue;
        };
        match message {
            Inbound::PaneSpawned { pid, .. } => {
                if *process == Process::Starting {
                    *process = Process::Live { pid: *pid };
                    effects.write(Effect::ResizePty {
                        pane,
                        rows: size.rows,
                        cols: size.cols,
                    });
                } else {
                    bevy_log::warn!("pane {pane} spawned while {:?}", *process);
                }
            }
            Inbound::PaneEof { .. } => match *process {
                Process::Live { pid } => *process = Process::Eof { pid },
                Process::Eof { .. } | Process::Terminating { .. } | Process::Exited { .. } => {}
                Process::Starting => bevy_log::warn!("EOF for pane {pane} before it spawned"),
            },
            Inbound::PaneExited { code, .. } if !matches!(*process, Process::Exited { .. }) => {
                *process = Process::Exited { code: *code };
            }
            Inbound::PaneSpawnFailed { error, .. } => {
                *process = Process::Exited {
                    code: SPAWN_FAILED_CODE,
                };
                title.set_if_neq(Title(printable(error, MAX_TITLE_CHARS)));
            }
            _ => {}
        }
    }
}

/// Feeds PTY output to the emulators: bytes are grouped per pane (their buffers moved out of
/// the messages, not copied), every terminal with output is fed in parallel, and the serial
/// tail publishes titles, host replies and pacing, then recycles the buffers.
fn ingest_output(
    mut inbound: MessageMutator<Inbound>,
    mut terminals: Query<(Entity, &mut Terminal, &mut Title, &mut OutputPacing), AnyPane>,
    mut effects: MessageWriter<Effect>,
    time: Res<Time>,
    limits: Res<Limits>,
    mut batch: Local<Vec<(Entity, Vec<u8>)>>,
    mut replies: Local<Vec<u8>>,
) {
    batch.clear();
    for message in inbound.read() {
        let Inbound::PaneOutput { pane, bytes } = message else {
            continue;
        };
        let bytes = std::mem::take(bytes);
        if terminals.contains(*pane) {
            batch.push((*pane, bytes));
        } else {
            bevy_log::warn!("output for unknown pane {}", *pane);
            effects.write(Effect::RecycleBuffer(bytes));
        }
    }
    if batch.is_empty() {
        return;
    }
    batch.sort_by_key(|(pane, _)| *pane);
    let grouped: &[(Entity, Vec<u8>)] = &batch;
    terminals
        .par_iter_mut()
        .for_each(|(entity, mut terminal, _, _)| {
            let start = grouped.partition_point(|(pane, _)| *pane < entity);
            let end = grouped.partition_point(|(pane, _)| *pane <= entity);
            if start == end {
                return;
            }
            let chunks = grouped.get(start..end).unwrap_or(&[]);
            terminal.feed_all(chunks.iter().map(|(_, bytes)| bytes.as_slice()));
        });
    let now_ms = u64::try_from(time.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut previous = None;
    for (pane, _) in grouped {
        if previous == Some(*pane) {
            continue;
        }
        previous = Some(*pane);
        let Ok((_, mut terminal, mut title, mut pacing)) = terminals.get_mut(*pane) else {
            continue;
        };
        let terminal = terminal.bypass_change_detection();
        if let Some(changed) = terminal.take_title_change() {
            title.set_if_neq(Title(changed));
        }
        if terminal.take_host_replies(&mut replies) {
            effects.write(Effect::WritePty {
                pane: *pane,
                bytes: replies.clone(),
            });
        }
        let seq = terminal.seq();
        if pacing.last_event_seq != seq
            && now_ms.saturating_sub(pacing.last_event_ms) >= limits.output_pacing_ms
        {
            pacing.set_if_neq(OutputPacing {
                last_event_ms: now_ms,
                last_event_seq: seq,
            });
        }
    }
    for (_, bytes) in batch.drain(..) {
        effects.write(Effect::RecycleBuffer(bytes));
    }
}

/// A pane whose [`PaneSize`] changed this update.
type ResizedPane = (Changed<PaneSize>, AnyPane);

/// A changed [`PaneSize`] resizes the emulator; the kernel PTY follows once the process exists
/// (a starting pane is sized by `SpawnPane` and resized again on `PaneSpawned`).
fn resize_terminals(
    mut panes: Query<(Entity, &PaneSize, &mut Terminal, &Process), ResizedPane>,
    mut effects: MessageWriter<Effect>,
) {
    for (pane, size, mut terminal, process) in &mut panes {
        if (terminal.rows(), terminal.cols()) != (size.rows, size.cols) {
            terminal.resize(size.rows, size.cols);
        }
        if process.pid().is_some() {
            effects.write(Effect::ResizePty {
                pane,
                rows: size.rows,
                cols: size.cols,
            });
        }
    }
}
