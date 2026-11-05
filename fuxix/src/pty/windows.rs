//! Pseudo consoles on Windows, and starting a program on one.
//!
//! [`open`] makes a pseudo console with the ConPTY [`super::load`] chose,
//! over two of [`crate::io::pipe_for_child`]'s pipes, whose console ends
//! fuxix closes at once: the [`Master`] keeps the program's output and
//! input as two [`Stream`]s on the process's completion port, so a poll
//! waits on it and [`crate::io::read`] and [`crate::io::write`] use it as
//! on Unix. [`Slave::spawn`] starts one program on it, in a job object of
//! its own ([`Job`]).
//!
//! **The end of a program and of its output:** the output's pipe breaks
//! (the master is told `HUP`) once no process is left on the console,
//! 1 to 5 ms after the program's end if it left none; everything it wrote
//! has been read by then. A process it left on the console holds the pipe,
//! as a job left on a Unix PTY holds it. Only after the program's end is
//! its exit code read ([`Job::ended`]). That is with a ConPTY that has
//! `ReleasePseudoConsole` (the bundled one has); with the built-in one of
//! older Windows 11, which has not, the pipe breaks only once the console
//! is closed, so its user watches the program's process too
//! ([`crate::process::watch`]) and hangs up after it ended.
//!
//! **Signals are keys:** Ctrl-C is the byte `0x03` written to the master,
//! Ctrl-Break is [`BREAK`]; the hang-up is [`Master::hang_up`]. The
//! console's processes get them as a console's do.
use super::{Error, at, conpty};
use crate::errno::{Errno, Result as SysResult};
use crate::io::Stream;
use crate::poll::{Events, Source, sealed::Sealed};
use crate::process::{Dispositions, Job, Pid};
use crate::terminal::Size;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{COORD, HPCON};

/// Ctrl-Break, as the pseudo console's win32-input-mode (`ESC[?9001h`,
/// which every ConPTY turns on) takes it: Ctrl down, Break down and up,
/// Ctrl up (spike S4). It reaches every process on the console that is
/// not ignoring it, new process groups included.
pub const BREAK: &[u8] =
    b"\x1b[17;29;0;1;8;1_\x1b[3;70;0;1;264;1_\x1b[3;70;0;0;264;1_\x1b[17;29;0;0;0;1_";

/// How long a hang-up waits for the console's output to drain into the
/// master before it closes the console anyway: closing with output unsent
/// blocks the close for seconds on older ConPTYs and drops it on newer
/// ones. Bounded, so that a program that keeps writing can't hold the
/// close back; well under the 100 ms grace fux gives a hung-up program
/// (D-136), which runs from the hang-up, not from the close.
const DRAIN: Duration = Duration::from_millis(50);

/// A pseudo console, closed once: by [`Master::hang_up`], or as the last
/// of the master and slave drops. Never on the caller's thread, where a
/// close that waits for its output to be read would wait for itself.
struct Console {
    pseudo: Mutex<Option<HPCON>>,
    api: &'static conpty::Api,
}

impl Console {
    fn pseudo(&self) -> MutexGuard<'_, Option<HPCON>> {
        self.pseudo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Closes it on a short-lived thread, first waiting up to [`DRAIN`]
    /// for `output` (the master's end, if it is still read) to be empty.
    /// If no thread can be started, it stays open and the error is told.
    fn close(&self, output: Option<OwnedHandle>) -> SysResult<()> {
        // Taken out first: a resize, under the same lock, finds it gone.
        let Some(console) = self.pseudo().take() else {
            return Ok(());
        };
        let close = self.api.close;
        let spawned = std::thread::Builder::new()
            .name("fuxix-conpty-close".into())
            .spawn(move || {
                if let Some(output) = output {
                    drain(&output);
                }
                // SAFETY: the console is open, and closed only here, once:
                // its slot was emptied before this thread started.
                unsafe { close(console) };
            });
        if let Err(error) = spawned {
            *self.pseudo() = Some(console);
            return Err(Errno::from_io_error(&error).unwrap_or_else(Errno::last));
        }
        Ok(())
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if self.close(None).is_err()
            && let Some(console) = self.pseudo().take()
        {
            // No thread to close it on: closed here, which is safe now, as
            // the master, and so its reader, is gone (see `Master`).
            // SAFETY: the console is open, and its slot is empty now.
            unsafe { (self.api.close)(console) };
        }
    }
}

