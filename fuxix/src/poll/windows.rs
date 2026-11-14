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
//! A child's end, which Windows can't put on a port itself, is a
//! thread-pool wait (`RegisterWaitForSingleObject`) whose callback only
//! posts a packet. A timeout is a high-resolution waitable timer, waited on
//! by a thread of its own at time-critical priority that posts a packet as
//! it fires: the port's own millisecond timeout ends a 1 ms wait 13 to 15
//! ms late, and a thread-pool wait on the timer was up to 3.7 ms late on
//! CI (windows-11-arm). The port's own timeout stays behind the timer, so
//! a timed wait ends, late, should the timer's thread ever stop.
//!
//! **One thread polls at a time.** A packet one poller takes may be what
//! another waits for, so a second thread's poll while one waits fails
//! with `ERROR_BUSY` rather than miss it. fux polls on one thread (D-2).
use crate::errno::{Errno, Result};
use std::marker::PhantomData;
use std::ops::{BitOr, BitOrAssign};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::IO::{
    CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY,
    PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateWaitableTimerExW,
    GetCurrentThread, INFINITE, RegisterWaitForSingleObject, SetThreadPriority, SetWaitableTimer,
    THREAD_PRIORITY_TIME_CRITICAL, TIMER_ALL_ACCESS, UnregisterWaitEx, WT_EXECUTEINWAITTHREAD,
    WT_EXECUTEONLYONCE, WaitForSingleObject,
};

/// What a source is waited for, or was found ready for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events(u8);

impl Events {
    /// None asked for, and nothing told: no read is armed for it, and
    /// what ended is not told over and over to a loop done with it.
    pub const NONE: Self = Self(0);
    /// A read gives bytes, or the end, or its error, at once.
    pub const IN: Self = Self(1);
    /// A write takes bytes at once: none is in flight.
    pub const OUT: Self = Self(2);
    /// A read or write failed, for a reason other than the stream's end.
    pub const ERR: Self = Self(4);
    /// The other end is gone: a read came to the end and nothing read is
    /// left, or only its end is asked for (`HUP`, not `IN`), or a write
    /// found no reader.
    ///
    /// Asked for without `IN`, it is what a source not read yet is waited
    /// for to learn of its end, which only a read that ends tells: a read
    /// is kept in flight, and what it reads is kept for later, up to a
    /// bound (`crate::io`).
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
        /// What [`crate::io::read`] does with it: `BADF` if it reads
        /// nothing. No default: each source says.
        fn read(&self, buffer: &mut [u8]) -> crate::errno::Result<usize>;
        /// What [`crate::io::write`] does with it: `BADF` if it writes
        /// nothing.
        fn write(&self, bytes: &[u8]) -> crate::errno::Result<usize>;
        /// What [`crate::io::written`] says of it: whether all it was
        /// given to write is written. A source that keeps nothing to
        /// write has.
        fn written(&self) -> bool {
            true
        }
    }
}

/// Something a poll waits on: an [`crate::io::Stream`], a
/// [`crate::signal::Pipe`], a [`crate::pty::Master`], a
/// [`crate::socket::Client`], a [`Wake`], or this process's stdin and
/// stdout. Sealed: Windows tells readiness only of what fuxix itself keeps
/// a read or a queue for.
pub trait Source: sealed::Sealed {}

impl<S: Source + ?Sized> sealed::Sealed for &S {
    fn arm(&self, interest: Events) {
        (**self).arm(interest);
    }

    fn ready(&self, interest: Events) -> Events {
        (**self).ready(interest)
    }

    fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        (**self).read(buffer)
    }

    fn write(&self, bytes: &[u8]) -> Result<usize> {
        (**self).write(bytes)
    }

    fn written(&self) -> bool {
        (**self).written()
    }
}

impl<S: Source + ?Sized> Source for &S {}

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
        // What finished already, taken in without waiting: one batch, so
        // that packets that keep coming can't keep a poll from looking.
        take(port, 0)?;
        // Armed after the take: a packet taken may have left a source with
        // nothing in flight (a read of no bytes), which is started again
        // here rather than told ready with nothing to read.
        for fd in fds.iter() {
            fd.source.arm(fd.events);
        }
        let mut ready = 0usize;
        for fd in fds.iter_mut() {
            fd.revents = match fd.events.is_empty() {
                true => Events::NONE,
                false => fd.source.ready(fd.events),
            };
            ready = ready.saturating_add(usize::from(!fd.revents.is_empty()));
        }
        if ready > 0 {
            return Ok(ready);
        }
        // Looked at after every batch: wakes for others can't hold off
        // the timeout.
        let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if left.is_some_and(|left| left.is_zero()) {
            return Ok(0);
        }
        let mut backstop = INFINITE;
        if let Some(left) = left {
            timer()?.set(left)?;
            *timed = true;
            // Whole milliseconds plus one, never shorter than the timer:
            // the timer ends the wait first; this only if its thread has
            // stopped.
            let millis = left.as_millis().saturating_add(1);
            backstop = u32::try_from(millis).unwrap_or(INFINITE).min(INFINITE - 1);
        }
        take(port, backstop)?;
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

