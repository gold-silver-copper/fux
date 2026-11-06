//! The processes this process starts or is handed, known by their pids as
//! Unix knows its children: each held by its handle, which keeps its pid
//! its own, and watched, so its end is told as `Chld`. Unix's `waitpid`,
//! `kill` and `prctl(PR_SET_CHILD_SUBREAPER)` have these for counterparts.
//!
//! - [`start`] starts a program detached, with no console and no window,
//!   handing it exactly the handles it is given as its standard ones
//!   (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so nothing else this process
//!   made inheritable meanwhile leaks into it; out of a job it is in when
//!   the job allows it, and never silently ([`Breakaway`], D-135).
//! - [`adopt`] takes a copy of a program's process handle from the
//!   process that started it, checked to be that program by its pid and
//!   creation time (D-134, D-138): a server's copy of a pane's program,
//!   kept so that the program is the server's to end and reap should the
//!   pane's process crash.
//! - [`reap`], [`ended`] and [`kill`] act on either; [`reap_orphans`]
//!   lets go of each one started that ended and is not kept, and
//!   [`release`] of one adopted.
use super::{Identity, Offer, Pid, Status, Watch, terminate, watch};
use crate::errno::{Errno, Result};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// A process known by its pid: its handle, its watch, and whether this
/// process started it (else it was adopted).
struct Known {
    process: OwnedHandle,
    _watch: Watch,
    started: bool,
}

static KNOWN: Mutex<BTreeMap<Pid, Known>> = Mutex::new(BTreeMap::new());

/// This process, as its pseudo-handle names it, which needs no closing.
fn this() -> HANDLE {
    // SAFETY: it takes nothing and touches no memory.
    unsafe { GetCurrentProcess() }
}

fn known() -> MutexGuard<'static, BTreeMap<Pid, Known>> {
    KNOWN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a program [`start`] started left the job this process is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakaway {
    /// This process is in no job: there was nothing to leave.
    Free,
    /// It left this process's job.
    Left,
    /// The job refused it (no breakaway allowed): it runs in this
    /// process's job, and ends as that job's processes do.
    Refused,
}

/// What [`start`] started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub pid: Pid,
    pub breakaway: Breakaway,
}

