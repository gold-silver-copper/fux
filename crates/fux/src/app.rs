//! App assembly (prompt 3.1): the server `App` and the headless variant tests drive with
//! `app.update()` while feeding `Inbound` and reading `Effect` directly.

use async_channel::Sender;
use bevy_app::TaskPoolPlugin;
use bevy_app::prelude::*;
use bevy_asset::AssetPlugin;
use bevy_diagnostic::{DiagnosticsPlugin, EntityCountDiagnosticsPlugin};
use bevy_ecs::error::FallbackErrorHandler;
use bevy_log::LogPlugin;
use bevy_scene::ScenePlugin;
use bevy_state::app::StatesPlugin;
use bevy_time::TimePlugin;

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
use crate::remote::RemoteControlPlugin;
use crate::scene::LayoutDir;
use crate::surface::SurfacePlugin;

/// The full server: OS-facing plugins included. Errors from systems are logged, never panic.
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
        },
        AttachPlugin { inbound },
    ))
    .insert_resource(LayoutDir::new(paths))
    .insert_resource(FallbackErrorHandler(bevy_ecs::error::error));
    app
}

/// The World-only server: model, layout, terminal ingest and lifecycle, without PTY, BRP or
/// attachment adapters. System errors panic so tests see them.
pub fn build_headless(config: &Config) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        StatesPlugin,
        TimePlugin,
        AssetPlugin::default(),
        ScenePlugin,
    ));
    core(&mut app, config);
    app
}

fn core(app: &mut App, config: &Config) {
    app.insert_resource(config.limits())
        .insert_resource(DefaultCommand(config.default_command.argv.clone()))
        .add_plugins((
            ModelPlugin,
            LayoutPlugin,
            TerminalPlugin,
            LifecyclePlugin,
            PointerPlugin,
            SurfacePlugin,
            (InputOpsPlugin, FinalsPlugin, EventsPlugin),
        ));
}
