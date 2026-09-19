//! zor: the agent and task policy layer over fux, as a `bevy_app::App` whose authoritative state
//! is a `bevy_ecs` World. Design: `docs/prompts/ecs-native-rewrite-prompt.md` section 4; the model
//! contract: `crates/zor/docs/model.md`.
#![forbid(unsafe_code)]

pub mod app;
pub mod checks;
pub mod cli;
pub mod config;
pub mod dashboard;
pub mod fux_client;
pub mod git;
pub mod groups;
pub mod journal;
pub mod lifecycle;
pub mod machines;
pub mod model;
pub mod paths;
pub mod plugins;
pub mod providers;
pub mod remote;
pub mod runner;
pub mod worktrees;
