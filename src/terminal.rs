use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Read, Write},
    os::fd::BorrowedFd,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};

use async_channel::{Receiver, Sender};
use async_io::Async;
use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use bevy_tasks::{
    IoTaskPool, Task,
    futures_lite::future::{race, yield_now},
};
use nix::{
    errno::Errno,
    libc,
    sys::signal::{Signal, kill, killpg},
    unistd::{Pid, dup, getsid},
};
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::model::{Launch, ProcessState, Status, Wake};
mod rows;

const CHUNK: usize = 8192;
const OUTPUT_SLOTS: usize = 16;
/// One write must fit the largest accepted paste plus its bracketed envelope,
/// so a paste the policy layer accepts is never dropped by the transport.
const MAX_INPUT: usize = crate::paste::LIMIT + crate::paste::ENVELOPE;
/// Input may wait for the PTY writer up to this many bytes -- the ceiling the
/// old sixteen slots of `MAX_INPUT` had -- however many pieces it came in.
const INPUT_BYTES: usize = 16 * MAX_INPUT;
/// What one queued piece costs beyond its bytes: its allocation and its node.
const ENTRY_COST: usize = 64;

/// Input and terminal replies waiting for the PTY writer, bounded by what they
/// cost rather than by how many pieces they came in (hunt 8 finding 021). The
/// old bound was sixteen pieces, so once the PTY's own buffer was full every
/// key past the sixteenth was refused and lost, however few bytes waited.
#[derive(Clone)]
struct InputQueue {
    tx: Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    capacity: usize,
}

impl InputQueue {
    fn new(capacity: usize) -> (Self, async_channel::Receiver<Vec<u8>>) {
        let (tx, rx) = async_channel::unbounded();
        let queue = Self {
            tx,
            queued: Arc::default(),
            capacity,
        };
        (queue, rx)
    }

    fn push(&self, bytes: Vec<u8>) -> Result<(), String> {
        let cost = bytes.len() + ENTRY_COST;
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
                queued
                    .checked_add(cost)
                    .filter(|total| *total <= self.capacity)
            })
            .map_err(|_| {
                "the pane's program is not reading its input; nothing more is queued until it does"
                    .to_owned()
            })?;
        self.tx.try_send(bytes).map_err(|error| {
            self.written(cost - ENTRY_COST);
            error.to_string()
        })
    }

    /// The writer has passed a piece of `len` bytes to the PTY.
    fn written(&self, len: usize) {
        self.queued.fetch_sub(len + ENTRY_COST, Ordering::AcqRel);
    }
}
const UPDATE_BYTES: usize = 65536;

pub struct TerminalPlugin;

/// Native changes are visible after this set, before presentation is computed.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalSystems;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Notify {
            wake: app.world().resource::<Wake>().clone(),
            pending: Arc::default(),
        })
        .add_systems(
            Update,
            (remove_terminals, spawn_terminals, update_terminals)
                .chain()
                .in_set(TerminalSystems),
        );
    }
}

#[derive(Component)]
pub struct Terminal {
    parser: fux_vt::Parser,
    runtime: Runtime,
    /// The reader outlives the process: it drains output queued in the PTY
    /// after the group is killed, then ends at EOF.
    reader: Option<Task<()>>,
    output: Receiver<Output>,
    notify: Notify,
    published_size: (u16, u16),
    status: Status,
    revision: u64,
    rows: rows::Rows,
    /// Process-wide unique. Row IDs are parser-local, so a selection must
    /// also remember which terminal instance issued them.
    instance: u64,
}

static INSTANCES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn next_instance() -> u64 {
    INSTANCES.fetch_add(1, Ordering::Relaxed)
}

/// Everything that exists only while the child runs. Input after exit is a
/// type error here, not a runtime check spread over optional fields.
enum Runtime {
    Live(Live),
    Stopped,
}
struct Live {
    job: Job,
    input: InputQueue,
    writer: Task<()>,
    reader_stop: Sender<()>,
}

struct Job {
    // MasterPty is Send but not Sync. No I/O executor locks this mutex.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    pid: u32,
    exited: Arc<Mutex<Option<Result<i32, String>>>>,
    waiter: Option<JoinHandle<()>>,
    owned: bool,
}

enum Output {
    Bytes(Vec<u8>),
    Error(String),
    Eof,
}

