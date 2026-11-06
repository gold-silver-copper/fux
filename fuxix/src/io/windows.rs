//! Reading and writing on Windows: a [`Stream`], a pipe end bound to the
//! process's completion port (`crate::poll`), which reads ahead into a
//! buffer of its own and writes from another, one overlapped call each at
//! a time. [`read`], [`write`] and [`write_what_fits`] take from and give
//! to those buffers, and fail with `AGAIN` as a nonblocking descriptor's
//! calls do on Unix.
use crate::errno::{Errno, Result};
use crate::poll::{Events, KEY_IO, Source, sealed::Sealed};
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    RtlNtStatusToDosError, STATUS_PENDING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SetFileCompletionNotificationModes, WriteFile,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, OVERLAPPED, OVERLAPPED_ENTRY,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::FILE_SKIP_COMPLETION_PORT_ON_SUCCESS;

/// Each buffer's size, and each pipe's in the kernel: as much as one read
/// gives or one write takes.
const BUFFER: usize = 64 * 1024;

/// How many reads in a row may finish at once with nothing before one
/// call gives up: a writer writing nothing over and over can't hold a
/// poll from its timeout.
const EMPTY_READS: usize = 16;

/// How much a stream only watched for its end ([`Events::WATCH`]) keeps of
/// what it reads, to read on to that end. A peer that wrote past it and
/// more than its pipe holds is read no further until it is read: that end
/// is told then, as a peer blocked on a full Unix socket has not ended.
/// With the read buffer's own bytes, a stream holds 256 KiB at most.
const AHEAD: usize = 3 * BUFFER;

/// A byte stream on Windows: one end of a pipe fuxix made ([`pipe`],
/// [`pipe_for_child`]), to wait on with [`crate::poll`] and read and write
/// with [`read`], [`write`] and [`write_what_fits`].
///
/// Dropped, it cancels its read and closes its handle at once, or, with a
/// write in flight, once that write has landed, polled or not ([`linger`]):
/// what was written reaches the reader, and then its end, as a closed
/// descriptor's bytes do on Unix.
pub struct Stream {
    /// Shared with a write in flight, which closes it if it lands last.
    handle: Arc<OwnedHandle>,
    read: Arc<Op>,
    write: Arc<Op>,
}

/// One overlapped call at a time, a read or a write, and its buffer.
///
/// **The buffer and the `OVERLAPPED` stay put while the kernel uses them:**
/// both are in this `Op`, behind an `Arc`, and the buffer is a box that is
/// never resized. Before each call a count of the `Arc` is lent to the
/// kernel ([`Op::lend`]) and stays lent while the call is in flight: only
/// taking its packet ([`finished`]) gives it back, so neither a dropped
/// stream nor anything else frees them first. A call that finishes, or
/// fails, at once queues no packet (`FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`)
/// and its count is taken back there and then. Neither is touched while
/// `pending`: every access holds `state`'s lock and checks it first.
#[repr(C)]
struct Op {
    /// First, so that a packet's `OVERLAPPED` pointer is the `Op`'s.
    overlapped: UnsafeCell<OVERLAPPED>,
    bytes: UnsafeCell<Box<[u8]>>,
    kind: Kind,
    /// A write's: set by the kernel as its call ends, for [`linger`] to
    /// wait on; a manual-reset event, reset before each call.
    event: Option<OwnedHandle>,
    state: Mutex<State>,
}

// SAFETY: the kernel uses `overlapped` and `bytes` only while a call is
// in flight, and fuxix only under `state`'s lock while none is (`bytes`,
// `fresh`), so no two threads touch them at once.
unsafe impl Sync for Op {}
// SAFETY: as above; nothing in an `Op` belongs to one thread.
unsafe impl Send for Op {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Read,
    Write,
}

