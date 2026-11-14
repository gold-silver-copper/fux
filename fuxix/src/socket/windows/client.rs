//! A client's end of a server's pipe, off the process's completion port.
//!
//! One thread polls at a time (`crate::poll`), and a poller takes every
//! packet on the port, so an end bound to the port can't be waited on by
//! any other thread. An attached client's worker waits on its requests
//! while the main thread polls, so a [`Client`] is bound to no port: each
//! call has its own event, a thread that reads waits on that, and a poll
//! that watches it does so with a one-shot wait on the same event. It has
//! `UnixStream`'s method names, and what is `UnixStream` on Unix is this
//! here.
use super::{BUSY, Peer, checked, peer};
use crate::errno::{Errno, Result};
use crate::poll::{Events, Source, Wait, sealed::Sealed, woken};
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use windows_sys::Win32::Foundation::{
    ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, WaitForSingleObject,
};

/// What one read takes in.
const BUFFER: usize = 64 * 1024;

/// How much a client only watched for its end ([`Events::HUP`] without `IN`) keeps of
/// what it reads, as `crate::io`'s streams do: past it, that end is told
/// once it is read.
const AHEAD: usize = 3 * BUFFER;

/// A connection to a server, from [`Client::connect`]. Clones share it: a
/// [`Client::shutdown`] on one ends a read or a write waiting on another,
/// on any thread. Dropped, the last clone cancels its read and waits for
/// it, so what the kernel writes to outlives its use.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    connected: SystemTime,
}

/// **What the kernel uses stays put:** this lives behind an `Arc`, its
/// buffer is a box never resized, and neither `read` nor `bytes` is
/// touched while `State::pending`, nor at all but under `state`'s lock,
/// and the last clone waits for a read in flight before they go. A write
/// runs from its caller's bytes, inside the call, which waits for it.
struct Inner {
    handle: OwnedHandle,
    read: UnsafeCell<OVERLAPPED>,
    bytes: UnsafeCell<Box<[u8]>>,
    write: UnsafeCell<OVERLAPPED>,
    /// Set by the kernel as a read ends; manual-reset.
    read_done: OwnedHandle,
    /// And as a write does.
    write_done: OwnedHandle,
    state: Mutex<State>,
    /// One write at a time, held while it waits, as `state` never is.
    writer: Mutex<()>,
}

// SAFETY: the kernel uses `read` and `bytes` only while a call is in
// flight, and fuxix only under `state`'s lock while none is; `write` only
// under `writer`'s; so no two threads touch them at once.
unsafe impl Sync for Inner {}
// SAFETY: as above; nothing in it belongs to one thread.
unsafe impl Send for Inner {}

#[derive(Default)]
struct State {
    /// A read is in flight: the kernel holds the buffer.
    pending: bool,
    /// Bytes read and not yet taken: `start..end` of the buffer.
    start: usize,
    end: usize,
    /// Bytes before those, set aside while it was only watched.
    ahead: VecDeque<u8>,
    /// What the last read ended with, the stream's end among them.
    failed: Option<Errno>,
    /// [`Client::shutdown`] was called: reads end, writes fail.
    shut: bool,
    timeout: Option<Duration>,
    /// The poll's wait on `read_done` while a read is in flight.
    wait: Option<Wait>,
}

