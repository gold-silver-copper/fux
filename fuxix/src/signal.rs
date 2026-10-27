//! Signals as bytes on a pipe, for a poll loop to wait on.
use crate::errno::{Errno, Result, check};
use crate::process::Signal;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

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
/// Make `write` non-blocking: a full pipe then drops the byte rather than
/// hold the handler, and one byte waiting is enough to wake a poll. Several
/// notices of one signal each get a byte.
///
/// The handler replaces what the signal did before, for good, with
/// `SA_RESTART`. Once every notice of a signal is dropped, the signal is
/// caught and does nothing. This fails with `INVAL` for a signal that can't
/// be caught (`KILL`, `STOP`), and with `NOSPC` when 64 notices are held
/// already.
pub fn notify(signal: Signal, write: OwnedFd) -> Result<Notice> {
    install(signal)?;
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
    Ok(Notice {
        slot,
        _write: write,
    })
}

/// A signal's arrivals written to a pipe, from [`notify`]; dropped, they
/// stop.
pub struct Notice {
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

/// Makes [`handle`] what `signal` runs.
fn install(signal: Signal) -> Result<()> {
    // SAFETY: `sigaction` is plain data, for which all zeros is valid: no
    // handler, no flags, and an empty mask.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: the mask is a `sigset_t` this function owns.
    check(unsafe { libc::sigemptyset(&mut action.sa_mask) })?;
    // SAFETY: `action` is a valid action for the call, and the old one is
    // not asked for.
    check(unsafe { libc::sigaction(signal.raw(), &action, std::ptr::null_mut()) }).map(drop)
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
            if fd != FREE {
                // SAFETY: `write` is async-signal-safe, and the byte is
                // readable for the call. The descriptor is open: its notice
                // closes it only after clearing the signal and seeing no
                // handler running, and this one counted itself first. A
                // write that fails (a full pipe) is dropped.
                unsafe { libc::write(fd, [0u8].as_ptr().cast(), 1) };
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
    use std::io::{PipeReader, Read};
    use std::time::Duration;

    /// The pipe's next byte, or 0 for its end; `None` if none comes in 5 s.
    fn next(pipe: &mut PipeReader) -> Option<usize> {
        let mut fds = [PollFd::new(pipe, Events::IN)];
        poll(&mut fds, Some(Duration::from_secs(5))).ok()?;
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
        kill(pid, Signal::Usr1)?;
        assert_eq!(next(&mut two), Some(1));
        assert_eq!(next(&mut one), Some(0), "a dropped notice's pipe is closed");
        Ok(())
    }
}