#[derive(Resource, Clone)]
struct Notify {
    wake: Wake,
    pending: Arc<AtomicBool>,
}

impl Notify {
    fn send(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.wake.notify();
        }
    }
}

impl Terminal {
    #[cfg(test)]
    pub(crate) fn for_test(parser: fux_vt::Parser) -> Self {
        let (_, output) = async_channel::bounded(1);
        let published_size = parser.screen().size();
        Self {
            parser,
            runtime: Runtime::Stopped,
            reader: None,
            output,
            notify: Notify {
                wake: Wake(thread::current()),
                pending: Arc::default(),
            },
            published_size,
            status: Status::Exited { code: 0 },
            revision: 0,
            rows: rows::Rows::default(),
            instance: next_instance(),
        }
    }

    pub fn screen(&self) -> &fux_vt::Screen {
        self.parser.screen()
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// Input is accepted atomically into a bounded queue, never partially queued.
    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_INPUT {
            return Err(format!(
                "input exceeds {MAX_INPUT} bytes; send smaller chunks"
            ));
        }
        match &self.runtime {
            Runtime::Live(live) => live.input.push(bytes.to_vec()),
            Runtime::Stopped => Err("process has exited".into()),
        }
    }

    /// Reuse row extraction while returning EVERY row of a complete frame.
    pub fn snapshot(
        &mut self,
        scrollback: usize,
        visible_rows: u16,
        visible_cols: u16,
    ) -> (&[Arc<str>], &fux_vt::Screen) {
        let screen = self.parser.screen();
        (
            self.rows
                .snapshot(screen, scrollback, visible_rows, visible_cols),
            screen,
        )
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn selection_grid(&self, scrollback: usize) -> Result<crate::selection::Grid, String> {
        crate::selection::Grid::capture(self.parser.screen(), scrollback)
    }

    /// The history offset the emulator can actually show for a request, so a
    /// viewer never accumulates an offset past the oldest retained line.
    pub fn clamp_scrollback(&self, scrollback: usize) -> usize {
        scrollback.min(self.screen().history_len())
    }

    pub fn copy_text(&self, scrollback: usize) -> Result<String, String> {
        let (rows, cols) = self.screen().size();
        self.screen()
            .window(scrollback, rows, cols)
            .text(
                (0, 0),
                (rows - 1, cols - 1),
                crate::selection::MAX_CELLS,
                crate::selection::MAX_COPY_BYTES,
            )
            .map(|mut text| {
                // Whole-pane copy omits trailing empty rows; an explicitly
                // selected range retains its requested hard line breaks.
                text.truncate(text.trim_end_matches('\n').len());
                text
            })
            .map_err(|e| e.to_string())
    }

    fn spawn(launch: &Launch, rows: u16, cols: u16, notify: Notify) -> Result<Self, String> {
        if rows == 0 || cols == 0 {
            return Err("terminal dimensions must be nonzero".into());
        }
        let parser =
            fux_vt::Parser::new(rows, cols, launch.history_lines).map_err(|e| e.to_string())?;
        let program = launch.argv.first().ok_or("argv must contain a program")?;
        let pair = native_pty_system()
            .openpty(size(rows, cols))
            .map_err(|e| e.to_string())?;
        let raw = pair
            .master
            .as_raw_fd()
            .ok_or("PTY has no Unix descriptor")?;
        // The master remains alive throughout these duplications. Async sets O_NONBLOCK
        // on the shared open-file description; neither pool task can block in read/write.
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        let read_fd = Async::new(File::from(dup(fd).map_err(|e| e.to_string())?))
            .map_err(|e| e.to_string())?;
        let write_fd = Async::new(File::from(dup(fd).map_err(|e| e.to_string())?))
            .map_err(|e| e.to_string())?;
        let mut command = CommandBuilder::new(program);
        command.args(launch.argv.get(1..).unwrap_or_default());
        if !launch.cwd.is_empty() {
            command.cwd(&launch.cwd);
        }
        command.env("TERM", "xterm-256color");
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let Some(pid) = child.process_id() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("native child has no process ID".into());
        };
        let exited = Arc::new(Mutex::new(None));
        let mut job = Job {
            master: Mutex::new(Some(pair.master)),
            child,
            pid,
            exited: exited.clone(),
            waiter: None,
            owned: true,
        };
        let waiter_notify = notify.clone();
        // waitid(WNOWAIT) blocks, so it must not occupy a shared Bevy executor.
        // Unlike Child::wait(), it keeps the leader/PGID reserved until group cleanup.
        job.waiter = match thread::Builder::new()
            .name(format!("fux-wait-{pid}"))
            .spawn(move || {
                *exited.lock() = Some(wait_unreaped(pid, false));
                waiter_notify.send();
            }) {
            Ok(waiter) => Some(waiter),
            Err(error) => {
                // Close I/O duplicates before Job's error-path reap.
                drop(read_fd);
                drop(write_fd);
                return Err(error.to_string());
            }
        };
        let (input, input_rx) = InputQueue::new(INPUT_BYTES);
        let released = input.clone();
        let (output_tx, output) = async_channel::bounded(OUTPUT_SLOTS);
        let (reader_stop, stop_rx) = async_channel::bounded(1);
        let reader_notify = notify.clone();
        let reader_tx = output_tx.clone();
        let reader = IoTaskPool::get().spawn(async move {
            let mut draining = false;
            let mut drained = 0;
            loop {
                let mut bytes = vec![0; CHUNK];
                let result = if draining {
                    // After the group is killed, preserve bytes already in the PTY,
                    // but do not let an escaped descendant retain our descriptor.
                    if drained >= OUTPUT_SLOTS * CHUNK {
                        Ok(0)
                    } else {
                        let mut fd = read_fd.get_ref();
                        fd.read(&mut bytes)
                    }
                } else {
                    match race(
                        async { Some(read_fd.read_with(|mut fd| fd.read(&mut bytes)).await) },
                        async {
                            let _ = stop_rx.recv().await;
                            None
                        },
                    )
                    .await
                    {
                        Some(result) => result,
                        None => {
                            draining = true;
                            drained = 0;
                            continue;
                        }
                    }
                };
                let event = match result {
                    Ok(0) => Output::Eof,
                    Ok(n) => {
                        drained += n;
                        bytes.truncate(n);
                        Output::Bytes(bytes)
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if draining && e.kind() == io::ErrorKind::WouldBlock => Output::Eof,
                    // Linux PTY masters return EIO, not EOF, after the slave closes.
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => Output::Eof,
                    Err(e) => Output::Error(format!("PTY read: {e}")),
                };
                let done = !matches!(event, Output::Bytes(_));
                if reader_tx.send(event).await.is_err() {
                    break;
                }
                reader_notify.send();
                if done {
                    break;
                }
                yield_now().await;
            }
        });
        let writer_notify = notify.clone();
        let writer = IoTaskPool::get().spawn(async move {
            while let Ok(bytes) = input_rx.recv().await {
                let mut remaining: &[u8] = &bytes;
                while !remaining.is_empty() {
                    match write_fd.write_with(|mut fd| fd.write(remaining)).await {
                        Ok(0) => {
                            let _ = output_tx
                                .send(Output::Error("PTY write returned zero".into()))
                                .await;
                            writer_notify.send();
                            return;
                        }
                        Ok(n) => remaining = remaining.get(n..).unwrap_or_default(),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            let _ = output_tx
                                .send(Output::Error(format!("PTY write: {e}")))
                                .await;
                            writer_notify.send();
                            return;
                        }
                    }
                    yield_now().await;
                }
                released.written(bytes.len());
            }
        });
        Ok(Self {
            parser,
            status: Status::Running { pid, error: None },
            runtime: Runtime::Live(Live {
                job,
                input,
                writer,
                reader_stop,
            }),
            reader: Some(reader),
            output,
            revision: 1,
            instance: next_instance(),
            notify,
            published_size: (rows, cols),
            rows: rows::Rows::default(),
        })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        if rows == 0 || cols == 0 {
            return Err("terminal dimensions must be nonzero".into());
        }
        // Backing dimensions are exact; pane-layout usability minima are separate.
        if self.parser.screen().size() == (rows, cols) {
            return Ok(());
        }
        if let Runtime::Live(live) = &self.runtime {
            live.job
                .master
                .lock()
                .as_ref()
                .ok_or("PTY is closed")?
                .resize(size(rows, cols))
                .map_err(|e| e.to_string())?;
        }
        if let Err(error) = self.parser.resize(rows, cols) {
            if let Runtime::Live(live) = &self.runtime {
                let (old_rows, old_cols) = self.parser.screen().size();
                if let Some(master) = live.job.master.lock().as_ref() {
                    master
                        .resize(size(old_rows, old_cols))
                        .map_err(|rollback| format!("{error}; PTY rollback: {rollback}"))?;
                }
            }
            return Err(error.to_string());
        }
        self.revision = self.revision.wrapping_add(1);
        self.notify.send();
        Ok(())
    }

