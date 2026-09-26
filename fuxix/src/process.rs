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

/// The signals fux sends, and `Tstp`, which koh's client sends itself to
/// suspend: a shell reports a job `Tstp` stopped as "Stopped", and one
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
        }
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

/// Makes this process the leader of a new session: the session's ID.
pub fn setsid() -> Result<Pid> {
    // SAFETY: setsid takes nothing and touches no memory.
    let session = check(unsafe { libc::setsid() })?;
    Pid::from_raw(session).ok_or(Errno::INVAL)
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

/// Every process the system lists.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn processes() -> Vec<Pid> {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
                .filter_map(Pid::from_raw)
                .collect()
        })
        .unwrap_or_default()
}

/// Every process the system lists.
#[cfg(target_os = "macos")]
pub fn processes() -> Vec<Pid> {
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

    /// Every process's session can be asked for, kernel threads included,
    /// whose session is 0 (CI run 36031171126, under rustix's getsid).
    #[test]
    fn every_process_session_can_be_read() {
        let pids = processes();
        assert!(!pids.is_empty());
        let own = Pid::from_raw(i32::try_from(std::process::id()).unwrap_or(0));
        assert!(own.and_then(session).is_some());
        for pid in pids {
            let _ = session(pid);
        }
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