/// Waits until nothing waits in the pipe `output` to be read, twice in a
/// row a millisecond apart, or [`DRAIN`] has passed.
fn drain(output: &OwnedHandle) {
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let start = Instant::now();
    let mut empty = 0u32;
    while empty < 2 && start.elapsed() < DRAIN {
        let mut waiting = 0u32;
        // SAFETY: the handle is open, this thread's own copy; only the
        // count is written.
        let peeked = unsafe {
            PeekNamedPipe(
                output.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut waiting,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            return;
        }
        empty = match waiting {
            0 => empty.saturating_add(1),
            _ => 0,
        };
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A pseudo console's master: its program's output to read and its input
/// to write, as a [`Source`] to poll and with [`crate::io`] or as a
/// `std::io` reader and writer, and its size.
///
/// Dropped, it drops the output's reader first, then the input's writer,
/// then the console, which closes on a thread of its own (if no slave
/// holds it): with nobody reading, the console's writes fail, so its close
/// can't wait on them.
pub struct Master {
    output: Stream,
    input: Stream,
    console: Arc<Console>,
    size: Mutex<Size>,
}

impl std::fmt::Debug for Master {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Master").finish_non_exhaustive()
    }
}

/// A pseudo console's slave: the right to start one program on it
/// ([`Slave::spawn`]).
pub struct Slave {
    console: Arc<Console>,
}

impl std::fmt::Debug for Slave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slave").finish_non_exhaustive()
    }
}

impl Master {
    /// Hangs the pseudo console up: closes it, as Unix's last close of a
    /// master hangs a PTY up, so its programs get `CTRL_CLOSE_EVENT` (a
    /// handler-less one exits `0xC000013A`), and the output's pipe breaks
    /// once they are gone. Keep reading the master meanwhile: the close
    /// runs on a short-lived thread, after the output drained or
    /// [`DRAIN`] passed, and the output goes on to its `HUP`. A second
    /// hang-up does nothing; one that can't start its thread fails, and
    /// the console stays open.
    pub fn hang_up(&self) -> SysResult<()> {
        let output = self
            .output
            .as_handle()
            .try_clone_to_owned()
            .map_err(|error| Errno::from_io_error(&error).unwrap_or_else(Errno::last))?;
        self.console.close(Some(output))
    }

    /// The size it was last set to.
    pub(crate) fn size(&self) -> Size {
        *self.size.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Resizes the pseudo console; one hung up keeps its size.
    pub(crate) fn resize(&self, size: Size) -> SysResult<()> {
        let coord = coord(size)?;
        let pseudo = self.console.pseudo();
        if let Some(console) = *pseudo {
            // SAFETY: the console is open while its slot holds it, and
            // the lock held keeps it so.
            let result = unsafe { (self.console.api.resize)(console, coord) };
            hresult(result)?;
        }
        drop(pseudo);
        *self.size.lock().unwrap_or_else(PoisonError::into_inner) = size;
        Ok(())
    }
}

/// `size` as a console takes it: at most 32767 by 32767.
fn coord(size: Size) -> SysResult<COORD> {
    let rows = i16::try_from(size.rows.get()).map_err(|_| Errno::INVAL)?;
    let cols = i16::try_from(size.cols.get()).map_err(|_| Errno::INVAL)?;
    Ok(COORD { X: cols, Y: rows })
}

/// A failed `HRESULT` as the error it carries.
fn hresult(result: i32) -> SysResult<()> {
    match result {
        0.. => Ok(()),
        _ => Err(Errno::from_code(result.cast_unsigned() & 0xFFFF)),
    }
}

impl Sealed for Master {
    fn arm(&self, interest: Events) {
        self.output.arm(interest);
    }