    /// The reflected state this terminal publishes. The sync system copies it
    /// every update; `terminate` also publishes it in the step that ends the
    /// process, as Launch removal does, so a caller never reads a process that
    /// is already reaped as running, and a frame publishes the size it gives
    /// the PTY (`published`).
    pub(crate) fn state(&self) -> ProcessState {
        let (rows, cols) = self.published_size;
        ProcessState {
            rows,
            cols,
            status: self.status.clone(),
            revision: self.revision,
        }
    }

    /// Takes the emulator's current size as the published one and returns
    /// the state to publish.
    pub(crate) fn published(&mut self) -> ProcessState {
        self.published_size = self.parser.screen().size();
        self.state()
    }

    /// Terminates the owned process group, reaps its leader, and retains the screen.
    pub fn stop(&mut self) -> Result<(), String> {
        let Runtime::Live(live) = std::mem::replace(&mut self.runtime, Runtime::Stopped) else {
            return Ok(());
        };
        // Cancel our own reader first: nothing should still hold the descriptor
        // while the group is killed.
        self.reader.take();
        let result = self.finish(live, None);
        self.revision = self.revision.wrapping_add(1);
        self.notify.send();
        result
    }

    /// Records a running process's I/O error without ending the process.
    fn fault(&mut self, error: String) {
        match &mut self.status {
            Status::Running { error: slot, .. } => *slot = Some(error),
            Status::Starting => self.status = Status::Failed { error },
            Status::Exited { .. } | Status::Failed { .. } => {}
        }
    }

