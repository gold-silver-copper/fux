//! A pane's process's socket and the links made at it, as each OS has
//! them: a Unix domain socket, or a named pipe on Windows. How a server
//! binds one for a pane and starts its process is fux-server's.
#[cfg_attr(unix, path = "socket/unix.rs")]
#[cfg_attr(windows, path = "socket/windows.rs")]
mod endpoint;
pub use endpoint::*;
