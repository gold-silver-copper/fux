#[cfg(test)]
mod tests;
use crate::model::{Launch, PaneView, ProcessState, Wake, Workspace};
use bevy_app::{App, PostUpdate};
use bevy_asset::{
    Asset, AssetApp, AssetEvent, AssetEventSystems, AssetLoadFailedEvent, AssetLoader,
    AssetMetaCheck, AssetPlugin, AssetServer, Assets, Handle, LoadContext, UnapprovedPathMode,
    io::{AssetSourceBuilder, AssetSourceEvent, AssetSourceId, Reader, file::FileWatcher},
};
use bevy_ecs::{
    entity::{EntityHashMap, EntityHashSet},
    prelude::*,
    reflect::ReflectComponent,
};
use bevy_reflect::{FromReflect, Reflect, TypePath, std_traits::ReflectDefault};
use bevy_tasks::IoTaskPool;
use bevy_world_serialization::{
    DynamicWorld, DynamicWorldBuilder, WorldSerializationPlugin, serde::WorldDeserializer,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, de::DeserializeSeed};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Reflect, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub key: String,
    pub action: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardPolicy {
    #[default]
    Disabled,
    WriteOnly,
}

#[derive(Asset, Resource, Clone, Reflect, Serialize, Deserialize)]
#[reflect(Resource, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub prefix: String,
    pub bindings: Vec<Binding>,
    pub shell: Vec<String>,
    pub history_lines: usize,
    pub clipboard: ClipboardPolicy,
    /// A native .scn.ron asset path, relative to the configuration directory.
    pub layout: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        let bindings = [
            ("h", "split_horizontal"),
            ("v", "split_vertical"),
            ("x", "close"),
            ("k", "terminate"),
            ("n", "focus_next"),
            ("z", "zoom"),
            ("w", "workspace_next"),
            ("c", "workspace_new"),
            ("r", "rename_pane"),
            ("R", "rename_workspace"),
            ("left", "shrink_width"),
            ("right", "grow_width"),
            ("up", "grow_height"),
            ("down", "shrink_height"),
            ("shift-left", "reorder_prev"),
            ("shift-right", "reorder_next"),
            ("p", "move_workspace"),
            ("pageup", "scroll_up"),
            ("pagedown", "scroll_down"),
            ("y", "copy"),
            ("s", "save_layout"),
            ("l", "load_layout"),
            ("d", "detach"),
            ("?", "help"),
            ("t", "tab_new"),
            ("]", "tab_next"),
            ("[", "copy_mode"),
            ("{", "tab_previous"),
            ("T", "tab_choose"),
            ("W", "workspace_choose"),
            ("P", "workspace_previous"),
            ("u", "focus_previous"),
            ("!", "focus_last"),
            ("alt-left", "focus_left"),
            ("alt-right", "focus_right"),
            ("alt-up", "focus_up"),
            ("alt-down", "focus_down"),
            (";", "pane_menu"),
            ("'", "tab_menu"),
            ("`", "workspace_menu"),
        ]
        .into_iter()
        .map(|(key, action)| Binding {
            key: key.into(),
            action: action.into(),
        })
        .collect();
        Self {
            prefix: "ctrl-b".into(),
            bindings,
            shell: vec![
                std::env::var("SHELL")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "/bin/sh".into()),
            ],
            history_lines: 10_000,
            clipboard: ClipboardPolicy::Disabled,
            layout: None,
        }
    }
}

#[derive(Default, TypePath)]
struct SettingsLoader;

impl AssetLoader for SettingsLoader {
    type Asset = Settings;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _: &(),
        _: &mut LoadContext<'_>,
    ) -> Result<Settings, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let settings: Settings = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if settings.prefix.is_empty()
            || settings.shell.first().is_none_or(String::is_empty)
            || settings
                .bindings
                .iter()
                .any(|b| b.key.is_empty() || b.action.is_empty())
        {
            return Err(std::io::Error::other(
                "prefix, shell executable, binding keys and actions must not be empty",
            ));
        }
        Ok(settings)
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }
}