    /// Leaves `Live`: closes input, drops the writer, reaps the group and
    /// publishes the final status. `observed` is the waiter's verdict when the
    /// child ended on its own; an explicit stop has none.
    fn finish(&mut self, live: Live, observed: Option<Result<i32, String>>) -> Result<(), String> {
        let Live {
            mut job,
            input,
            writer,
            reader_stop,
        } = live;
        input.tx.close();
        drop(writer);
        let result = match observed {
            Some(observed) => job.finish().and(observed),
            None => job.finish(),
        };
        // Let a natural exit's reader drain what the PTY still holds.
        let _ = reader_stop.try_send(());
        match result {
            Ok(code) => {
                self.status = Status::Exited { code };
                Ok(())
            }
            Err(error) => {
                self.status = Status::Failed {
                    error: error.clone(),
                };
                Err(error)
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl Job {
    fn finish(&mut self) -> Result<i32, String> {
        if !self.owned {
            return Err("process ownership already released".into());
        }
        // Confirm ownership without reaping. ECHILD means some external code has
        // reaped it: never signal that numeric PID/PGID again in that case.
        let status = match wait_unreaped(self.pid, true) {
            Err(error) => {
                self.owned = false;
                return Err(error);
            }
            Ok(status) => status,
        };
        if status == -1 {
            // Let an interactive shell hang up its job-control groups before the
            // hard kill. Killing the shell first strands ordinary background jobs.
            let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGHUP);
            // Deliver the hangup a closed terminal means to every job in the
            // session, whichever shell started it. bash and zsh forward it to
            // their jobs themselves; dash, which is /bin/sh on Debian, does
            // not, and its jobs sit in their own groups, out of reach of the
            // group signal above. A job that ignores the hangup (`nohup`)
            // survives, as it would a closed terminal.
            signal_session(self.pid, Signal::SIGHUP);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
            while std::time::Instant::now() < deadline {
                match wait_unreaped(self.pid, true) {
                    Ok(-1) => thread::sleep(std::time::Duration::from_millis(2)),
                    Ok(_) => break,
                    Err(error) => {
                        self.owned = false;
                        return Err(error);
                    }
                }
            }
            // After shell hangup propagation, release the master before blocking
            // reap: macOS can hold a dying writer while PTY output is queued.
            self.master.get_mut().take();
        }
        let signal_error = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL)
            .err()
            .filter(|error| *error != Errno::ESRCH);
        // Closing a busy PTY can leave its leader briefly exiting: Darwin rejects
        // another signal before waitid can observe the zombie. Classify EPERM
        // only after that transition, while the unreaped leader still owns its ID.
        let observed = wait_unreaped(self.pid, false);
        #[cfg(target_os = "macos")]
        let signal_error = if signal_error == Some(Errno::EPERM) && observed.is_ok() {
            // Darwin returns EPERM for a group containing only its zombie leader.
            // Verify that exact condition; never hide a denial for live members.
            let mut members = [0 as libc::pid_t; 2];
            let count = unsafe {
                libc::proc_listpgrppids(
                    self.pid as libc::pid_t,
                    members.as_mut_ptr().cast(),
                    std::mem::size_of_val(&members) as libc::c_int,
                )
            };
            if count == 1 && members[0] == self.pid as libc::pid_t {
                None
            } else {
                signal_error
            }
        } else {
            signal_error
        };
        // No signal can follow this reap. The leader has reserved its group ID
        // through the entire kill operation, including natural-exit cleanup.
        let reaped = self.child.wait().map_err(|e| e.to_string());
        self.owned = false;
        if let Some(waiter) = self.waiter.take() {
            waiter
                .join()
                .map_err(|_| "child waiter panicked".to_string())?;
        }
        reaped?;
        if let Some(error) = signal_error {
            return Err(format!("process group termination: {error}"));
        }
        observed
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.owned {
            let _ = self.finish();
        }
    }
}

/// Signals every process in the session `leader` leads, other than the leader
/// (hunt 7 finding 013). Used for the hangup only: a job that ignores it, as
/// `nohup` arranges, has asked to outlive its terminal. Job control puts each background job in its own
/// process group, so signalling the leader's group never reaches it, but the
/// job stays in the leader's session unless it leaves with `setsid`.
///
/// The session ID is the leader's pid, and the leader is held unreaped for the
/// whole kill, so the ID cannot have been reused by another session. A member
/// is re-checked immediately before it is signalled; a process that left the
/// session in between is skipped. A process that called `setsid` itself has
/// left on purpose and is not chased.
fn signal_session(leader: u32, signal: Signal) {
    let Ok(leader) = i32::try_from(leader).map(Pid::from_raw) else {
        return;
    };
    let in_session = |pid: Pid| pid != leader && getsid(Some(pid)) == Ok(leader);
    for pid in processes().into_iter().filter(|pid| in_session(*pid)) {
        if in_session(pid) {
            let _ = kill(pid, signal);
        }
    }
}

/// Every process id the system lists, as candidates for `signal_session`.
#[cfg(target_os = "linux")]
fn processes() -> Vec<Pid> {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .map(Pid::from_raw)
                .collect()
        })
        .unwrap_or_default()
}