#[derive(Default)]
struct State {
    /// A call is in flight: the kernel holds the buffer.
    pending: bool,
    /// A read's bytes read and not yet taken, or a write's not yet
    /// written: `start..end` of the buffer.
    start: usize,
    end: usize,
    /// A read's bytes before those, set aside while the stream was only
    /// watched for its end ([`Op::set_aside`]).
    ahead: VecDeque<u8>,
    /// Why it stopped: the error a call ended with, a read's end among
    /// them. Told by every later call.
    failed: Option<Errno>,
    /// A write in flight's handle, for its rest and to close it should
    /// the stream be dropped before it lands.
    handle: Option<Arc<OwnedHandle>>,
}

fn lock(op: &Op) -> MutexGuard<'_, State> {
    op.state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Op {
    fn new(kind: Kind) -> Result<Arc<Op>> {
        let event = match kind {
            Kind::Read => None,
            Kind::Write => {
                // SAFETY: makes a new unnamed manual-reset event, not set,
                // with no security attributes.
                let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
                if raw.is_null() {
                    return Err(Errno::last());
                }
                // SAFETY: `raw` was just made, and nothing else owns it.
                Some(unsafe { OwnedHandle::from_raw_handle(raw) })
            }
        };
        Ok(Arc::new(Op {
            overlapped: UnsafeCell::new(OVERLAPPED::default()),
            bytes: UnsafeCell::new(vec![0; BUFFER].into_boxed_slice()),
            kind,
            event,
            state: Mutex::new(State::default()),
        }))
    }

    /// The buffer, while no call is in flight; `state` is this op's,
    /// locked.
    fn bytes<'a>(&'a self, state: &'a mut State) -> &'a mut [u8] {
        if state.pending {
            return &mut [];
        }
        // SAFETY: no call is in flight, so the kernel is not using the
        // buffer, and `state` borrows this op's lock, so no other thread
        // is either, for as long as the slice lives.
        unsafe { &mut *self.bytes.get() }
    }

    /// A zeroed `OVERLAPPED` for the next call, while none is in flight.
    fn fresh(&self, state: &State) -> *mut OVERLAPPED {
        let overlapped = self.overlapped.get();
        if !state.pending {
            // A write's event, which the kernel sets as the call ends; its
            // low bit is clear, so the port still gets the packet.
            let event = self.event.as_ref().map_or(std::ptr::null_mut(), |event| {
                // SAFETY: the event is this op's own and open.
                unsafe { ResetEvent(event.as_raw_handle()) };
                event.as_raw_handle()
            });
            let fresh = OVERLAPPED {
                hEvent: event,
                ..OVERLAPPED::default()
            };
            // SAFETY: no call is in flight, and the lock `state` is under
            // is held: nothing else reads or writes it now.
            unsafe { overlapped.write(fresh) };
        }
        overlapped
    }

    /// Lends the kernel a count before a call; [`Op::reclaim`] takes it
    /// back if the call queues no packet.
    fn lend(self: &Arc<Self>) -> *const Op {
        Arc::into_raw(Arc::clone(self))
    }

    /// Takes back a count [`Op::lend`] lent for a call that is not in
    /// flight.
    fn reclaim(lent: *const Op) {
        // SAFETY: `lent` came from `lend`, and the call it was lent for
        // finished or failed at once, so no packet will hand it back.
        drop(unsafe { Arc::from_raw(lent) });
    }

    /// Starts a read into the buffer, emptied. One that finishes at once
    /// leaves its bytes there; one that fails at once, its error. One that
    /// finishes at once with nothing (a writer wrote nothing) queues no
    /// packet, so it is started again, up to [`EMPTY_READS`] times: past
    /// that, nothing is in flight, and the poll is woken so that it checks
    /// its timeout and arms it again rather than sleeps; it is not told
    /// readable, as it has nothing to read (D-6).
    fn read(self: &Arc<Self>, handle: &OwnedHandle, state: &mut State) {
        for _ in 0..EMPTY_READS {
            if !self.read_once(handle, state) {
                return;
            }
        }
        // A wake that can't be posted has a broken port: the poll fails.
        let _ = crate::poll::wake();
    }

    /// One read call: whether it finished at once with nothing and no
    /// error, to be made again.
    fn read_once(self: &Arc<Self>, handle: &OwnedHandle, state: &mut State) -> bool {
        let overlapped = self.fresh(state);
        let buffer = self.bytes(state);
        let (into, room) = (
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).unwrap_or(0),
        );
        let lent = self.lend();
        let mut got = 0u32;
        // SAFETY: the handle is open; `into` is writable for `room` bytes
        // and `overlapped` is fresh, and both stay put, unused by fuxix,
        // until the call is done (see `Op`).
        let done = unsafe { ReadFile(handle.as_raw_handle(), into, room, &mut got, overlapped) };
        let errno = (done == 0).then(Errno::last);
        if errno == Some(Errno::from_code(ERROR_IO_PENDING)) {
            state.pending = true;
            return false;
        }
        Op::reclaim(lent);
        (state.start, state.end) = (0, usize::try_from(got).unwrap_or(0));
        state.failed = errno;
        got == 0 && errno.is_none()
    }

    /// Moves what a finished read left aside, if it fits in [`AHEAD`], so
    /// that the next read can look for the end behind it: whether it did.
    fn set_aside(&self, state: &mut State) -> bool {
        let (start, end) = (state.start, state.end);
        let held = state.ahead.len().saturating_add(end.saturating_sub(start));
        if state.pending || state.failed.is_some() || start >= end || held > AHEAD {
            return false;
        }
        let mut ahead = std::mem::take(&mut state.ahead);
        ahead.extend(self.bytes(state).get(start..end).unwrap_or_default());
        state.ahead = ahead;
        (state.start, state.end) = (0, 0);
        true
    }

    /// Writes `start..end` of the buffer: at once as far as the pipe takes
    /// it, then one call left in flight, holding `handle`.
    fn write(self: &Arc<Self>, handle: &Arc<OwnedHandle>, state: &mut State) {
        while state.start < state.end && state.failed.is_none() {
            let overlapped = self.fresh(state);
            let (start, end) = (state.start, state.end);
            let Some(rest) = self.bytes(state).get(start..end) else {
                return;
            };
            let (from, count) = (rest.as_ptr(), u32::try_from(rest.len()).unwrap_or(0));
            let lent = self.lend();
            let mut put = 0u32;
            // SAFETY: the handle is open; `from` is readable for `count`
            // bytes and `overlapped` is fresh, and both stay put, unused by
            // fuxix, until the call is done (see `Op`).
            let done =
                unsafe { WriteFile(handle.as_raw_handle(), from, count, &mut put, overlapped) };
            let errno = (done == 0).then(Errno::last);
            if errno == Some(Errno::from_code(ERROR_IO_PENDING)) {
                state.pending = true;
                state.handle = Some(Arc::clone(handle));
                return;
            }
            Op::reclaim(lent);
            state.failed = errno;
            state.start = start.saturating_add(usize::try_from(put).unwrap_or(0));
            if put == 0 {
                return;
            }
        }
    }
}

