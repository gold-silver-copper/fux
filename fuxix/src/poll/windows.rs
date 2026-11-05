//! Waiting on several sources at once, on Windows, over one I/O completion
//! port for the whole process, which no thread of fuxix's own waits on.
//!
//! A Windows handle can't be asked whether it is ready, so readiness is
//! rebuilt from what has finished: a [`crate::io::Stream`] keeps one
//! overlapped read in flight and is ready to read once it finished, and
//! ready to write while no write is in flight; a [`crate::signal::Pipe`]
//! is ready while it holds a signal. Every finished read or write, every
//! signal raised and every timer that fires queues a packet on the port,
//! and [`poll()`] waits for packets until one of its sources is ready.
//!
//! Waits Windows can't put on a port themselves (a child's end, the
//! timer) are thread-pool waits (`RegisterWaitForSingleObject`) whose
//! callback only posts a packet. A timeout is a high-resolution waitable
//! timer: the port's own millisecond timeout ends a 1 ms wait 13 to 15 ms
//! late.
//!
//! **One thread polls at a time.** A packet one poller takes may be what
//! another waits for, so a second thread's poll while one waits fails
//! with `ERROR_BUSY` rather than miss it. fux polls on one thread (D-2).
use crate::errno::{Errno, Result};
use std::marker::PhantomData;
use std::ops::{BitOr, BitOrAssign};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY,
    PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateWaitableTimerExW, INFINITE,
    RegisterWaitForSingleObject, SetWaitableTimer, TIMER_ALL_ACCESS, WT_EXECUTEINWAITTHREAD,
};

/// What a source is waited for, or was found ready for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events(u8);

impl Events {
    /// None asked for: what is told unasked (`HUP`, `ERR`) only.
    pub const NONE: Self = Self(0);
    /// A read gives bytes, or the end, or its error, at once.
    pub const IN: Self = Self(1);
    /// A write takes bytes at once: none is in flight.
    pub const OUT: Self = Self(2);
    /// A read or write failed, for a reason other than the stream's end.
    pub const ERR: Self = Self(4);
    /// The other end is gone: a read came to the end and nothing read is
    /// left, or a write found no reader.
    pub const HUP: Self = Self(8);
    /// Never told on Windows: a source borrowed is always open.
    pub const NVAL: Self = Self(16);

    /// Whether any event of `other` is here.
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Events {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for Events {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

pub(crate) mod sealed {
    use super::Events;

    /// What a [`super::Source`] does for a poll; private, so only fuxix's
    /// types are sources.
    pub trait Sealed {
        /// Starts what must be in flight for `interest` to be told: a
        /// stream's read. Called before every wait.
        fn arm(&self, interest: Events);
        /// What it is ready for now: of `interest`, and `ERR` and `HUP`
        /// unasked.
        fn ready(&self, interest: Events) -> Events;
    }
}

/// Something a poll waits on: an [`crate::io::Stream`] or a
/// [`crate::signal::Pipe`]. Sealed: Windows tells readiness only of what
/// fuxix itself keeps a read or a queue for.
pub trait Source: sealed::Sealed {}

/// A source to wait on, borrowed for as long as it is waited on.
pub struct PollFd<'fd> {
    source: &'fd dyn Source,
    events: Events,
    revents: Events,
}

impl<'fd> PollFd<'fd> {
    pub fn new(source: &'fd impl Source, events: Events) -> Self {
        Self {
            source,
            events,
            revents: Events::NONE,
        }
    }

    /// What `poll` found it ready for.
    pub fn revents(&self) -> Events {
        self.revents
    }
}

/// Poll storage. On Windows a list keeps its own sources, so this holds
/// nothing; it is here so that a loop reads the same on every OS.
#[derive(Default)]
pub struct Poll {
    _private: (),
}

/// Sources borrowed until this list has finished waiting on them. Each is
/// told by the [`Entry`] its push gave.
pub struct List<'poll, 'fd> {
    fds: Vec<PollFd<'fd>>,
    _poll: PhantomData<&'poll mut Poll>,
}

impl Poll {
    pub fn list<'fd>(&mut self) -> List<'_, 'fd> {
        List {
            fds: Vec::new(),
            _poll: PhantomData,
        }
    }
}

/// A source pushed onto a [`List`], to ask it what that one was found
/// ready for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry(usize);

impl<'fd> List<'_, 'fd> {
    pub fn push(&mut self, fd: PollFd<'fd>) -> Entry {
        self.fds.push(fd);
        Entry(self.fds.len().saturating_sub(1))
    }