/// Every process id the system lists, as candidates for `signal_session`.
#[cfg(target_os = "macos")]
fn processes() -> Vec<Pid> {
    // The count can grow between the two calls; the headroom covers that, and
    // a process created after the listing cannot have been a background job
    // of a shell that is already being hung up.
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let bytes = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())).unwrap_or(0);
    // SAFETY: the buffer is `bytes` long and writable.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(listed).unwrap_or(0));
    pids.into_iter()
        .filter(|pid| *pid > 0)
        .map(Pid::from_raw)
        .collect()
}

/// Other platforms have no listing here; the process group kill still runs.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn processes() -> Vec<Pid> {
    Vec::new()
}

/// -1 denotes a live child only in the nonblocking probe.
fn wait_unreaped(pid: u32, nonblocking: bool) -> Result<i32, String> {
    loop {
        // libc is needed because nix 0.30 does not expose waitid on macOS.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOWAIT | if nonblocking { libc::WNOHANG } else { 0 };
        let result = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("waitid({pid}): {error}"));
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(-1);
        }
        // macOS reports a stopped, traced or continued child even though only
        // WEXITED was asked for, and WNOWAIT leaves the report in place (hunt 8
        // finding 020). A stop is not an exit: the leader is alive. The probe
        // says so; the blocking waiter waits on, without spinning on the
        // repeated report. Linux never reports these here.
        if !matches!(
            info.si_code,
            libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
        ) {
            if nonblocking {
                return Ok(-1);
            }
            thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }
        let code = unsafe { info.si_status() };
        return Ok(if info.si_code == libc::CLD_EXITED {
            code
        } else {
            128 + code
        });
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn remove_terminals(
    mut removed: RemovedComponents<Launch>,
    mut commands: Commands,
    mut terminals: Query<(&mut Terminal, Option<&mut ProcessState>)>,
) {
    for entity in removed.read() {
        if let Ok((mut terminal, state)) = terminals.get_mut(entity) {
            commands.entity(entity).remove::<Terminal>();
            let _ = terminal.stop();
            if let Some(mut state) = state {
                state.status = terminal.status.clone();
                state.revision = terminal.revision;
            }
        }
    }
}

