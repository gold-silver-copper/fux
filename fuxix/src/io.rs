//! Reading and writing: on Unix, any descriptor, and its flags
//! (`unix.rs`); on Windows, a `Stream`, the one kind of stream fuxix
//! can wait on there (`windows.rs`). On both, a read or write that would
//! wait fails with [`crate::Errno::AGAIN`].
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
