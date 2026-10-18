//! Processes: their IDs, signals, sessions, and how they end.
use crate::errno::{Errno, Result, check, fit};
use std::fmt;
use std::num::NonZeroI32;
use std::path::{Path, PathBuf};

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
    /// `Kill`, and any of these for `fux signal`), and `Tstp`, which koh's client sends itself to suspend: a shell reports a job `Tstp` stopped as "Stopped", and one
    /// `Stop` stopped as "Stopped (signal)". The last six are those a
    /// program dies of on its own, which fux names when it tells a death.
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

/// Makes this process the leader of a new session, whose ID is its pid.
fn setsid() -> Result<()> {
    // SAFETY: setsid takes nothing and touches no memory.
    check(unsafe { libc::setsid() }).map(drop)
}

/// Has `command`'s program, once spawned, lead a new session, holding no
/// descriptor but its stdin, stdout and stderr: what this process holds
/// without close-on-exec, inherited from a shell that leaks descriptors or
/// opened by a thread racing the spawn, goes no further. With `terminal`,
/// its stdin, a terminal, becomes the session's controlling terminal. The
/// spawn clears the signal mask, and `exec` restores the dispositions Rust
/// and signal handlers changed, so the program starts as it would from a
/// shell.
pub fn lead_session(command: &mut std::process::Command, terminal: bool) {
    use std::os::unix::process::CommandExt;
    // Listed now: between fork and exec nothing may allocate.
    let listed = crate::io::open_end();
    let lead = move || -> std::io::Result<()> {
        crate::io::cloexec_from(3, listed)?;
        setsid()?;
        if terminal {
            let set = crate::terminal::request(libc::TIOCSCTTY);
            let steal: libc::c_int = 0;
            // SAFETY: TIOCSCTTY takes an int and touches no memory.
            check(unsafe { libc::ioctl(0, set, steal) })?;
        }
        Ok(())
    };
    // SAFETY: `lead` runs between fork and exec, where only
    // async-signal-safe calls may be made, and makes only those:
    // close_range, or fcntl; setsid; ioctl. It allocates nothing, takes
    // no lock, and its errors are numbers.
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
/// are asked for; macOS reports a stop all the same (bevy-final finding
/// 020), so the report's code decides.
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
/// init's, so it stays in its [`tree`], where its system calls may be read,
/// and this process must reap it ([`reap_orphans`]). macOS has no reaper of
/// orphans but launchd: none are adopted.
pub fn adopt_orphans() -> Result<()> {
    // SAFETY: prctl with PR_SET_CHILD_SUBREAPER takes numbers and touches
    // no memory.
    #[cfg(not(target_os = "macos"))]
    check(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) })?;
    Ok(())
}

/// Reaps every child of this process that has exited and that `kept` does
/// not keep: the orphans it adopted ([`adopt_orphans`]); none on macOS. Its
/// own children are kept, for whoever waits for them.
pub fn reap_orphans(kept: impl Fn(Pid) -> bool) {
    #[cfg(not(target_os = "macos"))]
    for pid in children("self").into_iter().filter(|pid| !kept(*pid)) {
        let _ = reap(pid);
    }
    #[cfg(target_os = "macos")]
    let _ = kept;
}

/// The processes this one started, those they started in turn, and the
/// orphans it adopted ([`adopt_orphans`]); not itself. A runaway tree is
/// cut short past 4,096.
#[cfg(not(target_os = "macos"))]
pub fn tree() -> Vec<Pid> {
    below("self")
}

/// The processes under `root` (or `self`), by each one's threads'
/// children: a runaway tree is cut short past 4,096.
#[cfg(not(target_os = "macos"))]
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
#[cfg(not(target_os = "macos"))]
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
    let room = usize::try_from(count).unwrap_or(0).saturating_add(64);
    let pids = listed(room, |buffer, bytes| {
        // SAFETY: the buffer is `bytes` long and writable.
        unsafe { libc::proc_listallpids(buffer, bytes) }
    });
    (pids.into_iter().flatten())
        .filter_map(Pid::from_raw)
        .collect()
}

