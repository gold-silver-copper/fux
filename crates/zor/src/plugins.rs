//! The plugin host (prompt 4.4, owner PluginHost): zor owns the host surface Herdr describes
//! and plugins are ordinary processes. A plugin is a directory with a `zor-plugin.toml`
//! ([`manifest`]) that is installed (copied under `<state_dir>/plugins/<name>/plugin/`) or
//! linked (used in place); its record is a `HostedPlugin` entity with one `PluginAction` per
//! manifest action, the [`PluginEnabled`] marker being the journaled intent. Everything else
//! here is runtime state rebuilt from the manifest at startup.
//!
//! Processes (build, startup, actions, surface drivers, link handlers and the per-plugin hook
//! process) run through `Effect::RunPlugin` ([`host::PluginAdapter`]) with `ZOR_BRP` (a
//! per-plugin descriptor carrying a token minted for it), `FUX_BRP` (a per-run descriptor
//! with a `fux/token.mint` token scoped to the run's workspace) and the `ZOR_PLUGIN_*`
//! context. Plugin panes are compositions of `fux/*` calls ("Placements" below) with a
//! `PaneTemplate` carrying the same environment, or a surface the plugin drives. Event hooks:
//! one `zor plugin hook <name>` process per plugin ([`hooks`]) that streams `zor/events+watch`
//! and `fux/events+watch` from the persisted cursors, runs the matching `[[events]]` commands
//! and reports progress through `zor/plugin.cursor`; it is restarted with backoff when it exits.
//!
//! Placements over `node.*`/`root.*`: `split` = `fux/pane.new` beside the target pane;
//! `tab` = `fux/root.new`; `zoomed` = `split` then `fux/viewer.zoom` for every viewer of the
//! workspace; `overlay` = `fux/node.spawn` of an absolute node covering the root; `popup` =
//! the same node centred at half the root's size.

pub mod hooks;
pub mod host;
pub mod manifest;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use fux::remote::client::{Descriptor, read_descriptor};
use fux::remote::descriptor::write_descriptor;
use fux::remote::methods::{NodeSpawned, PaneCreated, RootCreated, ViewerList, WorkspaceList};
use fux::remote::surface_methods::SurfaceOpened;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use manifest::{Manifest, ManifestError, PaneKind, Placement};

use crate::journal::Journal;
use crate::model::graph::{spawn_plugin, spawn_plugin_action};
use crate::model::{
    Actions, ActionOf, Clock, Effect, HostedPlugin, Ids, Inbound, ModelError, Phase, PluginActionId,
    PluginId, PluginManifest, ServerInstance,
};
use crate::remote::token::{Capabilities, Grant, Tokens};
use crate::remote::{DescriptorGuard, DescriptorPath};

/// Tag in the high byte of every `Effect::FuxCall.call` this module issues (lifecycle `0x01`,
/// providers `0x03`, dashboard `0x06`).
pub const CALL_TAG: u64 = 0x07 << 56;
const TAG_MASK: u64 = 0xff << 56;
/// Under `state_dir`.
pub const PLUGINS_DIR: &str = "plugins";
pub const MAX_PLUGINS: usize = 32;
/// Run records retained per plugin.
pub const MAX_RUNS_RETAINED: usize = 32;
/// Hook restart backoff: doubles from the first to the last value.
pub const HOOK_BACKOFF_MIN_MS: u64 = 250;
pub const HOOK_BACKOFF_MAX_MS: u64 = 30_000;
/// Bytes copied per installed plugin directory.
pub const MAX_INSTALL_BYTES: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------------------------

/// Where fux's descriptor lives (`Paths::fux_descriptor`); absent in headless tests.
#[derive(Resource, Debug, Clone)]
pub struct FuxDescriptor(pub PathBuf);

/// Host directories and the binary that runs hook processes.
#[derive(Resource, Debug, Clone)]
pub struct PluginPaths {
    /// `<state_dir>/plugins`.
    pub root: PathBuf,
    /// The `zor` executable for `zor plugin hook`; tests point it at the built binary.
    pub zor_bin: PathBuf,
}

impl PluginPaths {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(PLUGINS_DIR),
            zor_bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zor")),
        }
    }

    /// `<root>/<name>`: everything zor keeps for one plugin.
    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    /// The copied plugin files of an installed plugin.
    pub fn install_dir(&self, name: &str) -> PathBuf {
        self.dir(name).join("plugin")
    }
    /// `ZOR_PLUGIN_STATE_DIR`: durable plugin-owned state.
    pub fn state_dir(&self, name: &str) -> PathBuf {
        self.dir(name).join("state")
    }
    pub fn log(&self, name: &str) -> PathBuf {
        self.dir(name).join("plugin.log")
    }
    pub fn cursors(&self, name: &str) -> PathBuf {
        self.dir(name).join("cursors.json")
    }
    /// The per-plugin zor descriptor (`ZOR_BRP`).
    pub fn descriptor(&self, name: &str) -> PathBuf {
        self.dir(name).join("zor.brp.json")
    }
    /// The per-run fux descriptor (`FUX_BRP`).
    pub fn fux_descriptor(&self, name: &str, run: u64) -> PathBuf {
        self.dir(name).join("runs").join(format!("{run}.fux.brp.json"))
    }
}

/// Pending fux calls and run/opening counters.
#[derive(Resource, Default)]
struct Host {
    next_run: u64,
    next_call: u64,
    calls: HashMap<u64, Pending>,
    /// This update's messages for this module.
    inbox: Vec<Item>,
}

enum Item {
    Exited {
        plugin: Entity,
        run: u64,
        code: Option<i32>,
    },
    Reply {
        call: u64,
        result: Result<Value, String>,
    },
}

enum Pending {
    /// `fux/token.mint` for a run; the reply spawns it.
    Mint { plugin: Entity, run: u64 },
    /// `fux/token.mint` for a pane composition.
    PaneMint { plugin: Entity, opening: u64 },
    PaneList { plugin: Entity, opening: u64 },
    PaneCreate { plugin: Entity, opening: u64 },
    PaneViewers { plugin: Entity, opening: u64 },
    PaneSurface { plugin: Entity, opening: u64 },
    /// Fire-and-forget (`viewer.zoom`, `surface.close`).
    Ignore,
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// The journaled intent: the plugin's hook and startup run and its actions are invocable.
#[derive(
    Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component)]
pub struct PluginEnabled;

/// The manifest as loaded for this platform, with the directory its commands run in. Absent
/// while the manifest cannot be read (see [`PluginProblem`]).
#[derive(Component, Clone, Debug)]
pub struct Loaded {
    pub manifest: Manifest,
    pub root: PathBuf,
}

/// Why the plugin is not usable (manifest unreadable, build failed, ...).
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct PluginProblem(pub String);

/// The manifest action behind a `PluginAction` entity.
#[derive(Component, Clone, Debug)]
pub struct ActionSpec(pub manifest::Action);

/// Enabled and reachable: the per-plugin descriptor is written with this token.
#[derive(Component, Clone, Debug)]
pub struct Active {
    token: String,
}

