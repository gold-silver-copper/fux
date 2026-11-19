//! fux2: a terminal multiplexer with no server. Each pane is one small
//! process that owns a terminal, keeps its screen and answers whoever
//! connects; a directory of panes is the whole session.
//!
//! This library is the protocol ([`wire`]), a connection ([`conn`]), a blocking
//! client ([`client`]), the session directory ([`dir`]) and the pane itself
//! ([`model`], [`pane`]). It links no language: the viewer, `fux2-view`, is
//! where keys and commands are configured.
pub mod client;
pub mod conn;
pub mod dir;
pub mod history;
pub mod model;
pub mod pane;
pub mod sys;
pub mod wire;