/// What a call fills a buffer of `room` `T`s with, given the buffer and
/// its length in bytes, and answering how many it wrote: `None` if it
/// failed.
#[cfg(target_os = "macos")]
fn listed<T: Default + Clone>(
    room: usize,
    call: impl FnOnce(*mut libc::c_void, libc::c_int) -> libc::c_int,
) -> Option<Vec<T>> {
    let mut list = vec![T::default(); room];
    let bytes = libc::c_int::try_from(size_of_val(list.as_slice())).ok()?;
    let written = usize::try_from(call(list.as_mut_ptr().cast(), bytes)).ok()?;
    list.truncate(written);
    Some(list)
}

/// `pid`'s command line, when the system says.
#[cfg(not(target_os = "macos"))]
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

/// What the programs in the foreground group of the terminal whose master
/// is `master` do, as fux decides it: [`activity`] of its foreground group
/// (with none, `leader`'s own group: busy once none is left in it), with a
/// read that may return on a timer (`VMIN` and `VTIME`) counted as
/// [`Activity::Polling`], and a shell back at its prompt counted as
/// [`Activity::Reading`] whatever jobs it left running in its group.
/// - `since_key`: how long ago input was last written to the terminal;
///   `Duration::MAX` if none was (a read timed from a key waits with no
///   limit before one).
/// - `answered`: whether the screen changed after that input: the program
///   answered it.
/// - `prompt`: whether the shell `leader` runs has ended its last command
///   and is back at its prompt, as its marks say (OSC 133 `D`, and no `C`
///   since). Then a shell that reads the terminal itself reads, though its
///   group is busy, polling or waiting.
///
/// On macOS, as [`activity`] there, it gives only `Busy`, `Idle` or
/// `Unknown`.
pub fn terminal_activity(
    master: &crate::pty::Master,
    leader: Pid,
    since_key: std::time::Duration,
    answered: bool,
    prompt: bool,
) -> Activity {
    let group = crate::terminal::foreground_group(master).unwrap_or(leader);
    // A read timed from a key (`VMIN` and `VTIME`) times the gap after the
    // first key it took: once the program wrote after the last key (and
    // the terminal echoes nothing), the read that took it returned, and
    // the one it is in has taken no key yet, so it waits with no limit.
    let timed = |t: &crate::terminal::Termios| {
        let since = match answered && !t.modes().echo {
            true => std::time::Duration::MAX,
            false => since_key,
        };
        t.timed_reads(since)
    };
    let tty = master.slave_path();
    let found = match activity(group, tty, leader) {
        // A read with a time limit is a poll: the program goes on alone
        // when it runs out.
        Activity::Reading if crate::terminal::attributes(master).is_ok_and(|t| timed(&t)) => {
            Activity::Polling
        }
        found @ (Activity::Reading
        | Activity::Polling
        | Activity::Waiting
        | Activity::Busy
        | Activity::Idle
        | Activity::Unknown) => found,
    };
    // A shell back at its prompt, reading, its last command ended: what
    // else runs in its group is a job it left there (job control off, or
    // fish's disowned start-up work), not the command typed.
    let left = match found {
        Activity::Busy | Activity::Polling | Activity::Waiting => {
            prompt && reads_terminal(leader, tty)
        }
        Activity::Reading | Activity::Idle | Activity::Unknown => false,
    };
    if left { Activity::Reading } else { found }
}

/// What a blocked thread waits in, by its system call.
#[cfg(not(target_os = "macos"))]
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

/// Where the descriptors a poll watches are: an array of `len` `pollfd`s
/// at `at` in the program's memory (poll), the set of the descriptors below
/// `len` read from, at `at` (select), or an epoll descriptor (epoll_wait).
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watched {
    Fds { at: u64, len: u64 },
    Set { len: u64, at: u64 },
    Epoll(u64),
}

/// What one thread of a process does, as the probe weighs it.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy)]
enum Thread {
    /// Running, or about to.
    Runs,
    /// Waiting for the terminal with no time limit.
    Reads,
    /// Waiting for the terminal with a time limit.
    Polls,
    /// Blocked on something else.
    Waits,
    /// Asleep on a timer.
    Sleeps,
    /// Blocked in what cannot be read.
    Unread,
    /// Parked on a futex: what the others do decides.
    Parked,
    /// Waiting for a child, or ended: doing nothing of its own.
    Out,
}

