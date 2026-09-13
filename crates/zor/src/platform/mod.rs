pub use crate::rules::ident::{Job, Pid, Process};

pub(crate) mod files;
pub(crate) mod notification;
pub mod probe;
pub(crate) mod process;
pub mod usage;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(feature = "wrap")]
mod wrap;
#[cfg(feature = "wrap")]
pub use wrap::{forward_signal, suspend_self};

#[cfg(target_os = "linux")]
pub(crate) use linux::{process_children, process_identity};
#[cfg(target_os = "macos")]
pub(crate) use macos::{process_children, process_identity};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProcessRef {
    pub pid: Pid,
    pub birth: (u64, u64),
}

#[cfg(target_os = "linux")]
pub use linux::{Guard, foreground_pgid, job, leader, process_cwd, set_raw, winsize};
#[cfg(target_os = "macos")]
pub use macos::{Guard, foreground_pgid, job, leader, process_cwd, set_raw, winsize};

/// Terminal geometry independent of any PTY allocation library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}
