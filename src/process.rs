//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
pub use fuxix::process::Pid;
use fuxix::process::{Signal, Status};
use fuxix::pty::Master;
use std::path::{Path, PathBuf};

/// Why a pane's process could not be started.
#[derive(Debug)]
pub enum Error {
    Pty(fuxix::pty::Error),
    NoProgram,
    /// The `fux` binary, to run the launcher from, could not be found.
    Launcher(std::io::Error),
    Start {
        program: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Pty(error) => error.fmt(f),
            Error::NoProgram => f.write_str("no program to run"),
            Error::Launcher(error) => write!(f, "finding the fux binary: {error}"),
            Error::Start { program, source } => write!(f, "starting {program}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Pty(error) => Some(error),
            Error::Launcher(error) | Error::Start { source: error, .. } => Some(error),
            Error::NoProgram => None,
        }
    }
}

/// A pane's program: the leader of a session of its own, started by fux
/// and not yet reaped. Unreaped, its pid is its group's and its session's
/// ID and cannot be reused, so it is signalled through this alone; reaping
/// it takes it.
#[must_use = "a leader dropped unreaped is a zombie no one reaps"]
pub struct Leader(Pid);

/// A pane's program and the master side of its PTY.
pub struct Child {
    pub leader: Leader,
    pub master: Master,
}

/// Starts `argv` on a new PTY of `size`, in its own session with the PTY as
/// its controlling terminal, through the launcher in the `fux` binary
/// (`fuxix::pty::launched`).
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(&str, String)],
    size: fux_vt::Size,
) -> Result<Child, Error> {
    let program = argv.first().ok_or(Error::NoProgram)?;
    let (master, slave) = fuxix::pty::open(size.nonzero().into()).map_err(Error::Pty)?;
    let fux = launcher().map_err(Error::Launcher)?;
    let (child, pid) = slave
        .spawn(&fux, argv, |command| {
            // TERM_PROGRAM is fux's, as tmux sets its own: the outer
            // terminal's would have programs use features of a terminal they
            // are not talking to (Claude Code, under Ghostty, pushed kitty
            // keyboard flags fux does not honour).
            command
                .current_dir(cwd)
                .env("TERM", "xterm-256color")
                .env("TERM_PROGRAM", "fux")
                .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
            for (key, value) in env {
                command.env(key, value);
            }
        })
        .map_err(|source| Error::Start {
            program: program.clone(),
            source,
        })?;
    // The std handle is dropped without waiting: fux reaps the pid itself, and
    // std never waits on a dropped child.
    drop(child);
    Ok(Child {
        leader: Leader(pid),
        master,
    })
}

/// The `fux` binary to run the launcher from: on Linux the server's own
/// image, even once its file is replaced or removed.
fn launcher() -> std::io::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe()
}

/// The status a shell reports for a process: its exit code, or 128 plus the
/// signal that killed it, whose numbers are small.
fn shell_status(status: Status) -> i32 {
    match status {
        Status::Exited(code) => code,
        Status::Signalled(signal) => 128i32.saturating_add(signal),
    }
}

impl Leader {
    pub fn pid(&self) -> Pid {
        self.0
    }

    /// Whether it has exited, without reaping it: its exit status, or
    /// `None` while it lives, stopped or not (bevy-final finding 020). The
    /// check does not wait, so no signal interrupts it, and it fails only
    /// for a pid that is not an unreaped child, which this always is.
    pub fn exited(&self) -> Option<i32> {
        fuxix::process::ended(self.0).map_or(Some(0), |status| status.map(shell_status))
    }

    /// Signals its process group and every other process in its session.
    /// A background job started by dash sits in its own group, out of reach
    /// of the group signal, but stays in the session (bevy-final finding
    /// 013).
    pub fn hang_up(&self) {
        let _ = fuxix::process::kill_group(self.0, Signal::Hup);
        // Checked just before each signal: a process that left is skipped.
        for pid in fuxix::process::processes() {
            if pid != self.0 && fuxix::process::session(pid) == Some(self.0) {
                let _ = fuxix::process::kill(pid, Signal::Hup);
            }
        }
    }

    /// Ends its group and reaps it, without waiting, so uninterrupted:
    /// itself back if it has not exited yet, to try again once it has.
    /// Called after `hang_up` and a grace period, with the master closed.
    pub fn finish(self) -> Result<(), Leader> {
        let _ = fuxix::process::kill_group(self.0, Signal::Kill);
        match fuxix::process::reap(self.0) {
            Ok(None) => Err(self),
            Ok(Some(_)) | Err(_) => Ok(()),
        }
    }
}

/// Sends SIGTERM to a process group.
pub fn terminate(group: Pid) -> Result<(), fuxix::Errno> {
    fuxix::process::kill_group(group, Signal::Term)
}
