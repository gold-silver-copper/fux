//! Named pipes on Windows, as a server's clients reach it: one duplex,
//! overlapped instance per connection, local clients only, under a name
//! only one server holds, that only its own user may open.
//!
//! - **Owner-only.** Every instance is made with an explicit security
//!   descriptor that lets this process's user in and no one else
//!   (`D:P(A;;GA;;;<user SID>)`), never the default one, which lets any
//!   user open a pipe to read.
//! - **First.** A [`Listener`]'s first instance is made with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`: a name any process holds already
//!   fails with [`Errno::ADDRINUSE`] rather than join that process's pipe.
//!   It always keeps one instance of its own, made before the one a
//!   client took is handed out, so the name is never free while it lives.
//! - **Checked both ways.** A connection's [`peer`] is read from its
//!   process (the pipe's client or server pid, then that process's token),
//!   which must have been made before this end was known to be connected,
//!   as the run record binds a pid to its creation time (D-138), so a pid
//!   another process took since is refused. Its [`User`] is a SID and
//!   whether the token is elevated, so that an
//!   elevated and a normal process of one user are two users here and
//!   refuse each other (D-137). [`connect`] refuses a server that isn't
//!   this process's user, or a pipe not owned as this process makes its
//!   own, before anything is sent.
//! - **Handed to a child.** An instance not yet connected (a [`Spare`])
//!   or connected (a [`Connection`]) becomes a plain handle, given to one
//!   child as std gives a `Stdio::from` handle: inheritable only in the
//!   copy made for that child's spawn. Neither is bound to this process's
//!   completion port until it becomes an [`crate::io::Stream`], so the
//!   child can bind it to its own.
//!
//! A [`Listener`] and a [`Spare`] are [`crate::poll::Source`]s, ready to
//! read once a client connected: a connect in flight is waited on by a
//! thread-pool wait on its event, whose callback only wakes the poll.
//!
//! A [`Client`] is a client's own end, kept off the process's completion
//! port, so that a thread may wait on it while another polls.
use super::Peer;
use crate::errno::{Errno, Result};
use crate::io::Stream;
use crate::poll::{Events, Source, Wait, sealed::Sealed, woken};
use std::cell::UnsafeCell;
use std::fmt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE, ERROR_IO_INCOMPLETE,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT,
    ERROR_SUCCESS, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetLengthSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    PSID, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS,
    TOKEN_QUERY, TokenElevation, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeInfo, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_SERVER_END, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
    WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, ResetEvent,
};

mod client;
pub use client::Client;

/// Each instance's buffers in the kernel, as `io`'s pipes have.
const BUFFER: u32 = 64 * 1024;

/// The longest SID, in bytes.
const SID: usize = SECURITY_MAX_SID_SIZE as usize;

/// A user, as a process's token says it: its SID, and whether the token
/// is elevated. Two processes of one account, one elevated and one not,
/// are two users (D-137). Shown as its SID, "S-1-5-21-…-1001", with
/// " (elevated)" after it when it is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct User {
    /// The SID's bytes, `len` of them; the rest zero.
    sid: [u8; SID],
    len: u8,
    elevated: bool,
}

impl User {
    /// This process's user.
    pub fn current() -> Result<User> {
        static CURRENT: OnceLock<Result<User>> = OnceLock::new();
        // SAFETY: takes nothing; the pseudo-handle needs no closing.
        let this = unsafe { GetCurrentProcess() };
        *CURRENT.get_or_init(|| User::of(this))
    }

    /// Whether its token is elevated: an elevated fux keeps its own
    /// default socket, lock and record (D-137).
    pub fn elevated(&self) -> bool {
        self.elevated
    }

    /// Whether it is this process's user: the same SID, elevated or not
    /// alike. A process whose own user can't be read has none.
    pub fn is_current(&self) -> bool {
        User::current().is_ok_and(|own| own == *self)
    }