/// What a system call waits for, by its number (`libc::SYS_*`, this
/// machine's) and its arguments.
#[cfg(all(
    not(target_os = "macos"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn blocked_in(nr: libc::c_long, args: &[u64]) -> Blocked {
    let arg = |i: usize| args.get(i).copied().unwrap_or(0);
    // An `int` timeout, its low 32 bits, is no limit if it is negative.
    let forever = |t: u64| t & 0x8000_0000 != 0;
    let (fds, set, epoll) = (
        Watched::Fds {
            at: arg(0),
            len: arg(1),
        },
        Watched::Set {
            len: arg(0),
            at: arg(1),
        },
        Watched::Epoll(arg(0)),
    );
    match nr {
        // read and its kind; splice (`cat` from a terminal).
        libc::SYS_read
        | libc::SYS_pread64
        | libc::SYS_readv
        | libc::SYS_preadv
        | libc::SYS_splice => Blocked::Read(arg(0)),
        // poll, select and epoll_wait are x86_64's alone: aarch64 has only
        // their later forms.
        #[cfg(target_arch = "x86_64")]
        libc::SYS_poll => Blocked::Poll(forever(arg(2)), fds),
        #[cfg(target_arch = "x86_64")]
        libc::SYS_select => Blocked::Poll(arg(4) == 0, set),
        libc::SYS_pselect6 => Blocked::Poll(arg(4) == 0, set),
        libc::SYS_ppoll => Blocked::Poll(arg(2) == 0, fds),
        #[cfg(target_arch = "x86_64")]
        libc::SYS_epoll_wait => Blocked::Poll(forever(arg(3)), epoll),
        libc::SYS_epoll_pwait => Blocked::Poll(forever(arg(3)), epoll),
        libc::SYS_epoll_pwait2 => Blocked::Poll(arg(3) == 0, epoll),
        libc::SYS_nanosleep | libc::SYS_clock_nanosleep => Blocked::Sleep,
        libc::SYS_wait4 | libc::SYS_waitid => Blocked::Child,
        // A futex with a time limit (its fourth argument) wakes on a timer:
        // a sleep (Python's `Event.wait(0.1)`).
        libc::SYS_futex if arg(3) != 0 => Blocked::Sleep,
        libc::SYS_futex => Blocked::Futex,
        _ => Blocked::Other,
    }
}

/// Another machine: nothing is known of its system call numbers.
#[cfg(all(
    not(target_os = "macos"),
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn blocked_in(_nr: libc::c_long, _args: &[u64]) -> Blocked {
    Blocked::Other
}

/// What thread `tid` of `pid` is blocked in; `None` while it runs.
#[cfg(not(target_os = "macos"))]
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
    // `running`, or the call's number: -1 (`-1 SP PC`) while it is blocked
    // outside one.
    let mut words = call.split_whitespace();
    let nr: libc::c_long = words.next()?.parse().ok()?;
    let args: Vec<u64> = words
        .take(6)
        .filter_map(|w| u64::from_str_radix(w.trim_start_matches("0x"), 16).ok())
        .collect();
    Some(blocked_in(nr, &args))
}

/// Whether `pid`'s descriptor `fd` is the terminal at `tty` (or its own
/// controlling terminal, as `/dev/tty`).
#[cfg(not(target_os = "macos"))]
fn is_terminal(pid: Pid, fd: u64, tty: &Path) -> bool {
    std::fs::read_link(format!("/proc/{pid}/fd/{fd}"))
        .is_ok_and(|target| target == tty || target == Path::new("/dev/tty"))
}

