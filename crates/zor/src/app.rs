//! App assembly (prompt 4.1): the zor server `App` and the headless variant tests drive with
//! `app.update()` while feeding `Inbound` and reading `Effect` directly.

use std::path::Path;

use async_channel::Sender;
use bevy_app::TaskPoolPlugin;
use bevy_app::prelude::*;
use bevy_asset::AssetPlugin;
use bevy_asset::io::{AssetSourceBuilders, AssetSourceId};
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
    // The asset file watcher compares canonical paths (`/var` vs `/private/var`); a config
    // directory that does not exist yet stays as named.
    let asset_root = paths
        .config_dir
        .canonicalize()
        .unwrap_or_else(|_| paths.config_dir.clone());
    let mut app = App::new();
    let wake_sender = inbound.clone();
    let wake: fux::assets::Wake = std::sync::Arc::new(move || {
        let _ = wake_sender.try_send(Inbound::Wake);
    });
    app.world_mut()
        .get_resource_or_init::<AssetSourceBuilders>()
        .insert(
            AssetSourceId::Default,
            fux::assets::waking_source(&asset_root.display().to_string(), wake),
        );
    app.add_plugins((
        TaskPoolPlugin::default(),
        StatesPlugin,
        TimePlugin,
        LogPlugin {
            filter: "info,wgpu=error,naga=warn".into(),
            ..Default::default()
        },
        AssetPlugin {
            file_path: asset_root.display().to_string(),
            watch_for_changes_override: Some(true),
            ..Default::default()
        },
        DiagnosticsPlugin,
        EntityCountDiagnosticsPlugin::default(),
    ));
    app.insert_resource(crate::plugins::FuxDescriptor(
        paths.fux_descriptor(&config.fux_server),
    ));
    app.insert_resource(crate::machines::MachinesFile {
        path: paths.config_dir.join(crate::machines::CATALOG_FILE),
        asset_root: Some(asset_root.clone()),
    });
    core(&mut app, config, &paths.state_dir);
    crate::machines::install_wake(app.world_mut(), inbound.clone());
    crate::plugins::install_wake(app.world_mut(), inbound.clone());
    app.add_plugins(crate::providers::ProvidersPlugin {
        asset_root: Some(asset_root),
    });
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
    app.add_plugins(crate::providers::ProvidersPlugin::default());
    app
}

fn core(app: &mut App, config: &Config, state_dir: &Path) {
    app.insert_resource(config.limits()).add_plugins((
        ModelPlugin,
        JournalPlugin {
            state_dir: state_dir.to_path_buf(),
        },
        crate::git::GitPlugin,
        crate::worktrees::WorktreesPlugin {
            state_dir: state_dir.to_path_buf(),
        },
        crate::groups::GroupsPlugin,
        crate::lifecycle::LifecyclePlugin,
        crate::checks::ChecksPlugin,
        crate::plugins::PluginsPlugin {
            state_dir: state_dir.to_path_buf(),
        },
        crate::dashboard::DashboardPlugin,
        crate::machines::MachinesPlugin,
    ));
}