/// Consume in Update, then use apply_layout with the asset from Assets<DynamicWorld>.
/// The caller owns which workspace/viewers to replace; no processes are created.
#[derive(Message, Clone, Reflect)]
#[reflect(Message)]
pub struct LayoutReload {
    pub handle: Handle<DynamicWorld>,
}

// Only outstanding loads have deadlines. FileWatcher remains the sole OS watcher.
#[derive(Resource, Clone, Default)]
struct PendingAssets(Arc<Mutex<HashMap<PathBuf, bool>>>);

impl PendingAssets {
    fn start(&self, path: PathBuf) {
        self.0.lock().insert(path, true);
    }
    fn finish(&self, path: &Path) {
        if let Some(pending) = self.0.lock().get_mut(path) {
            *pending = false;
        }
    }
}

#[derive(Resource)]
struct ConfigAssets {
    settings: Handle<Settings>,
    settings_path: PathBuf,
    layout: Option<(String, Handle<DynamicWorld>)>,
    layout_needs_apply: bool,
}

struct LoadWake(Wake);
impl Drop for LoadWake {
    fn drop(&mut self) {
        self.0.notify();
    }
}

/// Call after TaskPoolPlugin and insertion of Wake, before installing AssetPlugin.
/// No file is needed: a missing or invalid configuration leaves usable defaults.
pub fn install(app: &mut App, path: &Path) -> Result<(), String> {
    let wake = app
        .world()
        .get_resource::<Wake>()
        .ok_or("insert Wake before assets::install")?
        .clone();
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let directory = absolute
        .parent()
        .ok_or("configuration path needs a parent directory")?
        .to_path_buf();
    // Native macOS watcher paths resolve /tmp and other symlinked parents.
    let directory = directory.canonicalize().unwrap_or(directory);
    let filename = absolute
        .file_name()
        .ok_or("configuration path needs a filename")?;
    let filename = PathBuf::from(filename);
    let root = directory
        .to_str()
        .ok_or("configuration directory is not UTF-8")?
        .to_owned();
    let pending = PendingAssets::default();
    let watcher_pending = pending.clone();
    let watcher_wake = wake.clone();
    let source = AssetSourceBuilder::platform_default(&root, None).with_watcher(move |sender| {
        let (forward, receiver) = async_channel::unbounded();
        let watcher = match FileWatcher::new(directory.clone(), forward, Duration::from_millis(300))
        {
            Ok(watcher) => watcher,
            Err(error) => {
                bevy_log::warn!(%error, "configuration file watcher unavailable");
                return None;
            }
        };
        let pending = watcher_pending.clone();
        let wake = watcher_wake.clone();
        IoTaskPool::get()
            .spawn(async move {
                while let Ok(event) = receiver.recv().await {
                    match &event {
                        AssetSourceEvent::AddedAsset(path)
                        | AssetSourceEvent::ModifiedAsset(path)
                        | AssetSourceEvent::ModifiedMeta(path) => {
                            if let Some(loading) = pending.0.lock().get_mut(path) {
                                *loading = true;
                            }
                        }
                        _ => {}
                    }
                    if sender.send(event).await.is_err() {
                        break;
                    }
                    wake.notify();
                }
            })
            .detach();
        Some(Box::new(watcher))
    });
    app.register_asset_source(AssetSourceId::Default, source)
        .add_plugins(AssetPlugin {
            file_path: root,
            watch_for_changes_override: Some(true),
            meta_check: AssetMetaCheck::Never,
            unapproved_path_mode: UnapprovedPathMode::Allow,
            ..Default::default()
        })
        .add_plugins(WorldSerializationPlugin)
        .init_asset::<Settings>()
        .init_asset_loader::<SettingsLoader>()
        .init_resource::<Settings>()
        .register_type::<Settings>()
        .register_type::<Binding>()
        .register_type::<LayoutReload>()
        .register_type::<Workspace>()
        .register_type::<crate::model::Tab>()
        .register_type::<crate::model::WorkspaceOrder>()
        .register_type::<PaneView>()
        .register_type::<Name>()
        .register_type::<ChildOf>()
        .register_type::<Children>()
        .register_type::<bevy_ui::Node>()
        .register_type::<bevy_world_serialization::DynamicWorldRoot>()
        .register_type::<bevy_world_serialization::WorldAssetRoot>()
        .add_message::<LayoutReload>()
        .add_systems(
            PostUpdate,
            (update_settings, track_layout, layout_events)
                .chain()
                .after(AssetEventSystems),
        );
    pending.start(filename.clone());
    let settings = app
        .world()
        .resource::<AssetServer>()
        .load_builder()
        .with_guard(LoadWake(wake))
        .load(filename.clone());
    app.insert_resource(pending).insert_resource(ConfigAssets {
        settings,
        settings_path: filename,
        layout: None,
        layout_needs_apply: false,
    });
    Ok(())
}

