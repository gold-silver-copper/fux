//! Waiting on several things at once: on Unix, descriptors, with `ppoll`
//! or `poll` (`unix.rs`); on Windows, fuxix's own sources (`Source`: an
//! `io::Stream`, a `signal::Pipe`, stdin and stdout among them) over one
//! I/O completion port for the whole process (`windows.rs`). Both have the
//! same [`Poll`], [`List`], [`Entry`], [`PollFd`], [`Events`], [`poll()`]
//! and [`waker()`].
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
