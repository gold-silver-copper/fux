//! Processes on Windows: their IDs, a child's end told as `Chld`, and a
//! pseudo console's program in its job object ([`Job`]).
use super::{Reach, Scan, Sent, Status, TREE_MAX, Tree};
use crate::errno::{Errno, Result};
use std::fmt;
use std::num::NonZeroU32;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};

mod children;
pub use children::*;

/// A process ID: a nonzero `DWORD`. 0 is the System Idle Process, which
/// fux never names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Pid(NonZeroU32);

impl Pid {
    /// `raw` as a process ID, if it is one: nonzero.
    pub fn from_raw(raw: u32) -> Option<Pid> {
        NonZeroU32::new(raw).map(Pid)
    }

    /// The ID of a child std started. `Option`, as on Unix, though std's
    /// `u32` is Windows' own type: only a 0 is refused.
    pub fn of(child: &std::process::Child) -> Option<Pid> {
        Pid::from_raw(child.id())
    }

    pub fn as_raw(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A child's end, told as [`super::Signal::Chld`] to every
/// [`crate::signal::Pipe`] that asked for it, as Unix tells every child's.
/// It holds its own copy of the child's process handle, so the child may
/// be waited for or dropped meanwhile; dropped, it tells no more.
pub struct Watch {
    wait: windows_sys::Win32::Foundation::HANDLE,
    _process: OwnedHandle,
}

// SAFETY: a registered wait may be unregistered from any thread.
unsafe impl Send for Watch {}

/// Watches `process`, a child's handle (a std `Child`, or the handle a
/// pseudo console's `CreateProcessW` gives): once it ends (or at once, if
/// it has), `Chld` is raised, once, by a thread-pool wait whose callback
/// only raises it.
pub fn watch(process: impl AsHandle) -> Result<Watch> {
    use windows_sys::Win32::System::Threading::{
        INFINITE, RegisterWaitForSingleObject, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
    };
    let process = (process.as_handle().try_clone_to_owned())
        .map_err(|error| Errno::from_io_error(&error).unwrap_or_else(Errno::last))?;
    // Made now, so that the callback always has a port to wake.
    crate::poll::port()?;
    let mut wait = std::ptr::null_mut();
    // SAFETY: the process handle is this watch's own, open till the wait
    // is unregistered (`Drop`); `ended` takes no context and only raises a
    // signal, short enough to run in the wait thread.
    let registered = unsafe {
        RegisterWaitForSingleObject(
            &mut wait,
            process.as_raw_handle(),
            Some(watched_ended),
            std::ptr::null(),
            INFINITE,
            WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD,
        )
    };
    match registered {
        0 => Err(Errno::last()),
        _ => Ok(Watch {
            wait,
            _process: process,
        }),
    }
}

/// What the thread pool runs when a watched child ends.
unsafe extern "system" fn watched_ended(_: *mut core::ffi::c_void, _: bool) {
    // A wake that can't be posted has nobody to tell.
    let _ = crate::signal::tell(super::Signal::Chld);
}

impl Drop for Watch {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Threading::UnregisterWaitEx;
        // SAFETY: unregisters this watch's wait, once, waiting for a
        // callback already running to finish, before the process handle
        // it waits on closes.
        unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) };
    }
}

/// The exit code a process fux's own [`Job::kill`] ended exits with: an
/// `NTSTATUS` with the customer bit set (`0xE0F0_0009`), which no Windows
/// status is, so that fux can tell its kill from the program's own end.
/// A [`Status`] stays the native code (D-136): "killed" is fux's to say,
/// when it sent the kill and the program then exited with this.
pub const KILLED: i32 = 0xE0F0_0009_u32.cast_signed();

/// A program started contained, and what it starts: the leader of a job
/// object of its own ([`crate::pty::Slave::spawn`]), and its process.
/// The job has no kill-on-close (D-134): dropping it ends nothing, and
/// its processes may break away, as one may `setsid` on Unix.
#[must_use = "a job dropped ends nothing, and its exit code is lost"]
#[derive(Debug)]
pub struct Job {
    process: OwnedHandle,
    job: OwnedHandle,
    leader: Pid,
}