    /// What `poll` found `entry`, pushed onto this list, ready for.
    pub fn events(&self, entry: Entry) -> Events {
        self.fds.get(entry.0).map_or(Events::NONE, PollFd::revents)
    }

    pub fn poll(&mut self, timeout: Option<Duration>) -> Result<usize> {
        poll(&mut self.fds, timeout)
    }

    pub fn revents(&self) -> impl Iterator<Item = Events> + '_ {
        self.fds.iter().map(PollFd::revents)
    }
}

/// Waits until one of `fds` is ready, or `timeout` passes (`None`: no
/// timeout). How many are ready. Like `ppoll`, it returns 0 only once the
/// timeout has passed, never before, and a zero timeout still takes in
/// what finished. Fails with `ERROR_BUSY` while another thread polls.
pub fn poll(fds: &mut [PollFd<'_>], timeout: Option<Duration>) -> Result<usize> {
    let _polling = Polling::start()?;
    // A timeout too far to count is none.
    let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
    let mut timed = false;
    let ready = wait(fds, deadline, &mut timed);
    if timed {
        // A timer that fired anyway only wakes a later poll, which looks
        // and waits again.
        timer().map(Timer::cancel)?;
    }
    ready
}

fn wait(fds: &mut [PollFd<'_>], deadline: Option<Instant>, timed: &mut bool) -> Result<usize> {
    let port = port()?;
    loop {
        for fd in fds.iter() {
            fd.source.arm(fd.events);
        }
        // What finished already, taken in without waiting.
        while take(port, 0)? == PACKETS {}
        let mut ready = 0usize;
        for fd in fds.iter_mut() {
            fd.revents = fd.source.ready(fd.events);
            ready = ready.saturating_add(usize::from(!fd.revents.is_empty()));
        }
        if ready > 0 {
            return Ok(ready);
        }
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(0);
            }
            timer()?.set(left)?;
            *timed = true;
        }
        take(port, INFINITE)?;
    }
}

/// Whether a thread is polling now.
static POLLING: AtomicBool = AtomicBool::new(false);

/// This thread's turn to poll, until dropped.
struct Polling(());

impl Polling {
    fn start() -> Result<Polling> {
        POLLING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Polling(()))
            .map_err(|_| Errno::BUSY)
    }
}

impl Drop for Polling {
    fn drop(&mut self) {
        POLLING.store(false, Ordering::SeqCst);
    }
}

/// The key of a packet a stream's read or write finished with; its
/// `OVERLAPPED` is that operation's (`crate::io`).
pub(crate) const KEY_IO: usize = 1;
/// The key of a packet that only wakes a poll: a signal raised, the timer.
const KEY_WAKE: usize = 2;

/// Packets taken at most per call.
const PACKETS: usize = 64;

