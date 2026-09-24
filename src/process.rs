//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, WaitIdOptions, WaitOptions};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// A running child and the master side of its PTY.
pub struct Child {
    pub pid: Pid,
    pub master: OwnedFd,
}

fn size(rows: u16, cols: u16) -> Winsize {
    Winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// Opens a PTY pair. Both ends are close-on-exec; the master is nonblocking.
/// rustix sets `CLOEXEC` at open only on Linux and the BSDs, so it is set
/// again here on every platform. The server has one thread, so no fork can
/// happen in between.
pub fn open_pty(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd), String> {
    let error = |what: &str, e: rustix::io::Errno| format!("{what}: {e}");
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)
        .map_err(|e| error("openpt", e))?;
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)
        .map_err(|e| error("fcntl", e))?;
    rustix::pty::grantpt(&master).map_err(|e| error("grantpt", e))?;
    rustix::pty::unlockpt(&master).map_err(|e| error("unlockpt", e))?;
    let name = rustix::pty::ptsname(&master, Vec::new()).map_err(|e| error("ptsname", e))?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| error("opening the PTY slave", e))?;
    rustix::termios::tcsetwinsize(&master, size(rows, cols))
        .map_err(|e| error("tcsetwinsize", e))?;
    rustix::io::ioctl_fionbio(&master, true).map_err(|e| error("fionbio", e))?;
    Ok((master, slave))
}

/// Starts `argv` on a new PTY, in its own session with the PTY as its
/// controlling terminal. std restores SIGPIPE and clears the signal mask in
/// the child, and `exec` resets the handlers signal-hook installed, so the
/// program starts with default dispositions and an empty mask.
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(&str, String)],
    rows: u16,
    cols: u16,
) -> Result<Child, String> {
    let (program, args) = argv.split_first().ok_or("no program to run")?;
    let (master, slave) = open_pty(rows, cols)?;
    let stdio = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(|e| e.to_string());
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("TERM", "xterm-256color")
        .stdin(stdio(&slave)?)
        .stdout(stdio(&slave)?)
        .stderr(stdio(&slave)?);
    for (key, value) in env {
        command.env(key, value);
    }
    // SAFETY: the hook runs in the child between fork and exec and calls only
    // `setsid` and the `TIOCSCTTY` ioctl, both async-signal-safe system calls
    // that allocate nothing and take no locks.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid().map_err(std::io::Error::from)?;
            let stdin = rustix::stdio::stdin();
            rustix::process::ioctl_tiocsctty(stdin).map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|e| format!("starting {program}: {e}"))?;
    drop(slave);
    let pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or("the child has no valid pid")?;
    // The std handle is dropped without waiting: fux reaps the pid itself, and
    // std never waits on a dropped child.
    drop(child);
    Ok(Child { pid, master })
}

/// Resizes a PTY; the kernel sends SIGWINCH to its foreground group.
pub fn resize(master: impl AsFd, rows: u16, cols: u16) {
    let _ = rustix::termios::tcsetwinsize(master, size(rows, cols));
}

/// The pid of the PTY's foreground process group, if there is one.
pub fn foreground(master: impl AsFd) -> Option<Pid> {
    rustix::termios::tcgetpgrp(master).ok()
}

/// Whether `pid` has exited, without reaping it: its exit status, or `None`
/// while it lives. Only an exited, killed or dumped report counts. macOS also
/// reports stops here even when only exits are asked for (bevy-final finding
/// 020), and a stopped process is alive.
pub fn exited(pid: Pid) -> Option<i32> {
    loop {
        match rustix::process::waitid(
            rustix::process::WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => {
                if status.exited() {
                    return Some(status.exit_status().unwrap_or(0));
                }
                if status.killed() || status.dumped() {
                    return Some(128 + status.terminating_signal().unwrap_or(0));
                }
                return None;
            }
            Ok(None) => return None,
            Err(rustix::io::Errno::INTR) => continue,
            // Not our child any more (already reaped): treat as gone.
            Err(_) => return Some(0),
        }
    }
}

/// Signals the leader's process group and every other process in its
/// session. A background job started by dash sits in its own group, out of
/// reach of the group signal, but stays in the session (bevy-final finding
/// 013). The leader is unreaped, so its pid, pgid and sid cannot be reused.
pub fn hangup(leader: Pid) {
    let _ = rustix::process::kill_process_group(leader, Signal::HUP);
    let in_session =
        |pid: Pid| pid != leader && rustix::process::getsid(Some(pid)).ok() == Some(leader);
    for pid in processes().into_iter().filter(|pid| in_session(*pid)) {
        // Re-checked just before the signal: a process that left is skipped.
        if in_session(pid) {
            let _ = rustix::process::kill_process(pid, Signal::HUP);
        }
    }
}

/// Ends the leader's group and reaps the leader. Called after `hangup` and a
/// grace period, with the master already closed.
pub fn finish(leader: Pid) -> Option<i32> {
    let _ = rustix::process::kill_process_group(leader, Signal::KILL);
    loop {
        match rustix::process::waitpid(Some(leader), WaitOptions::empty()) {
            Ok(Some((_, status))) => {
                return status
                    .exit_status()
                    .or_else(|| status.terminating_signal().map(|s| 128 + s));
            }
            Ok(None) => return None,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return None,
        }
    }
}

/// Sends SIGTERM to a process group.
pub fn terminate(group: Pid) -> Result<(), String> {
    rustix::process::kill_process_group(group, Signal::TERM).map_err(|e| e.to_string())
}

/// The current directory of a process, when the system says.
#[cfg(target_os = "linux")]
pub fn cwd(pid: Pid) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid.as_raw_nonzero())).ok()
}

/// The current directory of a process, when the system says.
#[cfg(target_os = "macos")]
pub fn cwd(pid: Pid) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let bytes = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: the buffer is exactly one `proc_vnodepathinfo`, which the call
    // fills; a short return is checked before the value is read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid.as_raw_nonzero().get(),
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
        .map(|c| *c as u8)
        .collect();
    (!path.is_empty()).then(|| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd(_pid: Pid) -> Option<std::path::PathBuf> {
    None
}

/// Every process id the system lists, as candidates for `hangup`.
#[cfg(target_os = "linux")]
fn processes() -> Vec<Pid> {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
                .filter_map(Pid::from_raw)
                .collect()
        })
        .unwrap_or_default()
}

/// Every process id the system lists, as candidates for `hangup`.
#[cfg(target_os = "macos")]
fn processes() -> Vec<Pid> {
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
    pids.into_iter().filter_map(Pid::from_raw).collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn processes() -> Vec<Pid> {
    Vec::new()
}
