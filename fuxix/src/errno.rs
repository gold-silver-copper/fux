//! The error a system call reports.
use std::fmt;

/// An `errno` value, from a call that failed.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Errno(i32);

/// A call's result.
pub type Result<T> = std::result::Result<T, Errno>;

impl Errno {
    pub const INTR: Errno = Errno(libc::EINTR);
    pub const AGAIN: Errno = Errno(libc::EAGAIN);
    /// The same value as `AGAIN` on every platform fuxix supports.
    pub const WOULDBLOCK: Errno = Errno(libc::EWOULDBLOCK);
    pub const INPROGRESS: Errno = Errno(libc::EINPROGRESS);
    pub const MFILE: Errno = Errno(libc::EMFILE);
    pub const NFILE: Errno = Errno(libc::ENFILE);
    pub(crate) const INVAL: Errno = Errno(libc::EINVAL);
    /// XNU's kernel-private `EREDRIVEOPEN` (`bsd/sys/errno.h`), which no POSIX
    /// call should return: macOS leaks it from `open("/dev/ptmx")` when
    /// concurrent openers keep racing for one PTY. See `pty::open`.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const REDRIVEOPEN: Errno = Errno(-6);
    pub(crate) const NAMETOOLONG: Errno = Errno(libc::ENAMETOOLONG);

    /// The number.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// The `errno` behind an I/O error, if it came from the system.
    pub fn from_io_error(error: &std::io::Error) -> Option<Errno> {
        error.raw_os_error().map(Errno)
    }

    /// The calling thread's `errno`, read straight after a call failed.
    pub(crate) fn last() -> Errno {
        Errno(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        )
    }
}

/// A call's return value, where -1 means it failed and set `errno`.
pub(crate) fn check(value: libc::c_int) -> Result<libc::c_int> {
    if value == -1 {
        Err(Errno::last())
    } else {
        Ok(value)
    }
}

/// A byte count, where -1 means the call failed and set `errno`.
pub(crate) fn check_size(value: isize) -> Result<usize> {
    usize::try_from(value).map_err(|_| Errno::last())
}

/// Shown as the system describes it: "Interrupted system call (os error 4)".
impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&std::io::Error::from_raw_os_error(self.0), f)
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Errno({}: {self})", self.0)
    }
}

impl std::error::Error for Errno {}

impl From<Errno> for std::io::Error {
    fn from(errno: Errno) -> Self {
        std::io::Error::from_raw_os_error(errno.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_errno_reads_as_the_system_says_and_round_trips() {
        assert_eq!(
            Errno::INTR.to_string(),
            std::io::Error::from_raw_os_error(libc::EINTR).to_string()
        );
        let io: std::io::Error = Errno::MFILE.into();
        assert_eq!(Errno::from_io_error(&io), Some(Errno::MFILE));
        assert_eq!(Errno::AGAIN, Errno::WOULDBLOCK);
        assert_eq!(check(3), Ok(3));
        assert_eq!(check_size(7), Ok(7));
    }
}