fn spawn_terminals(
    mut commands: Commands,
    notify: Res<Notify>,
    mut launches: Query<(Entity, &Launch, &mut ProcessState), Added<Launch>>,
) {
    for (entity, launch, mut state) in &mut launches {
        match Terminal::spawn(launch, state.rows, state.cols, notify.clone()) {
            Ok(terminal) => {
                commands.entity(entity).insert(terminal);
            }
            Err(error) => {
                state.status = Status::Failed { error };
                state.revision = state.revision.wrapping_add(1);
            }
        }
    }
}

fn update_terminals(
    notify: Res<Notify>,
    mut states: Query<(&mut Terminal, Option<&mut ProcessState>), With<Launch>>,
) {
    notify.pending.store(false, Ordering::Release);
    let mut remaining_output = false;
    for (mut terminal, mut state) in &mut states {
        // Direct reflected ProcessState dimension edits resize the actual PTY.
        if let Some(state) = &state
            && state.is_changed()
            && terminal.published_size != (state.rows, state.cols)
            && let Err(error) = terminal.resize(state.rows, state.cols)
        {
            terminal.fault(error);
        }
        let mut consumed = 0;
        while consumed < UPDATE_BYTES {
            let Ok(event) = terminal.output.try_recv() else {
                break;
            };
            match event {
                Output::Bytes(bytes) => {
                    consumed += bytes.len();
                    let input = match &terminal.runtime {
                        Runtime::Live(live) => Some(live.input.clone()),
                        Runtime::Stopped => None,
                    };
                    if let Err(error) = process_output(&mut terminal.parser, input.as_ref(), &bytes)
                    {
                        terminal.fault(error);
                    }
                }
                Output::Error(error) => terminal.fault(error),
                Output::Eof => {
                    terminal.reader.take();
                }
            }
            terminal.revision = terminal.revision.wrapping_add(1);
        }
        remaining_output |= !terminal.output.is_empty();
        let exited = match &terminal.runtime {
            Runtime::Live(live) => live.job.exited.lock().take(),
            Runtime::Stopped => None,
        };
        if let Some(observed) = exited
            && let Runtime::Live(live) = std::mem::replace(&mut terminal.runtime, Runtime::Stopped)
        {
            let _ = terminal.finish(live, Some(observed));
            terminal.revision = terminal.revision.wrapping_add(1);
        }
        let published = terminal.published();
        let Some(state) = &mut state else { continue };
        state.set_if_neq(published);
    }
    if remaining_output {
        notify.wake.notify();
    }
}

fn process_output(
    parser: &mut fux_vt::Parser,
    input: Option<&InputQueue>,
    bytes: &[u8],
) -> Result<(), String> {
    let mut reply_error = None;
    parser
        .process_with_replies(bytes, |reply| {
            if let Some(input) = input
                && let Err(error) = input.push(reply.to_vec())
            {
                reply_error = Some(format!("terminal reply: {error}"));
            }
        })
        .map_err(|e| e.to_string())?;
    reply_error.map_or(Ok(()), Err)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    foreground: fux_vt::Color,
    background: fux_vt::Color,
    flags: u8,
}

impl Style {
    fn of(cell: &fux_vt::Cell) -> Self {
        Self {
            foreground: cell.fgcolor(),
            background: cell.bgcolor(),
            flags: u8::from(cell.bold())
                | (u8::from(cell.dim()) << 1)
                | (u8::from(cell.italic()) << 2)
                | (u8::from(cell.underline()) << 3)
                | (u8::from(cell.inverse()) << 4),
        }
    }

