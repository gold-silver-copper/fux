//! The system calls fux makes, on Linux, Android and macOS, over `libc`.
//!
//! fux forbids `unsafe` code; this crate is where the `unsafe` it needs
//! lives, one call per block, each with its reasoning. Every function is
//! safe to call, and no value it returns has to be checked for validity: a
//! process ID, group or session is a [`process::Pid`], which is positive; a
//! failure is an [`Errno`] or `None`; a struct the kernel fills is read only
//! after the call says it filled it.
//!
//! Only what fux, its tests, its fuzz targets and its walk use is here.
//!
//! **`EINTR`:** no function retries. A call a signal interrupts fails with
//! [`Errno::INTR`], as the system call did, and the caller decides: fux's
//! loops retry, and its blocking reads let a signal through to be handled.
//! The one retry fuxix makes is not of that kind: [`pty::open`] on macOS
//! works around two kernel bugs, retrying an open the kernel gave up on and
//! replacing a PTY it left without a replica.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
compile_error!("fuxix supports Linux, Android and macOS");

mod errno;
pub mod file;
pub mod io;
pub mod poll;
pub mod process;
pub mod pty;
pub mod socket;
pub mod terminal;

pub use errno::{Errno, Result};
