//! App assembly (prompt 3.1): the server `App` and the headless variant tests drive with
//! `app.update()` while feeding `Inbound` and reading `Effect` directly.

use async_channel::Sender;
use bevy_app::TaskPoolPlugin;
use bevy_app::prelude::*;
use bevy_asset::AssetPlugin;
use bevy_asset::io::{AssetSourceBuilders, AssetSourceId};
use bevy_diagnostic::{DiagnosticsPlugin, EntityCountDiagnosticsPlugin};
use bevy_ecs::error::FallbackErrorHandler;
use bevy_log::LogPlugin;
use bevy_scene::ScenePlugin;
use bevy_state::app::StatesPlugin;
use bevy_time::TimePlugin;

use crate::assets::{ServerConfigPlugin, asset_root, inbound_wake, waking_source};
use crate::attach::AttachPlugin;
use crate::config::Config;
use crate::events::EventsPlugin;
use crate::finals::FinalsPlugin;
use crate::input_ops::InputOpsPlugin;
use crate::layout::LayoutPlugin;
use crate::lifecycle::{DefaultCommand, LifecyclePlugin};
use crate::model::{Inbound, ModelPlugin};
use crate::paths::Paths;
use crate::pointer::PointerPlugin;
use crate::pty::TerminalPlugin;
use crate::remote::{HttpTransport, RemoteControlPlugin};
use crate::scene::LayoutDir;
use crate::session::{self, SessionPlugin};
use crate::surface::SurfacePlugin;

/// The full server: OS-facing plugins included. Errors from systems are logged, never panic.
pub fn build(config: &Config, paths: &Paths, name: &str, inbound: Sender<Inbound>) -> App {
    let mut app = App::new();
    let root = asset_root(&paths.config_dir);
    app.world_mut()
        .get_resource_or_init::<AssetSourceBuilders>()
        .insert(
            AssetSourceId::Default,
            waking_source(&root, inbound_wake(inbound.clone())),
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
            file_path: root,
            ..Default::default()
        },
        ScenePlugin,
        DiagnosticsPlugin,
        EntityCountDiagnosticsPlugin::default(),
    ));
    core(&mut app, config);
    app.add_plugins((
        RemoteControlPlugin {
            runtime_dir: paths.runtime_dir.clone(),
            server_name: name.into(),
            inbound: inbound.clone(),
            transport: HttpTransport::default(),
        },
        AttachPlugin { inbound },
        SessionPlugin {
            dir: paths.state_dir.join(session::SESSION_DIR),
            server: name.into(),
        },
    ))
    .insert_resource(LayoutDir::new(paths))
    .insert_resource(FallbackErrorHandler(bevy_ecs::error::error));
    app
}

/// The World-only server: model, layout, terminal ingest and lifecycle, without PTY, BRP or
/// attachment adapters. System errors panic so tests see them. Assets are served from the
/// default directory (no `fux.toml`, no user layouts).
pub fn build_headless(config: &Config) -> App {
    headless(config, AssetPlugin::default())
}

/// [`build_headless`] over a user's directories: `fux.toml` and `layouts/` under
/// `paths.config_dir` are loaded and watched.
pub fn build_headless_in(config: &Config, paths: &Paths) -> App {
    let mut app = headless(
        config,
        AssetPlugin {
            file_path: crate::assets::asset_root(&paths.config_dir),
            ..Default::default()
        },
    );
    app.insert_resource(LayoutDir::new(paths));
    app
}

fn headless(config: &Config, assets: AssetPlugin) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        StatesPlugin,
        TimePlugin,
        assets,
        ScenePlugin,
    ));
    core(&mut app, config);
    app
}

/// Everything both servers share; needs `AssetPlugin` and `ScenePlugin` first. The
/// configuration resources start from `config` (the file as read at startup) and follow the
/// `fux.toml` asset from then on.
fn core(app: &mut App, config: &Config) {
    app.insert_resource(config.limits())
        .insert_resource(DefaultCommand(config.default_command.argv.clone()))
        .insert_resource(config.clone())
        .add_plugins((
            ServerConfigPlugin,
            crate::scene::LayoutAssetPlugin,
            ModelPlugin,
            LayoutPlugin,
            TerminalPlugin,
            LifecyclePlugin,
            PointerPlugin,
            SurfacePlugin,
            (InputOpsPlugin, FinalsPlugin, EventsPlugin),
        ));
}
