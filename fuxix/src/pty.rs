//! Pseudoterminals, and starting a program on one: on Unix a PTY
//! (`unix.rs`), on Windows a pseudo console (`windows.rs`), with the same
//! [`open`], `Master` and `Slave`.
use std::fmt;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod conpty;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use conpty::{fetch, fetched, load};
#[cfg(windows)]
pub use windows::*;

/// A step of opening a PTY that failed, and why.
#[derive(Debug)]
pub struct Error {
    pub(crate) call: &'static str,
    pub(crate) source: std::io::Error,
}

/// "grantpt: Permission denied (os error 13)".
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.call, self.source)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn at<E: Into<std::io::Error>>(call: &'static str) -> impl FnOnce(E) -> Error {
    move |source| Error {
        call,
        source: source.into(),
    }
}
