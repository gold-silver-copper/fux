//! Every OS-specific call the runner makes, and nothing else: start a task
//! (on pipes or a PTY) as the root of a tree the runner can end, wait for
//! its output or its end, and end the whole tree. Unix only, through fuxix;
//! the report says what Windows needs in its place.
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{self, Dispositions, Pid, Signal};
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How a task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Exited(i32),
    /// Killed by a signal, by name ("TERM"), or its number if unnamed.
    Killed(String),
}

/// Set up once per run: told of every child's end, and the parent of the
/// orphans a task leaves (Linux), so they stay findable to end.
pub struct Os {
    ended: fuxix::signal::Pipe,
    poll: Poll,
}

/// What a task writes to: one pipe for stdout and stderr, or a PTY.
enum Output {
    Pipe(io::PipeReader),
    Pty(fuxix::pty::Master),
}

impl AsFd for Output {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        match self {
            Output::Pipe(pipe) => pipe.as_fd(),
            Output::Pty(master) => master.as_fd(),
        }
    }
}

/// A started task: its root process, which leads a session and group of
/// its own, and its output.
pub struct Task {
    pid: Pid,
    output: Output,
    /// Its output is at an end (closed, or never more to read).
    closed: bool,
    status: Option<Status>,
}

fn errno(error: Errno) -> io::Error {
    error.into()
}

impl Os {
    pub fn new() -> io::Result<Os> {
        process::adopt_orphans().map_err(errno)?;
        Ok(Os {
            ended: fuxix::signal::Pipe::new(&[Signal::Chld]).map_err(errno)?,
            poll: Poll::default(),
        })
    }

    /// Starts `argv`, on a PTY of `rows` x `cols` or on a pipe, in `dir`.
    pub fn spawn(&self, argv: &[String], pty: Option<(u16, u16)>) -> io::Result<Task> {
        let (pid, output) = match pty {
            Some((rows, cols)) => {
                let size = fuxix::terminal::Size::new(rows, cols)
                    .ok_or_else(|| io::Error::other("a PTY of no rows or columns"))?;
                let (master, slave) = fuxix::pty::open(size).map_err(io::Error::other)?;
                let (_child, pid) = slave.spawn(argv, |_| {}, Dispositions::Default)?;
                (pid, Output::Pty(master))
            }
            None => {
                let (program, args) = argv
                    .split_first()
                    .ok_or_else(|| io::Error::other("no program to run"))?;
                let (reader, writer) = io::pipe()?;
                let mut command = Command::new(program);
                command
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(OwnedFd::from(writer.try_clone()?))
                    .stderr(OwnedFd::from(writer));
                process::lead_session(&mut command, false, Dispositions::Inherited);
                let child = command.spawn()?;
                drop(command); // closes our copies of the pipe's write end
                let pid = Pid::of(&child).ok_or_else(|| io::Error::other("a child with no pid"))?;
                fuxix::io::set_nonblocking(&reader, true).map_err(errno)?;
                (pid, Output::Pipe(reader))
            }
        };
        Ok(Task {
            pid,
            output,
            closed: false,
            status: None,
        })
    }

    /// Waits up to `timeout` for the task to write or end, passing what it
    /// wrote to `sink`; its status once it has ended and all its output is
    /// read.
    pub fn wait(
        &mut self,
        task: &mut Task,
        timeout: Duration,
        sink: &mut dyn FnMut(&[u8]),
    ) -> io::Result<Option<Status>> {
        if task.status.is_none() {
            let mut list = self.poll.list();
            list.push(PollFd::new(self.ended.reader(), Events::IN));
            if !task.closed {
                list.push(PollFd::new(&task.output, Events::IN));
            }
            match list.poll(Some(timeout)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(errno(error)),
            }
            while self.ended.next().is_some() {}
        }
        task.drain(sink)?;
        if task.status.is_none()
            && let Some(status) = process::ended(task.pid).map_err(errno)?
        {
            // Everything it wrote is readable now (fuxix's rule for the end
            // of a program's output): read it, then reap.
            task.drain(sink)?;
            process::reap(task.pid).map_err(errno)?;
            task.status = Some(match status {
                process::Status::Exited(code) => Status::Exited(code),
                process::Status::Signalled { signal, .. } => Status::Killed(
                    Signal::from_raw(signal).map_or_else(|| signal.to_string(), |s| s.name().into()),
                ),
            });
        }
        Ok(task.status.clone())
    }

    /// Ends every process of the task's tree: TERM to all, then, after
    /// `grace`, KILL to what is left. Returns how many processes the tree
    /// had. Its root stays to be reaped by [`Os::wait`].
    pub fn kill_tree(&mut self, task: &Task, grace: Duration) -> usize {
        let found = members(task.pid).len();
        for signal in [Signal::Term, Signal::Kill] {
            // A group: the root's own; each other member on its own, as a
            // job may sit in a group (or, as an orphan, a session) of its own.
            if task.status.is_none() || process::group_alive(task.pid).unwrap_or(false) {
                let _ = process::kill_group(task.pid, signal);
            }
            for pid in members(task.pid) {
                let _ = process::kill(pid, signal);
            }
            if signal == Signal::Term {
                let deadline = Instant::now() + grace;
                while Instant::now() < deadline && !members(task.pid).is_empty() {
                    std::thread::sleep(Duration::from_millis(10));
                    let _ = process::reap_orphans(|pid| pid == task.pid);
                }
            }
        }
        // The orphans just ended are this process's to reap.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && !members(task.pid).is_empty() {
            std::thread::sleep(Duration::from_millis(10));
            let _ = process::reap_orphans(|pid| pid == task.pid);
        }
        found + usize::from(task.status.is_none())
    }

    /// Processes the task left running after its root ended.
    pub fn leftovers(&self, task: &Task) -> usize {
        let _ = process::reap_orphans(|pid| pid == task.pid);
        members(task.pid).len()
    }
}

impl Task {
    /// Reads what the task wrote until there is no more for now.
    fn drain(&mut self, sink: &mut dyn FnMut(&[u8])) -> io::Result<()> {
        let mut buffer = [0u8; 16 * 1024];
        while !self.closed {
            match fuxix::io::read(&self.output, &mut buffer) {
                Ok(0) | Err(Errno::IO) => self.closed = true,
                Ok(n) => sink(buffer.get(..n).unwrap_or_default()),
                Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => {}
                Err(error) => return Err(errno(error)),
            }
        }
        Ok(())
    }
}

/// The live processes of the tree rooted at `root`, but `root`.
///
/// Linux: every descendant of this process (`process::tree` lists only
/// those, orphans included as this process adopts them), as the runner runs
/// one task at a time. macOS: `tree` is every process on the system, so
/// only those in the root's session: a job that made a session of its own
/// there escapes, as macOS gives orphans to launchd.
fn members(root: Pid) -> Vec<Pid> {
    let mut pids = process::tree().pids;
    pids.retain(|&pid| pid != root && alive(pid));
    if cfg!(target_os = "macos") {
        pids.retain(|&pid| process::session(pid) == Some(root));
    }
    pids
}

/// Still there: it has a session. (fuxix has `group_alive`, but no
/// `kill(pid, 0)` for one process.)
fn alive(pid: Pid) -> bool {
    process::session(pid).is_some()
}
