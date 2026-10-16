//! Processes: their IDs, signals, sessions, and how they end.
use crate::errno::{Errno, Result, check};
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

/// The signals fux and its tests and walk send (fux itself: `Hup`, `Term`,
/// `Kill`, and any of these for `fux signal`), and `Tstp`, which koh's client sends itself to suspend: a shell reports a job `Tstp` stopped as "Stopped", and one
/// `Stop` stopped as "Stopped (signal)".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    Hup,
    Int,
    Term,
    Kill,
    Stop,
    Tstp,
    Cont,
    Winch,
    Quit,
    Usr1,
    Usr2,
}

impl Signal {
    pub fn raw(self) -> i32 {
        match self {
            Signal::Hup => libc::SIGHUP,
            Signal::Int => libc::SIGINT,
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
            Signal::Stop => libc::SIGSTOP,
            Signal::Tstp => libc::SIGTSTP,
            Signal::Cont => libc::SIGCONT,
            Signal::Winch => libc::SIGWINCH,
            Signal::Quit => libc::SIGQUIT,
            Signal::Usr1 => libc::SIGUSR1,
            Signal::Usr2 => libc::SIGUSR2,
        }
    }

    /// The signal a name names: `TERM`, `SIGTERM` or `term`.
    pub fn named(name: &str) -> Option<Signal> {
        let name = name.to_ascii_uppercase();
        let bare = name.strip_prefix("SIG").unwrap_or(&name);
        Some(match bare {
            "HUP" => Signal::Hup,
            "INT" => Signal::Int,
            "TERM" => Signal::Term,
            "KILL" => Signal::Kill,
            "STOP" => Signal::Stop,
            "TSTP" => Signal::Tstp,
            "CONT" => Signal::Cont,
            "WINCH" => Signal::Winch,
            "QUIT" => Signal::Quit,
            "USR1" => Signal::Usr1,
            "USR2" => Signal::Usr2,
            _ => return None,
        })
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
    let seconds = u64::try_from(time.tv_sec).map_err(|_| Errno::INVAL)?;
    let nanos = u32::try_from(time.tv_nsec).map_err(|_| Errno::INVAL)?;
    Ok(std::time::Duration::new(seconds, nanos))
}

/// Makes this process the leader of a new session, whose ID is its pid.
pub fn setsid() -> Result<()> {
    // SAFETY: setsid takes nothing and touches no memory.
    check(unsafe { libc::setsid() }).map(drop)
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
/// `None` while it runs, and while it is only stopped. macOS reports a stop
/// here although only exits are asked for (bevy-final finding 020), so the
/// report's code decides.
pub fn ended(pid: Pid) -> Result<Option<Status>> {
    let id = libc::id_t::try_from(pid.as_raw()).map_err(|_| Errno::INVAL)?;
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` is a whole siginfo_t, which waitid fills; with WNOHANG
    // and nothing to report, it leaves it zeroed.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
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

/// Reaps `pid`, a child of this process, if it has ended: `None` while it
/// runs.
pub fn reap(pid: Pid) -> Result<Option<Status>> {
    let mut status: libc::c_int = 0;
    // SAFETY: `status` is a live int that waitpid writes.
    let reaped = check(unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) })?;
    if reaped == 0 {
        return Ok(None);
    }
    // Without WUNTRACED only an exit or a kill is reported.
    Ok(Some(if libc::WIFSIGNALED(status) {
        Status::Signalled(libc::WTERMSIG(status))
    } else {
        Status::Exited(libc::WEXITSTATUS(status))
    }))
}

/// Makes this process the reaper of its descendants' orphans: a program
/// whose parent exits (a double fork, a daemon) becomes its child, not
/// init's, so it stays in its [`tree`], where its system calls may be read,
/// and this process must reap it ([`reap_orphans`]).
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn adopt_orphans() -> Result<()> {
    // SAFETY: prctl with PR_SET_CHILD_SUBREAPER takes numbers and touches
    // no memory.
    check(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) }).map(drop)
}

/// Reaps every child of this process that has exited and that `kept` does
/// not keep: the orphans it adopted ([`adopt_orphans`]). Its own children
/// are kept, for whoever waits for them.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn reap_orphans(kept: impl Fn(Pid) -> bool) {
    for pid in children("self").into_iter().filter(|pid| !kept(*pid)) {
        let _ = reap(pid);
    }
}

/// macOS has no reaper of orphans but launchd: none are adopted.
#[cfg(target_os = "macos")]
pub fn adopt_orphans() -> Result<()> {
    Ok(())
}

/// macOS: none were adopted ([`adopt_orphans`]), so none are reaped.
#[cfg(target_os = "macos")]
pub fn reap_orphans(_kept: impl Fn(Pid) -> bool) {}

/// The processes this one started, those they started in turn, and the
/// orphans it adopted ([`adopt_orphans`]); not itself. A runaway tree is
/// cut short past 4,096.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn tree() -> Vec<Pid> {
    below("self")
}

/// The processes under `root` (or `self`), by each one's threads'
/// children: a runaway tree is cut short past 4,096.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn below(root: impl fmt::Display) -> Vec<Pid> {
    let mut found = children(root);
    let mut next = 0;
    while let Some(pid) = found.get(next).copied()
        && found.len() <= 4096
    {
        next = next.saturating_add(1);
        found.extend(children(pid));
    }
    found
}

/// The children of `pid` (or `self`), by each of its threads'.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn children(pid: impl fmt::Display) -> Vec<Pid> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task"));
    (tasks.into_iter().flatten().filter_map(|t| t.ok()))
        .flat_map(|task| std::fs::read_to_string(task.path().join("children")))
        .flat_map(|children| {
            (children.split_whitespace())
                .filter_map(|c| Pid::from_raw(c.parse().ok()?))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// macOS has no reaper of orphans but launchd: an orphan leaves this
/// process's tree, so every process the system lists is taken to be in it.
#[cfg(target_os = "macos")]
pub fn tree() -> Vec<Pid> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    // Room for processes started between the two calls.
    let Some(room) = usize::try_from(count).ok().and_then(|c| c.checked_add(64)) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; room];
    let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: the buffer is `bytes` long and writable.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(listed).unwrap_or(0));
    pids.into_iter().filter_map(Pid::from_raw).collect()
}

/// `pid`'s command line, when the system says.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn argv(pid: Pid) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let words: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|w| !w.is_empty())
        .map(|w| String::from_utf8_lossy(w).into_owned())
        .collect();
    (!words.is_empty()).then_some(words)
}

/// `pid`'s command line: macOS keeps it where only a privileged reader
/// finds it reliably, so it is not said.
#[cfg(target_os = "macos")]
pub fn argv(_pid: Pid) -> Option<Vec<String>> {
    None
}

/// The name of `pid`'s program, when the system says.
#[cfg(any(target_os = "linux", target_os = "android"))]
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
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn cwd(pid: Pid) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The current directory of `pid`, when the system says.
#[cfg(target_os = "macos")]
pub fn cwd(pid: Pid) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let bytes = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
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

    /// A stopped child is not an ended one, on either platform (020); an
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

    /// What `ps` reports as `pid`'s state: `T` while it is stopped.
    fn state(pid: Pid) -> std::result::Result<String, String> {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Polls `state` until `stopped` says it is `wanted`, or a few seconds pass.
    fn wait_stopped(pid: Pid, wanted: bool) -> std::result::Result<bool, String> {
        let deadline = Instant::now().checked_add(Duration::from_secs(5));
        loop {
            if state(pid)?.starts_with('T') == wanted {
                return Ok(true);
            }
            if deadline.is_none_or(|d| Instant::now() > d) {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `Tstp` is SIGTSTP: a child with the default disposition stops until
    /// continued, and one that traps SIGTSTP runs its trap. The system drops
    /// SIGTSTP sent to an orphaned process group, which the test's own group
    /// may be; the child's own group, whose parent (this process) is in the
    /// same session, is not orphaned.
    #[test]
    fn tstp_stops_a_child_until_it_continues() -> std::result::Result<(), String> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        kill(pid, Signal::Tstp).map_err(|e| e.to_string())?;
        assert!(wait_stopped(pid, true)?, "stopped: {:?}", state(pid));
        assert_eq!(ended(pid), Ok(None), "a stop is not an end");
        kill(pid, Signal::Cont).map_err(|e| e.to_string())?;
        assert!(wait_stopped(pid, false)?, "continued: {:?}", state(pid));
        kill(pid, Signal::Kill).map_err(|e| e.to_string())?;
        assert_eq!(wait_ended(pid), Some(Status::Signalled(libc::SIGKILL)));
        let _ = child.try_wait();

        let trapping = Command::new("sh")
            .args(["-c", "trap 'exit 7' TSTP; while :; do sleep 0.05; done"])
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&trapping).ok_or("a pid")?;
        // Give the shell time to install its trap.
        std::thread::sleep(Duration::from_millis(300));
        kill(pid, Signal::Tstp).map_err(|e| e.to_string())?;
        assert_eq!(wait_ended(pid), Some(Status::Exited(7)));
        assert_eq!(reap(pid), Ok(Some(Status::Exited(7))));
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

    #[test]
    fn a_threads_cpu_time_grows_with_its_work_and_not_with_sleep() -> Result<()> {
        let start = thread_cpu_time()?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        let slept = thread_cpu_time()?.saturating_sub(start);
        let mut n = 0u64;
        let before = thread_cpu_time()?;
        while thread_cpu_time()?.saturating_sub(before) < std::time::Duration::from_millis(20) {
            n = std::hint::black_box(n.wrapping_add(1));
        }
        assert!(
            slept < std::time::Duration::from_millis(20),
            "{slept:?} asleep"
        );
        assert!(n > 0);
        Ok(())
    }
}

/// What the programs in a terminal's foreground group are doing, as far as
/// the system says: whether they wait for the terminal, which is when an
/// agent driving them should look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Blocked reading the terminal, or waiting on it with no time limit.
    Reading,
    /// Waiting on the terminal with a time limit: a program that polls for
    /// keys and may draw again on its own.
    Polling,
    /// Blocked on something else: the network, a pipe, another program.
    Waiting,
    /// Running, sleeping on a timer, or starting.
    Busy,
    /// Every thread waits, on something the system will not name (macOS):
    /// the terminal, or anything else.
    Idle,
    /// The system does not say.
    Unknown,
}

/// How the system tells what a terminal's programs are doing, chosen when
/// fux is built: [`Host`].
pub trait Probe {
    /// What the programs in foreground group `group` of the terminal at
    /// `tty` do: among `leader`'s own, else among this process's [`tree`].
    fn activity(group: Pid, tty: &std::path::Path, leader: Pid) -> Activity;
    /// Whether process `pid` itself is blocked reading the terminal at
    /// `tty`.
    fn reads_terminal(pid: Pid, tty: &std::path::Path) -> bool;
}

/// The probe of Linux and Android: by each thread's system call.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub struct Linux;

/// The probe of macOS: by whether each thread runs.
#[cfg(target_os = "macos")]
pub struct Macos;

/// The probe of the system fux is built for.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub type Host = Linux;

/// The probe of the system fux is built for.
#[cfg(target_os = "macos")]
pub type Host = Macos;

/// What a blocked thread waits in, by its system call.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blocked {
    /// read and its kind, on this descriptor.
    Read(u64),
    /// poll and its kind, with no time limit (`true`) or with one, and
    /// what it watches.
    Poll(bool, Watched),
    /// A sleep on a timer.
    Sleep,
    /// A wait for a child.
    Child,
    /// A futex: a thread parked; what the others do decides.
    Futex,
    /// Its system call cannot be read (a program that may not be traced:
    /// set-uid, or non-dumpable).
    Unread,
    /// Ended, its parent yet to reap it (a zombie): it does nothing.
    Ended,
    Other,
}

/// Where the descriptors a poll watches are: an array of `pollfd` in the
/// program's memory and its length (poll), the length and address of the
/// set read from (select), or an epoll descriptor (epoll_wait).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watched {
    Fds(u64, u64),
    Set(u64, u64),
    Epoll(u64),
}

/// The system call numbers this machine uses, by what they wait for.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_arch = "x86_64"
))]
fn blocked_in(nr: u64, args: &[u64]) -> Blocked {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    let forever = |t: u64| t == u64::MAX || t == u64::from(u32::MAX);
    match nr {
        // read, pread64, readv, preadv, splice (`cat` from a terminal).
        0 | 17 | 19 | 295 | 275 => Blocked::Read(arg(0)),
        7 => Blocked::Poll(forever(arg(2)), Watched::Fds(arg(0), arg(1))),
        23 | 270 => Blocked::Poll(arg(4) == 0, Watched::Set(arg(0), arg(1))),
        271 => Blocked::Poll(arg(2) == 0, Watched::Fds(arg(0), arg(1))),
        232 | 281 => Blocked::Poll(forever(arg(3)), Watched::Epoll(arg(0))),
        441 => Blocked::Poll(arg(3) == 0, Watched::Epoll(arg(0))),
        35 | 230 => Blocked::Sleep,
        61 | 247 => Blocked::Child,
        // A futex with a time limit (its fourth argument) wakes on a timer:
        // a sleep (Python's `Event.wait(0.1)`).
        202 if arg(3) != 0 => Blocked::Sleep,
        202 => Blocked::Futex,
        _ => Blocked::Other,
    }
}

/// The system call numbers this machine uses, by what they wait for.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_arch = "aarch64"
))]
fn blocked_in(nr: u64, args: &[u64]) -> Blocked {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    let forever = |t: u64| t == u64::MAX || t == u64::from(u32::MAX);
    match nr {
        // read, readv, pread64, preadv, splice (`cat` from a terminal).
        63 | 65 | 67 | 69 | 76 => Blocked::Read(arg(0)),
        72 => Blocked::Poll(arg(4) == 0, Watched::Set(arg(0), arg(1))),
        73 => Blocked::Poll(arg(2) == 0, Watched::Fds(arg(0), arg(1))),
        22 => Blocked::Poll(forever(arg(3)), Watched::Epoll(arg(0))),
        441 => Blocked::Poll(arg(3) == 0, Watched::Epoll(arg(0))),
        101 | 115 => Blocked::Sleep,
        260 | 95 => Blocked::Child,
        // A futex with a time limit (its fourth argument) wakes on a timer:
        // a sleep (Python's `Event.wait(0.1)`).
        98 if arg(3) != 0 => Blocked::Sleep,
        98 => Blocked::Futex,
        _ => Blocked::Other,
    }
}

/// Another machine: nothing is known of its system call numbers.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn blocked_in(_nr: u64, _args: &[u64]) -> Blocked {
    Blocked::Other
}

/// What thread `tid` of `pid` is blocked in; `None` while it runs.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn thread_blocked(pid: Pid, tid: &str) -> Option<Blocked> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).ok()?;
    match stat.rsplit_once(')')?.1.split_whitespace().next()? {
        "S" => {}
        "Z" | "X" => return Some(Blocked::Ended),
        _ => return None,
    }
    let Ok(call) = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/syscall")) else {
        return Some(Blocked::Unread);
    };
    let mut words = call.split_whitespace();
    let nr: u64 = words.next()?.parse().ok()?;
    let args: Vec<u64> = words
        .take(6)
        .filter_map(|w| u64::from_str_radix(w.trim_start_matches("0x"), 16).ok())
        .collect();
    Some(blocked_in(nr, &args))
}

/// Whether `pid`'s descriptor `fd` is the terminal at `tty` (or its own
/// controlling terminal, as `/dev/tty`).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn is_terminal(pid: Pid, fd: u64, tty: &std::path::Path) -> bool {
    std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
        .is_ok_and(|target| target == tty || target == std::path::Path::new("/dev/tty"))
}

/// Whether a poll in `pid` watches the terminal at `tty` for input: a
/// `pollfd` asking `POLLIN` of it, its bit in the set select reads, or a
/// descriptor of the epoll set asking `EPOLLIN` (its `fdinfo`). Read from
/// the program's memory, which a process that may read its system calls
/// may read too.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn watches_terminal(pid: Pid, watched: Watched, tty: &std::path::Path) -> bool {
    use std::os::unix::fs::FileExt;
    // `len` bytes of its memory at `at`; none at a null address.
    let memory = |at: u64, len: u64| -> Vec<u8> {
        let mut bytes = vec![0; usize::try_from(len).unwrap_or(0)];
        let read = std::fs::File::open(format!("/proc/{pid}/mem"))
            .and_then(|mem| mem.read_exact_at(&mut bytes, at));
        if at == 0 || read.is_err() {
            bytes.clear();
        }
        bytes
    };
    let fds: Vec<u64> = match watched {
        Watched::Fds(at, n) => {
            let array = memory(at, n.min(1024).saturating_mul(8));
            let (pollfds, _) = array.as_chunks::<8>();
            (pollfds.iter())
                .filter_map(|&[a, b, c, d, events, more, _, _]| {
                    let asks = i16::from_ne_bytes([events, more]) & libc::POLLIN != 0;
                    asks.then(|| u64::try_from(i32::from_ne_bytes([a, b, c, d])).ok())?
                })
                .collect()
        }
        Watched::Set(n, at) => {
            let n = n.min(1024);
            let set = memory(at, n.div_ceil(64).saturating_mul(8));
            let (words, _) = set.as_chunks::<8>();
            let words: Vec<u64> = words.iter().map(|w| u64::from_ne_bytes(*w)).collect();
            (0..n)
                .filter(|fd| {
                    let word = usize::try_from(fd / 64).ok().and_then(|i| words.get(i));
                    let bit = u32::try_from(fd % 64).unwrap_or(0);
                    word.is_some_and(|w| w.checked_shr(bit).is_some_and(|w| w & 1 == 1))
                })
                .collect()
        }
        Watched::Epoll(fd) => std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                // `tfd:        5 events:       19 data: …`
                let mut words = line.split_whitespace();
                let fd = (words.next() == Some("tfd:")).then(|| words.next())??;
                let events = (words.next() == Some("events:")).then(|| words.next())??;
                let events = u32::from_str_radix(events, 16).ok()?;
                (events & 1 != 0).then(|| fd.parse().ok())?
            })
            .collect(),
    };
    fds.into_iter().any(|fd| is_terminal(pid, fd, tty))
}

/// What one process of the group does, by its main thread and its others,
/// if anything (not one ended, or waiting for a child): busy while one
/// runs, or the main one sleeps on a timer, or (parked, waiting for the
/// others) another does while none reads; not known while its system call
/// cannot be read; reading while it (or, the main one parked, another)
/// waits for the terminal and none of the others can go on alone, on a
/// timer or polling: else polling, as it may draw again on its own; else
/// waiting.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_activity(pid: Pid, tty: &std::path::Path) -> Option<Activity> {
    let on_terminal = |blocked: Blocked| match blocked {
        Blocked::Read(fd) if is_terminal(pid, fd, tty) => Some(Activity::Reading),
        Blocked::Poll(forever, watched) if watches_terminal(pid, watched, tty) => {
            Some(if forever {
                Activity::Reading
            } else {
                Activity::Polling
            })
        }
        Blocked::Read(_) | Blocked::Poll(..) | Blocked::Other => Some(Activity::Waiting),
        Blocked::Sleep => Some(Activity::Busy),
        Blocked::Unread => Some(Activity::Unknown),
        Blocked::Child | Blocked::Futex | Blocked::Ended => None,
    };
    let main = pid.to_string();
    let Some(blocked) = thread_blocked(pid, &main) else {
        return Some(Activity::Busy);
    };
    if matches!(blocked, Blocked::Child | Blocked::Ended) {
        return None;
    }
    // A main thread asleep is weighed with the others: another may read.
    let (first, mut asleep) = match on_terminal(blocked) {
        Some(Activity::Busy) if matches!(blocked, Blocked::Sleep) => (None, true),
        first => (first, false),
    };
    let (mut reading, mut polling) = (false, false);
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    for task in tasks.filter_map(|t| t.ok()) {
        let Some(tid) = task.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if tid == main {
            continue;
        }
        match thread_blocked(pid, &tid).map(on_terminal) {
            Some(Some(Activity::Reading)) => reading = true,
            Some(Some(Activity::Polling)) => polling = true,
            Some(Some(Activity::Busy)) => asleep = true,
            Some(Some(Activity::Waiting | Activity::Unknown | Activity::Idle) | None) => {}
            None => return Some(Activity::Busy),
        }
    }
    Some(match first {
        Some(Activity::Busy) => Activity::Busy,
        Some(Activity::Waiting | Activity::Idle) => Activity::Waiting,
        Some(Activity::Unknown) => Activity::Unknown,
        Some(Activity::Polling) => Activity::Polling,
        Some(Activity::Reading) if polling || asleep => Activity::Polling,
        Some(Activity::Reading) => Activity::Reading,
        None if reading && (polling || asleep) => Activity::Polling,
        None if reading => Activity::Reading,
        None if asleep => Activity::Busy,
        None if polling => Activity::Polling,
        None => Activity::Waiting,
    })
}

/// What the programs in foreground group `group` of the terminal at `tty`
/// do: busy if one is; else reading if one waits for the terminal, polling
/// if one polls it, waiting if they wait on something else.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn activity(group: Pid, tty: &std::path::Path, leader: Pid) -> Activity {
    let in_group = |pid: &Pid| {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(2)?.parse::<i32>().ok())
                == Some(group.as_raw())
        })
    };
    // The leader's own, and the orphans of the group this process adopted
    // (`adopt_orphans`), its children beside the panes' leaders, with
    // theirs: a few files read, not the whole tree.
    let adopted = children("self")
        .into_iter()
        .filter(|p| *p != leader && in_group(p));
    let roots: Vec<Pid> = std::iter::once(leader).chain(adopted).collect();
    let members: Vec<Pid> = (roots.iter())
        .flat_map(|root| std::iter::once(*root).chain(below(*root)))
        .filter(in_group)
        .collect();
    let seen: Vec<Activity> = (members.into_iter())
        .filter_map(|p| process_activity(p, tty))
        .collect();
    // None in the group (it is changing hands: a program starting or
    // ending), or every one waits for a child of another group: at work.
    if seen.is_empty() {
        return Activity::Busy;
    }
    [
        Activity::Busy,
        Activity::Reading,
        Activity::Polling,
        Activity::Waiting,
    ]
    .into_iter()
    .find(|a| seen.contains(a))
    .unwrap_or(Activity::Unknown)
}

/// Whether process `pid` itself is blocked reading the terminal at `tty`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn reads_terminal(pid: Pid, tty: &std::path::Path) -> bool {
    process_activity(pid, tty) == Some(Activity::Reading)
}

/// Linux and Android: what each thread is blocked in, by its system call
/// (`/proc/PID/task/TID/syscall`).
#[cfg(any(target_os = "linux", target_os = "android"))]
impl Probe for Linux {
    fn activity(group: Pid, tty: &std::path::Path, leader: Pid) -> Activity {
        activity(group, tty, leader)
    }

    fn reads_terminal(pid: Pid, tty: &std::path::Path) -> bool {
        reads_terminal(pid, tty)
    }
}

/// macOS keeps what a thread is blocked in to itself, but says whether
/// each runs or waits: busy while one of the group's runs (or waits on the
/// disk, uninterruptible), idle once every one waits.
#[cfg(target_os = "macos")]
impl Probe for Macos {
    fn activity(group: Pid, _tty: &std::path::Path, _leader: Pid) -> Activity {
        let mut pids: Vec<libc::pid_t> = vec![0; 1024];
        let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
            return Activity::Unknown;
        };
        // SAFETY: the buffer is `bytes` long and writable; the call says
        // how many IDs it wrote.
        let listed =
            unsafe { libc::proc_listpgrppids(group.as_raw(), pids.as_mut_ptr().cast(), bytes) };
        let Ok(listed) = usize::try_from(listed) else {
            return Activity::Unknown;
        };
        pids.truncate(listed);
        let seen: Vec<Option<bool>> = (pids.into_iter())
            .filter_map(Pid::from_raw)
            .filter_map(running)
            .collect();
        // None in the group (it is changing hands: a program starting or
        // ending), or one runs: at work.
        if seen.is_empty() || seen.contains(&Some(true)) {
            Activity::Busy
        } else if seen.contains(&None) {
            Activity::Unknown
        } else {
            Activity::Idle
        }
    }

    /// What a waiting thread waits on is not said: never known to read.
    fn reads_terminal(_pid: Pid, _tty: &std::path::Path) -> bool {
        false
    }
}

/// A thread's recent use of the processor, in tenths of a percent, from
/// which it is taken to run: one blocked in a read was seen said to run at
/// under 4%.
#[cfg(target_os = "macos")]
const IN_USE: i32 = 100;

/// Whether a thread of `pid` runs (or is uninterruptible): `Some(false)`
/// when every one waits, `Some(None)` when its threads cannot be read,
/// `None` when it has none (ended, its parent yet to reap it).
#[cfg(target_os = "macos")]
fn running(pid: Pid) -> Option<Option<bool>> {
    // `sys/proc_info.h`: the IDs of a process's threads, which `libc` lacks.
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    let mut threads: Vec<u64> = vec![0; 256];
    let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(threads.as_slice())) else {
        return Some(None);
    };
    // SAFETY: the buffer is `bytes` long and writable; the call says how
    // many bytes it wrote.
    let written = unsafe {
        libc::proc_pidinfo(
            pid.as_raw(),
            PROC_PIDLISTTHREADS,
            0,
            threads.as_mut_ptr().cast(),
            bytes,
        )
    };
    let written = usize::try_from(written).unwrap_or(0);
    if written == 0 {
        return exists(pid).then_some(None);
    }
    threads.truncate(written.div_euclid(std::mem::size_of::<u64>()));
    let Ok(size) = libc::c_int::try_from(std::mem::size_of::<libc::proc_threadinfo>()) else {
        return Some(None);
    };
    let mut known = false;
    for thread in threads {
        let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::zeroed();
        // SAFETY: the buffer is exactly one `proc_threadinfo`, which the
        // call fills; a short return is checked before the value is read.
        let read = unsafe {
            libc::proc_pidinfo(
                pid.as_raw(),
                libc::PROC_PIDTHREADINFO,
                thread,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        // A thread that ended between the two calls is skipped.
        if read != size {
            continue;
        }
        known = true;
        // SAFETY: fully written, as checked above.
        let info = unsafe { info.assume_init() };
        // A thread blocked in a read is now and then said to run, for an
        // instant, its recent use of the processor near none: it runs when
        // it uses some.
        let runs = info.pth_run_state == libc::TH_STATE_RUNNING && info.pth_cpu_usage >= IN_USE;
        if runs || info.pth_run_state == libc::TH_STATE_UNINTERRUPTIBLE {
            return Some(Some(true));
        }
    }
    Some(known.then_some(false))
}

/// The probe's pure parts: which system calls wait for what, and which
/// descriptors a poll watches, read back from this process's own memory.
#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod probe {
    use super::*;

    const NEVER: u64 = u64::MAX;

    /// Each kind of wait, by its system call and the arguments that say
    /// whether it has a time limit.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_64_system_calls_are_told_by_what_they_wait_for() {
        for read in [0, 17, 19, 295, 275] {
            assert_eq!(blocked_in(read, &[5]), Blocked::Read(5), "{read}");
        }
        let fds = Watched::Fds(9, 2);
        assert_eq!(blocked_in(7, &[9, 2, NEVER]), Blocked::Poll(true, fds));
        let int_never = u64::from(u32::MAX);
        assert_eq!(blocked_in(7, &[9, 2, int_never]), Blocked::Poll(true, fds));
        assert_eq!(blocked_in(7, &[9, 2, 100]), Blocked::Poll(false, fds));
        let set = Watched::Set(4, 9);
        for select in [23, 270] {
            assert_eq!(
                blocked_in(select, &[4, 9, 0, 0, 0]),
                Blocked::Poll(true, set)
            );
            assert_eq!(
                blocked_in(select, &[4, 9, 0, 0, 7]),
                Blocked::Poll(false, set)
            );
        }
        assert_eq!(blocked_in(271, &[9, 2, 0]), Blocked::Poll(true, fds));
        assert_eq!(blocked_in(271, &[9, 2, 7]), Blocked::Poll(false, fds));
        let epoll = Watched::Epoll(3);
        for epoll_wait in [232, 281] {
            assert_eq!(
                blocked_in(epoll_wait, &[3, 9, 1, NEVER]),
                Blocked::Poll(true, epoll)
            );
            assert_eq!(
                blocked_in(epoll_wait, &[3, 9, 1, 100]),
                Blocked::Poll(false, epoll)
            );
        }
        assert_eq!(blocked_in(441, &[3, 9, 1, 0]), Blocked::Poll(true, epoll));
        assert_eq!(blocked_in(441, &[3, 9, 1, 7]), Blocked::Poll(false, epoll));
        for (nr, kind) in [
            (35, Blocked::Sleep),
            (230, Blocked::Sleep),
            (61, Blocked::Child),
            (247, Blocked::Child),
            (202, Blocked::Futex),
            (1, Blocked::Other),
        ] {
            assert_eq!(blocked_in(nr, &[]), kind, "{nr}");
        }
        assert_eq!(
            blocked_in(202, &[9, 0, 0, 7]),
            Blocked::Sleep,
            "a futex with a limit"
        );
    }

    /// aarch64's numbers, which share few with x86_64's.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn aarch64_system_calls_are_told_by_what_they_wait_for() {
        for read in [63, 65, 67, 69, 76] {
            assert_eq!(blocked_in(read, &[5]), Blocked::Read(5), "{read}");
        }
        let set = Watched::Set(4, 9);
        assert_eq!(blocked_in(72, &[4, 9, 0, 0, 0]), Blocked::Poll(true, set));
        assert_eq!(blocked_in(72, &[4, 9, 0, 0, 7]), Blocked::Poll(false, set));
        let fds = Watched::Fds(9, 2);
        assert_eq!(blocked_in(73, &[9, 2, 0]), Blocked::Poll(true, fds));
        assert_eq!(blocked_in(73, &[9, 2, 7]), Blocked::Poll(false, fds));
        let epoll = Watched::Epoll(3);
        assert_eq!(
            blocked_in(22, &[3, 9, 1, NEVER]),
            Blocked::Poll(true, epoll)
        );
        assert_eq!(blocked_in(22, &[3, 9, 1, 100]), Blocked::Poll(false, epoll));
        assert_eq!(blocked_in(441, &[3, 9, 1, 0]), Blocked::Poll(true, epoll));
        assert_eq!(blocked_in(441, &[3, 9, 1, 7]), Blocked::Poll(false, epoll));
        for (nr, kind) in [
            (101, Blocked::Sleep),
            (115, Blocked::Sleep),
            (260, Blocked::Child),
            (95, Blocked::Child),
            (98, Blocked::Futex),
            (64, Blocked::Other),
        ] {
            assert_eq!(blocked_in(nr, &[]), kind, "{nr}");
        }
        assert_eq!(
            blocked_in(98, &[9, 0, 0, 7]),
            Blocked::Sleep,
            "a futex with a limit"
        );
    }

    /// This process, a descriptor of it that stands for its terminal (its
    /// own program, held open while it is used), and that file's path.
    fn own() -> std::result::Result<(Pid, std::fs::File, u64, std::path::PathBuf), String> {
        use std::os::fd::AsRawFd;
        let pid = i32::try_from(std::process::id())
            .ok()
            .and_then(Pid::from_raw);
        let path = std::env::current_exe().map_err(|e| e.to_string())?;
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let fd = u64::try_from(file.as_raw_fd()).map_err(|e| e.to_string())?;
        Ok((pid.ok_or("no pid")?, file, fd, path))
    }

    /// Where `bytes` are in this process's memory.
    fn address(bytes: &[u8]) -> u64 {
        u64::try_from(bytes.as_ptr().addr()).unwrap_or(0)
    }

    /// A poll's `pollfd` array counts a descriptor it asks `POLLIN` of.
    #[test]
    fn a_poll_watches_the_descriptors_it_asks_input_of() -> std::result::Result<(), String> {
        let (pid, _held, fd, path) = own()?;
        let tty = path.as_path();
        let pollfd = |events: i16| -> Vec<u8> {
            let fd = i32::try_from(fd).unwrap_or(-1).to_ne_bytes();
            [&fd[..], &events.to_ne_bytes()[..], &[0, 0][..]].concat()
        };
        let asks = pollfd(libc::POLLIN);
        assert!(watches_terminal(pid, Watched::Fds(address(&asks), 1), tty));
        let other = pollfd(libc::POLLPRI);
        assert!(!watches_terminal(
            pid,
            Watched::Fds(address(&other), 1),
            tty
        ));
        let elsewhere = std::path::Path::new("/nowhere");
        assert!(!watches_terminal(
            pid,
            Watched::Fds(address(&asks), 1),
            elsewhere
        ));
        assert!(!watches_terminal(pid, Watched::Fds(0, 1), tty));
        Ok(())
    }

    /// A select's set counts a descriptor by its bit, below its count.
    #[test]
    fn a_select_watches_the_descriptors_whose_bits_it_sets() -> std::result::Result<(), String> {
        let (pid, _held, fd, path) = own()?;
        let tty = path.as_path();
        let n = fd.saturating_add(1);
        let set = |bits: &[u64]| -> Vec<u8> {
            let mut words = vec![0u64; usize::try_from(n.div_ceil(64)).unwrap_or(0)];
            for bit in bits {
                if let Some(w) = usize::try_from(bit / 64)
                    .ok()
                    .and_then(|i| words.get_mut(i))
                {
                    *w |= 1 << (bit % 64);
                }
            }
            words.iter().flat_map(|w| w.to_ne_bytes()).collect()
        };
        let watched = set(&[fd]);
        assert!(watches_terminal(
            pid,
            Watched::Set(n, address(&watched)),
            tty
        ));
        let others = set(&[0]);
        assert!(!watches_terminal(
            pid,
            Watched::Set(n, address(&others)),
            tty
        ));
        let none = set(&[]);
        assert!(!watches_terminal(pid, Watched::Set(n, address(&none)), tty));
        // Below its count only.
        assert!(!watches_terminal(
            pid,
            Watched::Set(fd, address(&watched)),
            tty
        ));
        Ok(())
    }
}
