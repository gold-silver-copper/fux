//! Signals on Windows, which has none: what stands for each, queued on a
//! [`Pipe`] for a poll loop to wait on.
//!
//! - `Chld`: a watched child ended ([`crate::process::watch`]).
//! - `Int`, `Quit`, `Hup`, `Term`: the console's Ctrl-C, Ctrl-Break, close,
//!   and logoff or shutdown, from the handler fuxix sets once a pipe asks
//!   for one of them. Windows ends the process once the handler returns
//!   from a close, logoff or shutdown, so a loop has only until then.
//! - Any, [`raise`]d by this process, on any thread.
use crate::errno::{Errno, Result};
use crate::poll::{Events, Source, sealed::Sealed};
use crate::process::Signal;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::core::BOOL;

/// Ready to read once one of its signals came; [`Pipe::next`] tells which.
/// One signal is held once while it waits to be taken, as Unix holds one
/// of each pending, and each comes in the order it first came. Unlike a
/// Unix pipe it can't fill. Dropped, it is told no more.
pub struct Pipe {
    queue: Arc<Queue>,
}

struct Queue {
    signals: Vec<Signal>,
    pending: Mutex<VecDeque<Signal>>,
}

/// Every pipe made, to tell; a dropped one's entry goes at the next.
static PIPES: Mutex<Vec<Weak<Queue>>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pipe {
    pub fn new(signals: &[Signal]) -> Result<Pipe> {
        // Made now, so that a raise always has a port to wake.
        crate::poll::port()?;
        if signals.iter().any(|signal| is_console(*signal)) {
            handle_console(signals.contains(&Signal::Int))?;
        }
        let queue = Arc::new(Queue {
            signals: signals.to_vec(),
            pending: Mutex::new(VecDeque::new()),
        });
        {
            let mut pipes = lock(&PIPES);
            pipes.retain(|pipe| pipe.strong_count() > 0);
            pipes.push(Arc::downgrade(&queue));
        }
        Ok(Pipe { queue })
    }

    /// What to wait on: the pipe itself.
    pub fn reader(&self) -> &Pipe {
        self
    }

    /// The next signal that came, taken off the pipe: `None` once it is
    /// empty, so `while let Some(signal) = pipe.next()` drains it.
    pub fn next(&self) -> Option<Signal> {
        // A signal is never queued twice (`tell`).
        lock(&self.queue.pending).pop_front()
    }
}

impl Sealed for Pipe {
    fn arm(&self, _: Events) {}

    fn ready(&self, interest: Events) -> Events {
        let held = !lock(&self.queue.pending).is_empty();
        match interest.intersects(Events::IN) && held {
            true => Events::IN,
            false => Events::NONE,
        }
    }
}

impl Source for Pipe {}

/// Tells every pipe that asked for `signal` it came, from any thread, and
/// wakes the poll: each raise posts its own wake, so none is lost. Fails
/// only if the wake can't be posted (the system's error).
pub fn raise(signal: Signal) -> Result<()> {
    tell(signal).map(drop)
}

/// As [`raise`]: whether any pipe asked for it.
pub(crate) fn tell(signal: Signal) -> Result<bool> {
    let mut told = false;
    for queue in lock(&PIPES).iter().filter_map(Weak::upgrade) {
        if queue.signals.contains(&signal) {
            let mut pending = lock(&queue.pending);
            if !pending.contains(&signal) {
                pending.push_back(signal);
            }
            drop(pending);
            told = true;
        }
    }
    crate::poll::wake()?;
    Ok(told)
}

/// Whether a console control event stands for `signal`.
fn is_console(signal: Signal) -> bool {
    matches!(
        signal,
        Signal::Int | Signal::Quit | Signal::Hup | Signal::Term
    )
}

/// The signal a console control event stands for.
fn console_event(event: u32) -> Option<Signal> {
    match event {
        CTRL_C_EVENT => Some(Signal::Int),
        CTRL_BREAK_EVENT => Some(Signal::Quit),
        CTRL_CLOSE_EVENT => Some(Signal::Hup),
        CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Some(Signal::Term),
        _ => None,
    }
}

/// Sets [`handle`] as this process's console control handler, once. With
/// `int`, Ctrl-C is also let through again: a process started with it
/// ignored (as one started in a new process group is) is told it too.
fn handle_console(int: bool) -> Result<()> {
    static SET: OnceLock<Result<()>> = OnceLock::new();
    let set = SET.get_or_init(|| {
        // SAFETY: `handle` is an `extern "system"` function that lives as
        // long as the process, and is safe to run on any thread.
        match unsafe { SetConsoleCtrlHandler(Some(handle), 1) } {
            0 => Err(Errno::last()),
            _ => Ok(()),
        }
    });
    (*set)?;
    if int {
        // SAFETY: with no handler, this only clears the process's "ignore
        // Ctrl-C".
        if unsafe { SetConsoleCtrlHandler(None, 0) } == 0 {
            return Err(Errno::last());
        }
    }
    Ok(())
}

/// What a console control event runs, on a thread Windows starts for it:
/// it tells the pipes that asked. An event none asked for goes on to the
/// next handler, the system's own, which ends the process as it would
/// have.
unsafe extern "system" fn handle(event: u32) -> BOOL {
    match console_event(event).map(tell) {
        Some(Ok(true)) => 1,
        Some(Ok(false) | Err(_)) | None => 0,
    }
}
