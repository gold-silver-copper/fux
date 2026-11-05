//! The error a system call reports: on Unix an `errno` (its values, and
//! how a call's return says one, are in `unix.rs`); on Windows the code
//! `GetLastError` or a call's return gives, as std's `raw_os_error` holds
//! it. Either is shown as the system describes it.
use std::fmt;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::{check, check_size, fit};
#[cfg(windows)]
mod windows;

/// The system's code for why a call failed: an `errno` value on Unix, a
/// system error code on Windows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Errno(i32);

/// A call's result.
pub type Result<T> = std::result::Result<T, Errno>;

impl Errno {
    /// The number.
    pub fn raw(self) -> i32 {
        self.0
    }
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
        Self::from_raw_os_error(errno.0)
    }
}