/// The runner should park_timeout(25ms) while true, and otherwise park indefinitely.
/// This bridges native AssetServer reload tasks, which do not expose a completion wake hook.
pub fn pending(world: &World) -> bool {
    world
        .get_resource::<PendingAssets>()
        .is_some_and(|pending| pending.0.lock().values().any(|value| *value))
}

fn update_settings(
    mut events: MessageReader<AssetEvent<Settings>>,
    mut failures: MessageReader<AssetLoadFailedEvent<Settings>>,
    assets: Res<Assets<Settings>>,
    config: Res<ConfigAssets>,
    pending: Res<PendingAssets>,
    mut settings: ResMut<Settings>,
    wake: Res<Wake>,
) {
    for event in events.read() {
        if (event.is_added(config.settings.id()) || event.is_modified(config.settings.id()))
            && let Some(value) = assets.get(&config.settings)
        {
            *settings = value.clone();
            pending.finish(&config.settings_path);
            wake.notify();
        }
    }
    for failure in failures.read() {
        if failure.id == config.settings.id() {
            pending.finish(&config.settings_path);
            bevy_log::warn!(error = %failure.error, "keeping previous usable configuration");
        }
    }
}

fn track_layout(
    settings: Res<Settings>,
    mut config: ResMut<ConfigAssets>,
    pending: Res<PendingAssets>,
    server: Res<AssetServer>,
    wake: Res<Wake>,
) {
    if !settings.is_changed() {
        return;
    }
    if settings.layout.as_deref() == config.layout.as_ref().map(|(path, _)| path.as_str()) {
        return;
    }
    if let Some((old, _)) = config.layout.take() {
        pending.0.lock().remove(Path::new(&old));
    }
    if let Some(path) = &settings.layout {
        pending.start(PathBuf::from(path));
        let handle = server
            .load_builder()
            .with_guard(LoadWake(wake.clone()))
            .load(path.clone());
        config.layout = Some((path.clone(), handle));
        config.layout_needs_apply = true;
    }
}

fn layout_events(
    mut events: MessageReader<AssetEvent<DynamicWorld>>,
    mut failures: MessageReader<AssetLoadFailedEvent<DynamicWorld>>,
    mut config: ResMut<ConfigAssets>,
    pending: Res<PendingAssets>,
    assets: Res<Assets<DynamicWorld>>,
    mut reloads: MessageWriter<LayoutReload>,
    wake: Res<Wake>,
) {
    let mut changed = false;
    for event in events.read() {
        if let Some((_, handle)) = &config.layout {
            changed |= event.is_added(handle.id()) || event.is_modified(handle.id());
        }
    }
    if let Some((path, handle)) = &config.layout
        && (changed || config.layout_needs_apply)
        && assets.contains(handle.id())
    {
        pending.finish(Path::new(path));
        reloads.write(LayoutReload {
            handle: handle.clone(),
        });
        config.layout_needs_apply = false;
        wake.notify();
    }
    for failure in failures.read() {
        if let Some((path, handle)) = &config.layout
            && failure.id == handle.id()
        {
            pending.finish(Path::new(path));
            config.layout_needs_apply = false;
            bevy_log::warn!(error = %failure.error, "keeping previous layout");
        }
    }
}