    /// The output's `IN`, `HUP` and `ERR`; the input's `OUT` and `ERR`.
    fn ready(&self, interest: Events) -> Events {
        let wants = |events: Events| match interest.intersects(events) {
            true => events,
            false => Events::NONE,
        };
        self.output.ready(wants(Events::IN)) | self.input.ready(wants(Events::OUT))
    }

    fn reading(&self) -> Option<&Stream> {
        Some(&self.output)
    }

    fn writing(&self) -> Option<&Stream> {
        Some(&self.input)
    }
}

impl Source for Master {}

/// Read as `std::io` reads: with `AGAIN` as `WouldBlock`.
impl std::io::Read for &Master {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        crate::io::read(*self, buffer).map_err(Into::into)
    }
}

impl std::io::Read for Master {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        (&*self).read(buffer)
    }
}

/// Written as `std::io` writes, as it reads.
impl std::io::Write for &Master {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        crate::io::write(*self, bytes).map_err(Into::into)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::io::Write for Master {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (&*self).write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Opens a pseudo console of `size`: its master and its slave. Fails with
/// an error naming `load` before [`super::load`] chose a ConPTY. Its flags
/// are none: `PSEUDOCONSOLE_INHERIT_CURSOR` would stall the program
/// waiting for a cursor answer (spikes S1, S4).
pub fn open(size: Size) -> Result<(Master, Slave), Error> {
    let api = conpty::api()?;
    let coord = coord(size).map_err(at("pseudo console size"))?;
    let (output, their_output) = crate::io::pipe_for_child().map_err(at("output pipe"))?;
    let (input, their_input) = crate::io::pipe_for_child().map_err(at("input pipe"))?;
    let mut console: HPCON = 0;
    // SAFETY: both handles are open, and the console takes copies of its
    // own; `console` is written on success.
    let made = unsafe {
        (api.create)(
            coord,
            their_input.as_raw_handle(),
            their_output.as_raw_handle(),
            0,
            &mut console,
        )
    };
    hresult(made).map_err(at("CreatePseudoConsole"))?;
    // The console holds its own copies: ours, held, would keep the output
    // from ever breaking.
    drop((their_input, their_output));
    let console = Arc::new(Console {
        pseudo: Mutex::new(Some(console)),
        api,
    });
    let master = Master {
        output,
        input,
        console: Arc::clone(&console),
        size: Mutex::new(size),
    };
    Ok((master, Slave { console }))
}

impl Slave {
    /// Starts `argv` on this pseudo console, in a job object of its own
    /// from its first instruction, as its leader ([`Job`]); `setup` sets
    /// its directory and environment, and Ctrl-C reaches it as
    /// `dispositions` says. Returns once it runs, or with the system's
    /// error.
    ///
    /// The `Command` `setup` is given starts with this process's whole
    /// environment set explicitly, so an `env_clear` in `setup` is obeyed
    /// as on Unix; its program, arguments, environment and directory are
    /// read back from it, and nothing else of it is used. The program is
    /// looked for as `execvp` looks after a `chdir`: a path against the
    /// directory `setup` set, a bare name in each folder of the program's
    /// own `PATH` (as it is, then with `.exe`), and never in this
    /// process's current directory. A batch file is refused: run it with
    /// `cmd /c`. Its arguments are quoted as the C runtime reads them.
    ///
    /// With [`Dispositions::Default`], this process's "ignore Ctrl-C"
    /// (which a process started detached or in a new process group has)
    /// is cleared first, so the program starts with it clear; with
    /// `Inherited`, it starts with this process's.
    pub fn spawn<S: AsRef<OsStr>>(
        self,
        argv: &[S],
        setup: impl FnOnce(&mut Command),
        dispositions: Dispositions,
    ) -> std::io::Result<Job> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("no program to run"))?;
        let mut command = Command::new(program);
        command.env_clear().envs(std::env::vars_os()).args(args);
        setup(&mut command);
        let environment: Vec<(OsString, OsString)> = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_owned(), value?.to_owned())))
            .collect();
        let directory = command
            .get_current_dir()
            .map(std::path::absolute)
            .transpose()?;
        let path = resolve(command.get_program(), &environment, directory.as_deref())?;
        let mut line = Vec::new();
        for arg in std::iter::once(command.get_program()).chain(command.get_args()) {
            quote(arg, &mut line)?;
        }
        line.push(0);
        let block = environment_block(environment)?;
        let directory = directory.map(|d| wide(d.as_os_str())).transpose()?;
        if dispositions == Dispositions::Default {
            crate::signal::heed_ctrl_c()?;
        }
        // Held through the start and the release, as `resize` holds it: a
        // hang-up meanwhile waits, and finds a console no process uses.
        let pseudo = self.console.pseudo();
        let console = (*pseudo).ok_or_else(|| std::io::Error::other("hung up"))?;
        let job = crate::process::job_object()?;
        let start = Start {
            path: wide(path.as_os_str())?,
            line,
            block,
            directory,
            console,
        };
        let (process, thread) = match start.create(Some(&job), false) {
            Ok(created) => created,
            // Started in its job by attribute where the system takes it;
            // else suspended, assigned, resumed (as spike S4 ran).
            Err(_) => start.create_assigned(&job)?,
        };
        drop(thread);
        if let Some(release) = self.console.api.release {
            // SAFETY: the console is open while its slot holds it, and the
            // lock held keeps it so; this only lets its output break once
            // the program's console is empty.
            let _ = unsafe { release(console) };
        }
        drop(pseudo);
        let leader = Pid::from_raw(process_id(&process)?)
            .ok_or_else(|| std::io::Error::other("a process with no pid"))?;
        Ok(Job::new(process, job, leader))
    }
}

