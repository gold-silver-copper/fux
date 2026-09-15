//! App assembly (prompt 4.1): the zor server `App` and the headless variant tests drive with
//! `app.update()` while feeding `Inbound` and reading `Effect` directly.

use std::path::Path;

use async_channel::Sender;
use bevy_app::TaskPoolPlugin;
use bevy_app::prelude::*;
use bevy_asset::AssetPlugin;
use bevy_diagnostic::{DiagnosticsPlugin, EntityCountDiagnosticsPlugin};
use bevy_ecs::error::FallbackErrorHandler;
use bevy_log::LogPlugin;
use bevy_state::app::StatesPlugin;
use bevy_time::TimePlugin;

use crate::config::Config;
use crate::journal::JournalPlugin;
use crate::model::{Inbound, ModelPlugin};
use crate::paths::Paths;
use crate::remote::RemoteHostPlugin;

/// The full server. Errors from systems are logged, never panic.
pub fn build(config: &Config, paths: &Paths, name: &str, inbound: Sender<Inbound>) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        StatesPlugin,
        TimePlugin,
        LogPlugin {
            filter: "info,wgpu=error,naga=warn".into(),
            ..Default::default()
        },
        AssetPlugin {
            file_path: paths.config_dir.display().to_string(),
            watch_for_changes_override: Some(true),
            ..Default::default()
        },
        DiagnosticsPlugin,
        EntityCountDiagnosticsPlugin::default(),
    ));
    core(&mut app, config, &paths.state_dir);
    app.add_plugins(RemoteHostPlugin {
        runtime_dir: paths.runtime_dir.clone(),
        server_name: name.into(),
        inbound,
    })
    .insert_resource(FallbackErrorHandler(bevy_ecs::error::error));
    app
}

/// The World-only server: model and journal over `state_dir`, without BRP or adapters. System
/// errors panic so tests see them.
pub fn build_headless(config: &Config, state_dir: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        StatesPlugin,
        TimePlugin,
        AssetPlugin::default(),
    ));
    core(&mut app, config, state_dir);
    app
}

fn core(app: &mut App, config: &Config, state_dir: &Path) {
    app.insert_resource(config.limits()).add_plugins((
        ModelPlugin,
        JournalPlugin {
            state_dir: state_dir.to_path_buf(),
        },
    ));
}
