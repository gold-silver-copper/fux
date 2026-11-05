//! Processes on Unix: their IDs, signals, sessions, and how they end.
use super::{Dispositions, Reach, Scan, Sent, Signal, Status, TREE_MAX, Tree};
use crate::errno::{Errno, Result, check, fit};
use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroI32;
use std::path::PathBuf;

/// A process ID, process group or session: always positive. The system
/// uses 0 and negative numbers for "none" and for groups, and a signal sent
/// to one reaches far more than meant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Pid(NonZeroI32);

impl Pid {
    /// `raw` as a process ID, if it is one: positive.
    pub fn from_raw(raw: i32) -> Option<Pid> {
        NonZeroI32::new(raw).filter(|n| n.get() > 0).map(Pid)
    }

    /// The ID of a child std started, for [`reap`], [`kill_group`] and the
    /// rest. On Linux and macOS it is always `Some`: a pid there is a
    /// positive `pid_t`. It stays an `Option` because std gives the ID as a
    /// `u32`, and no made-up `Pid` could stand in for one that did not fit:
    /// [`kill_group`] of a wrong one signals other processes.
    pub fn of(child: &std::process::Child) -> Option<Pid> {
        i32::try_from(child.id()).ok().and_then(Pid::from_raw)
    }

    pub fn as_raw(self) -> i32 {
        self.0.get()
    }

    /// `raw`, a pid as a `u32` holds one (std's, a client's ancestry), as
    /// a process ID if it is one.
    pub fn from_u32(raw: u32) -> Option<Pid> {
        i32::try_from(raw).ok().and_then(Pid::from_raw)
    }

    /// It as a `u32`, as std gives a pid.
    pub fn as_u32(self) -> u32 {
        self.0.get().cast_unsigned()
    }

    /// `raw`, a pid as fux's own messages carry one (an `i32`).
    pub fn from_i32(raw: i32) -> Option<Pid> {
        Pid::from_raw(raw)
    }

    /// It as fux's own messages carry a pid.
    pub fn as_i32(self) -> i32 {
        self.0.get()
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Sends `signal` to the process `pid`.
pub fn kill(pid: Pid, signal: Signal) -> Result<()> {
    // SAFETY: kill takes two numbers and touches no memory; `pid` is
    // positive, so it names one process.
    check(unsafe { libc::kill(pid.as_raw(), signal.raw()) }).map(drop)
}

/// Sends `signal` to every process in the group `group`.
pub fn kill_group(group: Pid, signal: Signal) -> Result<()> {
    // SAFETY: killpg takes two numbers and touches no memory; `group` is
    // positive, so it names one group.
    check(unsafe { libc::killpg(group.as_raw(), signal.raw()) }).map(drop)
}

/// This process's effective user ID.
pub fn geteuid() -> u32 {
    // SAFETY: geteuid cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// The CPU time the calling thread has used: time it spent waiting for a
/// processor while other processes ran is not counted, so timing work on a
/// busy machine measures the work. For fux-bench's timings.
pub fn thread_cpu_time() -> Result<std::time::Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid timespec for clock_gettime to fill, and it
    // is read only after the call says it filled it.
    check(unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) })?;
    let seconds: u64 = fit(time.tv_sec)?;
    let nanos: u32 = fit(time.tv_nsec)?;
    Ok(std::time::Duration::new(seconds, nanos))
}

/// Has `command`'s program, once spawned, lead a new session, holding no
/// descriptor but its stdin, stdout and stderr: what this process holds
/// without close-on-exec, inherited from a shell that leaks descriptors or
/// opened by a thread racing the spawn, goes no further (on macOS, all
/// but one opened in the window `io::cloexec_from` states). With `terminal`,
/// its stdin, a terminal, becomes the session's controlling terminal. The
/// program starts with no signal blocked, and with each signal's
/// disposition as `dispositions` says.
pub fn lead_session(
    command: &mut std::process::Command,
    terminal: bool,
    dispositions: Dispositions,
) {
    use std::os::unix::process::CommandExt;
    // Listed now: between fork and exec nothing may allocate.
    let listed = crate::io::Listed::now();
    let lead = move || -> std::io::Result<()> {
        crate::io::cloexec_from(3, listed)?;
        crate::signal::start(dispositions)?;
        // A new session, this process its leader, whose ID is its pid.
        // SAFETY: setsid takes nothing and touches no memory.
        check(unsafe { libc::setsid() })?;
        if terminal {
            let set = crate::terminal::Request::from(libc::TIOCSCTTY);
            let steal: libc::c_int = 0;
            // SAFETY: TIOCSCTTY takes an int and touches no memory.
            check(unsafe { libc::ioctl(0, set, steal) })?;
        }
        Ok(())
    };
    // SAFETY: `lead` runs between fork and exec, where only
    // async-signal-safe calls may be made, and makes only those:
    // close_range, or open, getdents64, fcntl and close (macOS: getdtablesize and fcntl);
    // sigemptyset, sigaction and sigprocmask; setsid; ioctl. It allocates
    // nothing, takes no lock, and its errors are numbers.
    unsafe { command.pre_exec(lead) };
}

