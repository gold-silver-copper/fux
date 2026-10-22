//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
pub use fuxix::process::Pid;
use fuxix::process::{Signal, Status};
use fuxix::pty::Master;
use std::path::Path;

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
/// its controlling terminal: with `env` set and `unset` removed from the
/// server's environment.
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    (env, unset): (&[(&str, std::ffi::OsString)], &[String]),
    size: fux_vt::Size,
) -> Result<Child, String> {
    let program = argv.first().ok_or("no program to run")?;
    let (master, slave) = fuxix::pty::open(size.nonzero().into()).map_err(|e| e.to_string())?;
    let (child, pid) = slave
        .spawn(argv, |command| {
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

pub use fuxix::process::Status as Ended;

/// The end of a program whose status could not be read: no exit code is
/// negative, so it is never taken for a success.
pub const UNKNOWN: Ended = Ended::Exited(-1);

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
    /// `None` while it lives, stopped or not. The check does not wait, so
    /// no signal interrupts it; it fails only for a pid that is no unreaped
    /// child, one gone, whose status is lost.
    pub fn exited(&self) -> Option<Status> {
        fuxix::process::ended(self.0).unwrap_or(Some(UNKNOWN))
    }

    /// Signals its process group and every other process in its session.
    /// A background job started by dash sits in its own group, out of reach
    /// of the group signal, but stays in the session. The session is looked for in the server's own tree, which
    /// keeps the orphans its panes leave (`fuxix::process::tree`).
    pub fn hang_up(&self) {
        // Found before any signal: once the group's leader dies, its
        // children are on their way to this process, in no one's list.
        self.signal(self.session(), Signal::Hup);
    }

    /// Signals its process group, then each of `others`.
    fn signal(&self, others: Vec<Pid>, signal: Signal) {
        let _ = fuxix::process::kill_group(self.0, signal);
        for pid in others {
            let _ = fuxix::process::kill(pid, signal);
        }
    }

    /// The other processes in its session, in the server's tree.
    fn session(&self) -> Vec<Pid> {
        (fuxix::process::tree().into_iter())
            .filter(|pid| *pid != self.0 && fuxix::process::session(*pid) == Some(self.0))
            .collect()
    }

    /// Ends its group and the rest of its session and reaps it, without
    /// waiting, so uninterrupted: its pid back if it has not exited yet,
    /// for `reaped` to reap once it has. Called after `hang_up` and a grace
    /// period, with the master closed.
    pub fn finish(self) -> Result<(), Pid> {
        // Its pane closed by fux: as the hangup, every process left in its
        // session, a job in a group of its own that ignored the hangup
        // (`nohup cmd &`) too, found first. A program that ended by itself
        // leaves those it meant to (`nohup job & exit`).
        let session = if self.1 { self.session() } else { Vec::new() };
        self.signal(session, Signal::Kill);
        match reaped(self.0) {
            true => Ok(()),
            false => Err(self.0),
        }
    }
}

/// Reaps `pid`, a leader killed and not yet reaped, if it has exited:
/// whether it is gone.
pub fn reaped(pid: Pid) -> bool {
    !matches!(fuxix::process::reap(pid), Ok(None))
}

/// How an end is told: `exited 3`, or `killed by SIGSEGV`.
pub fn told(ended: Status) -> String {
    match ended {
        UNKNOWN => "ended, its status unknown".to_owned(),
        Status::Exited(code) => format!("exited {code}"),
        Status::Signalled(raw) => match Signal::from_raw(raw) {
            Some(signal) => format!("killed by SIG{}", signal.name()),
            None => format!("killed by signal {raw}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leader whose status can't be read (no child of this process, its
    /// status lost) is told ended, its status unknown: never a success.
    #[test]
    fn a_status_not_read_is_no_success() -> Result<(), String> {
        let me = i32::try_from(std::process::id()).map_err(|e| e.to_string())?;
        let leader = Leader(Pid::from_raw(me).ok_or("no pid")?, false);
        let ended = leader.exited();
        assert!(ended.is_some_and(|s| s != Status::Exited(0)), "{ended:?}");
        Ok(())
    }
}
