//! fux2: a terminal multiplexer with no server. Each pane is one small
//! process that owns a terminal, keeps its screen and answers whoever
//! connects; a directory of panes is the whole session.
//!
//! This library is the protocol ([`wire`]), a connection ([`conn`]), a blocking
//! client ([`client`]), the session directory ([`dir`]) and the pane itself
//! ([`model`], [`pane`]). It links no language: the viewer, `fux2-view`, is
//! where keys and commands are configured.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("fux2 runs on Linux and macOS only");

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod client;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod conn;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod dir;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod history;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod model;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod pane;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod sys;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod wire;