/// The process `process`'s pid.
fn process_id(process: &OwnedHandle) -> std::io::Result<u32> {
    use windows_sys::Win32::System::Threading::GetProcessId;
    // SAFETY: the handle is open; it touches no memory.
    match unsafe { GetProcessId(process.as_raw_handle()) } {
        0 => Err(std::io::Error::last_os_error()),
        pid => Ok(pid),
    }
}

/// One `CreateProcessW`'s inputs, as the system takes them.
struct Start {
    path: Vec<u16>,
    /// Written to by `CreateProcessW`, so kept per call.
    line: Vec<u16>,
    block: Vec<u16>,
    directory: Option<Vec<u16>>,
    console: HPCON,
}

impl Start {
    /// Creates the process on the console, in `job` by attribute if given,
    /// suspended if asked: its process and thread.
    fn create(
        &self,
        job: Option<&OwnedHandle>,
        suspended: bool,
    ) -> std::io::Result<(OwnedHandle, OwnedHandle)> {
        use windows_sys::Win32::System::Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            EXTENDED_STARTUPINFO_PRESENT, PROC_THREAD_ATTRIBUTE_JOB_LIST,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
            STARTUPINFOEXW, STARTUPINFOW,
        };
        let jobs: Vec<HANDLE> = job.iter().map(|job| job.as_raw_handle()).collect();
        let mut attributes = Attributes::new(1u32.saturating_add(u32::from(job.is_some())))?;
        // The console's value itself is the attribute's value.
        let console = std::ptr::without_provenance(self.console.cast_unsigned());
        attributes.set(
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            console,
            std::mem::size_of::<HPCON>(),
        )?;
        if !jobs.is_empty() {
            attributes.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                std::mem::size_of_val(jobs.as_slice()),
            )?;
        }
        let info = STARTUPINFOEXW {
            StartupInfo: STARTUPINFOW {
                cb: u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
                    .map_err(std::io::Error::other)?,
                dwFlags: STARTF_USESTDHANDLES,
                hStdInput: INVALID_HANDLE_VALUE,
                hStdOutput: INVALID_HANDLE_VALUE,
                hStdError: INVALID_HANDLE_VALUE,
                ..STARTUPINFOW::default()
            },
            lpAttributeList: attributes.list(),
        };
        let mut flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
        if suspended {
            flags |= CREATE_SUSPENDED;
        }
        let mut line = self.line.clone();
        let directory = self
            .directory
            .as_ref()
            .map_or(std::ptr::null(), |d| d.as_ptr());
        let mut created = PROCESS_INFORMATION::default();
        // SAFETY: every string is NUL-ended (the block double-NUL-ended)
        // and lives through the call; `line` is writable; the attribute
        // list and the values it points to (`jobs`, the console) live
        // through it too; no handle is inherited.
        let done = unsafe {
            CreateProcessW(
                self.path.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                self.block.as_ptr().cast(),
                directory,
                &info.StartupInfo,
                &mut created,
            )
        };
        if done == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both handles were just made for this process, and
        // nothing else owns them.
        let process = unsafe { OwnedHandle::from_raw_handle(created.hProcess) };
        // SAFETY: as above.
        let thread = unsafe { OwnedHandle::from_raw_handle(created.hThread) };
        Ok((process, thread))
    }

    /// Creates the process suspended, puts it in `job` and resumes it; one
    /// that can't be put there is ended before it ran.
    fn create_assigned(&self, job: &OwnedHandle) -> std::io::Result<(OwnedHandle, OwnedHandle)> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::{ResumeThread, TerminateProcess};
        let (process, thread) = self.create(None, true)?;
        // SAFETY: both handles are open; it touches no memory.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: the process is ours, and has not run.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
            return Err(error);
        }
        // SAFETY: the thread is the new process's, suspended once.
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            let error = std::io::Error::last_os_error();
            // SAFETY: as above.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
            return Err(error);
        }
        Ok((process, thread))
    }
}