/// The process's completion port, made on first use and kept till it
/// ends: every stream is bound to it as it is made, so a stream made
/// before a [`Poll`], or waited on by more than one, needs nothing more.
pub(crate) fn port() -> Result<HANDLE> {
    static PORT: OnceLock<Result<OwnedHandle>> = OnceLock::new();
    let port = PORT.get_or_init(|| {
        // SAFETY: with no file handle, this makes a new port, which one
        // thread at a time (the poller) waits on.
        let raw =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if raw.is_null() {
            return Err(Errno::last());
        }
        // SAFETY: `raw` was just made, and nothing else owns it.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    });
    port.as_ref()
        .map(AsRawHandle::as_raw_handle)
        .map_err(|errno| *errno)
}

/// Wakes the poll waiting, or the next one: a packet that carries
/// nothing. From any thread.
pub(crate) fn wake() -> Result<()> {
    let port = port()?;
    // SAFETY: the port is open for the process's life; the packet has no
    // `OVERLAPPED`, so `take` reads nothing through it.
    if unsafe { PostQueuedCompletionStatus(port, 0, KEY_WAKE, std::ptr::null()) } == 0 {
        return Err(Errno::last());
    }
    Ok(())
}

/// Takes the packets queued, up to [`PACKETS`], waiting at most `millis`
/// for the first: how many.
fn take(port: HANDLE, millis: u32) -> Result<usize> {
    let mut entries = [OVERLAPPED_ENTRY::default(); PACKETS];
    let mut taken = 0u32;
    let room = u32::try_from(entries.len()).unwrap_or(0);
    // SAFETY: `entries` is writable for `room` entries, and `taken` for
    // one count; the wait is not alertable.
    let done = unsafe {
        GetQueuedCompletionStatusEx(port, entries.as_mut_ptr(), room, &mut taken, millis, 0)
    };
    if done == 0 {
        let errno = Errno::last();
        return match errno == Errno::from_code(WAIT_TIMEOUT) {
            true => Ok(0),
            false => Err(errno),
        };
    }
    let taken = usize::try_from(taken).unwrap_or(0);
    for entry in entries.iter().take(taken) {
        if entry.lpCompletionKey == KEY_IO {
            crate::io::finished(entry);
        }
    }
    Ok(taken)
}

/// The timer a poll's timeout is waited out with: high-resolution, and
/// auto-reset, so a thread-pool wait registered once for good posts a
/// packet each time it fires.
struct Timer(OwnedHandle);

fn timer() -> Result<&'static Timer> {
    static TIMER: OnceLock<Result<Timer>> = OnceLock::new();
    TIMER
        .get_or_init(|| {
            // SAFETY: makes a new unnamed synchronization timer, with no
            // security attributes.
            let raw = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            if raw.is_null() {
                return Err(Errno::last());
            }
            // SAFETY: `raw` was just made, and nothing else owns it.
            let timer = unsafe { OwnedHandle::from_raw_handle(raw) };
            port()?;
            let mut wait: HANDLE = std::ptr::null_mut();
            // SAFETY: the timer is kept open for the process's life, as
            // this wait is, so it is never unregistered; `fired` takes no
            // context and only posts a packet, short enough to run in the
            // wait thread itself.
            let registered = unsafe {
                RegisterWaitForSingleObject(
                    &mut wait,
                    timer.as_raw_handle(),
                    Some(fired),
                    std::ptr::null(),
                    INFINITE,
                    WT_EXECUTEINWAITTHREAD,
                )
            };
            match registered {
                0 => Err(Errno::last()),
                _ => Ok(Timer(timer)),
            }
        })
        .as_ref()
        .map_err(|errno| *errno)
}

/// What the thread pool runs when the timer fires.
unsafe extern "system" fn fired(_: *mut core::ffi::c_void, _: bool) {
    // The port was made before the wait was registered; a post that
    // fails has nobody to tell, and the poll then waits on.
    let _ = wake();
}

impl Timer {
    /// Fires once, `after` from now, rounded up to its 100 ns units.
    fn set(&self, after: Duration) -> Result<()> {
        // Negative: relative to now.
        let due = i64::try_from(after.as_nanos().div_ceil(100)).map_or(i64::MIN, i64::wrapping_neg);
        // SAFETY: the timer is open; `due` is readable for the call, and
        // there is no completion routine.
        let set =
            unsafe { SetWaitableTimer(self.0.as_raw_handle(), &due, 0, None, std::ptr::null(), 0) };
        match set {
            0 => Err(Errno::last()),
            _ => Ok(()),
        }
    }

    fn cancel(&self) {
        // SAFETY: the timer is open; cancelling one not set does nothing.
        unsafe { CancelWaitableTimer(self.0.as_raw_handle()) };
    }
}