    /// The user of the process `process` is a handle to, opened for
    /// `PROCESS_QUERY_LIMITED_INFORMATION` at least.
    fn of(process: HANDLE) -> Result<User> {
        let token = token(process)?;
        let (sid, len) = sid(&token, TokenUser)?;
        let mut size = 0u32;
        let mut elevation = TOKEN_ELEVATION::default();
        let room = u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0);
        // SAFETY: `elevation` is writable for `room` bytes, `size` for
        // one count; the token is open for querying.
        let got = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenElevation,
                std::ptr::from_mut(&mut elevation).cast(),
                room,
                &mut size,
            )
        };
        if got == 0 {
            return Err(Errno::last());
        }
        Ok(User {
            sid,
            len,
            elevated: elevation.TokenIsElevated != 0,
        })
    }

    /// The SID, as Windows writes it: "S-1-5-21-…".
    fn sid(&self) -> String {
        sid_text(self.sid.get(..usize::from(self.len)).unwrap_or_default())
    }
}

/// The token of the process `process` is a handle to, for querying.
/// A SID's bytes as Windows writes them.
fn sid_text(bytes: &[u8]) -> String {
    let revision = bytes.first().copied().unwrap_or(0);
    let authority = (bytes.iter().skip(2).take(6)).fold(0u64, |n, b| n << 8 | u64::from(*b));
    let mut text = format!("S-{revision}-{authority}");
    let (parts, _) = bytes.get(8..).unwrap_or_default().as_chunks::<4>();
    for part in parts {
        text.push_str(&format!("-{}", u32::from_le_bytes(*part)));
    }
    text
}

fn token(process: HANDLE) -> Result<OwnedHandle> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is open, and `raw` is written once on success.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw) } == 0 {
        return Err(Errno::last());
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// The SID `token` holds as `class`, `TokenUser` or `TokenOwner`: its
/// bytes, zero after the first `len`.
fn sid(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<([u8; SID], u8)> {
    // A `TOKEN_USER` or `TOKEN_OWNER`, each a SID pointer first, and the
    // SID it points at, after it: aligned as the pointer must be.
    let mut buffer = [0usize; 16];
    let room = u32::try_from(size_of_val(&buffer)).unwrap_or(0);
    let mut size = 0u32;
    // SAFETY: `buffer` is writable for `room` bytes, `size` for one
    // count; the token is open for querying.
    let got = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            room,
            &mut size,
        )
    };
    if got == 0 {
        return Err(Errno::last());
    }
    // SAFETY: the call filled the buffer, which starts with the SID
    // pointer and is aligned for it.
    let pointer = unsafe { buffer.as_ptr().cast::<PSID>().read() };
    // SAFETY: the SID the call wrote, in `buffer`, which is alive.
    let len = usize::try_from(unsafe { GetLengthSid(pointer) }).unwrap_or(0);
    if len == 0 || len > SID {
        return Err(Errno::from_code(ERROR_ACCESS_DENIED));
    }
    // SAFETY: the SID is `len` bytes of `buffer`, which outlives the
    // slice.
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), len) };
    let mut sid = [0u8; SID];
    for (to, from) in sid.iter_mut().zip(bytes) {
        *to = *from;
    }
    Ok((sid, u8::try_from(len).unwrap_or(0)))
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sid())?;
        if self.elevated {
            f.write_str(" (elevated)")?;
        }
        Ok(())
    }
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "User({self})")
    }
}

/// The pipe name `base` has for this process's user, `\\.\pipe\<base>-<SID>`,
/// with `-elevated` after it when this process is: each user, and an
/// elevated and a normal process of one, have their own (D-137).
pub fn name(base: &str) -> Result<String> {
    let user = User::current()?;
    let elevated = if user.elevated { "-elevated" } else { "" };
    Ok(format!(r"\\.\pipe\{base}-{}{elevated}", user.sid()))
}

