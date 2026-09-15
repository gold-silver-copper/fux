//! User layouts as assets (prompt 3.7: everything user-editable is an `Asset` with a fux
//! `AssetLoader` and `file_watcher`). Every `<config_dir>/layouts/<name>.scn.ron` is a
//! [`LayoutAsset`]: the `AssetServer` (rooted at the config directory) loads it through
//! [`LayoutAssetLoader`], which parses the RON into a `DynamicWorld` and refuses anything
//! outside the layout allowlist, and its file watcher reloads it on every write. A reload that
//! fails to parse or validate keeps the previous asset and is logged. [`Layouts`] holds one
//! handle per file stem so the assets stay loaded; [`scan`] refreshes it from the directory
//! (startup, `scene.list`) and `scene::restore` prefers a loaded asset over the file.

use std::path::Path;

use bevy_app::prelude::*;
use bevy_asset::io::Reader;
use bevy_asset::{
    Asset, AssetApp, AssetEvent, AssetLoadFailedEvent, AssetLoader, AssetServer, Assets, Handle,
    LoadContext,
};
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_log::{debug, warn};
use bevy_platform::collections::HashMap;
use bevy_reflect::{TypePath, TypeRegistryArc};
use bevy_world_serialization::DynamicWorld;
use bevy_world_serialization::serde::WorldDeserializer;
use serde::de::DeserializeSeed as _;

use super::{
    EXTENSION, LAYOUTS_DIR, LayoutDir, MAX_DOCUMENT_BYTES, R, SceneError, check_allowlist, file_of,
    list,
};

/// A user layout file, parsed and allowlisted; `scene::apply_dynamic` validates the rest
/// against the live World when it is restored.
#[derive(Asset, TypePath)]
pub struct LayoutAsset {
    pub document: DynamicWorld,
}

/// Loads `*.scn.ron` under the asset root into [`LayoutAsset`]s with the app's type registry.
#[derive(TypePath)]
pub struct LayoutAssetLoader {
    registry: TypeRegistryArc,
}

impl FromWorld for LayoutAssetLoader {
    fn from_world(world: &mut World) -> Self {
        Self {
            registry: world.resource::<AppTypeRegistry>().0.clone(),
        }
    }
}

impl AssetLoader for LayoutAssetLoader {
    type Asset = LayoutAsset;
    type Settings = ();
    type Error = SceneError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        (): &Self::Settings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(SceneError::TooLarge {
                bytes: bytes.len(),
                max: MAX_DOCUMENT_BYTES,
            });
        }
        let mut de = ron::de::Deserializer::from_bytes(&bytes)
            .map_err(|e| SceneError::Parse(e.to_string()))?;
        let seed = WorldDeserializer {
            type_registry: &self.registry.read(),
            load_from_path: load_context,
        };
        let document = seed
            .deserialize(&mut de)
            .map_err(|e| SceneError::Parse(de.span_error(e).to_string()))?;
        check_allowlist(&document)?;
        Ok(LayoutAsset { document })
    }

    fn extensions(&self) -> &[&str] {
        &["scn.ron"]
    }
}

/// The user layouts the asset server has been asked for, by file stem. Holding the handles
/// keeps the assets loaded and hot-reloading; dropping one (its file vanished) unloads it.
#[derive(Resource, Default, Debug)]
pub struct Layouts {
    pub handles: HashMap<String, Handle<LayoutAsset>>,
}

/// Registers the asset and its loader, scans the layout directory at startup and logs reloads
/// and failed reloads. Requires `AssetPlugin`; reads `LayoutDir` when present.
pub struct LayoutAssetPlugin;

impl Plugin for LayoutAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<LayoutAsset>()
            .init_asset_loader::<LayoutAssetLoader>()
            .init_resource::<Layouts>()
            .add_systems(Startup, scan_at_startup)
            .add_systems(Update, report);
    }
}

fn scan_at_startup(world: &mut World) {
    match scan(world) {
        Ok(_) | Err(SceneError::NoLayoutDir) => {}
        Err(error) => warn!("layouts: scanning the layout directory: {error}"),
    }
}

fn report(
    mut failed: MessageReader<AssetLoadFailedEvent<LayoutAsset>>,
    mut events: MessageReader<AssetEvent<LayoutAsset>>,
    layouts: Res<Layouts>,
) {
    for event in failed.read() {
        let kept = layouts.handles.values().any(|h| h.id() == event.id);
        warn!(
            "layout {}: {}{}",
            event.path,
            event.error,
            if kept {
                "; keeping the previously loaded document"
            } else {
                ""
            }
        );
    }
    for event in events.read() {
        if let AssetEvent::Modified { id } = event
            && let Some(name) = layouts.handles.iter().find(|(_, h)| h.id() == *id)
        {
            debug!("layout {} reloaded", name.0);
        }
    }
}

/// The asset path of the layout `name` under the config directory.
fn asset_path(name: &str) -> String {
    format!("{LAYOUTS_DIR}/{name}{EXTENSION}")
}

/// Lists the layouts in `LayoutDir` (sorted) and, when an asset server is present, asks it for
/// every file not yet loaded and forgets those whose file is gone.
pub fn scan(world: &mut World) -> R<Vec<String>> {
    let dir = world
        .get_resource::<LayoutDir>()
        .map(|d| d.0.clone())
        .ok_or(SceneError::NoLayoutDir)?;
    let names = list(&dir)?;
    if let (Some(server), Some(mut layouts)) = (
        world.get_resource::<AssetServer>().cloned(),
        world.get_resource_mut::<Layouts>(),
    ) {
        layouts.handles.retain(|name, _| names.contains(name));
        for name in &names {
            layouts
                .handles
                .entry(name.clone())
                .or_insert_with(|| server.load(asset_path(name)));
        }
    }
    Ok(names)
}

/// The loaded asset of `<dir>/<name>.scn.ron` when `dir` is the asset-served layout directory
/// and the file still exists: a load is requested if none is, and `None` means the file is not
/// loaded yet (or failed with nothing to keep), so the caller reads it itself.
pub(crate) fn loaded(world: &mut World, dir: &Path, name: &str) -> Option<Handle<LayoutAsset>> {
    if world.get_resource::<LayoutDir>()?.0 != dir {
        return None;
    }
    let exists = file_of(dir, name).ok()?.is_file();
    let server = world.get_resource::<AssetServer>()?.clone();
    let mut layouts = world.get_resource_mut::<Layouts>()?;
    if !exists {
        layouts.handles.remove(name);
        return None;
    }
    let handle = layouts
        .handles
        .entry(name.to_owned())
        .or_insert_with(|| server.load(asset_path(name)))
        .clone();
    world
        .resource::<Assets<LayoutAsset>>()
        .contains(&handle)
        .then_some(handle)
}