/// Records what a stream's call finished with, from its packet, which
/// hands back the count it was lent. Every packet with [`KEY_IO`] is
/// one, as only [`Stream`]s are bound to the port.
pub(crate) fn finished(entry: &OVERLAPPED_ENTRY) {
    if entry.lpOverlapped.is_null() {
        return;
    }
    // SAFETY: the `OVERLAPPED` is the first field of an `Op` whose count
    // `Op::lend` lent for this call, which its packet hands back.
    let op = unsafe { Arc::from_raw(entry.lpOverlapped.cast::<Op>().cast_const()) };
    // The packet's own status, kept in its `OVERLAPPED` (`Internal`), not
    // the handle's: an `NTSTATUS`, as Windows' own code says it.
    // SAFETY: the call is done, so the kernel no longer writes it.
    let internal = unsafe { (*op.overlapped.get()).Internal };
    let status = u32::try_from(internal & 0xFFFF_FFFF)
        .unwrap_or(u32::MAX)
        .cast_signed();
    // SAFETY: it reads a number and touches no memory.
    let code = unsafe { RtlNtStatusToDosError(status) };
    let moved = usize::try_from(entry.dwNumberOfBytesTransferred).unwrap_or(0);
    let mut state = lock(&op);
    state.pending = false;
    let handle = state.handle.take();
    if code != 0 {
        state.failed = Some(Errno::from_code(code));
        return;
    }
    match op.kind {
        // A read of nothing (a writer wrote nothing) is not the end: the
        // next poll reads again.
        Kind::Read => (state.start, state.end) = (0, moved),
        Kind::Write => {
            state.start = state.start.saturating_add(moved);
            if let Some(handle) = &handle {
                op.write(handle, &mut state);
            }
        }
    }
    // A stream dropped while this call was in flight closes here, unless
    // the rest of its write took the handle on.
    drop(state);
    drop(handle);
}

