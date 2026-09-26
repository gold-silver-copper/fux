//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
pub use fuxix::process::Pid;
use fuxix::process::{Signal, Status};
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

/// Opens a PTY pair. Both ends are close-on-exec; the master is nonblocking.
pub fn open_pty(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd), String> {
    let (master, slave) = fuxix::pty::open(rows, cols).map_err(|e| e.to_string())?;
    fuxix::io::set_nonblocking(&master, true).map_err(|e| format!("nonblocking: {e}"))?;
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
    let pid = Pid::of(&child).ok_or("the child has no valid pid")?;
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
    // Whatever the server inherited without close-on-exec, from a shell that
    // leaks descriptors or a thread that raced its parent's spawn, it passed
    // on to here: marked, it goes no further, and the program gets stdio and
    // nothing else.
    let marked = fuxix::io::cloexec_from(3);
    // A close-on-exec copy, so a successful `exec` closes the pipe; the
    // program's stdout is the PTY.
    let report = std::io::stdout().as_fd().try_clone_to_owned();
    let failure = match marked {
        Ok(()) => become_program(argv),
        Err(errno) => std::io::Error::other(format!(
            "marking inherited descriptors close-on-exec: {errno}"
        )),
    };
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
    if let Err(errno) = fuxix::process::setsid() {
        return errno.into();
    }
    let terminal = std::io::stdin();
    if let Err(errno) = fuxix::terminal::make_controlling(&terminal) {
        return errno.into();
    }
    match terminal.as_fd().try_clone_to_owned() {
        Ok(stdout) => Command::new(program).args(args).stdout(stdout).exec(),
        Err(error) => error,
    }
}

/// Resizes a PTY; the kernel sends SIGWINCH to its foreground group.
pub fn resize(master: impl AsFd, rows: u16, cols: u16) {
    let _ = fuxix::terminal::set_window_size(master, rows.max(1), cols.max(1));
}

/// The pid of the PTY's foreground process group, if there is one.
pub fn foreground(master: impl AsFd) -> Option<Pid> {
    fuxix::terminal::foreground_group(master)
}

/// The status a shell reports for a process: its exit code, or 128 plus the
/// signal that killed it, whose numbers are small.
fn shell_status(status: Status) -> i32 {
    match status {
        Status::Exited(code) => code,
        Status::Signalled(signal) => 128i32.saturating_add(signal),
    }
}

/// Whether `pid` has exited, without reaping it: its exit status, or `None`
/// while it lives, stopped or not (bevy-final finding 020).
pub fn exited(pid: Pid) -> Option<i32> {
    loop {
        match fuxix::process::ended(pid) {
            Ok(status) => return status.map(shell_status),
            Err(fuxix::Errno::INTR) => continue,
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
    let _ = fuxix::process::kill_group(leader, Signal::Hup);
    let in_session = |pid: Pid| pid != leader && fuxix::process::session(pid) == Some(leader);
    for pid in fuxix::process::processes()
        .into_iter()
        .filter(|pid| in_session(*pid))
    {
        // Re-checked just before the signal: a process that left is skipped.
        if in_session(pid) {
            let _ = fuxix::process::kill(pid, Signal::Hup);
        }
    }
}

/// Ends the leader's group and reaps the leader, without waiting: `None`
/// if the leader has not exited yet, to try again shortly. Called after
/// `hangup` and a grace period, with the master already closed.
pub fn finish(leader: Pid) -> Option<i32> {
    let _ = fuxix::process::kill_group(leader, Signal::Kill);
    loop {
        match fuxix::process::reap(leader) {
            Ok(status) => return status.map(shell_status),
            Err(fuxix::Errno::INTR) => continue,
            // Already reaped, or not ours: nothing left to wait for.
            Err(_) => return Some(0),
        }
    }
}

/// Sends SIGTERM to a process group.
pub fn terminate(group: Pid) -> Result<(), String> {
    fuxix::process::kill_group(group, Signal::Term).map_err(|e| e.to_string())
}

/// The current directory of a process, when the system says.
pub fn cwd(pid: Pid) -> Option<std::path::PathBuf> {
    fuxix::process::cwd(pid)
}