/// The process at the other end of `pipe`, a named pipe's end: its
/// client's, on the server's end; its server's on a client's: the process
/// that made the pipe's first instance, whatever made the one reached. `connected` is a moment this end is known to
/// have been connected at, the earliest known: a [`Connection`]'s
/// ([`Connection::connected`]), or on a client's end, when [`connect`]
/// opened it. The peer's process must have been made by then: one made
/// after holds a pid another took first, and is refused with
/// [`Errno::ACCES`]. Its user is read from its token; a process that
/// can't be opened or read, or whose creation time can't be, is an error,
/// which is a refusal.
///
/// On a server's end the moment is when a poll first saw the client, not
/// when it connected: a client that connected and ended, and a process
/// made after it that took its pid, both before that poll, isn't told
/// apart. The gap is as long as the server takes to look.
pub fn peer(pipe: impl AsHandle, connected: SystemTime) -> Result<Peer> {
    let raw = pipe.as_handle().as_raw_handle();
    let mut flags = 0u32;
    let none = std::ptr::null_mut();
    // SAFETY: the handle is open; `flags` is written, the rest not asked.
    if unsafe { GetNamedPipeInfo(raw, &mut flags, none, none, none) } == 0 {
        return Err(Errno::last());
    }
    let mut pid = 0u32;
    let got = match flags & PIPE_SERVER_END != 0 {
        // SAFETY: the handle is open; `pid` is written once.
        true => unsafe { GetNamedPipeClientProcessId(raw, &mut pid) },
        // SAFETY: as above.
        false => unsafe { GetNamedPipeServerProcessId(raw, &mut pid) },
    };
    if got == 0 {
        return Err(Errno::last());
    }
    let pid = crate::process::Pid::from_raw(pid).ok_or(Errno::from_code(ERROR_ACCESS_DENIED))?;
    // SAFETY: opens a process by ID for querying only, not inherited.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_raw()) };
    if process.is_null() {
        return Err(Errno::last());
    }
    // SAFETY: `process` was just opened, and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    // Held open, its pid is its own till we close it: made after the end
    // was connected, it took the pid of the peer, gone since.
    if created(process.as_raw_handle())? > connected {
        return Err(Errno::ACCES);
    }
    let user = User::of(process.as_raw_handle())?;
    // A peer gone with nothing left to read is refused too; one that left
    // bytes queued, or a server whose instance a child holds, is caught by
    // its time alone.
    // SAFETY: the handle is open; with no buffer nothing is written.
    if unsafe { PeekNamedPipe(raw, std::ptr::null_mut(), 0, none, none, none) } == 0 {
        return Err(Errno::last());
    }
    Ok(Peer {
        user,
        pid: Some(pid),
    })
}

/// Connects to the pipe `name`, as a stream, once its server is found to
/// be this process's user ([`peer`]) and the pipe is owned as this
/// process's own pipes are; otherwise it is refused with [`Errno::ACCES`]
/// before anything is sent, as is a pipe the server's descriptor keeps
/// this user out of. A name with a NUL in it is [`Errno::INVAL`]. With
/// every instance taken (a server that hasn't yet made the next) it waits
/// for one, a second at most, then fails with [`Errno::AGAIN`], as a full
/// listener's connect does on Unix; with no such pipe, with
/// `ERROR_FILE_NOT_FOUND`. The server may only identify the client, never
/// act as it.
pub fn connect(name: &str) -> Result<Stream> {
    Stream::new(checked(name, BUSY)?.0)
}

/// [`connect`], without waiting: with every instance taken it fails with
/// [`Errno::AGAIN`] at once, as a full listener's connect does on Unix,
/// for a caller that must not wait on the pipe's server (one that takes
/// no client: stopped, say).
pub fn connect_now(name: &str) -> Result<Stream> {
    Stream::new(checked(name, 0)?.0)
}

/// How long [`connect`] waits for an instance of a pipe whose every one
/// is taken, in ms.
const BUSY: u32 = 1000;

/// The pipe `name` opened, waiting `busy` ms at most for an instance, and
/// found to be ours ([`connect`]), and when.
fn checked(name: &str, busy: u32) -> Result<(OwnedHandle, SystemTime)> {
    let pipe = open(&wide(name)?, busy)?;
    let opened = SystemTime::now();
    if !peer(&pipe, opened)?.user.is_current() || !owned_by_us(&pipe)? {
        return Err(Errno::ACCES);
    }
    Ok((pipe, opened))
}

/// `name` as a NUL-ended wide string; one with a NUL in it, which Windows
/// would cut short there, is refused with [`Errno::INVAL`].
fn wide(name: &str) -> Result<Vec<u16>> {
    match name.contains('\0') {
        true => Err(Errno::INVAL),
        false => Ok(name.encode_utf16().chain([0]).collect()),
    }
}

/// When the process `process` is a handle to was made, opened for
/// `PROCESS_QUERY_LIMITED_INFORMATION` at least.
fn created(process: HANDLE) -> Result<SystemTime> {
    /// 100 ns counts from 1601, when Windows' time starts, to 1970.
    const UNIX: u64 = 116_444_736_000_000_000;
    let [mut made, mut ended, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: the process is open for querying; each time is written once.
    if unsafe { GetProcessTimes(process, &mut made, &mut ended, &mut kernel, &mut user) } == 0 {
        return Err(Errno::last());
    }
    let counts = u64::from(made.dwHighDateTime) << 32 | u64::from(made.dwLowDateTime);
    (counts.checked_sub(UNIX))
        .and_then(|counts| counts.checked_mul(100))
        .and_then(|nanos| SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos)))
        .ok_or(Errno::from_code(ERROR_ACCESS_DENIED))
}