impl Stream {
    /// Binds `handle`, opened overlapped, to the process's port.
    pub(crate) fn new(handle: OwnedHandle) -> Result<Stream> {
        let raw = handle.as_raw_handle();
        let port = crate::poll::port()?;
        // SAFETY: binds an open handle to the open port, for good; the key
        // tells its packets.
        if unsafe { CreateIoCompletionPort(raw, port, KEY_IO, 0) }.is_null() {
            return Err(Errno::last());
        }
        let skip = u8::try_from(FILE_SKIP_COMPLETION_PORT_ON_SUCCESS).unwrap_or(0);
        // SAFETY: sets a flag on an open handle; no memory is touched.
        if unsafe { SetFileCompletionNotificationModes(raw, skip) } == 0 {
            return Err(Errno::last());
        }
        Ok(Stream {
            handle: Arc::new(handle),
            read: Op::new(Kind::Read)?,
            write: Op::new(Kind::Write)?,
        })
    }
}

impl Sealed for Stream {
    /// Keeps a read in flight for `IN`, and for [`Events::WATCH`] too: a
    /// poll that wants no bytes yet still learns that the other end went,
    /// which only a read that ends tells. What such a poll's reads bring
    /// is set aside, [`AHEAD`] at most, for the read that follows; a read
    /// that finishes at once queues no packet, so it goes on here.
    fn arm(&self, interest: Events) {
        if !interest.intersects(Events::IN | Events::HUP) {
            return;
        }
        let mut state = lock(&self.read);
        loop {
            if !state.pending && state.start >= state.end && state.failed.is_none() {
                self.read.read(&self.handle, &mut state);
            }
            if interest.intersects(Events::IN) || !self.read.set_aside(&mut state) {
                break;
            }
        }
        drop(state);
    }

    fn ready(&self, interest: Events) -> Events {
        let mut ready = Events::NONE;
        let read = lock(&self.read);
        let buffered = read.start < read.end || !read.ahead.is_empty();
        let reads = interest.intersects(Events::IN);
        if reads && (buffered || read.failed.is_some()) {
            ready |= Events::IN;
        }
        // Asked for no bytes, its end is told past those it holds, as
        // Unix's `poll` tells it past those a socket holds.
        match read.failed {
            Some(errno) if !(reads && buffered) && errno.is_end() => ready |= Events::HUP,
            Some(_) if !(reads && buffered) => ready |= Events::ERR,
            Some(_) | None => {}
        }
        drop(read);
        let write = lock(&self.write);
        let idle = !write.pending && write.start >= write.end;
        let failed = write.failed;
        drop(write);
        match failed {
            Some(errno) if errno.is_peer_gone() => ready |= Events::ERR | Events::HUP,
            Some(_) => ready |= Events::ERR,
            None if interest.intersects(Events::OUT) && idle => ready |= Events::OUT,
            None => {}
        }
        ready
    }

    fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        self.take(buffer)
    }

    fn write(&self, bytes: &[u8]) -> Result<usize> {
        self.give(bytes)
    }
}

impl Source for Stream {}

/// Its handle, to ask the system about it ([`crate::socket::peer`]).
impl std::os::windows::io::AsHandle for Stream {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        std::os::windows::io::AsHandle::as_handle(&*self.handle)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let read = lock(&self.read);
        if read.pending {
            // SAFETY: cancels this stream's own read, on its open handle;
            // its packet still comes, and hands the op back.
            unsafe { CancelIoEx(self.handle.as_raw_handle(), self.read.overlapped.get()) };
        }
        drop(read);
        // The handle closes as the fields drop, unless a write in flight
        // holds it: then once it lands, as a poll takes its packet
        // (`finished`) or, if none polls again, as `linger` sees it end.
        if lock(&self.write).pending {
            let write = Arc::clone(&self.write);
            // A thread that can't be started leaves it to the next poll.
            let _ = std::thread::Builder::new()
                .name("fuxix-linger".into())
                .spawn(move || linger(&write));
        }
    }
}

/// Waits for the write in flight of a dropped stream to land, then closes
/// the handle it holds, so the reader sees the end with no poll to take
/// the packet. The packet still comes, and hands back its count, later.
/// A poll that takes it first goes on with the rest of the write; that is
/// waited for too.
fn linger(op: &Op) {
    let Some(event) = &op.event else {
        return;
    };
    loop {
        // SAFETY: the event is the op's own, open while `op` lives.
        unsafe { WaitForSingleObject(event.as_raw_handle(), INFINITE) };
        let mut state = lock(op);
        if state.handle.is_none() {
            return;
        }
        // `Internal`, the status word, is an `OVERLAPPED`'s first field.
        let status = op.overlapped.get().cast::<usize>().cast_const();
        // SAFETY: reads the status word the kernel sets as the call ends;
        // a volatile read of one aligned word of the op's own `OVERLAPPED`,
        // as `HasOverlappedIoCompleted` does.
        let internal = unsafe { status.read_volatile() };
        if usize::try_from(STATUS_PENDING.cast_unsigned()) == Ok(internal) {
            // A later call, which `fresh` reset the event for.
            drop(state);
            continue;
        }
        let handle = state.handle.take();
        drop(state);
        drop(handle);
        return;
    }
}

/// Reads from `source` into `buffer`: how many bytes, 0 at the end, and
/// `AGAIN` while none have come. A [`Stream`] gives what it read ahead,
/// and taking the last starts the next read; a [`crate::pty::Master`] its
/// program's output; stdin what was typed or piped. A source that reads
/// nothing fails with `BADF`.
pub fn read(source: &(impl Source + ?Sized), buffer: &mut [u8]) -> Result<usize> {
    source.read(buffer)
}

/// Writes `bytes` to `source`: how many it took, or `AGAIN` while a
/// [`Stream`]'s write is in flight, whose error is told by the next. A
/// [`crate::pty::Master`] writes its program's input; stdout waits for its
/// reader, as a blocking descriptor does. A source that writes nothing
/// fails with `BADF`.
pub fn write(source: &(impl Source + ?Sized), bytes: &[u8]) -> Result<usize> {
    source.write(bytes)
}