/// The hook process of a plugin with `[[events]]`: its live run and restart schedule.
#[derive(Component, Clone, Debug, Default)]
pub struct Hook {
    pub run: Option<u64>,
    pub cursors: HookCursors,
    pub restarts: u32,
    /// `Clock` milliseconds before which no restart happens.
    pub next_ms: u64,
}

/// Persisted per plugin in `cursors.json`: the last event each stream delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HookCursors {
    pub zor: u64,
    pub fux: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum RunKind {
    Build,
    Startup,
    Hook,
    Action(String),
    /// The driver process of a surface pane.
    Surface(String),
    Link(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Waiting for `fux/token.mint`.
    Minting,
    Running,
    Exited { code: Option<i32>, ms: u64 },
}

/// One process run (bounded history in [`Runs`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    #[serde(flatten)]
    pub kind: RunKind,
    pub started_ms: u64,
    pub state: RunState,
    pub workspace: Option<String>,
    #[serde(skip)]
    pub task: Option<String>,
    #[serde(skip)]
    pub surface: Option<u64>,
    #[serde(skip)]
    pub link: Option<String>,
}

#[derive(Component, Clone, Debug, Default)]
pub struct Runs(pub VecDeque<Run>);

impl Runs {
    fn get_mut(&mut self, id: u64) -> Option<&mut Run> {
        self.0.iter_mut().find(|r| r.id == id)
    }

    fn push(&mut self, run: Run) {
        while self.0.len() >= MAX_RUNS_RETAINED {
            self.0.pop_front();
        }
        self.0.push_back(run);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PaneState {
    Opening,
    Open {
        node: u64,
        pane: Option<u64>,
        surface: Option<u64>,
    },
    Failed {
        problem: String,
    },
}

/// One pane composition requested through `zor/plugin.pane`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPane {
    pub id: u64,
    pub pane: String,
    pub workspace: String,
    pub placement: Placement,
    pub kind: PaneKind,
    #[serde(flatten)]
    pub state: PaneState,
    #[serde(skip)]
    target: Option<u64>,
    #[serde(skip)]
    fux_token: Option<String>,
    #[serde(skip)]
    fux_brp: Option<PathBuf>,
    #[serde(skip)]
    task: Option<String>,
}

#[derive(Component, Clone, Debug, Default)]
pub struct Panes(pub VecDeque<OpenPane>);

// ---------------------------------------------------------------------------------------------
// Errors and records
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    Manifest(ManifestError),
    Model(ModelError),
    NotFound(String),
    Refused(String),
    Io(String),
}

impl core::fmt::Display for PluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "{e}"),
            Self::Model(e) => write!(f, "{e}"),
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl core::error::Error for PluginError {}

impl From<ManifestError> for PluginError {
    fn from(e: ManifestError) -> Self {
        Self::Manifest(e)
    }
}

impl From<ModelError> for PluginError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

pub type Res<T> = Result<T, PluginError>;

fn refused<T>(why: impl Into<String>) -> Res<T> {
    Err(PluginError::Refused(why.into()))
}

/// The public record of one plugin (`zor/plugin.list`/`inspect`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginRecord {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// `installed` or `linked`.
    pub source: String,
    /// The manifest file.
    pub path: String,
    pub enabled: bool,
    /// Enabled, reachable, descriptor written.
    pub active: bool,
    pub problem: Option<String>,
    pub actions: Vec<ActionRecord>,
    pub events: Vec<String>,
    pub panes: Vec<String>,
    pub links: Vec<String>,
    pub hook: Option<HookRecord>,
    pub runs: Vec<Run>,
    pub open_panes: Vec<OpenPane>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub id: String,
    /// The global `PluginActionId`: `<plugin>__<action>`.
    pub qualified: String,
    pub title: String,
    pub keybinding: Option<String>,
    pub placement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRecord {
    pub run: Option<u64>,
    pub cursor_zor: u64,
    pub cursor_fux: u64,
    pub restarts: u32,
}

/// One exported keybinding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub chord: String,
    pub plugin: String,
    pub action: String,
}

/// Context of an action run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunContext {
    pub task: Option<String>,
    pub workspace: Option<String>,
}

/// `<plugin>__<action>`: the global action id.
pub fn qualified_action(plugin: &str, action: &str) -> String {
    format!("{plugin}__{action}")
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

pub struct PluginsPlugin {
    pub state_dir: PathBuf,
}

impl Plugin for PluginsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<PluginEnabled>()
            .insert_resource(PluginPaths::new(&self.state_dir))
            .init_resource::<Host>()
            .add_systems(PostStartup, reload_manifests)
            .add_systems(
                PreUpdate,
                (collect_inbound, ingest)
                    .chain()
                    .in_set(Phase::Completions),
            )
            .add_systems(
                Update,
                (activate, restart_hooks).chain().in_set(Phase::Lifecycle),
            );
    }
}

// ---------------------------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------------------------

pub fn find(world: &World, name: &str) -> Res<Entity> {
    world
        .resource::<Ids>()
        .plugins
        .get(name)
        .copied()
        .ok_or_else(|| PluginError::NotFound(format!("plugin {name}")))
}

fn name_of(world: &World, plugin: Entity) -> String {
    world
        .get::<PluginId>(plugin)
        .map(|id| id.0.clone())
        .unwrap_or_default()
}

fn now(world: &World) -> u64 {
    world.resource::<Clock>().now_ms
}

// ---------------------------------------------------------------------------------------------
// Install / link
// ---------------------------------------------------------------------------------------------

/// Copies the plugin directory of `source` (a directory or its manifest) under the host and
/// registers it; re-installing replaces the copy. Runs `build` when declared.
pub fn install(world: &mut World, source: &Path, enabled: bool) -> Res<Entity> {
    let source_dir = source_dir(source)?;
    let manifest = Manifest::read(&source_dir)?.for_platform(manifest::current_platform())?;
    let paths = world.resource::<PluginPaths>().clone();
    let target = paths.install_dir(&manifest.name);
    if target.exists() && !is_installed(&paths, &manifest.name, &target) {
        return refused("plugin directory is in use");
    }
    let staging = paths.dir(&manifest.name).join("plugin.new");
    let _ = std::fs::remove_dir_all(&staging);
    copy_tree(&source_dir, &staging, &mut 0).map_err(PluginError::Io)?;
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| PluginError::Io(e.to_string()))?;
    register(world, manifest, target.join(manifest::MANIFEST_FILE), enabled)
}

/// Registers the plugin at `dir` in place (development); the manifest is re-read at every
/// server start. Runs `build` when declared.
pub fn link(world: &mut World, dir: &Path, enabled: bool) -> Res<Entity> {
    let dir = source_dir(dir)?;
    if !dir.is_absolute() {
        return refused("link path must be absolute");
    }
    let manifest = Manifest::read(&dir)?.for_platform(manifest::current_platform())?;
    let paths = world.resource::<PluginPaths>();
    if dir.starts_with(&paths.root) {
        return refused("cannot link a plugin from the host directory; use install");
    }
    register(world, manifest, dir.join(manifest::MANIFEST_FILE), enabled)
}

