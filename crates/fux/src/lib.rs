#![forbid(unsafe_code)]
//! fux: a minimal persistent terminal multiplexer whose authoritative model lives in a
//! `bevy_ecs` World. See docs/design.md for the architecture.

pub mod client;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod ecs;
pub mod ids;
pub mod layout;
pub mod os;
pub mod proto;
pub mod server;
pub mod terminal;
pub mod view;