impl Stream {
    /// Takes bytes it read into `buffer`.
    fn take(&self, buffer: &mut [u8]) -> Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let op = &self.read;
        let mut state = lock(op);
        if !state.pending && state.start >= state.end && state.failed.is_none() {
            op.read(&self.handle, &mut state);
        }
        if !state.ahead.is_empty() {
            let mut taken = 0usize;
            for to in buffer.iter_mut() {
                let Some(from) = state.ahead.pop_front() else {
                    break;
                };
                *to = from;
                taken = taken.saturating_add(1);
            }
            return Ok(taken);
        }
        if state.start < state.end {
            let (start, end) = (state.start, state.end);
            let read = op.bytes(&mut state).get(start..end).unwrap_or_default();
            let mut taken = 0usize;
            for (to, from) in buffer.iter_mut().zip(read) {
                *to = *from;
                taken = taken.saturating_add(1);
            }
            state.start = start.saturating_add(taken);
            if state.start >= state.end && state.failed.is_none() {
                op.read(&self.handle, &mut state);
            }
            return Ok(taken);
        }
        match state.failed {
            Some(errno) if errno.is_end() => Ok(0),
            Some(errno) => Err(errno),
            None => Err(Errno::AGAIN),
        }
    }

    /// Gives it as much of `bytes` as its buffer holds, to be written from
    /// there.
    fn give(&self, bytes: &[u8]) -> Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let op = &self.write;
        let mut state = lock(op);
        if let Some(errno) = state.failed {
            return Err(errno);
        }
        if state.pending || state.start < state.end {
            return Err(Errno::AGAIN);
        }
        let mut taken = 0usize;
        for (to, from) in op.bytes(&mut state).iter_mut().zip(bytes) {
            *to = *from;
            taken = taken.saturating_add(1);
        }
        (state.start, state.end) = (0, taken);
        op.write(&self.handle, &mut state);
        // Refused at once, with nothing written: the error is this call's.
        let refused = state.failed.filter(|_| state.start == 0);
        if refused.is_some() {
            state.end = 0;
        }
        drop(state);
        refused.map_or(Ok(taken), Err)
    }
}

/// Writes as much of `bytes` as `stream` takes now: until all are written
/// or a write would wait (`AGAIN`). How many were written, maybe none; an
/// error only if none were, as one that came after some comes again with
/// the next write. [`Errno::is_peer_gone`] tells a reader gone.
pub fn write_what_fits(stream: &(impl Source + ?Sized), bytes: &[u8]) -> Result<usize> {
    let mut written = 0usize;
    while let Some(rest) = bytes.get(written..).filter(|rest| !rest.is_empty()) {
        match write(stream, rest) {
            Ok(0) | Err(Errno::AGAIN) => break,
            Ok(n) => written = written.saturating_add(n),
            Err(_) if written > 0 => break,
            Err(errno) => return Err(errno),
        }
    }
    Ok(written)
}

/// Two ends of one pipe, both waited on in this process: each reads what
/// the other writes.
pub fn pipe() -> Result<(Stream, Stream)> {
    let (ours, theirs) = named(FILE_FLAG_OVERLAPPED)?;
    Ok((Stream::new(ours)?, Stream::new(theirs)?))
}

/// A pipe whose other end is a plain handle, for a program to read and
/// write without overlapped calls: a child's stdio (std's
/// `Stdio::from`, which gives the child a copy it inherits), or a pseudo
/// console's. The handle itself is not inherited.
pub fn pipe_for_child() -> Result<(Stream, OwnedHandle)> {
    let (ours, theirs) = named(0)?;
    Ok((Stream::new(ours)?, theirs))
}

/// A pipe whose other end is overlapped too, for a child that waits on
/// it with fuxix as this process does: it makes the handle it inherits a
/// [`Stream`] of its own (`Stream::try_from`).
pub fn pipe_for_fuxix_child() -> Result<(Stream, OwnedHandle)> {
    let (ours, theirs) = named(FILE_FLAG_OVERLAPPED)?;
    Ok((Stream::new(ours)?, theirs))
}

impl TryFrom<OwnedHandle> for Stream {
    type Error = Errno;

    /// `handle`, opened overlapped and bound to no completion port (one
    /// this process inherited, [`pipe_for_fuxix_child`]'s), as a stream
    /// of this process's.
    fn try_from(handle: OwnedHandle) -> Result<Stream> {
        Stream::new(handle)
    }
}

