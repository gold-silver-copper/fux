//! fux: a persistent terminal multiplexer whose authoritative state is a `bevy_ecs` World in a
//! `bevy_app::App`, whose layouts are `bevy_ui` scenes and whose control surface is the Bevy
//! Remote Protocol. Design: `docs/prompts/ecs-native-rewrite-prompt.md`.
#![forbid(unsafe_code)]

pub mod model;
pub mod wire;