/// Whether the pipe's owner, set by the kernel as its maker made it, is
/// this process's token's default owner. A squatter of another user can't
/// make its pipe owned by us, and this holds even should the server's pid
/// be another process's by now. The default owner (`TokenOwner`), not the
/// user (`TokenUser`): an elevated admin's objects are owned by the
/// Administrators group, and our own server's were made with our token.
fn owned_by_us(pipe: &OwnedHandle) -> Result<bool> {
    owned_as_ours(pipe, SE_KERNEL_OBJECT)
}

/// Whether `object`, a handle of kind `kind` opened with `READ_CONTROL`,
/// is owned by this process's token's default owner ([`owned_by_us`]).
pub(crate) fn owned_as_ours(
    object: &OwnedHandle,
    kind: windows_sys::Win32::Security::Authorization::SE_OBJECT_TYPE,
) -> Result<bool> {
    let (ours, _) = default_owner()?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is open, with `READ_CONTROL` (in `GENERIC_READ`);
    // `owner` and `descriptor` are written once, `owner` pointing into
    // the descriptor, which `Security` frees.
    let got = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            kind,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if got != ERROR_SUCCESS {
        return Err(Errno::from_code(got));
    }
    let descriptor = Security(descriptor);
    // SAFETY: `owner` points into `descriptor`, alive; `ours` is a whole
    // SID, as `sid` checked its length.
    let same = unsafe { EqualSid(owner, ours.as_ptr().cast_mut().cast()) } != 0;
    drop(descriptor);
    Ok(same)
}

/// This process's token's default owner: its user, or for an elevated
/// administrator the Administrators group, which the same user's normal
/// processes don't hold (D-137).
fn default_owner() -> Result<([u8; SID], u8)> {
    static OWNER: OnceLock<Result<([u8; SID], u8)>> = OnceLock::new();
    *OWNER.get_or_init(|| {
        // SAFETY: takes nothing; the pseudo-handle needs no closing.
        sid(&token(unsafe { GetCurrentProcess() })?, TokenOwner)
    })
}

/// Whether the folder `object`, opened with `READ_CONTROL`, lets in no one
/// but this process's default owner, and the system and its administrators,
/// who can't be kept out: Unix's mode 0700. A folder with no list lets in
/// all; an entry of a kind not known is taken to let its holder in, and
/// `CREATOR OWNER` only names who makes what is in it. Who is refused
/// access is no matter.
pub(crate) fn only_ours(object: &OwnedHandle) -> Result<bool> {
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, GetAce, INHERIT_ONLY_ACE,
        IsWellKnownSid, WinBuiltinAdministratorsSid, WinCreatorOwnerSid, WinLocalSystemSid,
    };
    const ALLOWED: u8 = 0;
    const DENIED: u8 = 1;
    let (ours, _) = default_owner()?;
    let mut list: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is open, with `READ_CONTROL`; `list` and
    // `descriptor` are written once, `list` pointing into the descriptor,
    // which `Security` frees.
    let got = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut list,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if got != ERROR_SUCCESS {
        return Err(Errno::from_code(got));
    }
    let descriptor = Security(descriptor);
    if list.is_null() {
        return Ok(false);
    }
    // SAFETY: `list` points into `descriptor`, alive, and is not null.
    let count = unsafe { (*list).AceCount };
    for index in 0..u32::from(count) {
        let mut entry: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `index` is below the list's count; `entry` is written
        // with a pointer into the list.
        if unsafe { GetAce(list, index, &mut entry) } == 0 {
            return Err(Errno::last());
        }
        let entry = entry.cast::<ACCESS_ALLOWED_ACE>().cast_const();
        // SAFETY: every entry starts with a header, here and whole.
        let header = unsafe { (*entry).Header };
        match header.AceType {
            DENIED => continue,
            ALLOWED => {}
            _ => return Ok(false),
        }
        // SAFETY: an allowing entry's SID starts at `SidStart`, inside the
        // list; nothing writes through it, as `PSID`'s type would allow.
        let holder: PSID = unsafe { std::ptr::addr_of!((*entry).SidStart) }
            .cast_mut()
            .cast();
        // SAFETY: `holder` is a whole SID in the list.
        let is = |kind| unsafe { IsWellKnownSid(holder, kind) } != 0;
        // SAFETY: as above, and `ours` is a whole SID, as `sid` checked its
        // length.
        let own = unsafe { EqualSid(holder, ours.as_ptr().cast_mut().cast()) } != 0;
        let trusted = own || is(WinLocalSystemSid) || is(WinBuiltinAdministratorsSid);
        let inherited_only = u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0;
        let creator = inherited_only && is(WinCreatorOwnerSid);
        if !trusted && !creator {
            return Ok(false);
        }
    }
    drop(descriptor);
    Ok(true)
}