/// A new named pipe's two ends, as std makes its own: one instance, local
/// clients only, under a name unique to this process. Ours is overlapped;
/// theirs is opened with `flags`. A name some other process holds already
/// is refused (`FILE_FLAG_FIRST_PIPE_INSTANCE`), and the next is tried; a
/// stranger who opened ours first leaves no instance for us, which fails.
fn named(flags: u32) -> Result<(OwnedHandle, OwnedHandle)> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let size = u32::try_from(BUFFER).unwrap_or(0);
    let mut tries = 0u32;
    loop {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!(r"\\.\pipe\fuxix-{}-{number}", std::process::id());
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE;
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: `name` is NUL-ended; no security attributes, so the
        // handle is not inherited and has the default security.
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open,
                mode,
                1,
                size,
                size,
                0,
                std::ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let errno = Errno::last();
            tries = tries.saturating_add(1);
            if errno == Errno::from_code(ERROR_ACCESS_DENIED) && tries < 16 {
                continue;
            }
            return Err(errno);
        }
        // SAFETY: `raw` was just made, and nothing else owns it.
        let ours = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: as above; opened for reading and writing, not shared,
        // not inherited.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(Errno::last());
        }
        // SAFETY: `raw` was just opened, and nothing else owns it.
        return Ok((ours, unsafe { OwnedHandle::from_raw_handle(raw) }));
    }
}

/// Whether this process's stdout is a pipe, whose reader can go.
pub fn stdout_is_fifo() -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
    // SAFETY: it takes a number and touches no memory.
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    // SAFETY: an invalid handle only fails; it touches no memory.
    unsafe { GetFileType(handle) == FILE_TYPE_PIPE }
}

/// How long a poll watching stdout's reader waits at most between looks:
/// nothing tells a pipe's writer its reader went (`stdout_reader_gone`).
pub const STDOUT_LOOK: Option<std::time::Duration> = Some(std::time::Duration::from_millis(100));

/// Whether stdout is a pipe whose reader closed it, as the pipe's own state
/// says: asked of a write-only handle, which `PeekNamedPipe` refuses. Where
/// the system won't say, not gone: the next write tells.
pub(crate) fn stdout_reader_gone() -> bool {
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_PIPE_CLOSING_STATE, FILE_PIPE_LOCAL_INFORMATION, FilePipeLocalInformation,
        NtQueryInformationFile,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    if !stdout_is_fifo() {
        return false;
    }
    // SAFETY: it takes a number and touches no memory.
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut status = IO_STATUS_BLOCK::default();
    let mut local = FILE_PIPE_LOCAL_INFORMATION::default();
    let size = u32::try_from(std::mem::size_of_val(&local)).unwrap_or(0);
    // SAFETY: the handle is this process's stdout; `status` and `local`
    // are writable, `local` for `size` bytes.
    let asked = unsafe {
        NtQueryInformationFile(
            handle,
            &mut status,
            (&raw mut local).cast(),
            size,
            FilePipeLocalInformation,
        )
    };
    asked >= 0 && local.NamedPipeState == FILE_PIPE_CLOSING_STATE
}

/// Points this process's standard output at a copy of `to`, closing what
/// it was, as Unix's `dup2` onto 1 does: a server done with the pipe its
/// starter reads points it at `NUL`, so that pipe breaks.
pub fn redirect_stdout(to: impl std::os::windows::io::AsHandle) -> Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Console::SetStdHandle;
    let copy = (to.as_handle().try_clone_to_owned())
        .map_err(|error| Errno::from_io_error(&error).unwrap_or_else(Errno::last))?;
    // SAFETY: it takes a number and touches no memory.
    let old = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    // SAFETY: the handle is open; from now on it is the process's stdout,
    // which std looks up at each write and never closes.
    if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, copy.as_raw_handle()) } == 0 {
        return Err(Errno::last());
    }
    let _ = copy.into_raw_handle();
    if !old.is_null() && old != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        // SAFETY: the old stdout, no longer the process's, was inherited
        // and is closed once, here.
        unsafe { CloseHandle(old) };
    }
    Ok(())
}
