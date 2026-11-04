//! A pane's process: its PTY, how it starts, how its exit is noticed, and how
//! it and its session are ended.
use fux_wire::answer::Ended;
pub use fuxix::process::Pid;
use fuxix::process::{Job, Reach, Scan, Signal};
use fuxix::pty::Master;
use std::path::Path;

/// A pane's program, the leader of its [`Job`], and whether its pane is
/// closed by fux: when it is hung up and killed, how far, and what is
/// logged of what was missed is decided here.
#[must_use = "a leader dropped unreaped is a zombie no one reaps"]
pub struct Leader(Job, bool);

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
    (env, unset): (&[(&str, std::ffi::OsString)], &[&str]),
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
    let (child, job) = slave
        .spawn(
            argv,
            |command| {
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
            },
            fuxix::process::Dispositions::Default,
        )
        .map_err(|source| format!("starting {program} in {cwd:?}: {source}"))?;
    // The std handle is dropped without waiting: fux reaps the pid itself, and
    // std never waits on a dropped child.
    drop(child);
    Ok(Child {
        leader: Leader(job, false),
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

impl Leader {
    pub fn pid(&self) -> Pid {
        self.0.leader()
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
        match self.0.ended() {
            Ok(status) => status.map(Ended::from),
            Err(_) => Some(Ended::Lost),
        }
    }

    /// Signals its process group and every other process in its session.
    /// A background job started by dash sits in its own group, out of reach
    /// of the group signal, but stays in the session, as an orphan does
    /// ([`Job::members`]).
    pub fn hang_up(&self) {
        self.signal(Signal::Hup, Reach::Session);
    }

    /// Signals as far as `reach` says. What it may have missed goes to the
    /// server's log: a scan for them that did not finish, and each signal
    /// the system refused (one to a process gone already is no loss).
    fn signal(&self, signal: Signal, reach: Reach) {
        let sent = self.0.signal(signal, reach);
        let refused: Vec<String> = (sent.failed.into_iter())
            .filter(|(_, errno)| *errno != fuxix::Errno::SRCH)
            .map(|(pid, errno)| format!("{pid}: {errno}"))
            .collect();
        let (name, session) = (signal.name(), self.pid());
        match sent.scan {
            Scan::Complete => {}
            Scan::Truncated => log!(
                "SIG{name} to session {session}: the scan of processes was cut short, the rest not found"
            ),
            Scan::Failed(errno) => {
                log!("SIG{name} to session {session}: processes not all found: {errno}")
            }
        }
        if !refused.is_empty() {
            let refused = refused.join(", ");
            log!("SIG{name} to session {session} refused: {refused}");
        }
    }

    /// Ends its group and the rest of its session and reaps it, without
    /// waiting, so uninterrupted: its job back if it has not exited yet,
    /// for `reaped` to reap once it has. Called after `hang_up` and a grace
    /// period, with the master closed.
    pub fn finish(self) -> Option<Job> {
        self.kill();
        reaped(self.0)
    }

    /// Kills its group and, if its pane is closed by fux, the rest of its
    /// session, without reaping it: what they wrote to its terminal can
    /// still be read.
    pub fn kill(&self) {
        // Its pane closed by fux: as the hangup, every process left in its
        // session, a job in a group of its own that ignored the hangup
        // (`nohup cmd &`) too. A program that ended by itself leaves those
        // it meant to (`nohup job & exit`).
        let reach = if self.1 { Reach::Session } else { Reach::Group };
        self.signal(Signal::Kill, reach);
    }
}

/// Reaps `job`, a leader killed and not yet reaped, if it has exited: the
/// job back while it runs.
pub fn reaped(job: Job) -> Option<Job> {
    job.reap().err()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leader whose status can't be read (reaped behind its back, its
    /// status lost) is told ended, its status unknown: never a success.
    #[test]
    fn a_status_not_read_is_no_success() -> Result<(), String> {
        let argv = ["/bin/sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()];
        let size = fux_vt::Size::new(4, 20).map_err(|e| format!("{e:?}"))?;
        let child = spawn(&argv, Path::new("/"), (&[], &[]), (size, None))?;
        let pid = child.leader.pid();
        for _ in 0..500 {
            if !matches!(fuxix::process::reap(pid), Ok(None)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(child.leader.exited(), Some(Ended::Lost));
        assert!(child.leader.finish().is_none());
        Ok(())
    }
}