fn source_dir(source: &Path) -> Res<PathBuf> {
    let dir = if source.is_file() {
        source.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        source.to_path_buf()
    };
    if !dir.is_dir() {
        return Err(PluginError::NotFound(format!("plugin directory {}", dir.display())));
    }
    dir.canonicalize()
        .map_err(|e| PluginError::Io(format!("{}: {e}", dir.display())))
}

fn is_installed(paths: &PluginPaths, name: &str, dir: &Path) -> bool {
    dir.starts_with(paths.install_dir(name))
}

fn copy_tree(from: &Path, to: &Path, copied: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == ".git" || name == "node_modules" || name == "target" {
            continue;
        }
        let path = entry.path();
        let dest = to.join(&name);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            copy_tree(&path, &dest, copied)?;
        } else {
            *copied += meta.len();
            if *copied > MAX_INSTALL_BYTES {
                return Err(format!("plugin larger than {MAX_INSTALL_BYTES} bytes"));
            }
            std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Creates or updates the entities for `manifest` and marks the journal.
fn register(world: &mut World, manifest: Manifest, file: PathBuf, enabled: bool) -> Res<Entity> {
    for action in &manifest.actions {
        if qualified_action(&manifest.name, &action.id).len() > crate::model::ids::MAX_ID_LEN {
            return refused(format!(
                "action id {} too long with the plugin name",
                action.id
            ));
        }
    }
    let existing = world.resource::<Ids>().plugins.get(manifest.name.as_str()).copied();
    let record = PluginManifest {
        path: file.display().to_string(),
        version: manifest.version.clone(),
    };
    let plugin = match existing {
        Some(plugin) => {
            world.entity_mut(plugin).insert(record);
            plugin
        }
        None => {
            if world.resource::<Ids>().plugins.len() >= MAX_PLUGINS {
                return refused(format!("at most {MAX_PLUGINS} plugins"));
            }
            spawn_plugin(world, &manifest.name, record)?
        }
    };
    let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let paths = world.resource::<PluginPaths>().clone();
    std::fs::create_dir_all(paths.state_dir(&manifest.name))
        .map_err(|e| PluginError::Io(e.to_string()))?;
    let cursors = read_cursors(&paths.cursors(&manifest.name));
    let build = manifest.build.clone();
    apply_manifest(world, plugin, manifest, root)?;
    {
        let mut entity = world.entity_mut(plugin);
        entity.remove::<PluginProblem>();
        if !entity.contains::<Hook>() {
            entity.insert(Hook {
                cursors,
                ..Default::default()
            });
        }
        if !entity.contains::<Runs>() {
            entity.insert((Runs::default(), Panes::default()));
        }
        if enabled {
            entity.insert(PluginEnabled);
        } else {
            entity.remove::<PluginEnabled>();
        }
    }
    world.resource_mut::<Journal>().mark_dirty();
    if !build.is_empty() {
        start_run(world, plugin, RunKind::Build, build, RunContext::default())?;
    }
    Ok(plugin)
}

/// Inserts `Loaded` and reconciles the `PluginAction` entities with the manifest's actions.
fn apply_manifest(world: &mut World, plugin: Entity, manifest: Manifest, root: PathBuf) -> Res<()> {
    let name = name_of(world, plugin);
    let wanted: HashMap<String, manifest::Action> = manifest
        .actions
        .iter()
        .map(|a| (qualified_action(&name, &a.id), a.clone()))
        .collect();
    let current: Vec<(Entity, String)> = world
        .get::<Actions>(plugin)
        .map(|actions| {
            actions
                .iter()
                .filter_map(|e| world.get::<PluginActionId>(e).map(|id| (e, id.0.clone())))
                .collect()
        })
        .unwrap_or_default();
    for (entity, id) in &current {
        match wanted.get(id) {
            Some(action) => {
                world.entity_mut(*entity).insert(ActionSpec(action.clone()));
            }
            None => {
                world.despawn(*entity);
            }
        }
    }
    for (id, action) in &wanted {
        if !current.iter().any(|(_, have)| have == id) {
            let entity = spawn_plugin_action(world, id, plugin)?;
            world.entity_mut(entity).insert(ActionSpec(action.clone()));
        }
    }
    world.entity_mut(plugin).insert(Loaded { manifest, root });
    Ok(())
}

/// `PostStartup`: re-read every journaled plugin's manifest; an unreadable one keeps its
/// record with a problem and no runnable entries.
fn reload_manifests(world: &mut World) {
    let plugins: Vec<(Entity, String)> = world
        .query_filtered::<(Entity, &PluginManifest), With<HostedPlugin>>()
        .iter(world)
        .map(|(e, m)| (e, m.path.clone()))
        .collect();
    let paths = world.resource::<PluginPaths>().clone();
    for (plugin, path) in plugins {
        let name = name_of(world, plugin);
        let cursors = read_cursors(&paths.cursors(&name));
        let mut entity = world.entity_mut(plugin);
        entity.insert((
            Hook {
                cursors,
                ..Default::default()
            },
            Runs::default(),
            Panes::default(),
        ));
        let file = PathBuf::from(&path);
        let loaded = Manifest::read(&file)
            .and_then(|m| m.for_platform(manifest::current_platform()))
            .map_err(PluginError::Manifest)
            .and_then(|m| {
                if m.name != name {
                    return refused(format!("manifest names {} but the record is {name}", m.name));
                }
                Ok(m)
            });
        match loaded {
            Ok(manifest) => {
                let root = file.parent().map(Path::to_path_buf).unwrap_or_default();
                if let Err(e) = apply_manifest(world, plugin, manifest, root) {
                    world.entity_mut(plugin).insert(PluginProblem(e.to_string()));
                }
            }
            Err(e) => {
                bevy_log::warn!("plugin {name}: {e}");
                world.entity_mut(plugin).insert(PluginProblem(e.to_string()));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Enable / disable
// ---------------------------------------------------------------------------------------------

pub fn enable(world: &mut World, plugin: Entity) -> Res<()> {
    check_plugin(world, plugin)?;
    world.entity_mut(plugin).insert(PluginEnabled);
    Ok(())
}

/// Disabling stops the hook process and revokes the plugin's token; running actions finish.
pub fn disable(world: &mut World, plugin: Entity) -> Res<()> {
    check_plugin(world, plugin)?;
    world.entity_mut(plugin).remove::<PluginEnabled>();
    deactivate(world, plugin);
    Ok(())
}

fn check_plugin(world: &World, plugin: Entity) -> Res<()> {
    if world.get::<HostedPlugin>(plugin).is_none() {
        return Err(PluginError::NotFound(format!("plugin {plugin}")));
    }
    Ok(())
}

fn deactivate(world: &mut World, plugin: Entity) {
    let Some(active) = world.entity_mut(plugin).take::<Active>() else {
        return;
    };
    world.resource_mut::<Tokens>().revoke(&active.token);
    crate::remote::watch::revoke(world, &active.token);
    let name = name_of(world, plugin);
    let _ = std::fs::remove_file(world.resource::<PluginPaths>().descriptor(&name));
    if let Some(run) = world.get_mut::<Hook>(plugin).and_then(|mut h| h.run.take()) {
        world.write_message(Effect::KillPlugin { plugin, run });
    }
}

/// `Update`: enabled plugins become `Active` once zor's own descriptor is written (token
/// minted, per-plugin descriptor written, `startup` run); plugins no longer enabled lose it.
fn activate(world: &mut World) {
    let Some(zor_brp) = world.get_resource::<DescriptorGuard>().map(|g| g.0.clone()) else {
        return;
    };
    let stale: Vec<Entity> = world
        .query_filtered::<Entity, (With<Active>, Without<PluginEnabled>)>()
        .iter(world)
        .collect();
    for plugin in stale {
        deactivate(world, plugin);
    }
    let fresh: Vec<Entity> = world
        .query_filtered::<Entity, (With<PluginEnabled>, With<Loaded>, Without<Active>, Without<PluginProblem>)>()
        .iter(world)
        .collect();
    if fresh.is_empty() {
        return;
    }
    let Ok(descriptor) = read_descriptor(&zor_brp) else {
        return;
    };
    for plugin in fresh {
        let name = name_of(world, plugin);
        let Ok(token) = fux::attach::random_hex256() else {
            continue;
        };
        let minted = world.resource_mut::<Tokens>().mint(
            token.clone(),
            Grant {
                workspace: None,
                capabilities: Capabilities::READ.union(Capabilities::MUTATE),
            },
        );
        if let Err(e) = minted {
            world
                .entity_mut(plugin)
                .insert(PluginProblem(format!("token: {e}")));
            continue;
        }
        let paths = world.resource::<PluginPaths>().clone();
        let own = Descriptor {
            token: token.clone(),
            ..descriptor.clone()
        };
        if let Err(e) = write_descriptor(&paths.descriptor(&name), &own) {
            world.resource_mut::<Tokens>().revoke(&token);
            world.entity_mut(plugin).insert(PluginProblem(format!("descriptor: {e}")));
            continue;
        }
        world.entity_mut(plugin).insert(Active { token });
        if let Some(mut hook) = world.get_mut::<Hook>(plugin) {
            hook.restarts = 0;
            hook.next_ms = 0;
        }
        let startup = world
            .get::<Loaded>(plugin)
            .map(|l| l.manifest.startup.clone())
            .unwrap_or_default();
        if !startup.is_empty()
            && let Err(e) = start_run(world, plugin, RunKind::Startup, startup, RunContext::default())
        {
            bevy_log::warn!("plugin {name}: startup: {e}");
        }
    }
}

/// `Update`: an active plugin with `[[events]]` and no live hook process gets one once its
/// backoff passed.
fn restart_hooks(world: &mut World) {
    let now = now(world);
    let due: Vec<Entity> = world
        .query_filtered::<(Entity, &Hook, &Loaded), With<Active>>()
        .iter(world)
        .filter(|(_, hook, loaded)| {
            hook.run.is_none() && !loaded.manifest.events.is_empty() && hook.next_ms <= now
        })
        .map(|(e, _, _)| e)
        .collect();
    for plugin in due {
        let paths = world.resource::<PluginPaths>().clone();
        let name = name_of(world, plugin);
        let argv = vec![
            paths.zor_bin.display().to_string(),
            "plugin".into(),
            "hook".into(),
            name.clone(),
        ];
        match start_run(world, plugin, RunKind::Hook, argv, RunContext::default()) {
            Ok(run) => {
                if let Some(mut hook) = world.get_mut::<Hook>(plugin) {
                    hook.run = Some(run);
                }
            }
            Err(e) => bevy_log::warn!("plugin {name}: hook: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------------------------

/// Runs `action` of `plugin` with the context; returns the run id (`Placement::None`) or the
/// pane opening id for placed actions.
pub fn run_action(world: &mut World, plugin: Entity, action: &str, ctx: RunContext) -> Res<u64> {
    check_active(world, plugin)?;
    let spec = world
        .get::<Loaded>(plugin)
        .and_then(|l| l.manifest.action(action).cloned())
        .ok_or_else(|| PluginError::NotFound(format!("action {action}")))?;
    if spec.placement == Placement::None {
        return start_run(world, plugin, RunKind::Action(action.into()), spec.command, ctx);
    }
    let Some(workspace) = ctx.workspace.clone() else {
        return refused("a placed action needs `workspace`");
    };
    open_composition(
        world,
        plugin,
        manifest::Pane {
            id: spec.id,
            title: spec.title,
            kind: PaneKind::Terminal,
            placement: spec.placement,
            command: spec.command,
            platforms: Vec::new(),
        },
        workspace,
        None,
        ctx.task,
    )
}

/// Opens manifest pane `pane` in `workspace` (beside `target` for split placements); returns
/// the opening id, whose progress `zor/plugin.inspect` shows.
pub fn open_pane(
    world: &mut World,
    plugin: Entity,
    pane: &str,
    workspace: &str,
    target: Option<u64>,
    task: Option<String>,
) -> Res<u64> {
    check_active(world, plugin)?;
    let spec = world
        .get::<Loaded>(plugin)
        .and_then(|l| l.manifest.pane(pane).cloned())
        .ok_or_else(|| PluginError::NotFound(format!("pane {pane}")))?;
    open_composition(world, plugin, spec, workspace.into(), target, task)
}

/// Runs the first enabled plugin's link handler whose pattern matches `url`.
pub fn open_link(world: &mut World, url: &str) -> Res<(Entity, u64)> {
    if url.len() > 4096 {
        return refused("url longer than 4096 bytes");
    }
    let candidates: Vec<(Entity, manifest::Link)> = world
        .query_filtered::<(Entity, &Loaded), With<Active>>()
        .iter(world)
        .flat_map(|(e, loaded)| loaded.manifest.links.iter().map(move |l| (e, l.clone())))
        .collect();
    for (plugin, link) in candidates {
        let matches = regex::Regex::new(&link.pattern).is_ok_and(|re| re.is_match(url));
        if matches {
            let run = start_run(
                world,
                plugin,
                RunKind::Link(link.id),
                link.command,
                RunContext {
                    task: None,
                    workspace: None,
                }
                .with_link(url),
            )?;
            return Ok((plugin, run));
        }
    }
    Err(PluginError::NotFound("link handler".into()))
}

impl RunContext {
    fn with_link(self, url: &str) -> LinkContext {
        LinkContext {
            ctx: self,
            link: Some(url.to_owned()),
            surface: None,
        }
    }
}

/// [`RunContext`] plus the link/surface facts of those run kinds.
struct LinkContext {
    ctx: RunContext,
    link: Option<String>,
    surface: Option<u64>,
}

impl From<RunContext> for LinkContext {
    fn from(ctx: RunContext) -> Self {
        Self {
            ctx,
            link: None,
            surface: None,
        }
    }
}

fn check_active(world: &World, plugin: Entity) -> Res<()> {
    check_plugin(world, plugin)?;
    if world.get::<PluginEnabled>(plugin).is_none() {
        return refused("plugin is disabled");
    }
    if let Some(problem) = world.get::<PluginProblem>(plugin) {
        return refused(format!("plugin is not usable: {}", problem.0));
    }
    if world.get::<Active>(plugin).is_none() {
        return refused("plugin is not active yet (server descriptor pending)");
    }
    Ok(())
}

/// Records the run, then either mints a fux token for it (`fux/token.mint`, spawned on the
/// reply) or spawns it at once when fux is not configured.
fn start_run(
    world: &mut World,
    plugin: Entity,
    kind: RunKind,
    argv: Vec<String>,
    ctx: impl Into<LinkContext>,
) -> Res<u64> {
    let ctx: LinkContext = ctx.into();
    let id = {
        let mut host = world.resource_mut::<Host>();
        host.next_run += 1;
        host.next_run
    };
    let ms = now(world);
    let run = Run {
        id,
        kind,
        started_ms: ms,
        state: RunState::Minting,
        workspace: ctx.ctx.workspace.clone(),
        task: ctx.ctx.task.clone(),
        surface: ctx.surface,
        link: ctx.link,
    };
    let Some(mut runs) = world.get_mut::<Runs>(plugin) else {
        return Err(PluginError::NotFound(format!("plugin {plugin}")));
    };
    runs.push(run);
    world.entity_mut(plugin).insert(RunArgv::insert(id, argv));
    let has_fux = world
        .get_resource::<FuxDescriptor>()
        .is_some_and(|d| d.0.is_file());
    if has_fux {
        let call = fux_call(
            world,
            Pending::Mint { plugin, run: id },
            "fux/token.mint",
            json!({
                "workspace": ctx.ctx.workspace,
                "capabilities": ["read", "mutate"],
            }),
        );
        let _ = call;
    } else {
        spawn_run(world, plugin, id, None);
    }
    Ok(id)
}

/// Commands of runs not spawned yet (the mint reply is pending).
#[derive(Component, Default)]
struct RunArgv(HashMap<u64, Vec<String>>);

impl RunArgv {
    fn insert(id: u64, argv: Vec<String>) -> impl Bundle {
        let mut map = HashMap::new();
        map.insert(id, argv);
        RunArgvInsert(map)
    }
}

/// Merges into an existing `RunArgv` on insert instead of replacing it.
#[derive(Component)]
#[component(on_insert = merge_argv)]
struct RunArgvInsert(HashMap<u64, Vec<String>>);

fn merge_argv(mut world: bevy_ecs::world::DeferredWorld, ctx: bevy_ecs::lifecycle::HookContext) {
    let pending = world
        .get_mut::<RunArgvInsert>(ctx.entity)
        .map(|mut p| core::mem::take(&mut p.0))
        .unwrap_or_default();
    world.commands().entity(ctx.entity).remove::<RunArgvInsert>();
    match world.get_mut::<RunArgv>(ctx.entity) {
        Some(mut argv) => argv.0.extend(pending),
        None => {
            world
                .commands()
                .entity(ctx.entity)
                .insert(RunArgv(pending));
        }
    }
}

/// Emits `Effect::RunPlugin` for a recorded run with the full plugin environment.
fn spawn_run(world: &mut World, plugin: Entity, run: u64, fux_token: Option<String>) {
    let name = name_of(world, plugin);
    let paths = world.resource::<PluginPaths>().clone();
    let argv = world
        .get_mut::<RunArgv>(plugin)
        .and_then(|mut a| a.0.remove(&run))
        .unwrap_or_default();
    let Some(record) = world.get::<Runs>(plugin).and_then(|r| r.0.iter().find(|r| r.id == run).cloned()) else {
        return;
    };
    let mut env = base_env(world, plugin, &name);
    env.push(("ZOR_PLUGIN_RUN".into(), run.to_string()));
    let kind = match &record.kind {
        RunKind::Build => "build",
        RunKind::Startup => "startup",
        RunKind::Hook => "hook",
        RunKind::Action(id) => {
            env.push(("ZOR_PLUGIN_ACTION".into(), id.clone()));
            "action"
        }
        RunKind::Surface(id) => {
            env.push(("ZOR_PLUGIN_PANE".into(), id.clone()));
            "surface"
        }
        RunKind::Link(id) => {
            env.push(("ZOR_PLUGIN_LINK_ID".into(), id.clone()));
            "link"
        }
    };
    env.push(("ZOR_PLUGIN_KIND".into(), kind.into()));
    if let Some(task) = &record.task {
        env.push(("ZOR_PLUGIN_TASK".into(), task.clone()));
    }
    if let Some(workspace) = &record.workspace {
        env.push(("ZOR_PLUGIN_WORKSPACE".into(), workspace.clone()));
    }
    if let Some(surface) = record.surface {
        env.push(("ZOR_PLUGIN_SURFACE".into(), surface.to_string()));
    }
    if let Some(link) = &record.link {
        env.push(("ZOR_PLUGIN_LINK".into(), link.clone()));
    }
    if record.kind == RunKind::Hook {
        let (cursors, hooks) = world
            .get::<Hook>(plugin)
            .map(|h| h.cursors)
            .zip(world.get::<Loaded>(plugin).map(|l| l.manifest.events.clone()))
            .unwrap_or_default();
        env.push(("ZOR_PLUGIN_CURSOR_ZOR".into(), cursors.zor.to_string()));
        env.push(("ZOR_PLUGIN_CURSOR_FUX".into(), cursors.fux.to_string()));
        env.push((
            "ZOR_PLUGIN_HOOKS".into(),
            serde_json::to_string(&hooks).unwrap_or_else(|_| "[]".into()),
        ));
    }
    if let Some(token) = fux_token {
        match write_fux_descriptor(world, &name, run, &token) {
            Ok(path) => {
                env.push(("FUX_BRP".into(), path.display().to_string()));
                env.push(("FUX_TOKEN".into(), token));
            }
            Err(e) => {
                let _ = host::append_line(
                    &paths.log(&name),
                    &format!("[{run}] "),
                    format!("zor: fux descriptor: {e}").as_bytes(),
                );
            }
        }
    }
    let cwd = world.get::<Loaded>(plugin).map(|l| l.root.display().to_string());
    if let Some(mut runs) = world.get_mut::<Runs>(plugin)
        && let Some(r) = runs.get_mut(run)
    {
        r.state = RunState::Running;
    }
    world.write_message(Effect::RunPlugin {
        plugin,
        run,
        argv,
        cwd,
        env,
        log: paths.log(&name),
    });
}

/// The environment every plugin process gets.
fn base_env(world: &World, plugin: Entity, name: &str) -> Vec<(String, String)> {
    let paths = world.resource::<PluginPaths>();
    let instance = world.resource::<ServerInstance>();
    let mut env = vec![
        ("ZOR_BRP".into(), paths.descriptor(name).display().to_string()),
        ("ZOR_INSTANCE".into(), instance.nonce.clone()),
        ("ZOR_SERVER".into(), instance.name.clone()),
        ("ZOR_PLUGIN_NAME".into(), name.to_owned()),
        (
            "ZOR_PLUGIN_STATE_DIR".into(),
            paths.state_dir(name).display().to_string(),
        ),
        ("ZOR_PLUGIN_LOG".into(), paths.log(name).display().to_string()),
    ];
    if let Some(active) = world.get::<Active>(plugin) {
        env.push(("ZOR_TOKEN".into(), active.token.clone()));
    }
    if let Some(loaded) = world.get::<Loaded>(plugin) {
        env.push(("ZOR_PLUGIN_ROOT".into(), loaded.root.display().to_string()));
        env.push(("ZOR_PLUGIN_VERSION".into(), loaded.manifest.version.clone()));
    }
    env
}

/// fux's descriptor with the minted token, under the plugin's run directory.
fn write_fux_descriptor(world: &World, name: &str, run: u64, token: &str) -> Result<PathBuf, String> {
    let fux = world
        .get_resource::<FuxDescriptor>()
        .ok_or_else(|| "fux is not configured".to_owned())?;
    let descriptor = read_descriptor(&fux.0).map_err(|e| e.to_string())?;
    let path = world.resource::<PluginPaths>().fux_descriptor(name, run);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_descriptor(
        &path,
        &Descriptor {
            token: token.to_owned(),
            ..descriptor
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(path)
}

fn fux_call(world: &mut World, pending: Pending, method: &str, params: Value) -> u64 {
    let call = {
        let mut host = world.resource_mut::<Host>();
        host.next_call += 1;
        let call = CALL_TAG | (host.next_call & !TAG_MASK);
        host.calls.insert(call, pending);
        call
    };
    world.write_message(Effect::FuxCall {
        call,
        method: method.into(),
        params,
    });
    call
}

// ---------------------------------------------------------------------------------------------
// Pane compositions
// ---------------------------------------------------------------------------------------------

fn open_composition(
    world: &mut World,
    plugin: Entity,
    spec: manifest::Pane,
    workspace: String,
    target: Option<u64>,
    task: Option<String>,
) -> Res<u64> {
    if world
        .get_resource::<FuxDescriptor>()
        .is_none_or(|d| !d.0.is_file())
    {
        return refused("fux is not configured; panes need a fux server");
    }
    if spec.kind == PaneKind::Surface && !matches!(spec.placement, Placement::Overlay | Placement::Popup) {
        return refused("surface panes use the overlay or popup placement");
    }
    let id = {
        let mut host = world.resource_mut::<Host>();
        host.next_run += 1;
        host.next_run
    };
    let Some(mut panes) = world.get_mut::<Panes>(plugin) else {
        return Err(PluginError::NotFound(format!("plugin {plugin}")));
    };
    while panes.0.len() >= MAX_RUNS_RETAINED {
        panes.0.pop_front();
    }
    panes.0.push_back(OpenPane {
        id,
        pane: spec.id.clone(),
        workspace: workspace.clone(),
        placement: spec.placement,
        kind: spec.kind,
        state: PaneState::Opening,
        target,
        fux_token: None,
        fux_brp: None,
        task,
    });
    world.entity_mut(plugin).insert(RunArgv::insert(id, spec.command));
    fux_call(
        world,
        Pending::PaneMint {
            plugin,
            opening: id,
        },
        "fux/token.mint",
        json!({ "workspace": workspace, "capabilities": ["read", "mutate"] }),
    );
    Ok(id)
}

fn pane_mut(world: &mut World, plugin: Entity, opening: u64) -> Option<Mut<'_, Panes>> {
    world
        .get_mut::<Panes>(plugin)
        .filter(|p| p.0.iter().any(|o| o.id == opening))
}

fn fail_pane(world: &mut World, plugin: Entity, opening: u64, problem: String) {
    if let Some(mut panes) = pane_mut(world, plugin, opening)
        && let Some(open) = panes.0.iter_mut().find(|o| o.id == opening)
    {
        open.state = PaneState::Failed { problem };
    }
    world.entity_mut(plugin).entry::<RunArgv>().and_modify(|mut a| {
        a.0.remove(&opening);
    });
}

/// The template for a terminal pane: the command with the plugin environment.
fn pane_template(world: &mut World, plugin: Entity, opening: u64) -> Value {
    let name = name_of(world, plugin);
    let argv = world
        .get::<RunArgv>(plugin)
        .and_then(|a| a.0.get(&opening).cloned())
        .unwrap_or_default();
    let open = world
        .get::<Panes>(plugin)
        .and_then(|p| p.0.iter().find(|o| o.id == opening).cloned());
    let mut env = base_env(world, plugin, &name);
    env.push(("ZOR_PLUGIN_KIND".into(), "pane".into()));
    if let Some(open) = &open {
        env.push(("ZOR_PLUGIN_PANE".into(), open.pane.clone()));
        env.push(("ZOR_PLUGIN_WORKSPACE".into(), open.workspace.clone()));
        if let Some(task) = &open.task {
            env.push(("ZOR_PLUGIN_TASK".into(), task.clone()));
        }
        if let (Some(token), Some(brp)) = (&open.fux_token, &open.fux_brp) {
            env.push(("FUX_TOKEN".into(), token.clone()));
            env.push(("FUX_BRP".into(), brp.display().to_string()));
        }
    }
    let cwd = world.get::<Loaded>(plugin).map(|l| l.root.display().to_string());
    json!({ "argv": argv, "cwd": cwd, "env": env, "stream": format!("{name}-{opening}") })
}

fn absolute_patch(placement: Placement) -> Value {
    match placement {
        Placement::Popup => json!({
            "position_type": "absolute",
            "left": "25%", "top": "25%", "width": "50%", "height": "50%", "z_index": 10,
        }),
        _ => json!({
            "position_type": "absolute",
            "left": "0%", "top": "0%", "width": "100%", "height": "100%", "z_index": 10,
        }),
    }
}

/// One step of a pane composition after `result` of the previous call.
fn pane_step(world: &mut World, pending: Pending, result: Result<Value, String>) {
    let (plugin, opening) = match pending {
        Pending::PaneMint { plugin, opening }
        | Pending::PaneList { plugin, opening }
        | Pending::PaneCreate { plugin, opening }
        | Pending::PaneViewers { plugin, opening }
        | Pending::PaneSurface { plugin, opening } => (plugin, opening),
        Pending::Mint { .. } | Pending::Ignore => return,
    };
    let value = match result {
        Ok(value) => value,
        Err(e) => return fail_pane(world, plugin, opening, e),
    };
    let Some(open) = world
        .get::<Panes>(plugin)
        .and_then(|p| p.0.iter().find(|o| o.id == opening).cloned())
    else {
        return;
    };
    let name = name_of(world, plugin);
    match pending {
        Pending::PaneMint { .. } => {
            let token = value
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match write_fux_descriptor(world, &name, opening, &token) {
                Ok(path) => {
                    if let Some(mut panes) = pane_mut(world, plugin, opening)
                        && let Some(o) = panes.0.iter_mut().find(|o| o.id == opening)
                    {
                        o.fux_token = Some(token);
                        o.fux_brp = Some(path);
                    }
                }
                Err(e) => return fail_pane(world, plugin, opening, e),
            }
            fux_call(
                world,
                Pending::PaneList { plugin, opening },
                "fux/workspace.list",
                json!({}),
            );
        }
        Pending::PaneList { .. } => {
            let list: WorkspaceList = match serde_json::from_value(value) {
                Ok(list) => list,
                Err(e) => return fail_pane(world, plugin, opening, e.to_string()),
            };
            let Some(ws) = list.workspaces.iter().find(|w| w.name == open.workspace) else {
                return fail_pane(world, plugin, opening, format!("workspace {} not found", open.workspace));
            };
            let (root, generation, first_pane) = ws
                .roots
                .first()
                .map(|r| (r.id, r.generation, r.panes.first().map(|p| p.id)))
                .unwrap_or((0, 0, None));
            let target = open.target.or(first_pane);
            let template = if open.kind == PaneKind::Terminal {
                Some(pane_template(world, plugin, opening))
            } else {
                None
            };
            let (method, params) = match open.placement {
                Placement::Split | Placement::Zoomed => {
                    let Some(target) = target else {
                        return fail_pane(world, plugin, opening, "no pane to split".into());
                    };
                    (
                        "fux/pane.new",
                        json!({ "split": target, "direction": "Right", "template": template }),
                    )
                }
                Placement::Tab => (
                    "fux/root.new",
                    json!({ "workspace": open.workspace, "name": format!("{name}-{}", open.pane), "template": template }),
                ),
                Placement::Overlay | Placement::Popup | Placement::None => {
                    if ws.roots.is_empty() {
                        return fail_pane(world, plugin, opening, "workspace has no root".into());
                    }
                    (
                        "fux/node.spawn",
                        json!({
                            "generation": generation,
                            "parent": root,
                            "patch": absolute_patch(open.placement),
                            "template": template,
                        }),
                    )
                }
            };
            fux_call(world, Pending::PaneCreate { plugin, opening }, method, params);
        }
        Pending::PaneCreate { .. } => {
            let (node, pane, generation) = match open.placement {
                Placement::Split | Placement::Zoomed => match serde_json::from_value::<PaneCreated>(value) {
                    Ok(c) => (c.node, Some(c.pane), c.generation),
                    Err(e) => return fail_pane(world, plugin, opening, e.to_string()),
                },
                Placement::Tab => match serde_json::from_value::<RootCreated>(value) {
                    Ok(c) => (c.root, Some(c.pane), c.generation),
                    Err(e) => return fail_pane(world, plugin, opening, e.to_string()),
                },
                _ => match serde_json::from_value::<NodeSpawned>(value) {
                    Ok(c) => (c.node, c.pane, c.generation),
                    Err(e) => return fail_pane(world, plugin, opening, e.to_string()),
                },
            };
            world.entity_mut(plugin).entry::<RunArgv>().and_modify(|mut a| {
                if open.kind == PaneKind::Terminal {
                    a.0.remove(&opening);
                }
            });
            set_open(world, plugin, opening, node, pane, None);
            match (open.kind, open.placement) {
                (PaneKind::Surface, _) => {
                    fux_call(
                        world,
                        Pending::PaneSurface { plugin, opening },
                        "fux/surface.open",
                        json!({ "workspace": open.workspace, "node": node, "provider": format!("plugin:{name}"), "generation": generation }),
                    );
                }
                (_, Placement::Zoomed) => {
                    fux_call(
                        world,
                        Pending::PaneViewers { plugin, opening },
                        "fux/viewer.list",
                        json!({}),
                    );
                }
                _ => {}
            }
        }
        Pending::PaneViewers { .. } => {
            let Ok(list) = serde_json::from_value::<ViewerList>(value) else {
                return;
            };
            let PaneState::Open { node, .. } = open.state else {
                return;
            };
            let viewers: Vec<u64> = list
                .viewers
                .iter()
                .filter(|v| v.workspace == open.workspace)
                .map(|v| v.id)
                .collect();
            for viewer in viewers {
                fux_call(
                    world,
                    Pending::Ignore,
                    "fux/viewer.zoom",
                    json!({ "viewer": viewer, "node": node }),
                );
            }
        }
        Pending::PaneSurface { .. } => {
            let surface = match serde_json::from_value::<SurfaceOpened>(value) {
                Ok(s) => s.surface,
                Err(e) => return fail_pane(world, plugin, opening, e.to_string()),
            };
            let PaneState::Open { node, pane, .. } = open.state else {
                return;
            };
            set_open(world, plugin, opening, node, pane, Some(surface));
            let argv = world
                .get_mut::<RunArgv>(plugin)
                .and_then(|mut a| a.0.remove(&opening))
                .unwrap_or_default();
            let ctx = LinkContext {
                ctx: RunContext {
                    task: open.task.clone(),
                    workspace: Some(open.workspace.clone()),
                },
                link: None,
                surface: Some(surface),
            };
            if let Err(e) = start_run(world, plugin, RunKind::Surface(open.pane.clone()), argv, ctx) {
                fail_pane(world, plugin, opening, e.to_string());
            }
        }
        Pending::Mint { .. } | Pending::Ignore => {}
    }
}

fn set_open(world: &mut World, plugin: Entity, opening: u64, node: u64, pane: Option<u64>, surface: Option<u64>) {
    if let Some(mut panes) = pane_mut(world, plugin, opening)
        && let Some(o) = panes.0.iter_mut().find(|o| o.id == opening)
    {
        o.state = PaneState::Open { node, pane, surface };
    }
}

// ---------------------------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------------------------

fn collect_inbound(mut inbound: MessageReader<Inbound>, mut host: ResMut<Host>) {
    for message in inbound.read() {
        match message {
            Inbound::PluginExited { plugin, run, code } => host.inbox.push(Item::Exited {
                plugin: *plugin,
                run: *run,
                code: *code,
            }),
            Inbound::FuxReply { call, result } if call & TAG_MASK == CALL_TAG => {
                host.inbox.push(Item::Reply {
                    call: *call,
                    result: result.clone(),
                });
            }
            _ => {}
        }
    }
}

fn ingest(world: &mut World) {
    let items = core::mem::take(&mut world.resource_mut::<Host>().inbox);
    for item in items {
        match item {
            Item::Exited { plugin, run, code } => on_exit(world, plugin, run, code),
            Item::Reply { call, result } => {
                let Some(pending) = world.resource_mut::<Host>().calls.remove(&call) else {
                    continue;
                };
                match pending {
                    Pending::Mint { plugin, run } => {
                        let token = match result {
                            Ok(value) => value
                                .get("token")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            Err(e) => {
                                let name = name_of(world, plugin);
                                let log = world.resource::<PluginPaths>().log(&name);
                                let _ = host::append_line(
                                    &log,
                                    &format!("[{run}] "),
                                    format!("zor: fux token not minted: {e}").as_bytes(),
                                );
                                None
                            }
                        };
                        spawn_run(world, plugin, run, token);
                    }
                    Pending::Ignore => {}
                    other => pane_step(world, other, result),
                }
            }
        }
    }
}

fn on_exit(world: &mut World, plugin: Entity, run: u64, code: Option<i32>) {
    let ms = now(world);
    let name = name_of(world, plugin);
    let paths = world.resource::<PluginPaths>().clone();
    let _ = std::fs::remove_file(paths.fux_descriptor(&name, run));
    let Some(record) = world.get_mut::<Runs>(plugin).and_then(|mut runs| {
        runs.get_mut(run).map(|r| {
            r.state = RunState::Exited { code, ms };
            r.clone()
        })
    }) else {
        return;
    };
    match record.kind {
        RunKind::Hook => {
            let enabled = world.get::<Active>(plugin).is_some();
            if let Some(mut hook) = world.get_mut::<Hook>(plugin)
                && hook.run == Some(run)
            {
                hook.run = None;
                if enabled {
                    let backoff = (HOOK_BACKOFF_MIN_MS << hook.restarts.min(16)).min(HOOK_BACKOFF_MAX_MS);
                    hook.restarts = hook.restarts.saturating_add(1);
                    hook.next_ms = ms + backoff;
                }
            }
        }
        RunKind::Build | RunKind::Startup => {
            if code != Some(0) {
                let what = if record.kind == RunKind::Build {
                    "build"
                } else {
                    "startup"
                };
                let problem = match code {
                    Some(code) => format!("{what} exited {code}"),
                    None => format!("{what} exited by signal"),
                };
                world.entity_mut(plugin).insert(PluginProblem(problem));
                deactivate(world, plugin);
            }
        }
        RunKind::Surface(_) => {
            if let Some(surface) = record.surface {
                fux_call(
                    world,
                    Pending::Ignore,
                    "fux/surface.close",
                    json!({ "surface": surface }),
                );
            }
        }
        RunKind::Action(_) | RunKind::Link(_) => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Cursors, bindings, records, logs
// ---------------------------------------------------------------------------------------------

fn read_cursors(path: &Path) -> HookCursors {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The hook process reports the last cursor it processed on either stream; persisted at once.
pub fn set_cursor(world: &mut World, plugin: Entity, zor: Option<u64>, fux: Option<u64>) -> Res<HookCursors> {
    check_plugin(world, plugin)?;
    let Some(mut hook) = world.get_mut::<Hook>(plugin) else {
        return Err(PluginError::NotFound("hook".into()));
    };
    if let Some(zor) = zor {
        hook.cursors.zor = hook.cursors.zor.max(zor);
    }
    if let Some(fux) = fux {
        hook.cursors.fux = hook.cursors.fux.max(fux);
    }
    let cursors = hook.cursors;
    let name = name_of(world, plugin);
    let path = world.resource::<PluginPaths>().cursors(&name);
    write_cursors(&path, &cursors).map_err(PluginError::Io)?;
    Ok(cursors)
}

fn write_cursors(path: &Path, cursors: &HookCursors) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(cursors).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Every keybinding of every enabled plugin: `{chord: <plugin>__<action>}` for the manual
/// merge into fux's `[bindings]`.
pub fn bindings(world: &mut World) -> Vec<Binding> {
    let mut out: Vec<Binding> = world
        .query_filtered::<(&PluginId, &Loaded), With<PluginEnabled>>()
        .iter(world)
        .flat_map(|(id, loaded)| {
            loaded.manifest.actions.iter().filter_map(|a| {
                a.keybinding.as_ref().map(|chord| Binding {
                    chord: chord.clone(),
                    plugin: id.0.clone(),
                    action: a.id.clone(),
                })
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.chord, &a.plugin, &a.action).cmp(&(&b.chord, &b.plugin, &b.action)));
    out
}

pub fn records(world: &mut World) -> Vec<PluginRecord> {
    let plugins: Vec<Entity> = world
        .query_filtered::<Entity, With<HostedPlugin>>()
        .iter(world)
        .collect();
    let mut out: Vec<PluginRecord> = plugins.into_iter().map(|p| record(world, p)).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn record(world: &World, plugin: Entity) -> PluginRecord {
    let name = name_of(world, plugin);
    let paths = world.resource::<PluginPaths>();
    let manifest = world.get::<PluginManifest>(plugin);
    let path = manifest.map(|m| m.path.clone()).unwrap_or_default();
    let loaded = world.get::<Loaded>(plugin);
    let actions = world
        .get::<Actions>(plugin)
        .map(|actions| {
            let mut rows: Vec<ActionRecord> = actions
                .iter()
                .filter_map(|e| {
                    let id = world.get::<PluginActionId>(e)?;
                    let spec = world.get::<ActionSpec>(e)?;
                    Some(ActionRecord {
                        id: spec.0.id.clone(),
                        qualified: id.0.clone(),
                        title: spec.0.title.clone(),
                        keybinding: spec.0.keybinding.clone(),
                        placement: spec.0.placement.name().into(),
                    })
                })
                .collect();
            rows.sort_by(|a, b| a.id.cmp(&b.id));
            rows
        })
        .unwrap_or_default();
    PluginRecord {
        name: name.clone(),
        version: loaded
            .map(|l| l.manifest.version.clone())
            .or_else(|| manifest.map(|m| m.version.clone()))
            .unwrap_or_default(),
        description: loaded.and_then(|l| l.manifest.description.clone()),
        source: if Path::new(&path).starts_with(paths.install_dir(&name)) {
            "installed".into()
        } else {
            "linked".into()
        },
        path,
        enabled: world.get::<PluginEnabled>(plugin).is_some(),
        active: world.get::<Active>(plugin).is_some(),
        problem: world.get::<PluginProblem>(plugin).map(|p| p.0.clone()),
        actions,
        events: loaded
            .map(|l| l.manifest.events.iter().map(|e| e.pattern.clone()).collect())
            .unwrap_or_default(),
        panes: loaded
            .map(|l| l.manifest.panes.iter().map(|p| p.id.clone()).collect())
            .unwrap_or_default(),
        links: loaded
            .map(|l| l.manifest.links.iter().map(|l| l.id.clone()).collect())
            .unwrap_or_default(),
        hook: world.get::<Hook>(plugin).map(|h| HookRecord {
            run: h.run,
            cursor_zor: h.cursors.zor,
            cursor_fux: h.cursors.fux,
            restarts: h.restarts,
        }),
        runs: world
            .get::<Runs>(plugin)
            .map(|r| r.0.iter().cloned().collect())
            .unwrap_or_default(),
        open_panes: world
            .get::<Panes>(plugin)
            .map(|p| p.0.iter().cloned().collect())
            .unwrap_or_default(),
    }
}

/// The last `limit` log lines of a plugin.
pub fn logs(world: &World, plugin: Entity, limit: usize) -> Res<Vec<String>> {
    check_plugin(world, plugin)?;
    let name = name_of(world, plugin);
    let log = world.resource::<PluginPaths>().log(&name);
    Ok(host::read_tail(&log, limit.clamp(1, 1000)))
}

/// The plugin an action entity belongs to.
pub fn plugin_of_action(world: &World, action: Entity) -> Option<Entity> {
    world.get::<ActionOf>(action).map(|a| a.0)
}
