//! Signals as bytes on a pipe, for a poll loop to wait on.
use crate::errno::{Errno, Result, check};
use crate::process::Signal;
use std::io::PipeReader;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};

/// A pipe that becomes readable when any of its signals arrives: each
/// writes its number as a byte to the other end, of which its notice holds
/// a copy, so [`Pipe::next`] tells which came. Both ends are non-blocking;
/// dropped, the notices stop, then the pipe closes.
pub struct Pipe {
    /// Declared first, so that they are dropped before the read end closes:
    /// a handler then never writes to a pipe with no reader.
    _notices: Vec<Notice>,
    reader: PipeReader,
}

impl Pipe {
    pub fn new(signals: &[Signal]) -> Result<Pipe> {
        let io = |error: std::io::Error| Errno::from_io_error(&error).unwrap_or(Errno::IO);
        let (reader, write) = std::io::pipe().map_err(io)?;
        crate::io::set_nonblocking(&reader, true)?;
        // A failure part-way through drops the notices made before it.
        let notices = signals
            .iter()
            .map(|&signal| notify(signal, write.try_clone().map_err(io)?.into()))
            .collect::<Result<_>>()?;
        Ok(Pipe {
            _notices: notices,
            reader,
        })
    }

    /// The read end, to wait on.
    pub fn reader(&self) -> &PipeReader {
        &self.reader
    }

    /// The next signal that came, taken off the pipe: `None` once it is
    /// empty, so `while let Some(signal) = pipe.next()` drains it. Each
    /// arrival is told once; one that came while the pipe was full is not
    /// told. A signal sent twice is told twice, or once if the second came
    /// before the first was handled (the system holds one of each pending).
    pub fn next(&self) -> Option<Signal> {
        let mut byte = [0u8];
        match crate::io::read(&self.reader, &mut byte) {
            Ok(1) => Signal::from_raw(i32::from(u8::from_ne_bytes(byte))),
            Ok(_) | Err(_) => None,
        }
    }
}

/// How many notices may be held at once, of every signal.
const COUNT: usize = 64;

/// A slot's descriptor while no notice holds it.
const FREE: libc::c_int = -1;

/// One notice's signal and the descriptor its handler writes to. A notice
/// takes a slot by its descriptor, then sets the signal; it gives it back
/// by clearing the signal, then the descriptor.
struct Slot {
    /// The signal's number; 0, which no signal has, while none.
    signal: AtomicI32,
    fd: AtomicI32,
}

static SLOTS: [Slot; COUNT] = [const {
    Slot {
        signal: AtomicI32::new(0),
        fd: AtomicI32::new(FREE),
    }
}; COUNT];