/// A registered thread-pool wait, unregistered when dropped.
pub(crate) struct Wait(HANDLE);

// SAFETY: a registered wait may be unregistered from any thread.
unsafe impl Send for Wait {}

impl Wait {
    /// Runs `callback` once, on the pool's wait thread, as `object` is
    /// signalled: the object stays open till this is dropped, and
    /// `callback` takes no context and is short.
    pub(crate) fn once(
        object: HANDLE,
        callback: unsafe extern "system" fn(*mut core::ffi::c_void, bool),
    ) -> Result<Wait> {
        port()?;
        let mut wait: HANDLE = std::ptr::null_mut();
        // SAFETY: `object` is open for as long as this wait lives, by this
        // function's own terms; the callback touches no memory of the
        // caller's.
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                object,
                Some(callback),
                std::ptr::null(),
                INFINITE,
                WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD,
            )
        };
        match registered {
            0 => Err(Errno::last()),
            _ => Ok(Wait(wait)),
        }
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        // SAFETY: unregisters this wait, once, waiting for a callback
        // already running to finish, before the object it waits on closes.
        unsafe { UnregisterWaitEx(self.0, INVALID_HANDLE_VALUE) };
    }
}

/// A [`Wait`]'s callback that only wakes the poll, which looks at what
/// ended.
pub(crate) unsafe extern "system" fn woken(_: *mut core::ffi::c_void, _: bool) {
    // A wake that can't be posted has nobody to tell.
    let _ = wake();
}

/// Ready to read once its [`Poke`] was poked, till [`Wake::drain`].
pub struct Wake(Arc<AtomicBool>);

/// What wakes a poll on its [`Wake`], from any thread.
#[derive(Clone)]
pub struct Poke(Arc<AtomicBool>);

/// A source another thread makes ready.
pub fn waker() -> std::io::Result<(Wake, Poke)> {
    port().map_err(std::io::Error::from)?;
    let poked = Arc::new(AtomicBool::new(false));
    Ok((Wake(poked.clone()), Poke(poked)))
}

impl Wake {
    /// Takes what was poked: it is not ready again till the next poke.
    pub fn drain(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Poke {
    /// Makes its [`Wake`] ready, and wakes the poll.
    pub fn poke(&self) {
        self.0.store(true, Ordering::SeqCst);
        // A wake that can't be posted has a broken port: the poll fails.
        let _ = wake();
    }
}

impl sealed::Sealed for Wake {
    fn arm(&self, _: Events) {}

    fn ready(&self, interest: Events) -> Events {
        match interest.intersects(Events::IN) && self.0.load(Ordering::SeqCst) {
            true => Events::IN,
            false => Events::NONE,
        }
    }

    fn read(&self, _: &mut [u8]) -> Result<usize> {
        Err(Errno::BADF)
    }

    fn write(&self, _: &[u8]) -> Result<usize> {
        Err(Errno::BADF)
    }
}

impl Source for Wake {}

/// Takes the packets queued, up to [`PACKETS`], waiting at most `millis`
/// for the first.
fn take(port: HANDLE, millis: u32) -> Result<()> {
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
            true => Ok(()),
            false => Err(errno),
        };
    }
    let taken = usize::try_from(taken).unwrap_or(0);
    for entry in entries.iter().take(taken) {
        if entry.lpCompletionKey == KEY_IO {
            crate::io::finished(entry);
        }
    }
    Ok(())
}

/// The timer a poll's timeout is waited out with: high-resolution, and
/// auto-reset, so its thread ([`ring`]) wakes the poll each time it
/// fires.
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
            let io =
                |error: std::io::Error| Errno::from_io_error(&error).unwrap_or_else(Errno::last);
            let ours = timer.try_clone().map_err(io)?;
            std::thread::Builder::new()
                .name("fuxix-timer".into())
                .spawn(move || ring(&ours))
                .map_err(io)?;
            Ok(Timer(timer))
        })
        .as_ref()
        .map_err(|errno| *errno)
}

/// The timer's thread: a wake each time `timer` fires, at a priority the
/// scheduler runs at once. Should a wait on it fail, it ends, and a timed
/// wait ends on the port's own timeout, late but never early.
fn ring(timer: &OwnedHandle) {
    // SAFETY: GetCurrentThread takes nothing and gives a pseudo handle.
    let this = unsafe { GetCurrentThread() };
    // SAFETY: the pseudo handle is this thread's; it touches no memory.
    // Should it refuse, the thread runs at normal priority.
    unsafe { SetThreadPriority(this, THREAD_PRIORITY_TIME_CRITICAL) };
    loop {
        // SAFETY: the timer is open, this thread's own copy.
        if unsafe { WaitForSingleObject(timer.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return;
        }
        // A post that fails has nobody to tell: the poll's own timeout
        // ends its wait.
        let _ = wake();
    }
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