pub fn extract_layout(world: &World, root: Entity) -> Result<DynamicWorld, String> {
    if world.get::<Workspace>(root).is_none() {
        return Err("layout root is not a workspace".into());
    }
    if world.get::<ChildOf>(root).is_some() {
        return Err("workspace root has a parent".into());
    }
    let mut selected = EntityHashSet::default();
    let mut remaining = vec![root];
    while let Some(entity) = remaining.pop() {
        if !selected.insert(entity) {
            return Err("layout hierarchy contains a cycle".into());
        }
        if world.get::<crate::model::Viewer>(entity).is_some() {
            return Err("viewers cannot belong to layout scenes".into());
        }
        if world.get::<crate::model::Tab>(entity).is_some()
            && world
                .get::<ChildOf>(entity)
                .is_none_or(|p| p.parent() != root)
        {
            return Err("tabs must be direct workspace children".into());
        }
        if world.get::<Launch>(entity).is_some() || world.get::<ProcessState>(entity).is_some() {
            return Err("process entities must live outside the layout hierarchy".into());
        }
        if world.get_entity(entity).is_err() {
            return Err("layout references a missing child".into());
        }
        if let Some(children) = world.get::<Children>(entity) {
            remaining.extend(children.iter());
        }
    }
    let registry = world
        .get_resource::<AppTypeRegistry>()
        .ok_or("missing reflection registry")?
        .read();
    Ok(DynamicWorldBuilder::from_world(world, &registry)
        .extract_entities(selected.into_iter())
        .build())
}

pub fn serialize_layout(world: &World, root: Entity) -> Result<String, String> {
    let scene = extract_layout(world, root)?;
    let registry = world.resource::<AppTypeRegistry>().read();
    scene.serialize(&registry).map_err(|e| e.to_string())
}

pub fn deserialize_layout(
    world: &mut World,
    text: &str,
    mapping: &[(Entity, Entity)],
) -> Result<Entity, String> {
    let registry = world
        .get_resource::<AppTypeRegistry>()
        .ok_or("missing reflection registry")?
        .clone();
    let mut server = world
        .get_resource::<AssetServer>()
        .ok_or("missing AssetServer")?
        .clone();
    let mut ron = ron::de::Deserializer::from_str(text).map_err(|e| e.to_string())?;
    let scene = WorldDeserializer {
        type_registry: &registry.read(),
        load_from_path: &mut server,
    }
    .deserialize(&mut ron)
    .map_err(|e| e.to_string())?;
    ron.end().map_err(|e| e.to_string())?;
    apply_layout(world, &scene, mapping)
}

fn component<T: Reflect + FromReflect + TypePath>(
    entity: &bevy_world_serialization::DynamicEntity,
) -> Result<Option<T>, String> {
    entity
        .components
        .iter()
        .find(|value| value.represents::<T>())
        .map(|value| {
            T::from_reflect(value.as_partial_reflect())
                .ok_or_else(|| format!("invalid {}", std::any::type_name::<T>()))
        })
        .transpose()
}

