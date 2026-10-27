//! Processes: their IDs, signals, sessions, and how they end.
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

    /// A child's ID.
    pub fn of(child: &std::process::Child) -> Option<Pid> {
        i32::try_from(child.id()).ok().and_then(Pid::from_raw)
    }

    pub fn as_raw(self) -> i32 {
        self.0.get()
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Expands `/// doc, then Variant "NAME" SIGNAL, ...` to the enum `Signal` of those
/// variants, `Signal::ALL` in that order, `raw` (each one's `libc::SIGNAL`)
/// and `from_raw` (the variant of a number), `name` (its `NAME`) and `named`
/// (the variant whose name, exactly as written, is given): `Term "TERM"
/// SIGTERM,` gives `Signal::Term`, raw `libc::SIGTERM`, named "TERM".
macro_rules! signals {
    ($(#[$doc:meta])* $($variant:ident $name:literal $raw:ident,)*) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Signal {
            $($variant,)*
        }

        impl Signal {
            /// Every signal, in the order they are declared.
            pub const ALL: [Signal; [$($name),*].len()] = [$(Signal::$variant),*];

            /// The signal's number.
            pub fn raw(self) -> i32 {
                match self {
                    $(Signal::$variant => libc::$raw,)*
                }
            }

            /// The signal numbered `raw`, if it is one of these.
            pub fn from_raw(raw: i32) -> Option<Signal> {
                match raw {
                    $(libc::$raw => Some(Signal::$variant),)*
                    _ => None,
                }
            }

            /// Its name without `SIG`, as `named` takes it: `TERM`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Signal::$variant => $name,)*
                }
            }

            /// The signal a name names, written as `fux signal` takes it:
            /// without `SIG`, in capitals (`TERM`).
            pub fn named(name: &str) -> Option<Signal> {
                match name {
                    $($name => Some(Signal::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

signals! {
    /// The signals fux and its tests and walk send (fux itself: `Hup`, `Term`,
    /// `Kill`, and any of these for `fux signal`), and `Tstp`, which a
    /// client sends itself to suspend: a shell reports a job `Tstp` stopped
    /// as "Stopped", and one `Stop` stopped as "Stopped (signal)". From
    /// `Ill` on, the rest of POSIX's, each numbered as the system numbers
    /// it: those a program may die of on its own, which fux names when it
    /// tells a death.
    Hup "HUP" SIGHUP,
    Int "INT" SIGINT,
    Term "TERM" SIGTERM,
    Kill "KILL" SIGKILL,
    Stop "STOP" SIGSTOP,
    Tstp "TSTP" SIGTSTP,
    Cont "CONT" SIGCONT,
    Winch "WINCH" SIGWINCH,
    Quit "QUIT" SIGQUIT,
    Usr1 "USR1" SIGUSR1,
    Usr2 "USR2" SIGUSR2,
    Ill "ILL" SIGILL,
    Abrt "ABRT" SIGABRT,
    Fpe "FPE" SIGFPE,
    Segv "SEGV" SIGSEGV,
    Pipe "PIPE" SIGPIPE,
    Alrm "ALRM" SIGALRM,
    Bus "BUS" SIGBUS,
    Trap "TRAP" SIGTRAP,
    Sys "SYS" SIGSYS,
    Xcpu "XCPU" SIGXCPU,
    Xfsz "XFSZ" SIGXFSZ,
    Vtalrm "VTALRM" SIGVTALRM,
    Prof "PROF" SIGPROF,
    Ttin "TTIN" SIGTTIN,
    Ttou "TTOU" SIGTTOU,
    Urg "URG" SIGURG,
    Chld "CHLD" SIGCHLD,
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

/// Whether a process `pid` exists: signal 0, which checks without sending.
/// One this process may not signal exists too.
pub fn exists(pid: Pid) -> bool {
    // SAFETY: kill with signal 0 sends nothing and touches no memory.
    let result = unsafe { libc::kill(pid.as_raw(), 0) };
    result == 0 || Errno::last().raw() == libc::EPERM
}

/// This process's effective user ID.
pub fn geteuid() -> u32 {
    // SAFETY: geteuid cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// The CPU time the calling thread has used: time it spent waiting for a
/// processor while other processes ran is not counted, so timing work on a
/// busy machine measures the work. For fux-diff's `--speed`.
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
/// spawn clears the signal mask, and `exec` restores the dispositions Rust
/// and signal handlers changed, so the program starts as it would from a
/// shell.
pub fn lead_session(command: &mut std::process::Command, terminal: bool) {
    use std::os::unix::process::CommandExt;
    // Listed now: between fork and exec nothing may allocate.
    let listed = crate::io::Listed::now();
    let lead = move || -> std::io::Result<()> {
        crate::io::cloexec_from(3, listed)?;
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
    // setsid; ioctl. It allocates nothing, takes no lock, and its errors
    // are numbers.
    unsafe { command.pre_exec(lead) };
}

/// The session of `pid`. `None` if there is none to name: the process is
/// gone, or is a kernel thread, whose session is 0.
pub fn session(pid: Pid) -> Option<Pid> {
    // SAFETY: getsid takes a number and touches no memory.
    Pid::from_raw(unsafe { libc::getsid(pid.as_raw()) })
}

/// How a process ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// It exited with this code.
    Exited(i32),
    /// This signal killed it.
    Signalled(i32),
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
        libc::CLD_KILLED | libc::CLD_DUMPED => Some(Status::Signalled(status)),
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
/// own children are kept, for whoever waits for them.
pub fn reap_orphans(kept: impl Fn(Pid) -> bool) {
    // Each child is looked at, however many: not cut at `TREE_MAX`, as
    // live ones listed first would hide every zombie after them.
    #[cfg(not(target_os = "macos"))]
    for task in std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
    {
        let listed = std::fs::read_to_string(task.path().join("children"));
        let children = listed.iter().flat_map(|l| l.split_whitespace());
        for pid in children.filter_map(|c| Pid::from_raw(c.parse().ok()?)) {
            if !kept(pid) {
                let _ = reap(pid);
            }
        }
    }
    #[cfg(target_os = "macos")]
    let _ = kept;
}

/// The most processes a [`tree`] holds on Linux: a runaway tree is cut
/// short here.
pub const TREE_MAX: usize = 4096;

/// What a [`tree`] found, and whether it looked everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub pids: Vec<Pid>,
    pub scan: Scan,
}

/// How a scan of the tree ended. `Complete` says it read every process it
/// found, not that what it found is all there is once used: a process may
/// start or end meanwhile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scan {
    #[default]
    Complete,
    /// There may be more: past [`TREE_MAX`] on Linux, or a list that
    /// filled its buffer on macOS.
    Truncated,
    /// The system refused a list, with this error; it read the others.
    Failed(Errno),
}

impl Tree {
    /// Takes `found` in until it holds [`TREE_MAX`]: whether all fit.
    #[cfg(not(target_os = "macos"))]
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
}

/// The processes this one started, those they started in turn, and the
/// orphans it adopted ([`adopt_orphans`]); not itself. Found by each one's
/// threads' children, at most [`TREE_MAX`].
#[cfg(not(target_os = "macos"))]
pub fn tree() -> Tree {
    let mut tree = Tree::default();
    // A kernel without the lists has none to read for any thread.
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

/// The system's error behind `error`.
#[cfg(not(target_os = "macos"))]
fn errno(error: &std::io::Error) -> Errno {
    Errno::from_io_error(error).unwrap_or(Errno::from_raw(libc::EIO))
}

/// macOS has no reaper of orphans but launchd: an orphan leaves this
/// process's tree, so every process the system lists is taken to be in it,
/// as many as there are (`kern.maxproc` bounds them). libproc says a
/// failure with a count of 0.
#[cfg(target_os = "macos")]
pub fn tree() -> Tree {
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

/// `pid`'s command line, when the system says: each argument, its bytes as
/// they are, ended by a NUL, empty ones too. A process that wrote over its
/// arguments, as `setproctitle` does, has what it wrote, read the same way;
/// ending in no NUL (the kernel then stops at the first one, past the
/// arguments), it is one argument. An empty line, as a kernel thread's or
/// a zombie's, is not said. Allocation beyond `limit` is refused.
#[cfg(not(target_os = "macos"))]
pub fn argv(pid: Pid, limit: usize) -> std::io::Result<Option<Vec<OsString>>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    let Ok(mut file) = std::fs::File::open(format!("/proc/{pid}/cmdline")) else {
        return Ok(None);
    };
    let exceeded =
        || std::io::Error::other("process arguments exceed the observation allocation limit");
    let mut raw = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        if count == 0 {
            break;
        }
        let needed = raw.len().checked_add(count).ok_or_else(exceeded)?;
        if needed > limit {
            return Err(exceeded());
        }
        if needed > raw.capacity() {
            let capacity = raw.capacity().saturating_mul(2).max(needed).min(limit);
            raw.try_reserve_exact(capacity.saturating_sub(raw.len()))
                .map_err(|_| exceeded())?;
        }
        raw.extend_from_slice(buffer.get(..count).unwrap_or_default());
    }
    let line = raw.strip_suffix(&[0]).unwrap_or(&raw);
    if raw.is_empty() {
        return Ok(None);
    }
    let count = line.split(|b| *b == 0).count();
    let slots = count
        .checked_mul(std::mem::size_of::<OsString>())
        .ok_or_else(exceeded)?;
    if raw
        .capacity()
        .checked_add(raw.len())
        .and_then(|n| n.checked_add(slots))
        .is_none_or(|n| n > limit)
    {
        return Err(exceeded());
    }
    let mut words = Vec::new();
    words.try_reserve_exact(count).map_err(|_| exceeded())?;
    for word in line.split(|b| *b == 0) {
        words.push(std::ffi::OsStr::from_bytes(word).to_owned());
    }
    Ok(Some(words))
}

/// `pid`'s command line: macOS keeps it where only a privileged reader
/// finds it reliably, so it is not said.
#[cfg(target_os = "macos")]
pub fn argv(_pid: Pid, _limit: usize) -> std::io::Result<Option<Vec<OsString>>> {
    Ok(None)
}

/// The name of `pid`'s program, when the system says.
#[cfg(not(target_os = "macos"))]
pub fn name(pid: Pid) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end_matches('\n').to_owned()).filter(|n| !n.is_empty())
}

/// The name of `pid`'s program, when the system says.
#[cfg(target_os = "macos")]
pub fn name(pid: Pid) -> Option<String> {
    let mut buffer = [0u8; 256];
    let room = u32::try_from(buffer.len()).ok()?;
    // SAFETY: the buffer is `room` bytes long and writable; the call writes
    // at most that, and says how much.
    let written = unsafe { libc::proc_name(pid.as_raw(), buffer.as_mut_ptr().cast(), room) };
    let name = buffer.get(..usize::try_from(written).ok()?)?;
    Some(String::from_utf8_lossy(name).into_owned()).filter(|n| !n.is_empty())
}

/// The current directory of `pid`, when the system says.
#[cfg(not(target_os = "macos"))]
pub fn cwd(pid: Pid) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The current directory of `pid`, when the system says.
#[cfg(target_os = "macos")]
pub fn cwd(pid: Pid) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let bytes = libc::c_int::try_from(size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: the buffer is exactly one `proc_vnodepathinfo`, which the call
    // fills; a short return is checked before the value is read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid.as_raw(),
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            bytes,
        )
    };
    if read != bytes {
        return None;
    }
    // SAFETY: fully written, as checked above.
    let info = unsafe { info.assume_init() };
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|c| **c != 0)
        .map(|c| c.cast_unsigned())
        .collect();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn a_signal_is_found_again_by_its_number_and_its_name() {
        for signal in Signal::ALL {
            assert_eq!(Signal::from_raw(signal.raw()), Some(signal));
            assert_eq!(Signal::named(signal.name()), Some(signal));
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
        assert!(exists(pid));
        assert_eq!(ended(pid), Ok(None));
        kill(pid, Signal::Stop).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(ended(pid), Ok(None), "a stop is not an end");
        assert_eq!(reap(pid), Ok(None));
        kill(pid, Signal::Cont).map_err(|e| e.to_string())?;
        kill(pid, Signal::Kill).map_err(|e| e.to_string())?;
        let status = wait_ended(pid);
        assert_eq!(status, Some(Status::Signalled(libc::SIGKILL)));
        // Reported without reaping: it is still there to reap.
        assert_eq!(reap(pid), Ok(Some(Status::Signalled(libc::SIGKILL))));
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

    /// The tree finds a child, says its scan finished, and, on Linux,
    /// stops at its bound saying so.
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
        #[cfg(not(target_os = "macos"))]
        {
            let mut cut = Tree::default();
            let many =
                (1..=i32::try_from(TREE_MAX).map_err(|e| e.to_string())?).filter_map(Pid::from_raw);
            assert!(cut.add(many));
            assert_eq!(cut.scan, Scan::Complete);
            assert!(!cut.add([pid]));
            assert_eq!((cut.pids.len(), cut.scan), (TREE_MAX, Scan::Truncated));
        }
        Ok(())
    }

    /// Empty arguments are arguments.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn argv_keeps_empty_arguments() -> std::result::Result<(), String> {
        use std::io::Read;
        use std::process::Stdio;
        let script = "echo; read x";
        let mut child = Command::new("sh")
            .args(["-c", script, "", "head", "", "tail", ""])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        // The shell runs, not the fork that has yet to become it.
        let mut started = [0u8; 1];
        let out = child.stdout.as_mut().ok_or("its stdout")?;
        out.read_exact(&mut started).map_err(|e| e.to_string())?;
        let read = argv(pid, 1 << 20).map_err(|e| e.to_string());
        let _ = child.kill();
        let _ = child.wait();
        let words = ["sh", "-c", script, "", "head", "", "tail", ""];
        assert_eq!(read?, Some(words.map(OsString::from).to_vec()));
        Ok(())
    }

    #[test]
    fn a_process_has_a_cwd() {
        let own = Pid::from_raw(i32::try_from(std::process::id()).unwrap_or(0));
        let here = std::env::current_dir()
            .ok()
            .and_then(|d| d.canonicalize().ok());
        let there = own.and_then(cwd).and_then(|d| d.canonicalize().ok());
        assert!(here.is_some());
        assert_eq!(here, there);
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