/// A process's attribute list, in zeroed storage aligned for it, deleted
/// when dropped.
struct Attributes(Vec<usize>);

impl Attributes {
    fn new(count: u32) -> std::io::Result<Attributes> {
        use windows_sys::Win32::System::Threading::InitializeProcThreadAttributeList;
        let mut size = 0usize;
        // SAFETY: with no list, this only writes the size one needs.
        let sized =
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
        let error = std::io::Error::last_os_error();
        if sized != 0 || error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER.cast_signed()) {
            return Err(error);
        }
        let units = size.div_ceil(std::mem::size_of::<usize>());
        let mut list = Attributes(vec![0; units]);
        // SAFETY: the storage is writable for `size` bytes, zeroed and
        // aligned for pointers.
        if unsafe { InitializeProcThreadAttributeList(list.list(), count, 0, &mut size) } == 0 {
            let error = std::io::Error::last_os_error();
            // Not made: nothing to delete.
            list.0 = Vec::new();
            return Err(error);
        }
        Ok(list)
    }

    fn list(&mut self) -> *mut core::ffi::c_void {
        self.0.as_mut_ptr().cast()
    }

    /// Sets `attribute` to `value`, `size` bytes, which must live as long
    /// as the list is used.
    fn set(
        &mut self,
        attribute: u32,
        value: *const core::ffi::c_void,
        size: usize,
    ) -> std::io::Result<()> {
        use windows_sys::Win32::System::Threading::UpdateProcThreadAttribute;
        let attribute = usize::try_from(attribute).map_err(std::io::Error::other)?;
        let list = self.list();
        // SAFETY: the list was made for this many attributes; `value` is
        // the caller's, kept alive while the list is used.
        let set = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        match set {
            0 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        }
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList;
        if !self.0.is_empty() {
            let list = self.list();
            // SAFETY: the list was made by `new`, and is deleted once.
            unsafe { DeleteProcThreadAttributeList(list) };
        }
    }
}

/// `text` as a NUL-ended UTF-16 string; one with a NUL in it is refused.
fn wide(text: &OsStr) -> std::io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(nul());
    }
    units.push(0);
    Ok(units)
}

fn nul() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "a NUL in an argument")
}