/// Preserve same-world live-pane IDs unless an explicit old-pane -> existing-pane mapping is supplied.
/// Validate every process reference before spawning any layout entities. Other registered component
/// types pass straight through Bevy reflection and native entity mapping, without an allowlist.
pub fn apply_layout(
    world: &mut World,
    scene: &DynamicWorld,
    mapping: &[(Entity, Entity)],
) -> Result<Entity, String> {
    if !scene.resources.is_empty() {
        return Err("a layout scene must not contain resources".into());
    }
    let registry = world
        .get_resource::<AppTypeRegistry>()
        .ok_or("missing reflection registry")?
        .clone();
    let registry = registry.read();
    let mut ids = EntityHashSet::default();
    let mut parents = EntityHashMap::default();
    let mut root = None;
    let mut entity_map = EntityHashMap::default();
    for &(old, existing) in mapping {
        if entity_map.insert(old, existing).is_some() {
            return Err(format!("duplicate pane mapping for {old}"));
        }
    }
    for entity in &scene.entities {
        if !ids.insert(entity.entity) {
            return Err("duplicate entity in layout".into());
        }
        if entity_map.contains_key(&entity.entity) {
            return Err("pane mappings must not overwrite layout entities".into());
        }
        for value in &entity.components {
            let info = value
                .get_represented_type_info()
                .ok_or("scene component has no represented type")?;
            let registration = registry
                .get(info.type_id())
                .ok_or_else(|| format!("unregistered scene type {}", info.type_path()))?;
            if registration.data::<ReflectComponent>().is_none() {
                return Err(format!("{} is not a reflected component", info.type_path()));
            }
        }
        if component::<crate::model::Viewer>(entity)?.is_some() {
            return Err("viewers cannot belong to layout scenes".into());
        }
        if component::<Launch>(entity)?.is_some() || component::<ProcessState>(entity)?.is_some() {
            return Err(
                "layout scenes refer to existing processes; they cannot contain process entities"
                    .into(),
            );
        }
        if component::<Workspace>(entity)?.is_some() && root.replace(entity.entity).is_some() {
            return Err("layout must contain exactly one workspace".into());
        }
        if let Some(parent) = component::<ChildOf>(entity)? {
            parents.insert(entity.entity, parent.parent());
        }
    }
    let root = root.ok_or("layout has no workspace root")?;
    if parents.contains_key(&root) {
        return Err("workspace root has a parent".into());
    }
    for entity in &scene.entities {
        if component::<crate::model::Tab>(entity)?.is_some()
            && parents.get(&entity.entity) != Some(&root)
        {
            return Err("tabs must be direct workspace children".into());
        }
        let mut cursor = entity.entity;
        let mut visited = EntityHashSet::default();
        while cursor != root {
            if !visited.insert(cursor) {
                return Err("layout hierarchy contains a cycle".into());
            }
            cursor = *parents
                .get(&cursor)
                .ok_or("layout entity is not beneath its workspace root")?;
        }
        if let Some(children) = component::<Children>(entity)? {
            for child in children.iter() {
                if !ids.contains(&child) || parents.get(&child) != Some(&entity.entity) {
                    return Err("layout Children and ChildOf relationships disagree".into());
                }
            }
        }
        if let Some(view) = component::<PaneView>(entity)? {
            if ids.contains(&view.pane) {
                return Err("PaneView must refer outside the layout to a live process".into());
            }
            let target = entity_map.get(&view.pane).copied().unwrap_or(view.pane);
            if world.get::<Launch>(target).is_none() || world.get::<ProcessState>(target).is_none()
            {
                return Err(format!(
                    "missing live pane {} (mapped to {target}); provide an old-to-existing pane mapping",
                    view.pane
                ));
            }
            entity_map.insert(view.pane, target);
        }
    }
    // Preallocate only new layout IDs; mappings never target existing layout entities.
    for entity in &scene.entities {
        entity_map.insert(entity.entity, world.spawn_empty().id());
    }
    if let Err(error) = scene.write_to_world_with(world, &mut entity_map, &registry) {
        for old in &ids {
            if let Some(&entity) = entity_map.get(old) {
                world.despawn(entity);
            }
        }
        return Err(error.to_string());
    }
    let root = entity_map[&root];
    crate::navigation::normalize_workspace(world, root);
    Ok(root)
}
