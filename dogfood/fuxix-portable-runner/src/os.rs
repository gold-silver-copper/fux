//! Every OS-specific call the runner makes, on Windows: a task runs on a
//! pseudo console as the leader of a job object, which is what ends its
//! whole tree. fuxix makes a `Job` only with a console, so a pipe task runs
//! on one too.
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{self, Dispositions, Job, Reach, Signal};
use fuxix::pty::Master;
use std::io;
use std::time::Duration;

/// How a task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Exited(i32),
    /// Killed by a signal, by name ("TERM"), or its number if unnamed.
    Killed(String),
}

/// Set up once per run: told of every child's end.
pub struct Os {
    ended: fuxix::signal::Pipe,
    poll: Poll,
}

/// A started task: its job, and the console it writes to.
pub struct Task {
    job: Job,
    master: Master,
    /// Its output is at an end (closed, or never more to read).
    closed: bool,
    status: Option<Status>,
}

fn errno(error: Errno) -> io::Error {
    error.into()
}

impl Os {
    pub fn new() -> io::Result<Os> {
        let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
        Ok(Os {
            ended: fuxix::signal::Pipe::new(&[Signal::Chld]).map_err(errno)?,
            poll: Poll::default(),
        })
    }

    /// Starts `argv` on a console of `rows` x `cols`, 24 x 80 for a pipe task.
    pub fn spawn(&self, argv: &[String], pty: Option<(u16, u16)>) -> io::Result<Task> {
        let (rows, cols) = pty.unwrap_or((24, 80));
        let size = fuxix::terminal::Size::new(rows, cols)
            .ok_or_else(|| io::Error::other("a PTY of no rows or columns"))?;
        let (master, slave) = fuxix::pty::open(size).map_err(io::Error::other)?;
        let job = slave.spawn(argv, |_| {}, Dispositions::Default)?;
        Ok(Task {
            job,
            master,
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
                list.push(PollFd::new(&task.master, Events::IN));
            }
            match list.poll(Some(timeout)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(error) => return Err(errno(error)),
            }
            while self.ended.next().is_some() {}
        }
        task.drain(sink)?;
        if task.status.is_none()
            && let Some(process::Status::Exited(code)) = task.job.ended().map_err(errno)?
        {
            task.drain(sink)?;
            task.status = Some(match code == process::KILLED {
                true => Status::Killed("KILL".to_owned()),
                false => Status::Exited(code),
            });
        }
        Ok(task.status.clone())
    }

    /// Ends every process of the task's job at once: Windows has no TERM to
    /// give a grace to. Returns how many processes it had.
    pub fn kill_tree(&mut self, task: &Task, _grace: Duration) -> usize {
        let found = task.job.members().pids.len();
        let _ = task.job.kill(Reach::Session);
        found
    }

    /// Processes the task left running after its root ended.
    pub fn leftovers(&self, task: &Task) -> usize {
        task.job.members().pids.len()
    }
}

impl Task {
    /// Reads what the task wrote until there is no more for now.
    fn drain(&mut self, sink: &mut dyn FnMut(&[u8])) -> io::Result<()> {
        let mut buffer = [0u8; 16 * 1024];
        while !self.closed {
            match fuxix::io::read(&self.master, &mut buffer) {
                Ok(0) | Err(Errno::IO) => self.closed = true,
                Ok(n) => sink(buffer.get(..n).unwrap_or_default()),
                Err(Errno::AGAIN) => break,
                Err(error) if error.is_peer_gone() => self.closed = true,
                Err(error) => return Err(errno(error)),
            }
        }
        Ok(())
    }
}