/// How `signal` reaches a pane's programs: on Unix always as a signal to
/// its foreground group, a shell's pane or not.
pub fn delivery(_signal: Signal, _shell: bool) -> super::Delivery {
    super::Delivery::Group
}

/// A program's handle offered for another process to keep (D-134) is
/// Windows' alone: on Unix a server adopts its panes' orphans instead
/// ([`adopt_orphans`]), so nothing is to take, and this does nothing.
pub fn adopt(_from: Pid, _offer: super::Offer) -> Result<()> {
    Ok(())
}

/// Nothing is adopted on Unix ([`adopt`]): nothing to let go.
pub fn release(_pid: Pid) {}

/// Unix has no jobs: what this process starts is free of them.
pub fn job_fate() -> super::Fate {
    super::Fate::Free
}

/// The session of `pid`. `None` if there is none to name: the process is
/// gone, or is a kernel thread, whose session is 0.
pub fn session(pid: Pid) -> Option<Pid> {
    // SAFETY: getsid takes a number and touches no memory.
    Pid::from_raw(unsafe { libc::getsid(pid.as_raw()) })
}

/// Whether `pid`, a child of this process, has ended, without reaping it:
/// `None` while it runs, and while it is only stopped.
pub fn ended(pid: Pid) -> Result<Option<Status>> {
    waited(pid, libc::WNOWAIT)
}

/// Reaps `pid`, a child of this process, if it has ended: `None` while it
/// runs.
pub fn reap(pid: Pid) -> Result<Option<Status>> {
    waited(pid, 0)
}

