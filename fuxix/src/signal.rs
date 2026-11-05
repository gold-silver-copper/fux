//! Signals told on a pipe, for a poll loop to wait on: on Unix, bytes a
//! handler writes (`unix.rs`); on Windows, where nothing is a signal, what
//! stands for one (a console's control events, a watched child's end, a
//! `raise` from any thread) queued for the loop (`windows.rs`). Either
//! way [`Pipe::next`] tells which came.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
