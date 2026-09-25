//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, WaitIdOptions, WaitOptions};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// controlling terminal, through the launcher (`launch`).
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(&str, String)],
    rows: u16,
    cols: u16,
) -> Result<Child, String> {
    let program = argv.first().ok_or("no program to run")?;
    let (master, slave) = open_pty(rows, cols)?;
    let fux = launcher().map_err(|e| format!("finding the fux binary: {e}"))?;
    let child = launch(&fux, argv, &slave, |command| {
        command.current_dir(cwd).env("TERM", "xterm-256color");
        for (key, value) in env {
            command.env(key, value);
        }
    })
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

/// The hidden `fux` subcommand that starts a program for `launch`:
/// `fux __launch PROGRAM [ARGS...]`. Only fux runs it, so it is in no usage
/// text; its interface is fixed, as an older server may run a newer binary.
pub const LAUNCH: &str = "__launch";

/// The `fux` binary to run the launcher from: on Linux the server's own
/// image, even once its file is replaced or removed.
fn launcher() -> std::io::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe()
}

/// Runs `argv` through the launcher in the `fux` binary, as the leader of a
/// new session with `slave` as its controlling terminal and its stdin,
/// stdout and stderr; `setup` sets the directory and environment. The child
/// is the program once this returns, with the pid the launcher had.
///
/// Starting a process in a new session needs code between `fork` and `exec`,
/// which std allows only through `unsafe`. The launcher runs that code as a
/// program of its own instead, and reports a failure, including its `exec`
/// failing, on a pipe that `exec` closes: as std reports its own, so this
/// returns only once the program runs or cannot.
pub fn launch<S: AsRef<OsStr>>(
    fux: &Path,
    argv: &[S],
    slave: &OwnedFd,
    setup: impl FnOnce(&mut Command),
) -> std::io::Result<std::process::Child> {
    let (mut failures, report) = std::io::pipe()?;
    let mut child = {
        let mut command = Command::new(fux);
        command.arg(LAUNCH).args(argv);
        setup(&mut command);
        command
            .stdin(slave.try_clone()?)
            .stdout(report)
            .stderr(slave.try_clone()?);
        // Dropping `command` closes this side's end of the pipe.
        command.spawn()?
    };
    let mut failure = String::new();
    let read = failures.read_to_string(&mut failure);
    if read.is_ok() && failure.is_empty() {
        return Ok(child);
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(read.err().unwrap_or_else(|| std::io::Error::other(failure)))
}

/// The launcher: `fux __launch PROGRAM [ARGS...]`, run by `launch` with its
/// stdin and stderr on a PTY slave and its stdout on the pipe that reports a
/// failure. It returns only if the program could not start.
pub fn launched(argv: &[String]) -> u8 {
    // A close-on-exec copy, so a successful `exec` closes the pipe; the
    // program's stdout is the PTY.
    let report = std::io::stdout().as_fd().try_clone_to_owned();
    let failure = become_program(argv);
    if let Ok(report) = report {
        let _ = std::fs::File::from(report).write_all(failure.to_string().as_bytes());
    }
    127
}

/// Makes this process the leader of a new session with its stdin as the
/// controlling terminal, then replaces it with `argv`. The error, if either
/// fails. The server's spawn of the launcher cleared the signal mask, and
/// `exec` restores the SIGPIPE Rust ignores and the handlers signal-hook
/// installed, so the program starts with default dispositions and an empty
/// mask.
fn become_program(argv: &[String]) -> std::io::Error {
    let Some((program, args)) = argv.split_first() else {
        return std::io::Error::other("no program to run");
    };
    if let Err(errno) = rustix::process::setsid() {
        return errno.into();
    }
    let terminal = std::io::stdin();
    if let Err(errno) = rustix::process::ioctl_tiocsctty(&terminal) {
        return errno.into();
    }
    match terminal.as_fd().try_clone_to_owned() {
        Ok(stdout) => Command::new(program).args(args).stdout(stdout).exec(),
        Err(error) => error,
    }
}

/// Resizes a PTY; the kernel sends SIGWINCH to its foreground group.
pub fn resize(master: impl AsFd, rows: u16, cols: u16) {
    let _ = rustix::termios::tcsetwinsize(master, size(rows, cols));
}

/// The pid of the PTY's foreground process group, if there is one.
#[cfg(not(target_os = "macos"))]
pub fn foreground(master: impl AsFd) -> Option<Pid> {
    // rustix turns a group of 0 into an error on Linux.
    rustix::termios::tcgetpgrp(master).ok()
}

/// The pid of the PTY's foreground process group, if there is one.
#[cfg(target_os = "macos")]
pub fn foreground(master: impl AsFd) -> Option<Pid> {
    // rustix builds its `Pid` from the result unchecked, and 0 would be
    // undefined behaviour.
    fux_sys::foreground_group(master).and_then(Pid::from_raw)
}

/// A process's session ID, read without trusting it to be non-zero: kernel
/// threads have session 0, and rustix's `getsid` builds its `Pid` from the
/// result unchecked, which for 0 panics in debug builds and is undefined
/// behaviour in release ones.
#[cfg(target_os = "linux")]
pub(crate) fn session(pid: Pid) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.as_raw_nonzero())).ok()?;
    // `pid (comm) state ppid pgrp session …`; comm may hold spaces and
    // parentheses, so fields are counted after the last `)`.
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(3)?.parse().ok()
}