/// Starts `program` with `args`, detached (no console, no window) and in
/// a process group of its own, its standard input, output and error the
/// handles `stdio` gives (none: an invalid handle), which are the only
/// handles it inherits. Out of this process's job when the job allows,
/// else in it, as [`Started::breakaway`] says. Known by its pid from then
/// on ([`reap`]), its end told as `Chld`.
pub fn start(
    program: &Path,
    args: &[&OsStr],
    stdio: [Option<BorrowedHandle<'_>>; 3],
) -> std::io::Result<Started> {
    start_in(program, args, stdio, None, &[])
}

/// As [`start`], in `directory` if one is given, this process's own
/// otherwise, with this process's environment but for `unset`.
pub fn start_in(
    program: &Path,
    args: &[&OsStr],
    stdio: [Option<BorrowedHandle<'_>>; 3],
    directory: Option<&Path>,
    unset: &[&str],
) -> std::io::Result<Started> {
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    let place = Place {
        directory: directory
            .map(|directory| crate::pty::wide(directory.as_os_str()))
            .transpose()?,
        environment: (!unset.is_empty()).then(|| environment_without(unset)),
    };
    let mut line = Vec::new();
    for arg in std::iter::once(program.as_os_str()).chain(args.iter().copied()) {
        crate::pty::quote(arg, &mut line)?;
    }
    line.push(0);
    let path = crate::pty::wide(program.as_os_str())?;
    // Inheritable copies, made for this start alone and closed after it.
    let copies: Vec<Option<OwnedHandle>> = (stdio.iter())
        .map(|handle| handle.map(inheritable).transpose())
        .collect::<std::io::Result<_>>()?;
    let mut inside = 0;
    // SAFETY: the pseudo-handle is this process; a null job asks about
    // any; `inside` is writable.
    if unsafe { IsProcessInJob(this(), std::ptr::null_mut(), &mut inside) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (process, breakaway) = match create(&path, &line, &copies, &place, inside != 0) {
        Ok(process) if inside != 0 => (process, Breakaway::Left),
        Ok(process) => (process, Breakaway::Free),
        Err(error) if inside != 0 && denied(&error) => (
            create(&path, &line, &copies, &place, false)?,
            Breakaway::Refused,
        ),
        Err(error) => return Err(error),
    };
    drop(copies);
    let pid = remember(process, true)?;
    Ok(Started { pid, breakaway })
}

/// Where a process starts: its directory, NUL-ended, and its environment
/// block; none, this process's.
struct Place {
    directory: Option<Vec<u16>>,
    environment: Option<Vec<u16>>,
}

/// This process's environment as a block `CreateProcessW` takes, but for
/// `unset`, whose names are matched as Windows matches them, in any case.
fn environment_without(unset: &[&str]) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    let mut block = Vec::new();
    for (name, value) in std::env::vars_os() {
        if unset.iter().any(|gone| name.eq_ignore_ascii_case(gone)) {
            continue;
        }
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    // Ended by an empty string: an empty block has two NULs too.
    block.push(0);
    block.push(0);
    block
}

fn denied(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_ACCESS_DENIED.cast_signed())
}

/// An inheritable copy of `handle`.
fn inheritable(handle: BorrowedHandle<'_>) -> std::io::Result<OwnedHandle> {
    let mut copy: HANDLE = std::ptr::null_mut();
    // SAFETY: both process handles are this process's pseudo-handle, the
    // source handle is open; `copy` is written once.
    let done = unsafe {
        DuplicateHandle(
            this(),
            handle.as_raw_handle(),
            this(),
            &mut copy,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if done == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `copy` was just made, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(copy) })
}

/// One `CreateProcessW`: `path` with the command line `line` (NUL-ended),
/// its standard handles `stdio`, which it alone inherits, at `place`, out
/// of this process's job if `breakaway`. Its process handle; its thread's
/// is closed at once.
fn create(
    path: &[u16],
    line: &[u16],
    stdio: &[Option<OwnedHandle>],
    place: &Place,
    breakaway: bool,
) -> std::io::Result<OwnedHandle> {
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT,
        CreateProcessW, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW,
    };
    let raw = |at: usize| {
        (stdio.get(at).and_then(Option::as_ref)).map_or(INVALID_HANDLE_VALUE, |h| h.as_raw_handle())
    };
    let mut handed: Vec<HANDLE> = (stdio.iter().flatten())
        .map(|h| h.as_raw_handle())
        .collect();
    handed.dedup();
    let mut attributes = crate::pty::Attributes::new(1)?;
    if !handed.is_empty() {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            handed.as_ptr().cast(),
            std::mem::size_of_val(handed.as_slice()),
        )?;
    }
    let info = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
                .map_err(std::io::Error::other)?,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: raw(0),
            hStdOutput: raw(1),
            hStdError: raw(2),
            ..STARTUPINFOW::default()
        },
        lpAttributeList: attributes.list(),
    };
    let mut flags = EXTENDED_STARTUPINFO_PRESENT
        | DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | CREATE_UNICODE_ENVIRONMENT;
    if breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    let environment = (place.environment.as_ref()).map_or(std::ptr::null(), |block| block.as_ptr());
    let directory = (place.directory.as_ref()).map_or(std::ptr::null(), |wide| wide.as_ptr());
    let mut line = line.to_vec();
    let mut created = PROCESS_INFORMATION::default();
    // SAFETY: `path` and `line` are NUL-ended and live through the call,
    // `line` writable; the attribute list and the handles it lists live
    // through it too; only listed handles are inherited.
    let done = unsafe {
        CreateProcessW(
            path.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(!handed.is_empty()),
            flags,
            environment.cast(),
            directory,
            &info.StartupInfo,
            &mut created,
        )
    };
    if done == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both handles were just made for this process, and nothing
    // else owns them.
    drop(unsafe { OwnedHandle::from_raw_handle(created.hThread) });
    // SAFETY: as above.
    Ok(unsafe { OwnedHandle::from_raw_handle(created.hProcess) })
}

/// Ends this process now with `code`, as `exec`'s program ending would.
pub fn end(code: i32) -> ! {
    use windows_sys::Win32::System::Threading::ExitProcess;
    // SAFETY: it takes a number, and doesn't return.
    unsafe { ExitProcess(code.cast_unsigned()) }
}

