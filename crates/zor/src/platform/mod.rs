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
