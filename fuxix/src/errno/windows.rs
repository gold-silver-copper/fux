//! The codes a Windows call reports, and how fuxix reads one.
use super::Errno;

/// `WSAEWOULDBLOCK`, the code Windows has for "this would wait", and the
/// one std reads as `ErrorKind::WouldBlock`. Windows' pipes have none of
/// their own: fuxix's streams buffer, and say this when they hold nothing.
const WOULD_BLOCK: i32 = 10035;

impl Errno {
    /// Nothing to read yet, or no room to write: try again once a poll
    /// says it is ready.
    pub const AGAIN: Self = Self(WOULD_BLOCK);
    /// A pipe name some process holds already (`WSAEADDRINUSE`, the code
    /// std reads as `AddrInUse`): `socket::Listener::bind` never joins it.
    pub const ADDRINUSE: Self = Self(10048);
    /// Access refused (`ERROR_ACCESS_DENIED`): a pipe whose server is not
    /// this process's user, or whose descriptor keeps this user out.
    pub const ACCES: Self = Self::from_code(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED);
    /// An argument refused (`ERROR_INVALID_PARAMETER`): a pipe name with
    /// a NUL in it.
    pub const INVAL: Self =
        Self::from_code(windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER);
    /// No such process (`ERROR_NOT_FOUND`): one a [`crate::process::Job`]
    /// was asked to end had ended already, as Unix's `ESRCH` says.
    pub const SRCH: Self = Self::from_code(windows_sys::Win32::Foundation::ERROR_NOT_FOUND);
    /// Not open for this (`ERROR_INVALID_HANDLE`): a read of a source that
    /// reads nothing, as Unix's `EBADF` says of a write-only descriptor.
    pub(crate) const BADF: Self =
        Self::from_code(windows_sys::Win32::Foundation::ERROR_INVALID_HANDLE);
    /// A call interrupted (`WSAEINTR`): no fuxix call on Windows is, so
    /// this is never told; named so that code retrying on it reads alike.
    pub const INTR: Self = Self(10004);
    /// An I/O error (`ERROR_IO_DEVICE`): what Unix says of a terminal no
    /// program holds; a pseudo console's end is a read of 0 here instead.
    pub const IO: Self = Self::from_code(windows_sys::Win32::Foundation::ERROR_IO_DEVICE);
    /// Not done on Windows (`ERROR_CALL_NOT_IMPLEMENTED`).
    pub const NOSYS: Self =
        Self::from_code(windows_sys::Win32::Foundation::ERROR_CALL_NOT_IMPLEMENTED);
    /// The resource is busy: another thread is polling (one at a time).
    pub(crate) const BUSY: Self = Self(windows_sys::Win32::Foundation::ERROR_BUSY.cast_signed());

    /// Whether it says the other end of a pipe is gone: closed
    /// (`ERROR_BROKEN_PIPE`), closing (`ERROR_NO_DATA`), or never there
    /// (`ERROR_PIPE_NOT_CONNECTED`).
    pub fn is_peer_gone(self) -> bool {
        use windows_sys::Win32::Foundation::{
            ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
        };
        [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED]
            .contains(&self.0.cast_unsigned())
    }

    /// Whether a read that failed so came to its stream's end: the writer
    /// closed (`ERROR_BROKEN_PIPE`), or a file ended (`ERROR_HANDLE_EOF`).
    pub(crate) fn is_end(self) -> bool {
        use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF};
        [ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF].contains(&self.0.cast_unsigned())
    }

    /// A system error code, as `GetLastError` gives it: std keeps it as
    /// the same 32 bits read as `i32`.
    pub(crate) const fn from_code(code: u32) -> Self {
        Self(code.cast_signed())
    }

    /// The system's description and code, its bytes admitted under
    /// `budget` before they are kept.
    pub fn message<A: crate::Allocation>(
        self,
        budget: &mut A,
    ) -> std::result::Result<String, A::Error> {
        let text = self.to_string();
        let mut kept = Vec::new();
        budget.reserve(&mut kept, text.len())?;
        kept.extend_from_slice(text.as_bytes());
        Ok(String::from_utf8(kept).unwrap_or(text))
    }

    /// The code behind an I/O error, if it came from the system.
    pub(crate) fn from_io_error(error: &std::io::Error) -> Option<Self> {
        error.raw_os_error().map(Self)
    }

    /// The code the calling thread's last failed call set, read straight
    /// after it.
    pub(crate) fn last() -> Self {
        // SAFETY: it takes nothing, and reads this thread's last error.
        Self::from_code(unsafe { windows_sys::Win32::Foundation::GetLastError() })
    }
}
