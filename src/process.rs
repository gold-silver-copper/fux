//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
pub use fuxix::process::Pid;
use fuxix::process::{Scan, Signal, Status};
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

/// Starts `argv` on a new PTY of `size`, in pixels as `cell` makes it
/// (`pixels`), in its own session with the PTY as its controlling
/// terminal: with `env` set and `unset` removed from the server's
/// environment. A start that fails says what was started where, and the
/// system's error as it is ([`fuxix::pty::Slave::spawn`]).
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    (env, unset): (&[(&str, std::ffi::OsString)], &[String]),
    (size, cell): (fux_vt::Size, Option<fux_vt::CellSize>),
) -> Result<Child, String> {
    let program = argv.first().ok_or("no program to run")?;
    let (master, slave) = fuxix::pty::open(size.nonzero().into()).map_err(|e| e.to_string())?;
    // Before the program starts, which may ask but once. Its size in
    // cells is set (`open`), or nothing starts; its pixels are told where
    // the system takes them, else left unknown (0 by 0), a program never
    // refused for this optional update.
    if let Some(pixels) = pixels(size, cell) {
        let _ = fuxix::terminal::set_window_size(&master, size.nonzero().into(), Some(pixels));
    }
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
        .map_err(|source| format!("starting {program} in {cwd:?}: {source}"))?;
    // The std handle is dropped without waiting: fux reaps the pid itself, and
    // std never waits on a dropped child.
    drop(child);
    Ok(Child {
        leader: Leader(pid, false),
        master,
    })
}

/// A terminal of `size` in pixels, as `cell` makes it: unknown without a
/// cell, or where the whole does not fit the kernel's count.
pub fn pixels(
    size: fux_vt::Size,
    cell: Option<fux_vt::CellSize>,
) -> Option<fuxix::terminal::Pixels> {
    let side = |cells: u16, pixels: u16| {
        let whole = u32::from(cells).checked_mul(u32::from(pixels))?;
        std::num::NonZeroU16::new(u16::try_from(whole).ok()?)
    };
    let cell = cell?;
    Some(fuxix::terminal::Pixels {
        width: side(size.cols(), cell.width())?,
        height: side(size.rows(), cell.height())?,
    })
}

/// How a pane's program ended: as the system says, or with its status
/// lost (its leader no unreaped child whose status can be read), which is
/// no exit code and no signal, and is said as unknown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ended {
    Exited(i32),
    Signalled(i32),
    Lost,
}

impl Ended {
    /// How a wait or an event says it: `exited`, its code, `null` if it
    /// was lost, or `signal`, the signal that killed it.
    pub fn field(self) -> (&'static str, crate::json::Json) {
        use crate::json::Json;
        match self {
            Ended::Exited(code) => ("exited", Json::int(code)),
            Ended::Signalled(signal) => ("signal", Json::int(signal)),
            Ended::Lost => ("exited", Json::Null),
        }
    }
}

impl From<Status> for Ended {
    fn from(status: Status) -> Self {
        match status {
            Status::Exited(code) => Ended::Exited(code),
            Status::Signalled(signal) => Ended::Signalled(signal),
        }
    }
}

/// The status a shell reports for a process: its exit code, or 128 plus the
/// signal that killed it, whose numbers are small; none for a status lost.
pub fn shell_status(ended: Ended) -> Option<i32> {
    match ended {
        Ended::Exited(code) => Some(code),
        Ended::Signalled(signal) => Some(128i32.saturating_add(signal)),
        Ended::Lost => None,
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
    pub fn exited(&self) -> Option<Ended> {
        match fuxix::process::ended(self.0) {
            Ok(status) => status.map(Ended::from),
            Err(_) => Some(Ended::Lost),
        }
    }

    /// Signals its process group and every other process in its session.
    /// A background job started by dash sits in its own group, out of reach
    /// of the group signal, but stays in the session. The session is looked
    /// for in the server's own tree, which keeps the orphans its panes
    /// leave (`fuxix::process::tree`).
    pub fn hang_up(&self) {
        // Found before any signal: once the group's leader dies, its
        // children are on their way to this process, in no one's list.
        self.signal(self.session(), Signal::Hup);
    }

    /// Signals its process group, then each of `others`. What it may have
    /// missed goes to the server's log: a scan for them that did not
    /// finish, and each signal the system refused (one to a process gone
    /// already is no loss).
    fn signal(&self, (others, scan): (Vec<Pid>, Scan), signal: Signal) {
        let group = fuxix::process::kill_group(self.0, signal).err();
        let refused: Vec<String> = (group.map(|errno| (self.0, errno)).into_iter())
            .chain(
                others
                    .into_iter()
                    .filter_map(|pid| Some((pid, fuxix::process::kill(pid, signal).err()?))),
            )
            .filter(|(_, errno)| *errno != fuxix::Errno::SRCH)
            .map(|(pid, errno)| format!("{pid}: {errno}"))
            .collect();
        let name = signal.name();
        match scan {
            Scan::Complete => {}
            Scan::Truncated => log!(
                "SIG{name} to session {}: the scan of processes was cut short, the rest not found",
                self.0
            ),
            Scan::Failed(errno) => log!(
                "SIG{name} to session {}: processes not all found: {errno}",
                self.0
            ),
        }
        if !refused.is_empty() {
            let refused = refused.join(", ");
            log!("SIG{name} to session {} refused: {refused}", self.0);
        }
    }

    /// The other processes in its session, in the server's tree, and how
    /// the scan of the tree went.
    fn session(&self) -> (Vec<Pid>, Scan) {
        let tree = fuxix::process::tree();
        let session = (tree.pids.into_iter())
            .filter(|pid| *pid != self.0 && fuxix::process::session(*pid) == Some(self.0));
        (session.collect(), tree.scan)
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
        let session = if self.1 {
            self.session()
        } else {
            (Vec::new(), Scan::Complete)
        };
        self.signal(session, Signal::Kill);
        reaped(self.0).then_some(()).ok_or(self.0)
    }
}

/// Reaps `pid`, a leader killed and not yet reaped, if it has exited:
/// whether it is gone.
pub fn reaped(pid: Pid) -> bool {
    !matches!(fuxix::process::reap(pid), Ok(None))
}

/// How an end is told: `exited 3`, `killed by SIGSEGV` (`killed by
/// signal 64` for one POSIX does not name), or `ended, its status lost`.
pub fn told(ended: Ended) -> String {
    Told(ended).to_string()
}

pub(crate) struct Told(pub Ended);
impl std::fmt::Display for Told {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Ended::Lost => out.write_str("ended, its status lost"),
            Ended::Exited(code) => write!(out, "exited {code}"),
            Ended::Signalled(raw) => match Signal::from_raw(raw) {
                Some(signal) => write!(out, "killed by SIG{}", signal.name()),
                None => write!(out, "killed by signal {raw}"),
            },
        }
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
        assert_eq!(leader.exited(), Some(Ended::Lost));
        Ok(())
    }
}