    fn write(self, output: &mut String) {
        output.push_str("\x1b[0");
        for (bit, sgr) in [(1, 1), (2, 2), (4, 3), (8, 4), (16, 7)] {
            if self.flags & bit != 0 {
                let _ = write!(output, ";{sgr}");
            }
        }
        for (color, selector) in [(self.foreground, 38), (self.background, 48)] {
            match color {
                fux_vt::Color::Default => {}
                fux_vt::Color::Idx(index) => {
                    let _ = write!(output, ";{selector};5;{index}");
                }
                fux_vt::Color::Rgb(r, g, b) => {
                    let _ = write!(output, ";{selector};2;{r};{g};{b}");
                }
            }
        }
        output.push('m');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    // Hunt 8 finding 020. macOS's waitid reports a stopped child even when
    // asked only for WEXITED, so a stopped pane leader read as exited (128 +
    // SIGSTOP) and fux ended the pane. A stop is not an exit: the probe must
    // report the leader live and the blocking waiter must keep waiting.
    #[test]
    fn a_stopped_leader_is_not_an_exited_one() -> Outcome {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = child.id();
        let raw = Pid::from_raw(i32::try_from(pid)?);
        // Observe everything first, then end the child, then assert: a failed
        // assertion must not leave a stopped process behind.
        let observed = (|| -> Result<_, Box<dyn std::error::Error>> {
            nix::sys::signal::kill(raw, Signal::SIGSTOP)?;
            std::thread::sleep(std::time::Duration::from_millis(200));
            let probe_stopped = wait_unreaped(pid, true);
            let waiter = thread::spawn(move || wait_unreaped(pid, false));
            std::thread::sleep(std::time::Duration::from_millis(300));
            let still_waiting = !waiter.is_finished();
            nix::sys::signal::kill(raw, Signal::SIGCONT)?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            let probe_continued = wait_unreaped(pid, true);
            Ok((probe_stopped, waiter, still_waiting, probe_continued))
        })();
        let _ = nix::sys::signal::kill(raw, Signal::SIGKILL);
        let (probe_stopped, waiter, still_waiting, probe_continued) = observed?;
        let ended = waiter.join().map_err(|_| "waiter panicked")?;
        child.wait()?;
        assert_eq!(probe_stopped, Ok(-1));
        assert!(still_waiting, "the blocking waiter returned for a stop");
        assert_eq!(probe_continued, Ok(-1));
        assert_eq!(ended, Ok(128 + Signal::SIGKILL as i32));
        Ok(())
    }

    // Hunt 8 finding 021. Input was queued for the PTY writer in 16 slots,
    // one per input, so once the pane's PTY buffer was full every key past
    // the sixteenth was refused ("sending into a full channel") and lost,
    // however few bytes were waiting. A program that reads slowly must get
    // every key, in order; only a byte bound refuses input.
    #[test]
    fn keys_wait_for_a_slow_reader_instead_of_being_lost() -> Outcome {
        let mut app = App::new();
        app.add_plugins(bevy_app::TaskPoolPlugin::default());
        let notify = Notify {
            wake: Wake(thread::current()),
            pending: Arc::default(),
        };
        let directory =
            std::env::temp_dir().join(format!("fux-slow-reader-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let got = directory.join("got");
        let recipe = Launch {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("stty raw -echo; exec cat > '{}'", got.display()),
            ],
            cwd: String::new(),
            history_lines: 4,
        };
        let mut terminal = Terminal::spawn(&recipe, 24, 80, notify)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let pid = match &terminal.runtime {
            Runtime::Live(live) => live.job.pid,
            Runtime::Stopped => return Err("child not running".into()),
        };
        let raw = Pid::from_raw(i32::try_from(pid)?);
        nix::sys::signal::kill(raw, Signal::SIGSTOP)?;
        let sent: Vec<u8> = (b'a'..=b'j').cycle().take(3000).collect();
        let refused = sent
            .iter()
            .filter(|key| terminal.input(std::slice::from_ref(*key)).is_err())
            .count();
        nix::sys::signal::kill(raw, Signal::SIGCONT)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut received = Vec::new();
        while std::time::Instant::now() < deadline {
            received = std::fs::read(&got).unwrap_or_default();
            if received.len() >= sent.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = terminal.stop();
        std::fs::remove_dir_all(&directory)?;
        assert_eq!(refused, 0);
        assert_eq!(received, sent);
        Ok(())
    }

    #[test]
    fn replies_remain_byte_exact_nonblocking_and_bounded() -> Outcome {
        let mut parser = fux_vt::Parser::new(2, 2, 0)?;
        // Room for exactly the three replies below (17 bytes); the bound is
        // on queued cost, released as the writer passes each reply on.
        let (queue, rx) = InputQueue::new(3 * ENTRY_COST + 17);
        process_output(&mut parser, Some(&queue), b"AB\x1b[5n\x1b[6n\x1b[c")?;
        for expected in [&b"\x1b[0n"[..], b"\x1b[1;3R", b"\x1b[?1;2c"] {
            let reply = rx.try_recv()?;
            assert_eq!(reply, expected);
            queue.written(reply.len());
        }
        assert!(process_output(&mut parser, Some(&queue), &b"\x1b[5n".repeat(1000)).is_err());
        assert_eq!(rx.len(), 3);
        rx.close();
        assert!(
            process_output(&mut parser, Some(&queue), b"\x1b[5n")
                .err()
                .need()?
                .starts_with("terminal reply:")
        );
        process_output(&mut parser, None, b"\x1b[5n")?;
        Ok(())
    }

    #[test]
    fn backing_pty_creation_and_resize_are_exact_and_bad_sizes_roll_back() -> Outcome {
        let mut app = App::new();
        app.add_plugins(bevy_app::TaskPoolPlugin::default());
        let notify = Notify {
            wake: Wake(thread::current()),
            pending: Arc::default(),
        };
        let recipe = Launch {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
            cwd: String::new(),
            history_lines: 4,
        };
        let mut terminal = Terminal::spawn(&recipe, 1, 1, notify.clone())?;
        for (rows, cols) in [(1, 1), (1, 12), (12, 1), (1, 1)] {
            terminal.resize(rows, cols)?;
            let Runtime::Live(live) = &terminal.runtime else {
                return Err("child not running".into());
            };
            let size = live.job.master.lock().as_ref().need()?.get_size()?;
            assert_eq!((size.rows, size.cols), (rows, cols));
            assert_eq!(terminal.screen().size(), (rows, cols));
            terminal.parser.process("\x1bc界ABCD".as_bytes())?;
        }
        terminal.resize(2, 5)?;
        terminal.parser.process(b"\x1bcabcdefgh\r\nlast")?;
        terminal.resize(2, 10)?;
        assert_eq!(terminal.copy_text(1)?, "abcdefgh");
        terminal.resize(3, 10)?;
        terminal.parser.process(b"\x1bcHELLO")?;
        assert_eq!(terminal.copy_text(0)?, "HELLO");
        terminal.resize(1, 1)?;
        assert!(terminal.resize(0, 1).is_err());
        assert!(terminal.resize(u16::MAX, u16::MAX).is_err());
        assert_eq!(terminal.screen().size(), (1, 1));
        let Runtime::Live(live) = &terminal.runtime else {
            return Err("child not running".into());
        };
        let size = live.job.master.lock().as_ref().need()?.get_size()?;
        assert_eq!((size.rows, size.cols), (1, 1));
        terminal.stop()?;
        assert!(Terminal::spawn(&recipe, 0, 1, notify.clone()).is_err());
        assert!(Terminal::spawn(&recipe, u16::MAX, u16::MAX, notify).is_err());
        Ok(())
    }

    #[test]
    fn recipe_replacement_reinsertion_and_despawn_preserve_process_ownership()
    -> crate::testing::Outcome {
        let mut app = App::new();
        app.insert_resource(Wake(thread::current()))
            .add_plugins((bevy_app::TaskPoolPlugin::default(), TerminalPlugin));
        let recipe = || Launch {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
            cwd: String::new(),
            history_lines: 20,
        };
        let entity = app.world_mut().spawn(recipe()).id();
        app.update();
        let pid = |app: &App| {
            app.world()
                .get::<ProcessState>(entity)
                .and_then(|state| match state.status {
                    Status::Running { pid, .. } => Some(pid),
                    _ => None,
                })
                .need()
        };
        let reaped =
            |pid| nix::sys::signal::kill(Pid::from_raw(pid as i32), None) == Err(Errno::ESRCH);
        let first = pid(&app)?;
        app.world_mut().entity_mut(entity).insert(recipe());
        app.update();
        assert_eq!(pid(&app)?, first);
        app.world_mut()
            .entity_mut(entity)
            .remove::<Launch>()
            .insert(recipe());
        app.update();
        let second = pid(&app)?;
        assert_ne!(first, second);
        assert!(reaped(first));
        app.world_mut().despawn(entity);
        app.update();
        assert!(reaped(second));
        Ok(())
    }
}