fn event() -> Result<OwnedHandle> {
    // SAFETY: makes a new unnamed manual-reset event, not set, with no
    // security attributes.
    let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if raw.is_null() {
        return Err(Errno::last());
    }
    // SAFETY: `raw` was just made, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

impl Client {
    /// Connects to the pipe `name`, checked as [`super::connect`] checks
    /// it and failing as that does.
    pub fn connect(name: &str) -> Result<Client> {
        let (handle, connected) = checked(name, BUSY)?;
        let inner = Inner {
            handle,
            read: UnsafeCell::new(OVERLAPPED::default()),
            bytes: UnsafeCell::new(vec![0; BUFFER].into_boxed_slice()),
            write: UnsafeCell::new(OVERLAPPED::default()),
            read_done: event()?,
            write_done: event()?,
            state: Mutex::new(State::default()),
            writer: Mutex::new(()),
        };
        Ok(Client {
            inner: Arc::new(inner),
            connected,
        })
    }

    /// Who serves it, as [`super::peer`] says.
    pub fn peer(&self) -> Result<Peer> {
        peer(self, self.connected)
    }

    /// Another handle on this connection.
    pub fn try_clone(&self) -> io::Result<Client> {
        Ok(self.clone())
    }

    /// A read waits `timeout` at most from now on (`None`, for ever): then
    /// it fails with `TimedOut`, its read left in flight, so nothing is
    /// lost.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.lock().timeout = timeout;
        Ok(())
    }

    /// Ends the connection for every clone: a read waiting or to come
    /// gives what is held, then the end; a write fails. Only `Both` is
    /// asked for, and only the whole of it can be done here.
    pub fn shutdown(&self, _: std::net::Shutdown) -> io::Result<()> {
        let mut state = self.inner.lock();
        state.shut = true;
        // Under the lock, which every call starts under: none can start
        // after this and miss the flag. A call in flight ends aborted; with
        // none, it fails, and there is nothing to do.
        // SAFETY: the handle is open; a null `OVERLAPPED` cancels all.
        unsafe { CancelIoEx(self.inner.handle.as_raw_handle(), std::ptr::null()) };
        drop(state);
        Ok(())
    }

    /// Reads into `buffer`: how many bytes, 0 at the end, waiting for some
    /// as a blocking socket does.
    fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let inner = &self.inner;
        let mut until = None;
        loop {
            let mut state = inner.lock();
            inner.settle(&mut state);
            match state.take(inner, buffer) {
                Some(result) => return result.map_err(io::Error::from),
                None => inner.start(&mut state),
            }
            if !state.pending {
                continue;
            }
            let timeout = state.timeout;
            drop(state);
            let until =
                *until.get_or_insert_with(|| timeout.map(|t| Instant::now().checked_add(t)));
            let millis = match until.flatten() {
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now()).as_millis();
                    u32::try_from(left.saturating_add(1)).unwrap_or(INFINITE - 1)
                }
                None => INFINITE,
            };
            // SAFETY: the event is open while `inner` lives; it only waits.
            if unsafe { WaitForSingleObject(inner.read_done.as_raw_handle(), millis) }
                != WAIT_OBJECT_0
            {
                return Err(ErrorKind::TimedOut.into());
            }
        }
    }

    /// Writes from `bytes`: how many went, waiting as a blocking socket
    /// does for a reader who is slow.
    fn send(&self, bytes: &[u8]) -> io::Result<usize> {
        let inner = &self.inner;
        let _one = inner.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let count = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let overlapped = inner.write.get();
        let state = inner.lock();
        if state.shut {
            return Err(ErrorKind::BrokenPipe.into());
        }
        // SAFETY: the event is open.
        unsafe { ResetEvent(inner.write_done.as_raw_handle()) };
        let fresh = OVERLAPPED {
            hEvent: inner.write_done.as_raw_handle(),
            ..OVERLAPPED::default()
        };
        // SAFETY: `writer`'s lock is held and no write is in flight.
        unsafe { overlapped.write(fresh) };
        let mut put = 0u32;
        // SAFETY: the handle is open; `bytes` is readable for `count` bytes
        // and outlives the call, which waits below till the write is done;
        // `overlapped` stays put under `writer`'s lock.
        let done = unsafe {
            WriteFile(
                inner.handle.as_raw_handle(),
                bytes.as_ptr(),
                count,
                &mut put,
                overlapped,
            )
        };
        let errno = (done == 0).then(Errno::last);
        // Started under `state`'s lock, so `shutdown` can't miss it.
        drop(state);
        match errno {
            None => return Ok(usize::try_from(put).unwrap_or(0)),
            Some(errno) if errno != Errno::from_code(ERROR_IO_PENDING) => {
                return Err(errno.into());
            }
            Some(_) => {}
        }
        // SAFETY: as above; it waits for the write this call started.
        let ended =
            unsafe { GetOverlappedResult(inner.handle.as_raw_handle(), overlapped, &mut put, 1) };
        match ended {
            0 if Errno::last() == Errno::from_code(ERROR_OPERATION_ABORTED) => {
                Err(ErrorKind::BrokenPipe.into())
            }
            0 => Err(Errno::last().into()),
            _ => Ok(usize::try_from(put).unwrap_or(0)),
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes in a read in flight that ended, if it did. A read cancelled
    /// by [`Client::shutdown`] is its end, told by [`State::take`]; one
    /// cancelled by anything else (the thread that made it ended) is no
    /// failure: the next read goes on.
    fn settle(&self, state: &mut State) {
        if !state.pending {
            return;
        }
        let mut got = 0u32;
        // SAFETY: the handle is open and `read` is the call's, in flight;
        // not waiting, it reads the status the kernel set.
        let done = unsafe {
            GetOverlappedResult(self.handle.as_raw_handle(), self.read.get(), &mut got, 0)
        };
        let errno = (done == 0).then(Errno::last);
        if errno == Some(Errno::from_code(ERROR_IO_INCOMPLETE)) {
            return;
        }
        state.pending = false;
        state.wait = None;
        (state.start, state.end) = (0, usize::try_from(got).unwrap_or(0));
        state.failed = errno.filter(|e| *e != Errno::from_code(ERROR_OPERATION_ABORTED));
    }

    /// Starts a read into the buffer, emptied, unless one is in flight, or
    /// the connection has ended. One that finishes at once leaves its
    /// bytes, or its error, there.
    fn start(&self, state: &mut State) {
        if state.pending || state.shut || state.failed.is_some() || state.start < state.end {
            return;
        }
        // SAFETY: the event is open.
        unsafe { ResetEvent(self.read_done.as_raw_handle()) };
        let fresh = OVERLAPPED {
            hEvent: self.read_done.as_raw_handle(),
            ..OVERLAPPED::default()
        };
        // SAFETY: no read is in flight, and `state`'s lock is held.
        unsafe { self.read.get().write(fresh) };
        // SAFETY: as above: the buffer is free, and this lock is held.
        let buffer = unsafe { &mut *self.bytes.get() };
        let room = u32::try_from(buffer.len()).unwrap_or(0);
        let mut got = 0u32;
        // SAFETY: the handle is open; the buffer is writable for `room`
        // bytes and it and `read` stay put till the call is done (`Inner`).
        let done = unsafe {
            ReadFile(
                self.handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                room,
                &mut got,
                self.read.get(),
            )
        };
        let errno = (done == 0).then(Errno::last);
        if errno == Some(Errno::from_code(ERROR_IO_PENDING)) {
            state.pending = true;
            return;
        }
        (state.start, state.end) = (0, usize::try_from(got).unwrap_or(0));
        state.failed = errno;
    }

    /// Moves what a finished read left aside, if it fits in [`AHEAD`], so
    /// that the next read can look for the end behind it: whether it did.
    fn set_aside(&self, state: &mut State) -> bool {
        let (start, end) = (state.start, state.end);
        let held = state.ahead.len().saturating_add(end.saturating_sub(start));
        if state.pending || state.failed.is_some() || start >= end || held > AHEAD {
            return false;
        }
        // SAFETY: no read is in flight, and `state`'s lock is held.
        let bytes = unsafe { &*self.bytes.get() };
        state
            .ahead
            .extend(bytes.get(start..end).unwrap_or_default());
        (state.start, state.end) = (0, 0);
        true
    }
}

impl State {
    fn ended(&self) -> bool {
        self.shut || self.failed.is_some_and(Errno::is_end)
    }

    /// What a read gives now, none if it must wait: bytes held, or the
    /// connection's end (0), or why it failed.
    fn take(&mut self, inner: &Inner, buffer: &mut [u8]) -> Option<Result<usize>> {
        if !self.ahead.is_empty() {
            let mut count = 0usize;
            for to in buffer.iter_mut() {
                let Some(from) = self.ahead.pop_front() else {
                    break;
                };
                *to = from;
                count = count.saturating_add(1);
            }
            return Some(Ok(count));
        }
        if !self.pending && self.start < self.end {
            // SAFETY: no read is in flight, and the caller holds the lock.
            let bytes = unsafe { &*inner.bytes.get() };
            let held = bytes.get(self.start..self.end).unwrap_or_default();
            let count = held.len().min(buffer.len());
            for (to, from) in buffer.iter_mut().zip(held) {
                *to = *from;
            }
            self.start = self.start.saturating_add(count);
            return Some(Ok(count));
        }
        if self.ended() {
            return Some(Ok(0));
        }
        self.failed.map(Err)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut state = self.lock();
        // Unregistered here: `read_done` closes before `state` drops.
        state.wait = None;
        if state.pending {
            let mut got = 0u32;
            let handle = self.handle.as_raw_handle();
            // SAFETY: the handle is open; this cancels the read in flight.
            unsafe { CancelIoEx(handle, self.read.get()) };
            // SAFETY: and waits for it, so the buffer outlives the kernel's
            // use.
            unsafe { GetOverlappedResult(handle, self.read.get(), &mut got, 1) };
        }
        drop(state);
    }
}

impl Sealed for Client {
    /// Keeps a read in flight for `IN` and for [`Events::HUP`] alone, with a
    /// wait that wakes the poll as it ends. What a watch's reads bring is
    /// set aside, as a stream's are.
    fn arm(&self, interest: Events) {
        if !interest.intersects(Events::IN | Events::HUP) {
            return;
        }
        let inner = &self.inner;
        let mut state = inner.lock();
        loop {
            inner.settle(&mut state);
            inner.start(&mut state);
            if interest.intersects(Events::IN) || !inner.set_aside(&mut state) {
                break;
            }
        }
        if state.pending && state.wait.is_none() {
            match Wait::once(inner.read_done.as_raw_handle(), woken) {
                Ok(wait) => state.wait = Some(wait),
                // No wake will come: told as the error.
                Err(errno) => state.failed = Some(errno),
            }
        }
    }

    fn ready(&self, interest: Events) -> Events {
        let inner = &self.inner;
        let mut state = inner.lock();
        inner.settle(&mut state);
        let buffered = state.start < state.end || !state.ahead.is_empty();
        let reads = interest.intersects(Events::IN);
        let mut ready = Events::NONE;
        if reads && (buffered || state.ended() || state.failed.is_some()) {
            ready |= Events::IN;
        }
        // Asked for no bytes, its end is told past those it holds, as a
        // stream's is.
        if !(reads && buffered) {
            match state.failed {
                Some(errno) if !errno.is_end() => ready |= Events::ERR,
                _ if state.ended() => ready |= Events::HUP,
                _ => {}
            }
        }
        if interest.intersects(Events::OUT) {
            ready |= Events::OUT;
        }
        ready
    }

    /// Nothing waits here, as a poll said it is ready: a read that would
    /// wait is `AGAIN`.
    fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        let inner = &self.inner;
        let mut state = inner.lock();
        inner.settle(&mut state);
        state.take(inner, buffer).unwrap_or(Err(Errno::AGAIN))
    }

    fn write(&self, bytes: &[u8]) -> Result<usize> {
        self.send(bytes)
            .map_err(|error| Errno::from_io_error(&error).unwrap_or(Errno::IO))
    }
}

impl Source for Client {}

impl AsHandle for Client {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.inner.handle.as_handle()
    }
}

impl io::Read for &Client {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.receive(buffer)
    }
}

impl io::Read for Client {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.receive(buffer)
    }
}

impl io::Write for &Client {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.send(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl io::Write for Client {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.send(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