/// A new job object for one program: breakaway allowed, no kill-on-close.
pub(crate) fn job_object() -> std::io::Result<OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // SAFETY: an unnamed job with the default security, not inherited.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` was just made, and nothing else owns it.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    let size = u32::try_from(std::mem::size_of_val(&limits)).map_err(std::io::Error::other)?;
    // SAFETY: the job is open; `limits` is readable for `size` bytes.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size,
        )
    };
    match set {
        0 => Err(std::io::Error::last_os_error()),
        _ => Ok(job),
    }
}

impl Job {
    pub(crate) fn new(process: OwnedHandle, job: OwnedHandle, leader: Pid) -> Job {
        Job {
            process,
            job,
            leader,
        }
    }

    /// Its leader's pid.
    pub fn leader(&self) -> Pid {
        self.leader
    }

    /// The processes in its job but its leader: what it started, and what
    /// they started, unless they broke away. None are left but the leader
    /// when the list is empty and its scan [`Scan::Complete`]; past
    /// [`TREE_MAX`], `Truncated`.
    pub fn members(&self) -> Tree {
        use windows_sys::Win32::Foundation::ERROR_MORE_DATA;
        use windows_sys::Win32::System::JobObjects::{
            JobObjectBasicProcessIdList, QueryInformationJobObject,
        };
        // `JOBOBJECT_BASIC_PROCESS_ID_LIST`: two `u32` counts, in one
        // `usize` on 64-bit Windows (the only Windows fux runs on), then
        // the ids, room for `TREE_MAX` and one more.
        const _: () = assert!(std::mem::size_of::<usize>() == 8);
        let mut list = vec![0usize; TREE_MAX.saturating_add(2)];
        let mut tree = Tree::default();
        let Ok(size) = u32::try_from(std::mem::size_of_val(list.as_slice())) else {
            tree.scan = Scan::Failed(Errno::INVAL);
            return tree;
        };
        // SAFETY: the job is open; `list` is writable for `size` bytes and
        // aligned for the struct.
        let queried = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicProcessIdList,
                list.as_mut_ptr().cast(),
                size,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            let errno = Errno::last();
            if errno != Errno::from_code(ERROR_MORE_DATA) {
                tree.scan = Scan::Failed(errno);
                return tree;
            }
        }
        let counts = list.first().copied().unwrap_or(0).to_le_bytes();
        let count = |at: usize| {
            let bytes = counts.get(at..at.saturating_add(4)).unwrap_or_default();
            usize::try_from(u32::from_le_bytes(bytes.try_into().unwrap_or_default())).unwrap_or(0)
        };
        let (assigned, listed) = (count(0), count(4));
        if queried == 0 || assigned > listed {
            tree.scan = Scan::Truncated;
        }
        let found = (list.iter().skip(1).take(listed))
            .filter_map(|id| Pid::from_raw(u32::try_from(*id).ok()?))
            .filter(|pid| *pid != self.leader);
        for pid in found {
            if tree.pids.len() >= TREE_MAX {
                tree.scan = Scan::Truncated;
                break;
            }
            tree.pids.push(pid);
        }
        tree
    }

    /// Ends it with [`KILLED`] as far as `reach` says: [`Reach::Group`] its
    /// leader alone (Windows has no process groups), [`Reach::Session`]
    /// every process in its job, [`Reach::Attached`] each of
    /// [`Job::attached`] that is still in its job. A leader ended already
    /// is a failure with [`Errno::SRCH`].
    pub fn kill(&self, reach: Reach) -> Sent {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        let code = KILLED.cast_unsigned();
        let done = match reach {
            Reach::Group if matches!(self.ended(), Ok(Some(_))) => Err(Errno::SRCH),
            Reach::Group => terminate(&self.process),
            // SAFETY: the job handle is open; it touches no memory.
            Reach::Session => match unsafe { TerminateJobObject(self.job.as_raw_handle(), code) } {
                0 => Err(Errno::last()),
                _ => Ok(()),
            },
            Reach::Attached => {
                let attached = self.attached();
                let failed = (attached.pids.into_iter())
                    .filter_map(|pid| Some((pid, self.terminate_member(pid).err()?)))
                    .collect();
                return Sent {
                    scan: attached.scan,
                    failed,
                };
            }
        };
        Sent {
            scan: Scan::Complete,
            failed: done
                .err()
                .map(|errno| (self.leader, errno))
                .into_iter()
                .collect(),
        }
    }

    /// Ends `pid` with [`KILLED`] if it is in this job: a pid reused
    /// since it was listed is not.
    fn terminate_member(&self, pid: Pid) -> Result<()> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::JobObjects::IsProcessInJob;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        };
        let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        // SAFETY: it takes numbers and touches no memory.
        let raw = unsafe { OpenProcess(access, 0, pid.as_raw()) };
        if raw.is_null() {
            return Err(Errno::SRCH);
        }
        // SAFETY: `raw` was just opened, and nothing else owns it.
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut inside = 0;
        // SAFETY: both handles are open; `inside` is writable.
        let asked = unsafe {
            IsProcessInJob(
                process.as_raw_handle(),
                self.job.as_raw_handle(),
                &mut inside,
            )
        };
        match (asked, inside) {
            (0, _) => Err(Errno::last()),
            (_, 0) => Err(Errno::SRCH),
            _ => terminate(&process),
        }
    }

    /// The processes on its pseudo console but this one and its leader:
    /// what a shell started on it and runs now, and anything else still
    /// on the console, in its job or not ([`Job::kill`] with
    /// [`Reach::Attached`] ends only those in its job). The console is
    /// asked by attaching to it (`AttachConsole`), which a process with a
    /// console of its own can't (`Scan::Failed` with `ACCES`): a pane
    /// process starts detached. Every console event is swallowed from
    /// before the attach to after the free, so a Ctrl-Break meanwhile ends
    /// nothing of this process (spike S5); if the swallowing can't be set,
    /// nothing is attached to and the scan fails.
    pub fn attached(&self) -> Tree {
        use std::sync::{Mutex, PoisonError};
        use windows_sys::Win32::System::Console::{
            AttachConsole, FreeConsole, GetConsoleProcessList, SetConsoleCtrlHandler,
        };
        /// Adds (`add` 1) or removes (0) `swallow` as a handler.
        fn swallowing(add: i32) -> Result<()> {
            // SAFETY: `swallow` lives as long as the process and is safe
            // on any thread; each one added is removed in `attached`.
            match unsafe { SetConsoleCtrlHandler(Some(swallow), add) } {
                0 => Err(Errno::last()),
                _ => Ok(()),
            }
        }
        static ATTACHING: Mutex<()> = Mutex::new(());
        let mut tree = Tree::default();
        let mut pids = vec![0u32; TREE_MAX.saturating_add(1)];
        let room = u32::try_from(pids.len()).unwrap_or(0);
        let one = ATTACHING.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(errno) = swallowing(1) {
            tree.scan = Scan::Failed(errno);
            return tree;
        }
        // SAFETY: it takes a number; this process has no console (else it
        // fails), and frees the one it attaches to below.
        if unsafe { AttachConsole(self.leader.as_raw()) } == 0 {
            let error = Errno::last();
            let _ = swallowing(0);
            tree.scan = Scan::Failed(error);
            return tree;
        }
        // Again once attached, as WINDOWS.md's rule has it.
        let again = swallowing(1);
        let listed = match again {
            // SAFETY: `pids` is writable for `room` pids.
            Ok(()) => Ok(unsafe { GetConsoleProcessList(pids.as_mut_ptr(), room) }),
            Err(errno) => Err(errno),
        };
        let error = Errno::last();
        // SAFETY: it takes nothing; it frees the console attached above.
        unsafe { FreeConsole() };
        if again.is_ok() {
            let _ = swallowing(0);
        }
        let _ = swallowing(0);
        drop(one);
        let listed = match listed {
            Ok(listed) => listed,
            Err(errno) => {
                tree.scan = Scan::Failed(errno);
                return tree;
            }
        };
        let listed = usize::try_from(listed).unwrap_or(usize::MAX);
        if listed == 0 {
            tree.scan = Scan::Failed(error);
            return tree;
        }
        if listed > pids.len() {
            tree.scan = Scan::Truncated;
        }
        let this = std::process::id();
        tree.pids = (pids.into_iter().take(listed))
            .filter(|pid| *pid != this)
            .filter_map(Pid::from_raw)
            .filter(|pid| *pid != self.leader)
            .take(TREE_MAX)
            .collect();
        tree
    }

    /// Whether its leader has ended, and how: its native exit code, read
    /// only once it has ended (259, `STILL_ACTIVE`, is a code like any).
    pub fn ended(&self) -> Result<Option<Status>> {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        // SAFETY: the process handle is open; it touches no memory.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Ok(None),
            _ => return Err(Errno::last()),
        }
        let mut code = 0u32;
        // SAFETY: the handle is open; `code` is writable.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(Errno::last());
        }
        Ok(Some(Status::Exited(code.cast_signed())))
    }

    /// How its leader ended, once it has ([`Job::ended`]); the job back
    /// while it runs.
    pub fn reap(self) -> std::result::Result<Result<Status>, Job> {
        match self.ended() {
            Ok(None) => Err(self),
            Ok(Some(status)) => Ok(Ok(status)),
            Err(errno) => Ok(Err(errno)),
        }
    }
}