/// Whether a poll in `pid` watches the terminal at `tty` for input: a
/// `pollfd` asking `POLLIN` of it, its bit in the set select reads, or a
/// descriptor of the epoll set asking `EPOLLIN` (its `fdinfo`). Read from
/// the program's memory, which a process that may read its system calls
/// may read too; `None` if what says cannot be read.
#[cfg(not(target_os = "macos"))]
fn watches_terminal(pid: Pid, watched: Watched, tty: &Path) -> Option<bool> {
    use std::os::unix::fs::FileExt;
    // `len` bytes of its memory at `at`; none at a null address.
    let memory = |at: u64, len: u64| -> Option<Vec<u8>> {
        if at == 0 {
            return Some(Vec::new());
        }
        let mut bytes = vec![0; usize::try_from(len).unwrap_or(0)];
        std::fs::File::open(format!("/proc/{pid}/mem"))
            .and_then(|mem| mem.read_exact_at(&mut bytes, at))
            .ok()?;
        Some(bytes)
    };
    let fds: Vec<u64> = match watched {
        Watched::Fds { at, len: n } => {
            let array = memory(at, n.min(1024).saturating_mul(8))?;
            let (pollfds, _) = array.as_chunks::<8>();
            (pollfds.iter())
                .filter_map(|&[a, b, c, d, events, more, _, _]| {
                    let asks = i16::from_ne_bytes([events, more]) & libc::POLLIN != 0;
                    asks.then(|| u64::try_from(i32::from_ne_bytes([a, b, c, d])).ok())?
                })
                .collect()
        }
        Watched::Set { len: n, at } => {
            let n = n.min(1024);
            let set = memory(at, n.div_ceil(64).saturating_mul(8))?;
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
            .ok()?
            .lines()
            .filter_map(|line| {
                // `tfd:        5 events:       19 data: …`
                let mut words = line.split_whitespace();
                let fd = (words.next() == Some("tfd:")).then(|| words.next())??;
                let events = (words.next() == Some("events:")).then(|| words.next())??;
                let events = u32::from_str_radix(events, 16).ok()?;
                (events & libc::EPOLLIN.cast_unsigned() != 0).then(|| fd.parse().ok())?
            })
            .collect(),
    };
    Some(fds.into_iter().any(|fd| is_terminal(pid, fd, tty)))
}

/// What one process of the group does, by its main thread and its others,
/// if anything (not one ended, or waiting for a child): busy while one
/// runs, or the main one sleeps on a timer, or (parked, waiting for the
/// others) another does, while none waits on the terminal; not known while its system call
/// cannot be read; reading while it (or, the main one parked, another)
/// waits for the terminal and none of the others can go on alone, on a
/// timer or polling: else polling, as it may draw again on its own; else
/// waiting.
#[cfg(not(target_os = "macos"))]
fn process_activity(pid: Pid, tty: &Path) -> Option<Activity> {
    let thread = |tid: &str| match thread_blocked(pid, tid) {
        None => Thread::Runs,
        Some(Blocked::Read(fd)) if is_terminal(pid, fd, tty) => Thread::Reads,
        Some(Blocked::Poll(forever, watched)) => match watches_terminal(pid, watched, tty) {
            Some(true) if forever => Thread::Reads,
            Some(true) => Thread::Polls,
            Some(false) => Thread::Waits,
            None => Thread::Unread,
        },
        Some(Blocked::Read(_) | Blocked::Other) => Thread::Waits,
        Some(Blocked::Sleep) => Thread::Sleeps,
        Some(Blocked::Unread) => Thread::Unread,
        Some(Blocked::Futex) => Thread::Parked,
        Some(Blocked::Child | Blocked::Ended) => Thread::Out,
    };
    let main = pid.to_string();
    let first = thread(&main);
    let (mut reading, mut polling, mut asleep) = (false, false, false);
    match first {
        Thread::Runs => return Some(Activity::Busy),
        Thread::Out => return None,
        // A main thread asleep is weighed with the others: another may read.
        Thread::Sleeps => asleep = true,
        Thread::Reads | Thread::Polls | Thread::Waits | Thread::Unread | Thread::Parked => {}
    }
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    for task in tasks.filter_map(|t| t.ok()) {
        let Some(tid) = task.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if tid == main {
            continue;
        }
        match thread(&tid) {
            Thread::Runs => return Some(Activity::Busy),
            Thread::Reads => reading = true,
            Thread::Polls => polling = true,
            Thread::Sleeps => asleep = true,
            Thread::Waits | Thread::Unread | Thread::Parked | Thread::Out => {}
        }
    }
    Some(match first {
        Thread::Runs => Activity::Busy,
        Thread::Out => return None,
        Thread::Waits => Activity::Waiting,
        Thread::Unread => Activity::Unknown,
        Thread::Polls => Activity::Polling,
        Thread::Reads if polling || asleep => Activity::Polling,
        Thread::Reads => Activity::Reading,
        Thread::Sleeps | Thread::Parked if reading && !polling && !asleep => Activity::Reading,
        // A thread that waits on the terminal, with or without a limit,
        // while another (or the main one) sleeps on a timer: a program's
        // input thread beside its frame loop (rig's, a game's): it polls.
        Thread::Sleeps | Thread::Parked if reading || polling => Activity::Polling,
        Thread::Sleeps | Thread::Parked if asleep => Activity::Busy,
        Thread::Sleeps | Thread::Parked => Activity::Waiting,
    })
}

/// What the programs in foreground group `group` of the terminal at `tty`
/// do, among `leader`'s own, else among this process's [`tree`]: busy if
/// one is; else reading if one waits for the terminal, polling if one polls
/// it, waiting if they wait on something else. Linux and Android tell by
/// what each thread is blocked in, its system call
/// (`/proc/PID/task/TID/syscall`).
#[cfg(not(target_os = "macos"))]
pub fn activity(group: Pid, tty: &Path, leader: Pid) -> Activity {
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
#[cfg(not(target_os = "macos"))]
pub fn reads_terminal(pid: Pid, tty: &Path) -> bool {
    process_activity(pid, tty) == Some(Activity::Reading)
}

/// macOS keeps what a thread is blocked in to itself, but says whether
/// each runs or waits: busy while one of the group's runs (or waits on the
/// disk, uninterruptible), idle once every one waits.
#[cfg(target_os = "macos")]
pub fn activity(group: Pid, _tty: &Path, _leader: Pid) -> Activity {
    let pids = listed(1024, |buffer, bytes| {
        // SAFETY: the buffer is `bytes` long and writable; the call says
        // how many IDs it wrote.
        unsafe { libc::proc_listpgrppids(group.as_raw(), buffer, bytes) }
    });
    let Some(pids) = pids else {
        return Activity::Unknown;
    };
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

/// Whether process `pid` itself is blocked reading the terminal at `tty`:
/// never known on macOS, which does not say what a waiting thread waits on.
#[cfg(target_os = "macos")]
pub fn reads_terminal(_pid: Pid, _tty: &Path) -> bool {
    false
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
    let threads: Vec<u64> = listed(256, |buffer, bytes| {
        // SAFETY: the buffer is `bytes` long and writable; the call says how
        // many bytes it wrote, a thread's ID 8 of them.
        let written =
            unsafe { libc::proc_pidinfo(pid.as_raw(), PROC_PIDLISTTHREADS, 0, buffer, bytes) };
        written.div_euclid(8)
    })
    .unwrap_or_default();
    if threads.is_empty() {
        return exists(pid).then_some(None);
    }
    let Ok(size) = libc::c_int::try_from(size_of::<libc::proc_threadinfo>()) else {
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
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn system_calls_are_told_by_what_they_wait_for() {
        use libc::*;
        for read in [SYS_read, SYS_pread64, SYS_readv, SYS_preadv, SYS_splice] {
            assert_eq!(blocked_in(read, &[5]), Blocked::Read(5), "{read}");
        }
        let fds = Watched::Fds { at: 9, len: 2 };
        let set = Watched::Set { len: 4, at: 9 };
        let epoll = Watched::Epoll(3);
        #[cfg(target_arch = "x86_64")]
        {
            let poll = SYS_poll;
            assert_eq!(blocked_in(poll, &[9, 2, NEVER]), Blocked::Poll(true, fds));
            let int_never = u64::from(u32::MAX);
            assert_eq!(
                blocked_in(poll, &[9, 2, int_never]),
                Blocked::Poll(true, fds)
            );
            assert_eq!(blocked_in(poll, &[9, 2, 100]), Blocked::Poll(false, fds));
            let select = SYS_select;
            assert_eq!(
                blocked_in(select, &[4, 9, 0, 0, 0]),
                Blocked::Poll(true, set)
            );
            assert_eq!(
                blocked_in(select, &[4, 9, 0, 0, 7]),
                Blocked::Poll(false, set)
            );
            let epoll_wait = SYS_epoll_wait;
            assert_eq!(
                blocked_in(epoll_wait, &[3, 9, 1, NEVER]),
                Blocked::Poll(true, epoll)
            );
            assert_eq!(
                blocked_in(epoll_wait, &[3, 9, 1, 100]),
                Blocked::Poll(false, epoll)
            );
        }
        let pselect = SYS_pselect6;
        assert_eq!(
            blocked_in(pselect, &[4, 9, 0, 0, 0]),
            Blocked::Poll(true, set)
        );
        assert_eq!(
            blocked_in(pselect, &[4, 9, 0, 0, 7]),
            Blocked::Poll(false, set)
        );
        let ppoll = SYS_ppoll;
        assert_eq!(blocked_in(ppoll, &[9, 2, 0]), Blocked::Poll(true, fds));
        assert_eq!(blocked_in(ppoll, &[9, 2, 7]), Blocked::Poll(false, fds));
        let pwait = SYS_epoll_pwait;
        assert_eq!(
            blocked_in(pwait, &[3, 9, 1, NEVER]),
            Blocked::Poll(true, epoll)
        );
        assert_eq!(
            blocked_in(pwait, &[3, 9, 1, 100]),
            Blocked::Poll(false, epoll)
        );
        // Any negative int is no limit, as poll(2) and epoll_wait(2) read it.
        let int_minus_two = u64::from((-2i32).cast_unsigned());
        for minus_two in [(-2i64).cast_unsigned(), int_minus_two] {
            assert_eq!(
                blocked_in(pwait, &[3, 9, 1, minus_two]),
                Blocked::Poll(true, epoll)
            );
        }
        let pwait2 = SYS_epoll_pwait2;
        assert_eq!(
            blocked_in(pwait2, &[3, 9, 1, 0]),
            Blocked::Poll(true, epoll)
        );
        assert_eq!(
            blocked_in(pwait2, &[3, 9, 1, 7]),
            Blocked::Poll(false, epoll)
        );
        for (call, kind) in [
            (SYS_nanosleep, Blocked::Sleep),
            (SYS_clock_nanosleep, Blocked::Sleep),
            (SYS_wait4, Blocked::Child),
            (SYS_waitid, Blocked::Child),
            (SYS_futex, Blocked::Futex),
            (SYS_write, Blocked::Other),
            // Blocked outside a system call.
            (-1, Blocked::Other),
        ] {
            assert_eq!(blocked_in(call, &[]), kind, "{call}");
        }
        assert_eq!(
            blocked_in(SYS_futex, &[9, 0, 0, 7]),
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
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Fds {
                    at: address(&asks),
                    len: 1
                },
                tty
            ),
            Some(true)
        );
        let other = pollfd(libc::POLLPRI);
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Fds {
                    at: address(&other),
                    len: 1
                },
                tty
            ),
            Some(false)
        );
        let elsewhere = std::path::Path::new("/nowhere");
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Fds {
                    at: address(&asks),
                    len: 1
                },
                elsewhere
            ),
            Some(false)
        );
        assert_eq!(
            watches_terminal(pid, Watched::Fds { at: 0, len: 1 }, tty),
            Some(false)
        );
        Ok(())
    }

    /// An epoll set whose `fdinfo` cannot be read is not known to watch
    /// nothing: here, a process that is gone.
    #[test]
    fn an_unreadable_epoll_set_is_not_known() -> std::result::Result<(), String> {
        let mut child = std::process::Command::new("true")
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = Pid::of(&child).ok_or("a pid")?;
        child.wait().map_err(|e| e.to_string())?;
        let tty = std::path::Path::new("/dev/tty");
        assert_eq!(watches_terminal(pid, Watched::Epoll(3), tty), None);
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
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Set {
                    len: n,
                    at: address(&watched)
                },
                tty
            ),
            Some(true)
        );
        let others = set(&[0]);
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Set {
                    len: n,
                    at: address(&others)
                },
                tty
            ),
            Some(false)
        );
        let none = set(&[]);
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Set {
                    len: n,
                    at: address(&none)
                },
                tty
            ),
            Some(false)
        );
        // Below its count only.
        assert_eq!(
            watches_terminal(
                pid,
                Watched::Set {
                    len: fd,
                    at: address(&watched)
                },
                tty
            ),
            Some(false)
        );
        Ok(())
    }
}