/// Opens the pipe `name` (NUL-ended), waiting up to `wait` ms in all for an
/// instance while every one is taken. A wait that fails but for its time
/// running out or the pipe being gone or busy again ends it with that
/// error, rather than trying again at once till the time is up.
fn open(name: &[u16], wait: u32) -> Result<OwnedHandle> {
    let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    let busy = Errno::from_code(ERROR_PIPE_BUSY);
    let start = std::time::Instant::now();
    loop {
        // SAFETY: `name` is NUL-ended; opened for reading and writing, not
        // shared, not inherited.
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
        if raw != INVALID_HANDLE_VALUE {
            // SAFETY: `raw` was just opened, and nothing else owns it.
            return Ok(unsafe { OwnedHandle::from_raw_handle(raw) });
        }
        let errno = Errno::last();
        let gone = u32::try_from(start.elapsed().as_millis()).unwrap_or(wait);
        let left = wait.saturating_sub(gone);
        if errno != busy || left == 0 {
            return Err(if errno == busy { Errno::AGAIN } else { errno });
        }
        // SAFETY: `name` is NUL-ended; it only waits. An instance free
        // again may be taken by another client first: then busy again.
        if unsafe { WaitNamedPipeW(name.as_ptr(), left) } == 0 {
            let errno = Errno::last();
            // Timed out, gone between the two calls, or taken again first.
            let retry = [ERROR_SEM_TIMEOUT, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY];
            let retry = retry.map(Errno::from_code);
            if !retry.contains(&errno) {
                return Err(errno);
            }
        }
    }
}

/// A security descriptor that lets this process's user in, and no one
/// else, from `LocalAlloc`.
pub(crate) struct Security(PSECURITY_DESCRIPTOR);

// SAFETY: memory the process owns, read by the kernel only during calls.
unsafe impl Send for Security {}
// SAFETY: as above; nothing writes it after it is made.
unsafe impl Sync for Security {}

impl Security {
    /// A descriptor for a folder whose files are to be private too: only
    /// this process's default owner is let in, so a folder of an elevated
    /// process's is closed to the same user's normal ones (D-137).
    pub(crate) fn private_folder() -> Result<Security> {
        let (owner, len) = default_owner()?;
        let owner = owner.get(..usize::from(len)).unwrap_or_default();
        Security::only(&format!("D:P(A;OICI;GA;;;{})", sid_text(owner)))
    }

    /// A descriptor letting only this process's user in, protected from
    /// what the object's parent would pass on.
    fn owner_only() -> Result<Security> {
        Security::only(&format!("D:P(A;;GA;;;{})", User::current()?.sid()))
    }

    /// The descriptor `sddl` says.
    fn only(sddl: &str) -> Result<Security> {
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-ended; `descriptor` is written once, with
        // memory this `Security` frees.
        let made = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        match made {
            0 => Err(Errno::last()),
            _ => Ok(Security(descriptor)),
        }
    }
}

impl Security {
    /// Attributes for a call that makes an object with this descriptor,
    /// not inherited; alive while `self` is.
    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: frees the descriptor the conversion allocated, once.
        unsafe { LocalFree(self.0) };
    }
}