/// Ends `process` with [`KILLED`].
fn terminate(process: &OwnedHandle) -> Result<()> {
    use windows_sys::Win32::System::Threading::TerminateProcess;
    // SAFETY: the handle is open; it touches no memory.
    match unsafe { TerminateProcess(process.as_raw_handle(), KILLED.cast_unsigned()) } {
        0 => Err(Errno::last()),
        _ => Ok(()),
    }
}

/// The console handler while [`Job::attached`] is attached: every event
/// is taken as handled, so none ends this process.
unsafe extern "system" fn swallow(_: u32) -> windows_sys::core::BOOL {
    1
}

/// Who a process is: its pid, and when it was made, in 100 ns counts
/// since 1601, as Windows tells it. A pid is reused once its process is
/// gone and every handle to it closed; the two together are never.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pid: Pid,
    pub created: u64,
}

/// Who `process`, a handle with `PROCESS_QUERY_LIMITED_INFORMATION`, is.
pub(crate) fn identity_of(process: &OwnedHandle) -> std::io::Result<Identity> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetProcessId, GetProcessTimes};
    // SAFETY: the handle is open; it touches no memory.
    let pid = Pid::from_raw(unsafe { GetProcessId(process.as_raw_handle()) })
        .ok_or_else(std::io::Error::last_os_error)?;
    let [mut made, mut ended, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: the handle is open for querying; each time is written once.
    let got = unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut made,
            &mut ended,
            &mut kernel,
            &mut user,
        )
    };
    if got == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let created = u64::from(made.dwHighDateTime) << 32 | u64::from(made.dwLowDateTime);
    Ok(Identity { pid, created })
}