/// Adds `arg` to a command line, after a space if it is not the first,
/// quoted as the C runtime reads it back: as it is if it has no space,
/// tab or quote and is not empty, else in quotes, with each quote and the
/// backslashes before it, and those before the closing quote, escaped.
fn quote(arg: &OsStr, line: &mut Vec<u16>) -> std::io::Result<()> {
    const QUOTE: u16 = 0x22;
    const BACKSLASH: u16 = 0x5C;
    let units: Vec<u16> = arg.encode_wide().collect();
    if units.contains(&0) {
        return Err(nul());
    }
    if !line.is_empty() {
        line.push(u16::from(b' '));
    }
    let plain = !units.is_empty()
        && !units
            .iter()
            .any(|unit| [u16::from(b' '), u16::from(b'\t'), QUOTE].contains(unit));
    if plain {
        line.extend(units);
        return Ok(());
    }
    line.push(QUOTE);
    let mut backslashes = 0usize;
    for unit in units {
        match unit {
            BACKSLASH => backslashes = backslashes.saturating_add(1),
            QUOTE => {
                let escaped = backslashes.saturating_mul(2).saturating_add(1);
                line.extend(std::iter::repeat_n(BACKSLASH, escaped));
                line.push(QUOTE);
                backslashes = 0;
            }
            _ => {
                line.extend(std::iter::repeat_n(BACKSLASH, backslashes));
                line.push(unit);
                backslashes = 0;
            }
        }
    }
    line.extend(std::iter::repeat_n(
        BACKSLASH,
        backslashes.saturating_mul(2),
    ));
    line.push(QUOTE);
    Ok(())
}

/// The environment as `CreateProcessW` takes it: `NAME=value` strings,
/// sorted by name ignoring ASCII case, each NUL-ended, and a NUL after.
fn environment_block(mut environment: Vec<(OsString, OsString)>) -> std::io::Result<Vec<u16>> {
    let upper = |key: &OsStr| -> Vec<u16> {
        key.encode_wide()
            .map(|unit| match u8::try_from(unit) {
                Ok(byte) => u16::from(byte.to_ascii_uppercase()),
                Err(_) => unit,
            })
            .collect()
    };
    environment.sort_by_cached_key(|(key, _)| upper(key));
    let mut block = Vec::new();
    for (key, value) in environment {
        let mut pair = key;
        pair.push("=");
        pair.push(value);
        block.extend(wide(&pair)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// The program `program` names, looked for as [`Slave::spawn`] says.
fn resolve(
    program: &OsStr,
    environment: &[(OsString, OsString)],
    directory: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let name = Path::new(program);
    let pathed = name.is_absolute() || name.components().count() > 1 || name.has_root();
    let candidates: Vec<PathBuf> = match pathed {
        true => vec![match directory {
            Some(directory) => directory.join(name),
            None => std::path::absolute(name)?,
        }],
        false => environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
            .map(|(_, path)| std::env::split_paths(path).collect::<Vec<PathBuf>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|folder: &PathBuf| folder.is_absolute())
            .map(|folder| folder.join(name))
            .collect(),
    };
    let found = candidates
        .into_iter()
        .flat_map(|candidate| {
            let exe = (candidate.extension().is_none()).then(|| candidate.with_extension("exe"));
            std::iter::once(candidate).chain(exe)
        })
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} not found", name.display()),
            )
        })?;
    let batch = found.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
    });
    match batch {
        true => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is a batch file: run it with cmd /c", found.display()),
        )),
        false => Ok(found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        let mut line = Vec::new();
        for arg in args {
            assert!(quote(OsStr::new(arg), &mut line).is_ok());
        }
        String::from_utf16_lossy(&line)
    }

    #[test]
    fn arguments_are_quoted_as_the_c_runtime_reads_them() {
        assert_eq!(line(&["a", "b c", ""]), r#"a "b c" """#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r"C:\a b\", r"x\y"]), r#""C:\a b\\" x\y"#);
        assert_eq!(line(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert!(quote(OsStr::new("a\0b"), &mut Vec::new()).is_err());
    }
}
