//! fux: a persistent terminal multiplexer whose authoritative state is a `bevy_ecs` World in a
//! `bevy_app::App`, whose layouts are `bevy_ui` scenes and whose control surface is the Bevy
//! Remote Protocol. Design: `docs/prompts/ecs-native-rewrite-prompt.md`.
// `deny`, not `forbid`: `runner::signals` installs the self-pipe signal handler and is the one
// module allowed to opt in.
#![deny(unsafe_code)]

pub mod app;
pub mod assets;
pub mod attach;
pub mod cli;
pub mod config;
pub mod events;
pub mod finals;
pub mod input_ops;
pub mod layout;
pub mod lifecycle;
pub mod model;
pub mod paths;
pub mod pointer;
pub mod pty;
pub mod remote;
pub mod runner;
pub mod scene;
pub mod session;
pub mod surface;
pub mod terminal;
pub mod viewer;
pub mod wire;