impl Job {
    /// What this process offers another of its leader, to [`adopt`] a
    /// copy of its handle: who it is, and the handle's value here, which
    /// stays its own while this job lives.
    pub fn offer(&self) -> std::io::Result<Offer> {
        let handle =
            u64::try_from(self.process.as_raw_handle().addr()).map_err(std::io::Error::other)?;
        Ok(Offer {
            program: identity_of(&self.process)?,
            handle,
        })
    }
}

/// Windows has no sessions: no process has one to name.
pub fn session(_pid: Pid) -> Option<Pid> {
    None
}

/// How fux's signals reach a pane's programs on Windows (D-136), which has
/// no signals: a key on the console, the console's processes ended, the
/// job ended, or refused.
pub fn delivery(signal: super::Signal, shell: bool) -> super::Delivery {
    use super::{Delivery, Signal};
    match (signal, shell) {
        (Signal::Int, _) => Delivery::Keys(b"\x03"),
        (Signal::Quit, _) | (Signal::Term, true) => Delivery::Keys(crate::pty::BREAK),
        (Signal::Kill, true) => Delivery::Attached,
        (Signal::Term | Signal::Kill, false) => Delivery::Job,
        _ => Delivery::Unsupported,
    }
}

/// Where `pid` works is not read on Windows yet: [`Errno::NOSYS`], so a
/// split there is refused, as Unix refuses one whose directory it can't
/// read (D-137).
pub fn cwd_observed<A: crate::Allocation>(
    _pid: Pid,
    _budget: &mut A,
) -> std::result::Result<Result<Option<std::path::PathBuf>>, A::Error> {
    Ok(Err(Errno::NOSYS))
}

/// Its leader's process handle: to [`watch`] it.
impl AsHandle for Job {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.process.as_handle()
    }
}