/// A new instance of the pipe `name` (NUL-ended), owner-only, local
/// clients only, overlapped, not inherited; with `first`, only if no
/// instance of that name exists anywhere.
fn instance(name: &[u16], security: &Security, first: bool) -> Result<OwnedHandle> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    let first = if first {
        FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        0
    };
    let open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first;
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` is NUL-ended, and `attributes` and the descriptor it
    // points at are alive for the call.
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open,
            mode,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER,
            BUFFER,
            0,
            &attributes,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(Errno::last());
    }
    // SAFETY: `raw` was just made, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// A server's pipe: the name it holds, and one instance waiting for a
/// client, to [`Listener::accept`] once a poll says it is ready to read,
/// or to hand to a child as a [`Spare`].
pub struct Listener {
    name: Vec<u16>,
    security: Security,
    waiting: Spare,
}

impl Listener {
    /// Holds the pipe `name` (see [`name`]): its first instance, with
    /// this process's owner-only descriptor. A name some process holds
    /// already, another server or a squatter, fails with
    /// [`Errno::ADDRINUSE`]: this one never joins a pipe it didn't make.
    pub fn bind(name: &str) -> Result<Listener> {
        let name = wide(name)?;
        let security = Security::owner_only()?;
        let first = instance(&name, &security, true).map_err(|errno| {
            let taken = [ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY].map(Errno::from_code);
            match taken.contains(&errno) {
                true => Errno::ADDRINUSE,
                false => errno,
            }
        })?;
        Ok(Listener {
            name,
            security,
            waiting: Spare::try_from(first)?,
        })
    }

    /// The instance a client connected to, once one has, with the next
    /// already made in its place; [`Errno::AGAIN`] while none has. An
    /// instance that failed (a poll says `IN | ERR`) is replaced, and its
    /// error told.
    pub fn accept(&mut self) -> Result<Connection> {
        if let Ok(None) = self.waiting.connected() {
            return Err(Errno::AGAIN);
        }
        let next = self.next()?;
        std::mem::replace(&mut self.waiting, next).accept()
    }

    /// The instance waiting for a client, for a child to take its client
    /// itself, with the next already made in its place. Clients may then
    /// reach either: each instance takes one.
    pub fn spare(&mut self) -> Result<Spare> {
        let next = self.next()?;
        Ok(std::mem::replace(&mut self.waiting, next))
    }

    fn next(&self) -> Result<Spare> {
        Spare::try_from(instance(&self.name, &self.security, false)?)
    }
}

impl Sealed for Listener {
    fn arm(&self, interest: Events) {
        self.waiting.arm(interest);
    }

    fn ready(&self, interest: Events) -> Events {
        self.waiting.ready(interest)
    }

    fn read(&self, _: &mut [u8]) -> Result<usize> {
        Err(Errno::BADF)
    }

    fn write(&self, _: &[u8]) -> Result<usize> {
        Err(Errno::BADF)
    }
}

impl Source for Listener {}

/// One pipe instance a client has not connected to yet: a
/// [`Listener`]'s, or one a child was handed (`TryFrom<OwnedHandle>`),
/// which takes one client itself ([`Spare::accept`]). Made a plain handle
/// (`OwnedHandle::try_from`) to hand on, with no connect in flight.
pub struct Spare {
    /// None once its client is taken.
    handle: Option<OwnedHandle>,
    connect: Box<Connect>,
}

/// A connect in flight, and what it ended with.
///
/// **The `OVERLAPPED` stays put while the kernel uses it:** it is boxed,
/// and a connect still in flight when its `Spare` goes is cancelled and
/// waited for ([`Spare::settle`]) before the box is freed. It is touched
/// only under `state`'s lock while no connect is in flight.
struct Connect {
    overlapped: UnsafeCell<OVERLAPPED>,
    /// Before `event`, so a wait it holds is unregistered before the
    /// event it waits on closes.
    state: Mutex<State>,
    /// Set by the kernel as the connect ends: manual-reset, reset before
    /// each.
    event: OwnedHandle,
}

// SAFETY: the kernel uses `overlapped` only while a connect is in flight,
// and fuxix only under `state`'s lock while none is; a registered wait may
// be unregistered from any thread.
unsafe impl Send for Connect {}
// SAFETY: as above.
unsafe impl Sync for Connect {}

#[derive(Default)]
struct State {
    pending: bool,
    /// When a client was first seen connected.
    connected: Option<SystemTime>,
    failed: Option<Errno>,
    /// The thread-pool wait on `event` while a connect is in flight.
    wait: Option<Wait>,
}

fn lock(connect: &Connect) -> MutexGuard<'_, State> {
    connect.state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TryFrom<OwnedHandle> for Spare {
    type Error = Errno;

    /// `handle`, a pipe instance's server end opened overlapped, as a
    /// spare: a child's, handed to it.
    fn try_from(handle: OwnedHandle) -> Result<Spare> {
        // SAFETY: makes a new unnamed manual-reset event, not set, with no
        // security attributes.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw.is_null() {
            return Err(Errno::last());
        }
        // SAFETY: `raw` was just made, and nothing else owns it.
        let event = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(Spare {
            handle: Some(handle),
            connect: Box::new(Connect {
                overlapped: UnsafeCell::new(OVERLAPPED::default()),
                state: Mutex::new(State::default()),
                event,
            }),
        })
    }
}

impl Spare {
    /// The client's connection, once one has connected; [`Errno::AGAIN`]
    /// while none has. It has one to give: then `ERROR_INVALID_HANDLE`,
    /// and a poll says `HUP`.
    pub fn accept(&mut self) -> Result<Connection> {
        if self.handle.is_none() {
            return Err(Errno::from_code(ERROR_INVALID_HANDLE));
        }
        let Some(at) = self.connected()? else {
            return Err(Errno::AGAIN);
        };
        let connected = Some(at);
        let handle = self.handle.take().ok_or(Errno::AGAIN)?;
        Ok(Connection { handle, connected })
    }

    /// When a client was seen connected, if one has, starting the connect
    /// if none is in flight; the error a connect failed with.
    fn connected(&self) -> Result<Option<SystemTime>> {
        self.arm(Events::IN);
        let state = self.settle(false);
        state.failed.map_or(Ok(state.connected), Err)
    }

    /// Takes in how the connect in flight ended, if it has; with `cancel`,
    /// cancels it first and waits for it to end. Its state, locked.
    fn settle(&self, cancel: bool) -> MutexGuard<'_, State> {
        let mut state = lock(&self.connect);
        let Some(handle) = &self.handle else {
            return state;
        };
        if !state.pending {
            return state;
        }
        let overlapped = self.connect.overlapped.get();
        if cancel {
            // SAFETY: cancels this spare's own connect, on its open handle.
            unsafe { CancelIoEx(handle.as_raw_handle(), overlapped) };
        }
        let mut moved = 0u32;
        // SAFETY: the handle is open and `overlapped` is the connect's, in
        // flight or done; with `cancel` it waits for the end, which the
        // cancel makes soon.
        let done = unsafe {
            GetOverlappedResult(
                handle.as_raw_handle(),
                overlapped,
                &mut moved,
                i32::from(cancel),
            )
        };
        let errno = (done == 0).then(Errno::last);
        if errno == Some(Errno::from_code(ERROR_IO_INCOMPLETE)) {
            return state;
        }
        state.pending = false;
        state.wait = None;
        // A connect cancelled leaves the instance waiting, for the next.
        match errno {
            None => state.connected = Some(SystemTime::now()),
            Some(errno) if errno == Errno::from_code(ERROR_PIPE_CONNECTED) => {
                state.connected = Some(SystemTime::now());
            }
            Some(errno) if errno == Errno::from_code(ERROR_NO_DATA) => again(handle),
            Some(_) if cancel => {}
            Some(errno) => state.failed = Some(errno),
        }
        state
    }
}

impl Sealed for Spare {
    /// Starts a connect, unless one is in flight or it has ended.
    fn arm(&self, interest: Events) {
        let Some(handle) = (interest.intersects(Events::IN))
            .then_some(self.handle.as_ref())
            .flatten()
        else {
            return;
        };
        let mut state = lock(&self.connect);
        if state.pending || state.connected.is_some() || state.failed.is_some() {
            return;
        }
        let event = self.connect.event.as_raw_handle();
        // SAFETY: the event is this spare's own and open.
        unsafe { ResetEvent(event) };
        let overlapped = self.connect.overlapped.get();
        let fresh = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        // SAFETY: no connect is in flight, and `state`'s lock is held:
        // nothing else reads or writes it now.
        unsafe { overlapped.write(fresh) };
        // SAFETY: the handle is open, and `overlapped` stays put, unused
        // by fuxix, until the connect ends (see `Connect`).
        let done = unsafe { ConnectNamedPipe(handle.as_raw_handle(), overlapped) };
        let errno = (done == 0).then(Errno::last);
        match errno {
            None => state.connected = Some(SystemTime::now()),
            Some(errno) if errno == Errno::from_code(ERROR_IO_PENDING) => {
                state.pending = true;
                // Unregistered before the event closes (`Connect`).
                match Wait::once(event, woken) {
                    Ok(wait) => state.wait = Some(wait),
                    // No wake will come: told as the error, and the
                    // connect cancelled as the spare goes.
                    Err(errno) => state.failed = Some(errno),
                }
            }
            // A client came before the connect: it is connected.
            Some(errno) if errno == Errno::from_code(ERROR_PIPE_CONNECTED) => {
                state.connected = Some(SystemTime::now());
            }
            Some(errno) if errno == Errno::from_code(ERROR_NO_DATA) => again(handle),
            Some(errno) => state.failed = Some(errno),
        }
    }

    fn ready(&self, interest: Events) -> Events {
        if self.handle.is_none() {
            return Events::HUP;
        }
        let state = self.settle(false);
        // Failed, it is readable too: `accept` tells the error and makes
        // the next instance, which a poll for `IN` alone would never do.
        match (state.failed, state.connected) {
            (Some(_), _) => Events::IN | Events::ERR,
            (None, Some(_)) if interest.intersects(Events::IN) => Events::IN,
            (None, _) => Events::NONE,
        }
    }

    fn read(&self, _: &mut [u8]) -> Result<usize> {
        Err(Errno::BADF)
    }

    fn write(&self, _: &[u8]) -> Result<usize> {
        Err(Errno::BADF)
    }
}

impl Source for Spare {}

/// A client came and went before the connect saw it: the instance is
/// disconnected, to wait for the next, and the poll woken to connect it
/// again, as nothing is in flight to wake it.
fn again(handle: &OwnedHandle) {
    // SAFETY: disconnects this spare's own open instance, whose connect
    // is done.
    unsafe { DisconnectNamedPipe(handle.as_raw_handle()) };
    // A wake that can't be posted has a broken port: the poll fails.
    let _ = crate::poll::wake();
}

impl Drop for Spare {
    fn drop(&mut self) {
        drop(self.settle(true));
    }
}

impl TryFrom<Spare> for OwnedHandle {
    type Error = Errno;

    /// Its handle, with no connect in flight, to hand to a child (as std's
    /// `Stdio::from`), whose own [`Spare`] connects it. One whose client
    /// came already is connected, and the child's spare says so at once.
    /// A spare whose client was taken has none (`ERROR_INVALID_HANDLE`).
    fn try_from(mut spare: Spare) -> Result<OwnedHandle> {
        drop(spare.settle(true));
        spare
            .handle
            .take()
            .ok_or(Errno::from_code(ERROR_INVALID_HANDLE))
    }
}

/// A pipe instance a client connected to, not yet bound to this process's
/// completion port: to serve here ([`Connection::stream`]) or to hand to
/// a child (`OwnedHandle::from`, and `Connection::from` there).
pub struct Connection {
    handle: OwnedHandle,
    /// When its client was first seen connected; none in a child it was
    /// handed to, which keeps no record of it.
    connected: Option<SystemTime>,
}

impl Connection {
    /// It as a stream of this process's, to wait on and read and write.
    pub fn stream(self) -> Result<Stream> {
        Stream::new(self.handle)
    }

    /// A moment its client is known to have been connected at, for
    /// [`peer`]: when it was first seen connected; in a child it was
    /// handed to, when this process was made, as the client connected
    /// before the instance was handed on.
    pub fn connected(&self) -> Result<SystemTime> {
        match self.connected {
            Some(at) => Ok(at),
            // SAFETY: takes nothing; the pseudo-handle needs no closing.
            None => created(unsafe { GetCurrentProcess() }),
        }
    }
}

impl AsHandle for Connection {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

impl From<Connection> for OwnedHandle {
    fn from(connection: Connection) -> OwnedHandle {
        connection.handle
    }
}

impl From<OwnedHandle> for Connection {
    /// `handle`, a connected pipe instance opened overlapped and bound to
    /// no completion port: a child's, handed to it. Its client must have
    /// connected before this process was made, as
    /// [`Connection::connected`] here is this process's creation time;
    /// an instance not yet connected is handed on as a [`Spare`].
    fn from(handle: OwnedHandle) -> Connection {
        let connected = None;
        Connection { handle, connected }
    }
}
