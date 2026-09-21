use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Read, Write},
    os::fd::BorrowedFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    sys::signal::{Signal, killpg},
    unistd::{Pid, dup},
};
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::model::{Launch, ProcessState, Status, Wake};

const CHUNK: usize = 8192;
const OUTPUT_SLOTS: usize = 16;
const INPUT_SLOTS: usize = 16;
/// One write must fit the largest accepted paste plus its bracketed envelope,
/// so a paste the policy layer accepts is never dropped by the transport.
const MAX_INPUT: usize = crate::paste::LIMIT + crate::paste::ENVELOPE;
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
    parser: vt100::Parser<Replies>,
    runtime: Runtime,
    /// The reader outlives the process: it drains output queued in the PTY
    /// after the group is killed, then ends at EOF.
    reader: Option<Task<()>>,
    output: Receiver<Output>,
    notify: Notify,
    published_size: (u16, u16),
    status: Status,
    revision: u64,
    snapshot: Option<(u64, usize, u16, Vec<String>)>,
}

/// Everything that exists only while the child runs. Input after exit is a
/// type error here, not a runtime check spread over optional fields.
enum Runtime {
    Live(Live),
    Stopped,
}
struct Live {
    job: Job,
    input: Sender<Vec<u8>>,
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
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Input is accepted atomically into a bounded queue, never partially queued.
    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_INPUT {
            return Err(format!(
                "input exceeds {MAX_INPUT} bytes; send smaller chunks"
            ));
        }
        match &self.runtime {
            Runtime::Live(live) => live
                .input
                .try_send(bytes.to_vec())
                .map_err(|e| e.to_string()),
            Runtime::Stopped => Err("process has exited".into()),
        }
    }

    /// The temporary history offset is always reset before accepting more output.
    pub fn snapshot(
        &mut self,
        scrollback: usize,
        visible_cols: u16,
    ) -> (&[String], &vt100::Screen) {
        let visible_cols = visible_cols.min(self.parser.screen().size().1);
        if self
            .snapshot
            .as_ref()
            .is_none_or(|(revision, offset, cols, _)| {
                *revision != self.revision || *offset != scrollback || *cols != visible_cols
            })
        {
            let screen = self.parser.screen_mut();
            screen.set_scrollback(scrollback);
            let (rows, cols) = screen.size();
            let cols = cols.min(visible_cols);
            let mut lines = Vec::with_capacity(usize::from(rows));
            // rows_formatted() carries wrapping state between rows and can emit CR/LF,
            // cursor moves and erases. Emit cells + SGR only: every line is relocatable.
            for row in 0..rows {
                let mut line = String::with_capacity(usize::from(cols) + 16);
                line.push_str("\x1b[0m");
                let mut previous = None;
                for col in 0..cols {
                    let Some(cell) = screen.cell(row, col) else {
                        continue;
                    };
                    if cell.is_wide_continuation() || (cell.is_wide() && col + 1 >= cols) {
                        continue;
                    }
                    let style = Style::of(cell);
                    if previous != Some(style) {
                        style.write(&mut line);
                        previous = Some(style);
                    }
                    if cell.has_contents() {
                        line.push_str(cell.contents());
                    } else {
                        line.push(' ');
                    }
                }
                line.push_str("\x1b[0m");
                lines.push(line);
            }
            screen.set_scrollback(0);
            self.snapshot = Some((self.revision, scrollback, visible_cols, lines));
        }
        let lines = self
            .snapshot
            .as_ref()
            .map_or(&[][..], |(_, _, _, lines)| lines.as_slice());
        (lines, self.parser.screen())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn selection_grid(&mut self, scrollback: usize) -> Result<crate::selection::Grid, String> {
        crate::selection::Grid::capture(self.parser.screen_mut(), scrollback)
    }

    /// The history offset the emulator can actually show for a request, so a
    /// viewer never accumulates an offset past the oldest retained line.
    pub fn clamp_scrollback(&mut self, scrollback: usize) -> usize {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(scrollback);
        let actual = screen.scrollback();
        screen.set_scrollback(0);
        actual
    }

    pub fn copy_text(&mut self, scrollback: usize) -> String {
        let screen = self.parser.screen_mut();
        screen.set_scrollback(scrollback);
        let text = screen.contents();
        screen.set_scrollback(0);
        text
    }

    fn spawn(launch: &Launch, rows: u16, cols: u16, notify: Notify) -> Result<Self, String> {
        if rows == 0 || cols == 0 {
            return Err("terminal dimensions must be nonzero".into());
        }
        let (rows, cols) = (rows.max(2), cols.max(2));
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
        let (input, input_rx) = async_channel::bounded::<Vec<u8>>(INPUT_SLOTS);
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
            }
        });
        Ok(Self {
            parser: vt100::Parser::new_with_callbacks(
                rows,
                cols,
                launch.history_lines,
                Replies {
                    input: input.clone(),
                    error: None,
                },
            ),
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
            notify,
            published_size: (rows, cols),
            snapshot: None,
        })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        if rows == 0 || cols == 0 {
            return Err("terminal dimensions must be nonzero".into());
        }
        // vt100 0.16.2 underflows on one-row wrapping and wide glyphs in one column.
        // Keep its backing PTY at least 2×2; the painter clips to actual viewer cells.
        let (rows, cols) = (rows.max(2), cols.max(2));
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
        self.parser.screen_mut().set_size(rows, cols);
        self.revision = self.revision.wrapping_add(1);
        self.notify.send();
        Ok(())
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
        input.close();
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
                    terminal.parser.process(&bytes);
                }
                Output::Error(error) => terminal.fault(error),
                Output::Eof => {
                    terminal.reader.take();
                }
            }
            terminal.revision = terminal.revision.wrapping_add(1);
        }
        remaining_output |= !terminal.output.is_empty();
        if let Some(error) = terminal.parser.callbacks_mut().error.take() {
            terminal.fault(error);
        }
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
        let (rows, cols) = terminal.parser.screen().size();
        terminal.published_size = (rows, cols);
        let Some(state) = &mut state else { continue };
        state.set_if_neq(ProcessState {
            rows,
            cols,
            status: terminal.status.clone(),
            revision: terminal.revision,
        });
    }
    if remaining_output {
        notify.wake.notify();
    }
}

struct Replies {
    input: Sender<Vec<u8>>,
    error: Option<String>,
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i1.is_some() || i2.is_some() {
            return;
        }
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (c, first) {
            ('n', 5) => b"\x1b[0n".to_vec(),
            ('n', 6) => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes()
            }
            ('c', 0) => b"\x1b[?1;2c".to_vec(),
            _ => return,
        };
        if let Err(error) = self.input.try_send(reply) {
            self.error = Some(format!("terminal reply: {error}"));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    foreground: vt100::Color,
    background: vt100::Color,
    flags: u8,
}

impl Style {
    fn of(cell: &vt100::Cell) -> Self {
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
                vt100::Color::Default => {}
                vt100::Color::Idx(index) => {
                    let _ = write!(output, ";{selector};5;{index}");
                }
                vt100::Color::Rgb(r, g, b) => {
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