/// A process's session ID.
#[cfg(target_os = "macos")]
pub(crate) fn session(pid: Pid) -> Option<i32> {
    fux_sys::session(pid.as_raw_nonzero().get())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn session(_pid: Pid) -> Option<i32> {
    None
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
        |pid: Pid| pid != leader && session(pid) == Some(leader.as_raw_nonzero().get());
    for pid in processes().into_iter().filter(|pid| in_session(*pid)) {
        // Re-checked just before the signal: a process that left is skipped.
        if in_session(pid) {
            let _ = rustix::process::kill_process(pid, Signal::HUP);
        }
    }
}

/// Ends the leader's group and reaps the leader, without waiting: `None`
/// if the leader has not exited yet, to try again shortly. Called after
/// `hangup` and a grace period, with the master already closed.
pub fn finish(leader: Pid) -> Option<i32> {
    let _ = rustix::process::kill_process_group(leader, Signal::KILL);
    loop {
        match rustix::process::waitpid(Some(leader), WaitOptions::NOHANG) {
            Ok(Some((_, status))) => {
                return Some(
                    status
                        .exit_status()
                        .or_else(|| status.terminating_signal().map(|s| 128 + s))
                        .unwrap_or(0),
                );
            }
            Ok(None) => return None,
            Err(rustix::io::Errno::INTR) => continue,
            // Already reaped, or not ours: nothing left to wait for.
            Err(_) => return Some(0),
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
    fux_sys::cwd(pid.as_raw_nonzero().get())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd(_pid: Pid) -> Option<std::path::PathBuf> {
    None
}

/// Every process id the system lists, as candidates for `hangup`.
#[cfg(target_os = "linux")]
pub(crate) fn processes() -> Vec<Pid> {
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
pub(crate) fn processes() -> Vec<Pid> {
    fux_sys::processes()
        .into_iter()
        .filter_map(Pid::from_raw)
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn processes() -> Vec<Pid> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every listed process's session can be asked for: on a Linux host the
    /// list includes kernel threads, whose session is 0, and asking through
    /// rustix's `getsid` panicked there (CI run 36031171126).
    #[test]
    fn every_process_session_can_be_read() {
        let pids = processes();
        assert!(!pids.is_empty());
        let own = i32::try_from(std::process::id())
            .ok()
            .and_then(Pid::from_raw)
            .and_then(session);
        assert!(own.is_some_and(|sid| sid > 0));
        for pid in pids {
            let _ = session(pid);
        }
    }
}