/// How `pid`, a child of this process, ended, if it has, `keep` (0 or
/// `WNOWAIT`) saying whether it stays to be reaped. Only exits and kills
/// are asked for; macOS reports a stop all the same, so the report's code
/// decides.
fn waited(pid: Pid, keep: libc::c_int) -> Result<Option<Status>> {
    let id: libc::id_t = fit(pid.as_raw())?;
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` is a whole siginfo_t, which waitid fills; with WNOHANG
    // and nothing to report, it leaves it zeroed.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | keep,
        )
    };
    check(result)?;
    // SAFETY: a zeroed siginfo_t is valid, and waitid wrote only whole
    // fields.
    let info = unsafe { info.assume_init() };
    // SAFETY: for a child's report the pid field is the one set, and it
    // is 0 when there was nothing to report.
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    // SAFETY: as above, for the status field.
    let status = unsafe { info.si_status() };
    Ok(match info.si_code {
        libc::CLD_EXITED => Some(Status::Exited(status)),
        libc::CLD_KILLED => Some(Status::Signalled {
            signal: status,
            core_dumped: false,
        }),
        libc::CLD_DUMPED => Some(Status::Signalled {
            signal: status,
            core_dumped: true,
        }),
        _ => None,
    })
}

/// Makes this process the reaper of its descendants' orphans: a program
/// whose parent exits (a double fork, a daemon) becomes its child, not
/// init's, so it stays in its [`tree`], and this process must reap it
/// ([`reap_orphans`]). macOS has no reaper of orphans but launchd: none are
/// adopted.
pub fn adopt_orphans() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let set: libc::c_ulong = 1;
        let unused: libc::c_ulong = 0;
        // SAFETY: this operation takes machine-word arguments and touches no memory.
        check(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, set, unused, unused, unused) })?;
    }
    Ok(())
}

/// Reaps every child of this process that has exited and that `kept` does
/// not keep: the orphans it adopted ([`adopt_orphans`]); none on macOS. Its
/// own children are kept, for whoever waits for them. Each reaped, and how
/// it ended.
pub fn reap_orphans(kept: impl Fn(Pid) -> bool) -> Vec<(Pid, Status)> {
    #[cfg(target_os = "macos")]
    {
        let _ = kept;
        Vec::new()
    }
    // Each child is looked at, however many: not cut at `TREE_MAX`, as
    // live ones listed first would hide every zombie after them.
    #[cfg(not(target_os = "macos"))]
    {
        let mut reaped = Vec::new();
        for task in std::fs::read_dir("/proc/self/task")
            .into_iter()
            .flatten()
            .flatten()
        {
            let listed = std::fs::read_to_string(task.path().join("children"));
            let children = listed.iter().flat_map(|l| l.split_whitespace());
            for pid in children.filter_map(|c| Pid::from_raw(c.parse().ok()?)) {
                if !kept(pid)
                    && let Ok(Some(status)) = reap(pid)
                {
                    reaped.push((pid, status));
                }
            }
        }
        reaped
    }
}

impl Tree {
    /// Takes `found` in until it holds [`TREE_MAX`]: whether all fit.
    fn add(&mut self, found: impl IntoIterator<Item = Pid>) -> bool {
        for pid in found {
            if self.pids.len() >= TREE_MAX {
                self.incomplete(Scan::Truncated);
                return false;
            }
            self.pids.push(pid);
        }
        true
    }

    /// Keeps the processes in the session `leader` leads, but `leader`. One
    /// gone since it was found is no member; one whose session the system
    /// won't tell is a miss, said.
    fn keep_session(&mut self, leader: Pid) {
        let mut missed = None;
        self.pids.retain(|pid| {
            // SAFETY: getsid takes a number and touches no memory.
            match check(unsafe { libc::getsid(pid.as_raw()) }) {
                Ok(sid) => sid == leader.as_raw() && *pid != leader,
                Err(Errno::SRCH) => false,
                Err(errno) => {
                    missed.get_or_insert(errno);
                    false
                }
            }
        });
        if let Some(errno) = missed {
            self.incomplete(Scan::Failed(errno));
        }
    }

    /// The scan missed something; the first miss is the one said.
    fn incomplete(&mut self, scan: Scan) {
        if self.scan == Scan::Complete {
            self.scan = scan;
        }
    }

    /// Takes in the children of `pid` (`None`: this process), by each of
    /// its threads', reading no more of a list than there is room for:
    /// whether all fit. One that ended since it was found has none; a list
    /// the system refuses is said, and the others are read.
    #[cfg(not(target_os = "macos"))]
    fn read_children(&mut self, pid: Option<Pid>) -> bool {
        use std::io::{ErrorKind::NotFound, Read};
        let of = pid.map_or_else(|| "self".to_owned(), |pid| pid.to_string());
        let tasks = match std::fs::read_dir(format!("/proc/{of}/task")) {
            Ok(tasks) => tasks,
            Err(error) if error.kind() == NotFound && pid.is_some() => return true,
            Err(error) => {
                self.incomplete(Scan::Failed(errno(&error)));
                return true;
            }
        };
        for task in tasks {
            // Room and one more, each a number of 7 digits at most
            // (PID_MAX_LIMIT, 2^22) and a blank, and one cut short.
            let room = TREE_MAX.saturating_sub(self.pids.len()).saturating_add(2);
            let cap = u64::try_from(room).map_or(u64::MAX, |room| room.saturating_mul(8));
            let mut listed = String::new();
            let read = (task.and_then(|t| std::fs::File::open(t.path().join("children"))))
                .and_then(|file| file.take(cap).read_to_string(&mut listed));
            match read {
                Ok(bytes) => {
                    // A list read to the cap may end in part of a number.
                    let cut = u64::try_from(bytes).is_ok_and(|bytes| bytes >= cap);
                    let whole = match listed.rsplit_once(' ') {
                        Some((whole, _)) if cut => whole,
                        None if cut => "",
                        _ => &listed,
                    };
                    let found = whole.split_whitespace();
                    if !self.add(found.filter_map(|c| Pid::from_raw(c.parse().ok()?))) {
                        return false;
                    }
                    // Not reached while a number has 7 digits at most; if
                    // one had more, what was cut is said missed.
                    if cut {
                        self.incomplete(Scan::Truncated);
                    }
                }
                Err(error) if error.kind() == NotFound => {}
                Err(error) => self.incomplete(Scan::Failed(errno(&error))),
            }
        }
        true
    }

    /// Takes in the children of `pid` (`None`: this process), reading no
    /// more of the list than there is room for: whether all fit. One that
    /// ended since it was found has none; a list the system refuses is
    /// said. libproc gives a count of pids, and 0 for a failure as for none.
    #[cfg(target_os = "macos")]
    fn read_children(&mut self, pid: Option<Pid>) -> bool {
        // SAFETY: getpid cannot fail and touches no memory.
        let parent = pid.map_or_else(|| unsafe { libc::getpid() }, Pid::as_raw);
        // Room and one more: a list that fills it may have had more.
        let room = TREE_MAX.saturating_sub(self.pids.len()).saturating_add(1);
        let mut found: Vec<libc::c_int> = vec![0; room];
        let Ok(bytes) = libc::c_int::try_from(size_of_val(found.as_slice())) else {
            self.incomplete(Scan::Failed(Errno::INVAL));
            return true;
        };
        clear_errno();
        // SAFETY: the buffer is `bytes` long and writable.
        let count = unsafe { libc::proc_listchildpids(parent, found.as_mut_ptr().cast(), bytes) };
        let count = usize::try_from(count).unwrap_or(0);
        if count == 0 {
            // A failure, said as no children: errno tells them apart. One
            // that ended since it was found (`ESRCH`) has none.
            match Errno::last() {
                errno if errno.raw() == 0 || errno == Errno::SRCH => {}
                errno => self.incomplete(Scan::Failed(errno)),
            }
            return true;
        }
        found.truncate(count);
        self.add(found.into_iter().filter_map(Pid::from_raw))
    }
}

/// The processes this one started, those they started in turn, and the
/// orphans it adopted ([`adopt_orphans`], Linux only: on macOS an orphan
/// leaves for launchd); not itself, and no other process. Found by each
/// one's children (on Linux, each of its threads'), at most [`TREE_MAX`].
pub fn tree() -> Tree {
    let mut tree = Tree::default();
    // A kernel without the lists has none to read for any thread.
    #[cfg(not(target_os = "macos"))]
    if let Err(error) = std::fs::metadata("/proc/thread-self/children") {
        tree.incomplete(Scan::Failed(errno(&error)));
    }
    let (mut parent, mut next) = (None, 0usize);
    while tree.read_children(parent) {
        let Some(pid) = tree.pids.get(next).copied() else {
            break;
        };
        (parent, next) = (Some(pid), next.saturating_add(1));
    }
    tree
}

/// Sets this thread's `errno` to 0, for a call that fails without saying so.
#[cfg(target_os = "macos")]
fn clear_errno() {
    // SAFETY: __error takes nothing and returns this thread's errno's
    // address, always valid.
    let errno = unsafe { libc::__error() };
    // SAFETY: it is this thread's errno, an int it may write.
    unsafe { errno.write(0) };
}

/// The system's error behind `error`.
#[cfg(not(target_os = "macos"))]
fn errno(error: &std::io::Error) -> Errno {
    Errno::from_io_error(error).unwrap_or(Errno::from_raw(libc::EIO))
}

/// Every process the system lists, as many as there are (`kern.maxproc`
/// bounds them): where a [`Job`]'s members are looked for on macOS, which
/// lists no session's processes alone (`sysctl` has no `KERN_PROC_SESSION`
/// node: `ENOENT`). libproc says a failure with a count of 0.
#[cfg(target_os = "macos")]
fn everyone() -> Tree {
    let mut tree = Tree::default();
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Some(count) = usize::try_from(count).ok().filter(|n| *n > 0) else {
        tree.incomplete(Scan::Failed(Errno::last()));
        return tree;
    };
    // Room for processes started between the two calls; a list that fills
    // it may have had more.
    let mut pids: Vec<libc::c_int> = vec![0; count.saturating_add(64)];
    let Ok(bytes) = libc::c_int::try_from(size_of_val(pids.as_slice())) else {
        tree.incomplete(Scan::Failed(Errno::INVAL));
        return tree;
    };
    // SAFETY: the buffer is `bytes` long and writable.
    let written = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let Some(written) = usize::try_from(written).ok().filter(|n| *n > 0) else {
        tree.incomplete(Scan::Failed(Errno::last()));
        return tree;
    };
    if written >= pids.len() {
        tree.incomplete(Scan::Truncated);
    }
    pids.truncate(written);
    tree.pids = pids.into_iter().filter_map(Pid::from_raw).collect();
    tree
}

/// A program started contained, and what it starts: the leader of a new
/// session and process group, whose IDs are its pid ([`lead_session`]),
/// not yet reaped. Unreaped, its pid cannot be reused, so its group and
/// session are signalled through this alone; reaping it takes it. When to
/// signal it, how far, and what to say of what was missed is the user's.
#[must_use = "a job dropped with its leader unreaped leaves a zombie no one reaps"]
#[derive(Debug, PartialEq, Eq)]
pub struct Job(Pid);

impl Job {
    /// Starts `command` contained ([`lead_session`]), with the std handle,
    /// which waits for nothing when dropped.
    pub(crate) fn start(
        command: &mut std::process::Command,
        terminal: bool,
        dispositions: Dispositions,
    ) -> std::io::Result<(std::process::Child, Job)> {
        lead_session(command, terminal, dispositions);
        let child = command.spawn()?;
        let pid = Pid::of(&child).ok_or_else(|| std::io::Error::other("a child with no pid"))?;
        Ok((child, Job(pid)))
    }

    /// Its leader's pid, which is also its group's and its session's ID.
    pub fn leader(&self) -> Pid {
        self.0
    }

    /// The processes in its session but its leader: what it started, a job
    /// in a group of its own, and an orphan. None are left but the leader
    /// when the list is empty and its scan [`Scan::Complete`]. On Linux
    /// they are looked for in this process's [`tree`], which keeps the
    /// orphans it adopts ([`adopt_orphans`]); on macOS, where orphans go to
    /// launchd, among every process the system lists.
    pub fn members(&self) -> Tree {
        let leader = self.0;
        #[cfg(not(target_os = "macos"))]
        let mut members = tree();
        #[cfg(target_os = "macos")]
        let mut members = everyone();
        members.keep_session(leader);
        members
    }

    /// Sends `signal` to its group and, as far as `reach` says, to each of
    /// its other members, found first: once its leader dies, the processes
    /// it started are on their way to their new parent, in no one's list.
    pub fn signal(&self, signal: Signal, reach: Reach) -> Sent {
        let others = match reach {
            Reach::Group => Tree::default(),
            Reach::Session | Reach::Attached => self.members(),
        };
        let group = (reach != Reach::Attached)
            .then(|| kill_group(self.0, signal).err())
            .flatten()
            .map(|errno| (self.0, errno));
        let found = others.pids.len();
        let each =
            (others.pids.into_iter()).filter_map(|pid| Some((pid, kill(pid, signal).err()?)));
        Sent {
            scan: others.scan,
            failed: group.into_iter().chain(each).collect(),
            found,
        }
    }

    /// Nothing to offer another process of its leader on Unix: a server
    /// adopts its panes' orphans instead ([`adopt_orphans`]).
    pub fn offer(&self) -> Option<super::Offer> {
        None
    }

    /// Kills it as far as `reach` says: [`Job::signal`] with
    /// [`Signal::Kill`], the one way to end a job that every OS has.
    pub fn kill(&self, reach: Reach) -> Sent {
        self.signal(Signal::Kill, reach)
    }

    /// Whether its leader has ended, without reaping it ([`ended`]).
    pub fn ended(&self) -> Result<Option<Status>> {
        ended(self.0)
    }

    /// Reaps its leader if it has ended ([`reap`]): how it ended, or the
    /// error that lost its status, once it is gone; the job back while it
    /// runs.
    pub fn reap(self) -> std::result::Result<Result<Status>, Job> {
        match reap(self.0) {
            Ok(None) => Err(self),
            Ok(Some(status)) => Ok(Ok(status)),
            Err(errno) => Ok(Err(errno)),
        }
    }
}

/// Opens one proc file without a heap-allocated path or CString.
#[cfg(not(target_os = "macos"))]
fn proc_path(pid: Pid, part: &str) -> Option<[u8; 64]> {
    let mut path = [0u8; 64];
    let mut cursor = std::io::Cursor::new(path.as_mut_slice());
    std::io::Write::write_fmt(&mut cursor, format_args!("/proc/{pid}/{part}\0")).ok()?;
    Some(path)
}

/// The observed current directory. Stack space handles ordinary paths;
/// longer paths grow under the same grant, including replacement peaks.
#[cfg(not(target_os = "macos"))]
pub fn cwd_observed<A: crate::Allocation>(
    pid: Pid,
    budget: &mut A,
) -> std::result::Result<Result<Option<PathBuf>>, A::Error> {
    use std::os::unix::ffi::OsStringExt;
    let Some(path) = proc_path(pid, "cwd") else {
        return Ok(Ok(None));
    };
    let mut stack = [0u8; 4096];
    // SAFETY: the path is NUL-terminated and stack is writable for its length.
    let read =
        unsafe { libc::readlink(path.as_ptr().cast(), stack.as_mut_ptr().cast(), stack.len()) };
    let Ok(count) = usize::try_from(read) else {
        return Ok(Err(Errno::last()));
    };
    if count < stack.len() {
        let bytes = stack.get(..count).ok_or_else(A::exceeded)?;
        return budget.os_string(bytes).map(|p| Ok(Some(PathBuf::from(p))));
    }
    let before = budget.remaining();
    let mut raw = Vec::new();
    let result = (|| {
        let mut room = stack.len().checked_mul(2).ok_or_else(A::exceeded)?;
        loop {
            let more = room.checked_sub(raw.len()).ok_or_else(A::exceeded)?;
            budget.reserve(&mut raw, more)?;
            raw.resize(room, 0);
            // SAFETY: path is NUL-terminated, and raw is initialized and writable.
            let read =
                unsafe { libc::readlink(path.as_ptr().cast(), raw.as_mut_ptr().cast(), raw.len()) };
            let Ok(count) = usize::try_from(read) else {
                return Ok(Err(Errno::last()));
            };
            if count < raw.len() {
                raw.truncate(count);
                return Ok(Ok(()));
            }
            room = room.checked_mul(2).ok_or_else(A::exceeded)?;
        }
    })();
    if matches!(&result, Ok(Ok(()))) {
        return Ok(Ok(Some(PathBuf::from(OsString::from_vec(raw)))));
    }
    drop(raw);
    budget.release(
        before
            .checked_sub(budget.remaining())
            .ok_or_else(A::exceeded)?,
    )?;
    result.map(|result| result.map(|()| None))
}

/// The observed current directory, copied from the kernel's stack result
/// only after its exact retained capacity has been admitted.
#[cfg(target_os = "macos")]
pub fn cwd_observed<A: crate::Allocation>(
    pid: Pid,
    budget: &mut A,
) -> std::result::Result<Result<Option<PathBuf>>, A::Error> {
    use std::os::unix::ffi::OsStringExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let Ok(bytes) = libc::c_int::try_from(size_of::<libc::proc_vnodepathinfo>()) else {
        return Ok(Ok(None));
    };
    // SAFETY: info is writable for exactly bytes; a short result is rejected.
    let read = unsafe {
        libc::proc_pidinfo(
            pid.as_raw(),
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            bytes,
        )
    };
    if read <= 0 {
        return Ok(Err(Errno::last()));
    }
    if read != bytes {
        return Ok(Ok(None));
    }
    // SAFETY: proc_pidinfo filled the whole structure, as checked above.
    let info = unsafe { info.assume_init() };
    let path = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|c| **c != 0);
    let count = path.clone().count();
    if count == 0 {
        return Ok(Ok(None));
    }
    let mut bytes = budget.vector(count)?;
    bytes.extend(path.map(|c| c.cast_unsigned()));
    Ok(Ok(Some(PathBuf::from(OsString::from_vec(bytes)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn a_signal_is_found_again_by_its_number() {
        for signal in Signal::ALL {
            assert_eq!(Signal::from_raw(signal.raw()), Some(signal));
        }
        assert_eq!(Signal::from_raw(libc::SIGSEGV), Some(Signal::Segv));
        assert_eq!(Signal::Usr1.name(), "USR1");
        assert_eq!(Signal::from_raw(0), None);
    }

    /// Polls `ended` until it answers, or a few seconds pass.
    fn wait_ended(pid: Pid) -> Option<Status> {
        let deadline = Instant::now().checked_add(Duration::from_secs(5));
        loop {
            if let Ok(Some(status)) = ended(pid) {
                return Some(status);
            }
            if deadline.is_none_or(|d| Instant::now() > d) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn pids_are_positive() {
        assert!(Pid::from_raw(0).is_none());
        assert!(Pid::from_raw(-5).is_none());
        assert_eq!(Pid::from_raw(42).map(Pid::as_raw), Some(42));
    }

    /// A stopped child is not an ended one, on either platform; an
    /// ended one is reported, then reaped, with the signal that ended it.
    #[test]
    fn a_stopped_child_has_not_ended_and_a_killed_one_has() -> std::result::Result<(), String> {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        assert_eq!(ended(pid), Ok(None));
        kill(pid, Signal::Stop).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(ended(pid), Ok(None), "a stop is not an end");
        assert_eq!(reap(pid), Ok(None));
        kill(pid, Signal::Cont).map_err(|e| e.to_string())?;
        kill(pid, Signal::Kill).map_err(|e| e.to_string())?;
        let status = wait_ended(pid);
        let killed = Status::Signalled {
            signal: libc::SIGKILL,
            core_dumped: false,
        };
        assert_eq!(status, Some(killed));
        // Reported without reaping: it is still there to reap.
        assert_eq!(reap(pid), Ok(Some(killed)));
        let _ = child.try_wait();
        Ok(())
    }

    #[test]
    fn an_exit_code_is_reported() -> std::result::Result<(), String> {
        let child = Command::new("sh")
            .args(["-c", "exit 3"])
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        assert_eq!(wait_ended(pid), Some(Status::Exited(3)));
        assert_eq!(reap(pid), Ok(Some(Status::Exited(3))));
        Ok(())
    }

    /// An adopted orphan is reaped and told with how it ended, and a child
    /// kept is left alone; on macOS nothing is adopted.
    #[test]
    fn reaped_orphans_are_told_with_their_ends() -> std::result::Result<(), String> {
        adopt_orphans().map_err(|e| e.to_string())?;
        // The orphan writes its pid, then exits 5 once its parent is gone.
        let out = Command::new("sh")
            .args(["-c", "sh -c 'echo $$; sleep 0.2; exit 5' &"])
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let orphan = Pid::from_raw(text.trim().parse().map_err(|_| "a pid")?).ok_or("a pid")?;
        let deadline = Instant::now().checked_add(Duration::from_secs(5));
        let mut reaped = Vec::new();
        while cfg!(not(target_os = "macos")) && reaped.is_empty() {
            // Only the orphan: the other tests' children are theirs.
            reaped.extend(reap_orphans(|pid| pid != orphan));
            if deadline.is_none_or(|d| Instant::now() > d) {
                return Err("never reaped".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if cfg!(not(target_os = "macos")) {
            assert_eq!(reaped, [(orphan, Status::Exited(5))]);
        }
        Ok(())
    }

    /// The tree finds a child, says its scan finished, and stops at its
    /// bound saying so.
    #[test]
    fn a_tree_says_whether_it_was_cut_short() -> std::result::Result<(), String> {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        let found = tree();
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.pids.contains(&pid));
        assert_eq!(found.scan, Scan::Complete);
        let mut cut = Tree::default();
        let many =
            (1..=i32::try_from(TREE_MAX).map_err(|e| e.to_string())?).filter_map(Pid::from_raw);
        assert!(cut.add(many));
        assert_eq!(cut.scan, Scan::Complete);
        assert!(!cut.add([pid]));
        assert_eq!((cut.pids.len(), cut.scan), (TREE_MAX, Scan::Truncated));
        Ok(())
    }

    #[test]
    fn cwd_observations_keep_os_failures_distinct_from_known_paths()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let own = Pid::from_raw(i32::try_from(std::process::id())?).ok_or("own pid")?;
        let mut quota = 8192usize;
        let observed = cwd_observed(own, &mut quota)??.ok_or("own cwd")?;
        assert_eq!(
            observed.canonicalize()?,
            std::env::current_dir()?.canonicalize()?
        );
        let missing = Pid::from_raw(i32::MAX).ok_or("missing pid")?;
        assert!(matches!(cwd_observed(missing, &mut quota), Ok(Err(_))));
        Ok(())
    }

    #[test]
    fn the_effective_uid_is_the_one_id_reports() -> std::result::Result<(), String> {
        let out = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| e.to_string())?;
        assert_eq!(
            geteuid().to_string(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
        Ok(())
    }
}