/// The path of the program process `pid` runs, as the system says.
pub fn image(pid: Pid) -> std::io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt as _;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    // SAFETY: it takes numbers and touches no memory.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_raw()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut name = vec![0u16; 32 * 1024];
    let mut length = u32::try_from(name.len()).unwrap_or(0);
    // SAFETY: the process is open; `name` is writable for `length` units.
    let asked = unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            name.as_mut_ptr(),
            &mut length,
        )
    };
    if asked == 0 {
        return Err(std::io::Error::last_os_error());
    }
    name.truncate(usize::try_from(length).unwrap_or(0));
    Ok(std::ffi::OsString::from_wide(&name).into())
}

/// This process and its ancestors, nearest first, as the system lists them.
/// A pid is reused once its process is gone: a parent made after its child
/// is no parent, and the chain ends there.
pub fn ancestry() -> Vec<u32> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let mut chain = vec![std::process::id()];
    // SAFETY: it takes numbers and touches no memory.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return chain;
    }
    // SAFETY: `raw` was just made, and nothing else owns it.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut parents = BTreeMap::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..PROCESSENTRY32W::default()
    };
    // SAFETY: the snapshot is open, and `entry` is writable, its size set.
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    let born = |pid: u32| {
        Pid::from_raw(pid)
            .and_then(|pid| identity(pid).ok())
            .map(|i| i.created)
    };
    let mut child = std::process::id();
    while let Some(parent) = (parents.get(&child).copied()).filter(|parent| *parent != 0) {
        let older = matches!((born(parent), born(child)), (Some(a), Some(b)) if a <= b);
        if !older || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        child = parent;
    }
    chain
}

/// `process` known by its pid from now on, watched.
fn remember(process: OwnedHandle, started: bool) -> std::io::Result<Pid> {
    let pid = super::identity_of(&process)?.pid;
    let watch = watch(&process)?;
    let entry = Known {
        process,
        _watch: watch,
        started,
    };
    known().insert(pid, entry);
    Ok(pid)
}

/// Takes a copy of the program's handle `offer` names in the process
/// `from` (a child [`start`] started, or one opened by its pid, as a
/// server taking a run back opens a pane's), if the copy is that program:
/// its pid and creation time are the offer's. A handle value since reused
/// for another process, or for anything else, is refused with
/// [`Errno::SRCH`]. The program is known by its pid from then on, till
/// [`reap`] or [`release`].
pub fn adopt(from: Pid, offer: Offer) -> Result<()> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };
    let opened;
    let held = known();
    let source = match held.get(&from) {
        Some(known) => known.process.as_raw_handle(),
        None => {
            // SAFETY: it takes numbers and touches no memory.
            let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, from.as_raw()) };
            if raw.is_null() {
                return Err(Errno::last());
            }
            // SAFETY: `raw` was just opened, and nothing else owns it.
            opened = unsafe { OwnedHandle::from_raw_handle(raw) };
            opened.as_raw_handle()
        }
    };
    let access = PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
    let value: HANDLE =
        std::ptr::without_provenance_mut(usize::try_from(offer.handle).map_err(|_| Errno::INVAL)?);
    let mut copy: HANDLE = std::ptr::null_mut();
    // SAFETY: `source` is open with `PROCESS_DUP_HANDLE`; `value` is only a
    // number in it, which the system checks; `copy` is written once.
    let done = unsafe { DuplicateHandle(source, value, this(), &mut copy, access, 0, 0) };
    drop(held);
    if done == 0 {
        return Err(Errno::SRCH);
    }
    // SAFETY: `copy` was just made, and nothing else owns it.
    let copy = unsafe { OwnedHandle::from_raw_handle(copy) };
    if super::identity_of(&copy).ok() != Some(offer.program) {
        return Err(Errno::SRCH);
    }
    let watch = watch(&copy)?;
    let entry = Known {
        process: copy,
        _watch: watch,
        started: false,
    };
    known().insert(offer.program.pid, entry);
    Ok(())
}

/// Lets go of `pid`, a program [`adopt`]ed: its handle is closed, and it
/// is known no more.
pub fn release(pid: Pid) {
    let mut held = known();
    if held.get(&pid).is_some_and(|known| !known.started) {
        held.remove(&pid);
    }
}

