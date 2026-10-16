//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
pub use fuxix::process::Pid;
use fuxix::process::{Signal, Status};
use fuxix::pty::Master;
use std::path::{Path, PathBuf};

/// A pane's program: the leader of a session of its own, started by fux
/// and not yet reaped. Unreaped, its pid is its group's and its session's
/// ID and cannot be reused, so it is signalled through this alone; reaping
/// it takes it.
#[must_use = "a leader dropped unreaped is a zombie no one reaps"]
pub struct Leader(Pid, bool);

/// A pane's program and the master side of its PTY.
pub struct Child {
    pub leader: Leader,
    pub master: Master,
}

/// Starts `argv` on a new PTY of `size`, in its own session with the PTY as
/// its controlling terminal, through the launcher in the `fux` binary
/// (`fuxix::pty::launched`): with `env` set and `unset` removed from the
/// server's environment.
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    (env, unset): (&[(&str, String)], &[String]),
    size: fux_vt::Size,
) -> Result<Child, String> {
    let program = argv.first().ok_or("no program to run")?;
    let (master, slave) = fuxix::pty::open(size.nonzero().into()).map_err(|e| e.to_string())?;
    let fux = launcher().map_err(|e| format!("finding the fux binary: {e}"))?;
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
            for key in unset {
                command.env_remove(key);
            }
            for (key, value) in env {
                command.env(key, value);
            }
        })
        .map_err(|source| format!("starting {program}: {source}"))?;
    // The std handle is dropped without waiting: fux reaps the pid itself, and
    // std never waits on a dropped child.
    drop(child);
    Ok(Child {
        leader: Leader(pid, false),
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

pub use fuxix::process::Status as Ended;

/// The status a shell reports for a process: its exit code, or 128 plus the
/// signal that killed it, whose numbers are small.
pub fn shell_status(status: Status) -> i32 {
    match status {
        Status::Exited(code) => code,
        Status::Signalled(signal) => 128i32.saturating_add(signal),
    }
}

impl Leader {
    pub fn pid(&self) -> Pid {
        self.0
    }

    /// Its pane is closed by fux, not ended by itself: its whole session
    /// goes when it is finished.
    pub fn closed(&mut self) {
        self.1 = true;
    }

    /// Whether it has exited, without reaping it: its exit status, or
    /// `None` while it lives, stopped or not (bevy-final finding 020). The
    /// check does not wait, so no signal interrupts it, and it fails only
    /// for a pid that is not an unreaped child, which this always is.
    pub fn exited(&self) -> Option<Status> {
        fuxix::process::ended(self.0).unwrap_or(Some(Status::Exited(0)))
    }

    /// Signals its process group and every other process in its session.
    /// A background job started by dash sits in its own group, out of reach
    /// of the group signal, but stays in the session (bevy-final finding
    /// 013). The session is looked for in the server's own tree, which
    /// keeps the orphans its panes leave (`fuxix::process::tree`).
    pub fn hang_up(&self) {
        // Found before any signal: once the group's leader dies, its
        // children are on their way to this process, in no one's list.
        let session = self.session();
        let _ = fuxix::process::kill_group(self.0, Signal::Hup);
        for pid in session {
            let _ = fuxix::process::kill(pid, Signal::Hup);
        }
    }

    /// The other processes in its session, in the server's tree.
    fn session(&self) -> Vec<Pid> {
        (fuxix::process::tree().into_iter())
            .filter(|pid| *pid != self.0 && fuxix::process::session(*pid) == Some(self.0))
            .collect()
    }

    /// Ends its group and the rest of its session and reaps it, without
    /// waiting, so uninterrupted:
    /// itself back if it has not exited yet, to try again once it has.
    /// Called after `hang_up` and a grace period, with the master closed.
    pub fn finish(self) -> Result<(), Leader> {
        // Its pane closed by fux: as the hangup, every process left in its
        // session, a job in a group of its own that ignored the hangup
        // (`nohup cmd &`) too, found first. A program that ended by itself
        // leaves those it meant to (`nohup job & exit`).
        let session = if self.1 { self.session() } else { Vec::new() };
        let _ = fuxix::process::kill_group(self.0, Signal::Kill);
        for pid in session {
            let _ = fuxix::process::kill(pid, Signal::Kill);
        }
        match fuxix::process::reap(self.0) {
            Ok(None) => Err(self),
            Ok(Some(_)) | Err(_) => Ok(()),
        }
    }
}

/// How an end is told: `exited 3`, or `killed by SIGSEGV`.
pub fn told(ended: Status) -> String {
    match ended {
        Status::Exited(code) => format!("exited {code}"),
        Status::Signalled(signal) => {
            let name = match signal {
                1 => "SIGHUP",
                2 => "SIGINT",
                3 => "SIGQUIT",
                4 => "SIGILL",
                6 => "SIGABRT",
                8 => "SIGFPE",
                9 => "SIGKILL",
                11 => "SIGSEGV",
                13 => "SIGPIPE",
                14 => "SIGALRM",
                15 => "SIGTERM",
                _ => return format!("killed by signal {signal}"),
            };
            format!("killed by {name}")
        }
    }
}
