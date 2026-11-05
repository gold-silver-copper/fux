//! Where a server's clients connect, and who is at the other end. On Unix,
//! stream sockets, made one step at a time (`unix.rs`); on Windows, named
//! pipes that only their user's processes may open, one instance per
//! connection, waited on with [`crate::poll`] (`windows.rs`). On both, a
//! connection's [`peer`] is a [`Peer`]: its [`User`], which says whether
//! it is this process's own ([`User::is_current`]), and its process.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// The process at the other end of a connection, as the system recorded
/// it: its user, and its ID where the system says it (macOS doesn't).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    pub user: User,
    pub pid: Option<crate::process::Pid>,
}