/// How `pid`'s process exited, if it has: its native exit code.
fn status(process: &OwnedHandle) -> Result<Option<Status>> {
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    // SAFETY: the process handle is open; it touches no memory.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return Ok(None),
        _ => return Err(Errno::last()),
    }
    let mut code = 0u32;
    // SAFETY: the handle is open; `code` is writable.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(Errno::last());
    }
    Ok(Some(Status::Exited(code.cast_signed())))
}

/// Whether `pid`, a process known here, has ended, letting it stay known:
/// `None` while it runs. One not known is [`Errno::SRCH`].
pub fn ended(pid: Pid) -> Result<Option<Status>> {
    let held = known();
    status(&held.get(&pid).ok_or(Errno::SRCH)?.process)
}

/// How `pid`, a process known here, ended, if it has, after which it is
/// known no more: `None` while it runs. One not known is [`Errno::SRCH`].
pub fn reap(pid: Pid) -> Result<Option<Status>> {
    let mut held = known();
    let ended = status(&held.get(&pid).ok_or(Errno::SRCH)?.process)?;
    if ended.is_some() {
        held.remove(&pid);
    }
    drop(held);
    Ok(ended)
}

/// Ends `pid`, a process known here, with [`super::KILLED`], for
/// [`crate::process::Signal::Kill`]: no other signal reaches a process on Windows
/// (`INVAL`).
pub fn kill(pid: Pid, signal: crate::process::Signal) -> Result<()> {
    if signal != crate::process::Signal::Kill {
        return Err(Errno::INVAL);
    }
    terminate(&known().get(&pid).ok_or(Errno::SRCH)?.process)
}

/// Windows has no orphans to adopt: what a program starts stays in its
/// job, and the program's own handle is what a server keeps ([`adopt`]).
/// Done, as nothing is to do.
pub fn adopt_orphans() -> Result<()> {
    Ok(())
}

/// Lets go of every process [`start`] started that has ended and that
/// `kept` does not keep: each, and how it ended. One [`adopt`]ed stays
/// till [`reap`] or [`release`], so a program whose pane's process is
/// ending is still there for its server to reap.
pub fn reap_orphans(kept: impl Fn(Pid) -> bool) -> Vec<(Pid, Status)> {
    let mut held = known();
    let ended: Vec<(Pid, Status)> = (held.iter())
        .filter(|(pid, known)| known.started && !kept(**pid))
        .filter_map(|(pid, known)| Some((*pid, status(&known.process).ok()??)))
        .collect();
    for (pid, _) in &ended {
        held.remove(pid);
    }
    ended
}

/// Whether this process is in a job, and if so whether what it starts may
/// leave it (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` or `SILENT_BREAKAWAY_OK`): what
/// a server says once as it starts (D-135). A job that can't be asked is
/// taken as one that binds.
pub fn job_fate() -> crate::process::Fate {
    use crate::process::Fate;
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        QueryInformationJobObject,
    };
    let mut inside = 0;
    // SAFETY: the pseudo-handle is this process; a null job asks about
    // any; `inside` is writable.
    if unsafe { IsProcessInJob(this(), std::ptr::null_mut(), &mut inside) } == 0 {
        return Fate::Bound;
    }
    if inside == 0 {
        return Fate::Free;
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let Ok(size) = u32::try_from(std::mem::size_of_val(&limits)) else {
        return Fate::Bound;
    };
    // SAFETY: a null job is this process's own; `limits` is writable for
    // `size` bytes.
    let asked = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    let leave = JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
    match asked != 0 && limits.BasicLimitInformation.LimitFlags & leave != 0 {
        true => Fate::Leaves,
        false => Fate::Bound,
    }
}

/// Who the process `pid` is now, if one has that pid and may be asked:
/// to tell whether the process a record names (by its pid and creation
/// time) is still the one running.
pub fn identity(pid: Pid) -> Result<Identity> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: it takes numbers and touches no memory.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_raw()) };
    if raw.is_null() {
        return Err(Errno::last());
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    super::identity_of(&process).map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::SRCH))
}

/// Who this process is.
pub fn own_identity() -> Result<Identity> {
    let pid = Pid::from_raw(std::process::id()).ok_or(Errno::SRCH)?;
    identity(pid)
}

/// Windows has no process groups: no signal reaches one (`INVAL`). fux
/// sends none there ([`crate::process::delivery`]).
pub fn kill_group(_group: Pid, _signal: crate::process::Signal) -> Result<()> {
    Err(Errno::INVAL)
}
