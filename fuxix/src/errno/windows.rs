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