/// Handlers running now, on any thread: a notice that has cleared its slot
/// waits for none to be running before it closes the descriptor, so that a
/// handler never writes to one closed, or since opened as another file.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Writes a byte to `write` each time `signal` arrives, until the notice is
/// dropped. Then `write` is closed, once no handler can still be writing to
/// it.
///
/// `write` is made non-blocking: a full pipe then drops the byte rather
/// than hold the handler, and one byte waiting is enough to wake a poll.
/// Several notices of one signal each get a byte.
///
/// The handler replaces what the signal did before, for good, with
/// `SA_RESTART`. Once every notice of a signal is dropped, the signal is
/// caught and does nothing. A program [`crate::process::lead_session`]
/// starts gets the signal as `Dispositions` says: as it was before, the
/// default or ignored, or the default. This fails with `NOSPC` when 64 notices
/// are held already, leaving the signal as it was, and with `INVAL` for a
/// signal that can't be caught (`KILL`, `STOP`).
pub(crate) fn notify(signal: Signal, write: OwnedFd) -> Result<Notice> {
    crate::io::set_nonblocking(&write, true)?;
    let fd = write.as_raw_fd();
    let slot = SLOTS
        .iter()
        .find(|slot| {
            (slot.fd)
                .compare_exchange(FREE, fd, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        })
        .ok_or(Errno::NOSPC)?;
    slot.signal.store(signal.raw(), Ordering::SeqCst);
    // Held before the handler is installed, so that no signal arriving in
    // between is lost; a failed install gives the slot back as it drops.
    let notice = Notice {
        slot,
        _write: write,
    };
    install(signal)?;
    Ok(notice)
}

/// A signal's arrivals written to a pipe, from [`notify`]; dropped, they
/// stop.
pub(crate) struct Notice {
    slot: &'static Slot,
    /// Held for the handler to write to, and closed on drop.
    _write: OwnedFd,
}

impl Drop for Notice {
    fn drop(&mut self) {
        self.slot.signal.store(0, Ordering::SeqCst);
        // A handler that read the signal before it was cleared counted
        // itself running first. Each makes one write, so this is short.
        while RUNNING.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        self.slot.fd.store(FREE, Ordering::SeqCst);
    }
}

/// The signals, by bit, that were ignored when a notice first caught them:
/// [`start`] ignores them again in a program started with
/// `Dispositions::Inherited`, as `exec` would have kept them had nothing
/// caught them.
static IGNORED: AtomicU64 = AtomicU64::new(0);

/// Makes [`handle`] what `signal` runs, noting whether it was ignored.
fn install(signal: Signal) -> Result<()> {
    let mut action = action(handle as extern "C" fn(libc::c_int) as libc::sighandler_t)?;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: as `action` says.
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: `action` is a valid action for the call, and `old` a whole
    // one for it to fill.
    check(unsafe { libc::sigaction(signal.raw(), &action, &mut old) })?;
    // SIGPIPE is ignored by Rust's runtime, not by whoever started this
    // process, and std's spawn sets it back for the program.
    if old.sa_sigaction == libc::SIG_IGN && signal != Signal::Pipe {
        IGNORED.fetch_or(bit(signal.raw()), Ordering::SeqCst);
    }
    Ok(())
}

/// An action that runs `handler`, with no flags and an empty mask.
fn action(handler: libc::sighandler_t) -> Result<libc::sigaction> {
    // SAFETY: `sigaction` is plain data, for which all zeros is valid: no
    // handler, no flags, and an empty mask.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    // SAFETY: the mask is a `sigset_t` this function owns.
    check(unsafe { libc::sigemptyset(&mut action.sa_mask) })?;
    Ok(action)
}

/// `signal`'s bit in [`IGNORED`]: every signal's number is below 64.
fn bit(signal: libc::c_int) -> u64 {
    u32::try_from(signal).map_or(0, |n| 1u64.checked_shl(n).unwrap_or(0))
}

/// Between `fork` and `exec`: sets the program's dispositions as
/// `dispositions` says, and unblocks every signal. Only async-signal-safe
/// calls; it allocates nothing.
pub(crate) fn start(dispositions: crate::process::Dispositions) -> Result<()> {
    use crate::process::Dispositions;
    match dispositions {
        Dispositions::Inherited => {
            let ignored = IGNORED.load(Ordering::SeqCst);
            let ignore = action(libc::SIG_IGN)?;
            for signal in (1..64).filter(|&n| ignored & bit(n) != 0) {
                // SAFETY: `ignore` is a valid action, and the old one is not
                // asked for.
                check(unsafe { libc::sigaction(signal, &ignore, std::ptr::null_mut()) })?;
            }
        }
        Dispositions::Default => {
            let default = action(libc::SIG_DFL)?;
            // Every number a signal may have: one that is none, or can't be
            // set (KILL, STOP), refuses, which leaves nothing to undo.
            for signal in 1..65 {
                // SAFETY: `default` is a valid action, and the old one is
                // not asked for.
                unsafe { libc::sigaction(signal, &default, std::ptr::null_mut()) };
            }
        }
    }
    let mut none = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: the set is a whole `sigset_t`, which the call fills.
    check(unsafe { libc::sigemptyset(none.as_mut_ptr()) })?;
    // SAFETY: the set is whole and empty; the old mask is not asked for.
    check(unsafe { libc::sigprocmask(libc::SIG_SETMASK, none.as_ptr(), std::ptr::null_mut()) })
        .map(drop)
}

/// What every notified signal runs. It makes only async-signal-safe calls,
/// and leaves `errno` as it found it.
extern "C" fn handle(signal: libc::c_int) {
    let errno = errno();
    // SAFETY: `errno` is this thread's, valid for as long as the thread runs.
    let saved = unsafe { errno.read() };
    RUNNING.fetch_add(1, Ordering::SeqCst);
    for slot in &SLOTS {
        if slot.signal.load(Ordering::SeqCst) == signal {
            let fd = slot.fd.load(Ordering::SeqCst);
            // Every signal's number is below 256.
            if fd != FREE
                && let Ok(byte) = u8::try_from(signal)
            {
                // SAFETY: `write` is async-signal-safe, and the byte is
                // readable for the call. The descriptor is open: its notice
                // closes it only after clearing the signal and seeing no
                // handler running, and this one counted itself first. A
                // write that fails (a full pipe) is dropped.
                unsafe { libc::write(fd, [byte].as_ptr().cast(), 1) };
            }
        }
    }
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    // SAFETY: as above.
    unsafe { errno.write(saved) };
}

/// The calling thread's `errno`.
fn errno() -> *mut libc::c_int {
    // SAFETY: it takes nothing, and gives this thread's `errno`.
    #[cfg(target_os = "linux")]
    let errno = unsafe { libc::__errno_location() };
    // SAFETY: as above.
    #[cfg(target_os = "android")]
    let errno = unsafe { libc::__errno() };
    // SAFETY: as above.
    #[cfg(target_os = "macos")]
    let errno = unsafe { libc::__error() };
    errno
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll::{Events, PollFd, poll};
    use crate::process::{Pid, kill};
    use std::io::Read;
    use std::time::Duration;

    /// The pipe's next byte, or 0 for its end; `None` if none comes in 5 s.
    fn next(pipe: &mut PipeReader) -> Option<usize> {
        let mut fds = [PollFd::new(pipe, Events::IN)];
        if poll(&mut fds, Some(Duration::from_secs(5))).ok()? == 0 {
            return None;
        }
        pipe.read(&mut [0u8; 1]).ok()
    }

    #[test]
    fn a_signal_reaches_each_notice_and_a_dropped_one_is_closed() -> std::io::Result<()> {
        let pid = Pid::from_raw(i32::try_from(std::process::id()).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("no pid"))?;
        let (mut one, one_write) = std::io::pipe()?;
        let (mut two, two_write) = std::io::pipe()?;
        let first = notify(Signal::Usr1, one_write.into())?;
        let _second = notify(Signal::Usr1, two_write.into())?;
        kill(pid, Signal::Usr1)?;
        assert_eq!(next(&mut one), Some(1));
        assert_eq!(next(&mut two), Some(1));
        drop(first);
        // One pipe for two signals tells each by name, and is empty after.
        let both = Pipe::new(&[Signal::Usr2, Signal::Winch])?;
        kill(pid, Signal::Winch)?;
        kill(pid, Signal::Usr2)?;
        let mut came = Vec::new();
        while came.len() < 2 {
            let mut fds = [PollFd::new(both.reader(), Events::IN)];
            assert!(poll(&mut fds, Some(Duration::from_secs(5))).is_ok_and(|n| n == 1));
            came.extend(std::iter::from_fn(|| both.next()));
        }
        let mut both_ways = [Signal::Usr2, Signal::Winch];
        both_ways.sort_by_key(|signal| signal.raw());
        came.sort_by_key(|signal| signal.raw());
        assert_eq!(came, both_ways);
        assert_eq!(both.next(), None);
        kill(pid, Signal::Usr1)?;
        assert_eq!(next(&mut two), Some(1));
        assert_eq!(next(&mut one), Some(0), "a dropped notice's pipe is closed");
        Ok(())
    }
}
