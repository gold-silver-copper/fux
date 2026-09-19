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
//! and `fux/events+watch` from incarnation-scoped cursors. It durably claims each event
//! through `zor/plugin.cursor` before matching commands; a crash can omit work, never
//! authorizes replay of claimed side effects. Hook exit triggers bounded restart backoff.
//!
//! Placements over `node.*`/`root.*`: `split` = `fux/pane.new` beside the target pane;
//! `tab` = `fux/root.new`; `zoomed` = `split` then `fux/viewer.zoom` for every viewer of the
//! workspace; `overlay` = `fux/node.spawn` of an absolute node covering the root; `popup` =
//! the same node centred at half the root's size.

pub mod hooks;
pub mod host;
pub mod manifest;
pub mod loading;

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
use crate::remote::DescriptorGuard;

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
    session: String,
}

impl PluginPaths {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(PLUGINS_DIR),
            zor_bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zor")),
            session: String::new(),
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
    /// Activation-specific descriptor: old children never inherit a newly minted grant.
    pub fn descriptor(&self, name: &str, token: &str) -> PathBuf {
        self.dir(name).join(format!("{token}.zor.brp.json"))
    }
    /// The per-run fux descriptor (`FUX_BRP`).
    pub fn fux_descriptor(&self, name: &str, run: u64) -> PathBuf {
        self.dir(name).join("runs").join(format!("{}-{run}.fux.brp.json", self.session))
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
    /// Effects await a successful journal commit; messages alone are not durable.
    effects: VecDeque<(u64, Effect)>,
    fux_instance: Option<String>,
    observed_fux_instance: Option<String>,
}

enum Item {
    Link(Option<String>),
    Gap,
    PaneClosed(u64),
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
    Retire { path: PathBuf },
    /// `fux/token.mint` for a run; the reply spawns it.
    Mint { plugin: Entity, run: u64 },
    /// `fux/token.mint` for a pane composition.
    PaneMint { plugin: Entity, opening: u64 },
    PaneList { plugin: Entity, opening: u64 },
    PaneCreate { plugin: Entity, opening: u64 },
    PaneViewers { plugin: Entity, opening: u64 },
    PaneSurface { plugin: Entity, opening: u64 },
    PaneClose { plugin: Entity, opening: u64, node: u64 },
    PaneRemoved { plugin: Entity, opening: u64 },
    ReconcilePanes { openings: Vec<(Entity, u64)> },
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

/// Unregistration waits for owned in-flight replies, so late creations can be cleaned up.
#[derive(Component, Reflect, Clone, Copy, Debug, Default)]
#[reflect(Component)]
pub struct Uninstalling;

/// Last accepted external dispatch. Recovery exposes this intent but never replays it:
/// absence of a completion is uncertainty, not permission to repeat a side effect.
#[derive(Component, Reflect, Clone, Debug, Default)]
#[reflect(Component)]
pub struct DispatchIntent {
    pub serial: u64,
    pub description: String,
}

#[derive(Component, Default)]
struct RunTokens(HashMap<u64, String>);

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
    pub descriptor: PathBuf,
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HookCursors {
    pub zor: u64,
    pub fux: u64,
    pub zor_instance: String,
    pub fux_instance: String,
}

#[derive(Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "entry")]
pub enum RunKind {
    Build,
    Startup,
    Hook,
    Action(String),
    /// The driver process of a surface pane.
    Surface(String),
    Link(String),
}

#[derive(Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Waiting for `fux/token.mint`.
    Minting,
    Running,
    /// Cancellation requested; process exit has not yet been observed.
    Stopping,
    /// Recovered intent whose external completion cannot be established; never replayed.
    Uncertain,
    Exited { code: Option<i32>, ms: u64 },
}

/// One process run (bounded history in [`Runs`]).
#[derive(Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Component, Reflect, Clone, Debug, Default)]
#[reflect(Component)]
pub struct Runs(pub VecDeque<Run>);

impl Runs {
    fn get_mut(&mut self, id: u64) -> Option<&mut Run> {
        self.0.iter_mut().find(|r| r.id == id)
    }

    fn push(&mut self, run: Run) {
        if self.0.len() >= MAX_RUNS_RETAINED
            && let Some(index) = self.0.iter().position(|r| matches!(r.state, RunState::Exited { .. }))
        {
            self.0.remove(index);
        }
        self.0.push_back(run);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum PaneState {
    Opening,
    Closed,
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
    #[serde(skip)]
    cleanup_state: Option<PaneState>,
    #[serde(skip)]
    generation: Option<u64>,
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
            .register_type::<DispatchIntent>()
            .register_type::<Runs>()
            .register_type::<Uninstalling>()
            .insert_resource(PluginPaths::new(&self.state_dir))
            .init_resource::<Host>()
            .init_resource::<loading::Loading>()
            .add_systems(PostStartup, reload_manifests)
            .add_systems(
                PreUpdate,
                (loading::collect, collect_inbound, ingest)
                    .chain()
                    .in_set(Phase::Completions),
            )
            .add_systems(
                Update,
                (activate, restart_hooks, flush_effects, finish_uninstall).chain().in_set(Phase::Lifecycle),
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
pub fn install(world: &mut World, source: &Path, enabled: bool) -> Res<u64> {
    loading::submit(world, source.to_path_buf(), enabled, true)
}

/// Registers the plugin at `dir` in place (development); the manifest is re-read at every
/// server start. Runs `build` when declared.
pub fn link(world: &mut World, dir: &Path, enabled: bool) -> Res<u64> {
    if !dir.is_absolute() { return refused("link path must be absolute"); }
    loading::submit(world, dir.to_path_buf(), enabled, false)
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


fn copy_tree(from: &Path, to: &Path, copied: &mut u64) -> Result<(), String> {
    copy_tree_bounded(from, to, copied, &mut 0, 0)
}

fn copy_tree_bounded(from: &Path, to: &Path, copied: &mut u64, entries_seen: &mut usize, depth: usize) -> Result<(), String> {
    if depth > 32 { return Err("plugin directory nesting exceeds 32".into()); }
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        *entries_seen += 1;
        if *entries_seen > 4096 { return Err("plugin contains more than 4096 entries".into()); }
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
            copy_tree_bounded(&path, &dest, copied, entries_seen, depth + 1)?;
        } else {
            if !meta.is_file() { return Err("plugin contains a non-regular file".into()); }
            use std::io::Read;
            let remaining = MAX_INSTALL_BYTES.saturating_sub(*copied);
            let input = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut output = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            let bytes = std::io::copy(&mut input.take(remaining + 1), &mut output).map_err(|e| e.to_string())?;
            *copied += bytes;
            if *copied > MAX_INSTALL_BYTES { return Err(format!("plugin larger than {MAX_INSTALL_BYTES} bytes")); }
            std::fs::set_permissions(&dest, meta.permissions()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Creates or updates the entities for `manifest` and marks the journal.
fn register(world: &mut World, manifest: Manifest, file: PathBuf, enabled: bool, cursors: HookCursors) -> Res<Entity> {
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
            if world.get::<Uninstalling>(plugin).is_some() { return refused("plugin is uninstalling"); }
            deactivate(world, plugin);
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
    let session = world.resource::<ServerInstance>().nonce.clone();
    world.resource_mut::<PluginPaths>().session = session;
    let plugins: Vec<(Entity, String)> = world
        .query_filtered::<(Entity, &PluginManifest), With<HostedPlugin>>()
        .iter(world)
        .map(|(e, m)| (e, m.path.clone()))
        .collect();
    for (plugin, path) in plugins {
        let cursors = HookCursors::default();
        let runs = world.get::<Runs>(plugin).cloned().unwrap_or_default();
        let mut entity = world.entity_mut(plugin);
        entity.insert((
            Hook {
                cursors,
                ..Default::default()
            },
            runs,
            Panes::default(),
        ));
        if let Some(mut runs) = world.get_mut::<Runs>(plugin) {
            for run in &mut runs.0 {
                if matches!(run.state, RunState::Minting | RunState::Running | RunState::Stopping) {
                    run.state = RunState::Uncertain;
                }
            }
        }
        world.resource_mut::<Journal>().mark_dirty();
        let last = world.get::<Runs>(plugin).and_then(|runs| runs.0.iter().map(|r| r.id).max()).unwrap_or(0);
        let last = world.resource::<Host>().next_run.max(last);
        world.resource_mut::<Host>().next_run = last;
        loading::reload(world, plugin, PathBuf::from(path));
    }
}

// ---------------------------------------------------------------------------------------------
// Enable / disable
// ---------------------------------------------------------------------------------------------

pub fn enable(world: &mut World, plugin: Entity) -> Res<()> {
    check_plugin(world, plugin)?;
    if world.get::<Uninstalling>(plugin).is_some() { return refused("plugin is uninstalling"); }
    world.entity_mut(plugin).insert(PluginEnabled);
    world.resource_mut::<Journal>().mark_dirty();
    Ok(())
}

/// Disabling revokes grants and stops every owned child, including pending launches.
pub fn disable(world: &mut World, plugin: Entity) -> Res<()> {
    check_plugin(world, plugin)?;
    world.entity_mut(plugin).remove::<PluginEnabled>();
    world.resource_mut::<Journal>().mark_dirty();
    deactivate(world, plugin);
    Ok(())
}

/// Unregister after bounded owned cleanup. Durable state and linked source are preserved.
pub fn uninstall(world: &mut World, plugin: Entity) -> Res<()> {
    disable(world, plugin)?;
    world.entity_mut(plugin).insert(Uninstalling);
    Ok(())
}

fn finish_uninstall(world: &mut World) {
    let ready: Vec<_> = world.query_filtered::<Entity, With<Uninstalling>>().iter(world)
        .filter(|plugin| world.get::<Runs>(*plugin).is_none_or(|runs| runs.0.iter().all(|r| r.state != RunState::Stopping)))
        .filter(|plugin| !world.resource::<Host>().calls.values().any(|pending| match pending {
            Pending::Mint { plugin: owner, .. } | Pending::PaneMint { plugin: owner, .. } |
            Pending::PaneList { plugin: owner, .. } | Pending::PaneCreate { plugin: owner, .. } |
            Pending::PaneViewers { plugin: owner, .. } | Pending::PaneSurface { plugin: owner, .. } => owner == plugin,
            Pending::Ignore | Pending::PaneClose { .. } | Pending::PaneRemoved { .. } | Pending::ReconcilePanes { .. } | Pending::Retire { .. } => false,
        })).collect();
    for plugin in ready {
        let actions = world.get::<Actions>(plugin).map(|a| a.iter().collect::<Vec<_>>()).unwrap_or_default();
        for action in actions { world.despawn(action); }
        world.despawn(plugin);
        world.resource_mut::<Journal>().mark_dirty();
    }
}

fn check_plugin(world: &World, plugin: Entity) -> Res<()> {
    if world.get::<HostedPlugin>(plugin).is_none() {
        return Err(PluginError::NotFound(format!("plugin {plugin}")));
    }
    Ok(())
}

fn deactivate(world: &mut World, plugin: Entity) {
    if let Some(active) = world.entity_mut(plugin).take::<Active>() {
        world.resource_mut::<Tokens>().revoke(&active.token);
        crate::remote::watch::revoke(world, &active.token);
        let _ = std::fs::remove_file(active.descriptor);
    }
    let ms = now(world);
    world.resource_mut::<Journal>().mark_dirty();
    let queued_runs: Vec<_> = world.resource::<Host>().effects.iter().filter_map(|(_, effect)| {
        match effect {
            Effect::RunPlugin { plugin: owner, run, .. } if *owner == plugin => Some(*run),
            _ => None,
        }
    }).collect();
    let mut kills = Vec::new();
    if let Some(mut runs) = world.get_mut::<Runs>(plugin) {
        for run in &mut runs.0 {
            if matches!(run.state, RunState::Minting | RunState::Running) {
                kills.push(run.id);
                run.state = if run.state == RunState::Minting || queued_runs.contains(&run.id) {
                    RunState::Exited { code: None, ms }
                } else { RunState::Stopping };
            }
        }
    }
    if let Some(mut argv) = world.get_mut::<RunArgv>(plugin) {
        argv.0.clear();
    }
    if let Some(mut hook) = world.get_mut::<Hook>(plugin) {
        hook.run = None;
    }
    let queued_calls: Vec<u64> = {
        let host = world.resource::<Host>();
        host.effects.iter().filter_map(|(_, effect)| {
            let Effect::FuxCall { call, .. } = effect else { return None };
            let owned = match host.calls.get(call) {
                Some(Pending::Mint { plugin: owner, .. } | Pending::PaneMint { plugin: owner, .. } |
                    Pending::PaneList { plugin: owner, .. } | Pending::PaneCreate { plugin: owner, .. } |
                    Pending::PaneViewers { plugin: owner, .. } | Pending::PaneSurface { plugin: owner, .. }) => *owner == plugin,
                _ => false,
            };
            owned.then_some(*call)
        }).collect()
    };
    let mut host = world.resource_mut::<Host>();
    host.effects.retain(|(_, effect)| match effect {
        Effect::RunPlugin { plugin: owner, .. } => *owner != plugin,
        Effect::FuxCall { call, .. } => !queued_calls.contains(call),
        _ => true,
    });
    for call in queued_calls { host.calls.remove(&call); }
    for run in kills {
        world.write_message(Effect::KillPlugin { plugin, run });
        retire_run_token(world, plugin, run);
    }
    let panes = world.get::<Panes>(plugin).map(|p| p.0.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
    for pane in panes {
        fail_pane(world, plugin, pane.id, "plugin disabled".into());
    }
}

fn revoke_fux_token(world: &mut World, token: String) {
    fux_call(world, Pending::Ignore, "fux/token.revoke", json!({"revoke": token}));
}

fn retire_run_token(world: &mut World, plugin: Entity, run: u64) {
    if let Some(token) = world.get_mut::<RunTokens>(plugin).and_then(|mut t| t.0.remove(&run)) {
        let name = name_of(world, plugin);
        let path = world.resource::<PluginPaths>().fux_descriptor(&name, run);
        fux_call(world, Pending::Retire { path }, "fux/token.revoke", json!({"revoke": token}));
    }
}

fn queue_effect(world: &mut World, plugin: Entity, description: &str, effect: Effect) {
    let generation = world.resource::<crate::model::Generation>().0;
    let serial = world.get::<DispatchIntent>(plugin).map_or(1, |i| i.serial.saturating_add(1));
    world.entity_mut(plugin).insert(DispatchIntent { serial, description: description.into() });
    world.resource_mut::<Journal>().mark_dirty();
    world.resource_mut::<Host>().effects.push_back((generation, effect));
}

fn flush_effects(world: &mut World) {
    if shutting_down(world) {
        world.resource_mut::<Host>().effects.clear();
        return;
    }
    let journal = world.resource::<Journal>();
    if journal.is_dirty() || journal.is_frozen() {
        return;
    }
    let generation = world.resource::<crate::model::Generation>().0;
    let ready: Vec<_> = {
        let mut host = world.resource_mut::<Host>();
        let count = host.effects.iter().take_while(|(before, _)| *before < generation).count();
        host.effects.drain(..count).map(|(_, effect)| effect).collect()
    };
    world.write_message_batch(ready);
}

/// Hook supervision is deadline-driven; active processes wake through inbound completion.
pub fn next_deadline(world: &mut World) -> Option<u64> {
    let host = world.resource::<Host>();
    let journal = world.resource::<Journal>();
    if !journal.is_dirty() && !journal.is_frozen()
        && host.effects.front().is_some_and(|(g, _)| *g < world.resource::<crate::model::Generation>().0)
    {
        return Some(now(world));
    }
    world.query_filtered::<(&Hook, &Loaded), With<Active>>()
        .iter(world)
        .filter(|(h, l)| h.run.is_none() && !l.manifest.events.is_empty())
        .map(|(h, _)| h.next_ms)
        .min()
}

/// The production runner supplies its bounded wake channel before the first update.
pub fn install_wake(world: &mut World, wake: async_channel::Sender<Inbound>) {
    world.resource_mut::<loading::Loading>().wake = Some(wake);
}

/// Plugin tokens may acknowledge only their own cursor, never another plugin's.
pub fn owns_token(world: &World, plugin: Entity, token: &str) -> bool {
    world.get::<Active>(plugin).is_some_and(|a| a.token == token)
}

/// `Update`: enabled plugins become `Active` once zor's own descriptor is written (token
/// minted, per-plugin descriptor written, `startup` run); plugins no longer enabled lose it.
fn shutting_down(world: &World) -> bool {
    world.get_resource::<bevy_state::prelude::State<crate::model::ServerMode>>()
        .is_some_and(|s| *s.get() == crate::model::ServerMode::ShuttingDown)
}

fn activate(world: &mut World) {
    if shutting_down(world) {
        let active: Vec<_> = world.query_filtered::<Entity, With<Active>>().iter(world).collect();
        for plugin in active { deactivate(world, plugin); }
        return;
    }
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
        if world.get::<Runs>(plugin).and_then(|runs| runs.0.iter().rev().find(|r| r.kind == RunKind::Build))
            .is_some_and(|run| !matches!(run.state, RunState::Exited { code: Some(0), .. })) {
            continue;
        }
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
        let own_path = paths.descriptor(&name, &token);
        if let Err(e) = write_descriptor(&own_path, &own) {
            world.resource_mut::<Tokens>().revoke(&token);
            world.entity_mut(plugin).insert(PluginProblem(format!("descriptor: {e}")));
            continue;
        }
        // Seed at activation, not when the delayed child finally opens its stream. Events
        // arriving between enable and the connection are retained and delivered normally.
        if world.get::<Hook>(plugin).is_some_and(|h| h.cursors.zor_instance != descriptor.instance) {
            let latest = world.resource::<crate::remote::events::EventLog>().latest();
            if let Err(e) = set_cursor_for(world, plugin, Some(latest), None, Some(descriptor.instance.clone()), None) {
                world.resource_mut::<Tokens>().revoke(&token);
                let _ = std::fs::remove_file(&own_path);
                world.entity_mut(plugin).insert(PluginProblem(e.to_string()));
                continue;
            }
        }
        world.entity_mut(plugin).insert(Active { token, descriptor: own_path });
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
    if shutting_down(world) { return; }
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
            Err(e) => {
                bevy_log::warn!("plugin {name}: hook: {e}");
                if let Some(mut hook) = world.get_mut::<Hook>(plugin) {
                    hook.next_ms = now.saturating_add(HOOK_BACKOFF_MAX_MS);
                }
            }
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
    if shutting_down(world) { return refused("server is shutting down"); }
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
    if shutting_down(world) { return refused("server is shutting down"); }
    if world.get::<Runs>(plugin).is_some_and(|runs| runs.0.iter().filter(|r| !matches!(r.state, RunState::Exited { .. })).count() >= MAX_RUNS_RETAINED) {
        return refused("plugin live run limit reached");
    }
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
    world.entity_mut(plugin).entry::<RunArgv>().or_default().into_mut().0.insert(id, argv);
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
                "capabilities": if ctx.ctx.workspace.is_some() { vec!["read", "mutate"] } else { vec!["read"] },
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


/// Emits `Effect::RunPlugin` for a recorded run with the full plugin environment.
fn spawn_run(world: &mut World, plugin: Entity, run: u64, fux_token: Option<String>) {
    let pending = world.get::<Runs>(plugin).is_some_and(|runs| runs.0.iter().any(|r| r.id == run && r.state == RunState::Minting));
    if !pending {
        if let Some(token) = fux_token { revoke_fux_token(world, token); }
        return;
    }
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
        env.push(("ZOR_PLUGIN_PROVIDER".into(), format!("plugin:{name}:{}:{surface}", world.resource::<ServerInstance>().nonce)));
    }
    if let Some(link) = &record.link {
        env.push(("ZOR_PLUGIN_LINK".into(), link.clone()));
    }
    if record.kind == RunKind::Hook {
        let (cursors, hooks) = world
            .get::<Hook>(plugin)
            .map(|h| h.cursors.clone())
            .zip(world.get::<Loaded>(plugin).map(|l| l.manifest.events.clone()))
            .unwrap_or_default();
        env.push(("ZOR_PLUGIN_CURSOR_ZOR".into(), cursors.zor.to_string()));
        env.push(("ZOR_PLUGIN_CURSOR_FUX".into(), cursors.fux.to_string()));
        env.push(("ZOR_PLUGIN_CURSORS".into(), serde_json::to_string(&cursors).unwrap_or_default()));
        env.push((
            "ZOR_PLUGIN_HOOKS".into(),
            serde_json::to_string(&hooks).unwrap_or_else(|_| "[]".into()),
        ));
    }
    if let Some(token) = fux_token {
        world.entity_mut(plugin).entry::<RunTokens>().or_default().into_mut().0.insert(run, token.clone());
        match write_fux_descriptor(world, &name, run, &token) {
            Ok(path) => {
                env.push(("FUX_BRP".into(), path.display().to_string()));
                env.push(("FUX_TOKEN".into(), token));
            }
            Err(e) => {
                world.entity_mut(plugin).insert(PluginProblem(format!("fux descriptor: {e}")));
                on_exit(world, plugin, run, None);
                return;
            }
        }
    }
    let cwd = world.get::<Loaded>(plugin).map(|l| l.root.display().to_string());
    if let Some(mut runs) = world.get_mut::<Runs>(plugin)
        && let Some(r) = runs.get_mut(run)
    {
        r.state = RunState::Running;
    }
    queue_effect(world, plugin, "run plugin process", Effect::RunPlugin {
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
        ("ZOR_BRP".into(), world.get::<Active>(plugin).map(|a| a.descriptor.clone()).unwrap_or_else(|| paths.descriptor(name, "inactive")).display().to_string()),
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
    if world.resource::<Host>().fux_instance.as_ref().is_some_and(|expected| *expected != descriptor.instance) {
        return Err("fux incarnation changed during plugin dispatch; reconcile before retrying".into());
    }
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

fn fux_call(world: &mut World, pending: Pending, method: &str, mut params: Value) -> u64 {
    if world.resource::<Host>().fux_instance.is_none() {
        let instance = world.get_resource::<FuxDescriptor>()
            .and_then(|f| read_descriptor(&f.0).ok()).map(|d| d.instance);
        world.resource_mut::<Host>().fux_instance = instance;
    }
    if params.get("_expected_instance").is_none()
        && let Some(instance) = &world.resource::<Host>().fux_instance {
        params["_expected_instance"] = json!(instance);
    }
    let call = {
        let mut host = world.resource_mut::<Host>();
        host.next_call += 1;
        let call = CALL_TAG | (host.next_call & !TAG_MASK);
        call
    };
    let plugin = match &pending {
        Pending::Mint { plugin, .. } | Pending::PaneMint { plugin, .. } |
        Pending::PaneList { plugin, .. } | Pending::PaneCreate { plugin, .. } |
        Pending::PaneViewers { plugin, .. } | Pending::PaneSurface { plugin, .. } => Some(*plugin),
        Pending::Ignore | Pending::PaneClose { .. } | Pending::PaneRemoved { .. } | Pending::ReconcilePanes { .. } | Pending::Retire { .. } => None,
    };
    world.resource_mut::<Host>().calls.insert(call, pending);
    let effect = Effect::FuxCall {
        call,
        method: method.into(),
        params,
    };
    if let Some(plugin) = plugin {
        queue_effect(world, plugin, method, effect);
    } else {
        world.write_message(effect);
    }
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
    if panes.0.len() >= MAX_RUNS_RETAINED {
        if let Some(index) = panes.0.iter().position(|p| p.cleanup_state.is_none() && matches!(p.state, PaneState::Failed { .. } | PaneState::Closed)) {
            panes.0.remove(index);
        } else {
            return refused("plugin pane limit reached");
        }
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
        cleanup_state: None,
        generation: None,
    });
    world.entity_mut(plugin).entry::<RunArgv>().or_default().into_mut().0.insert(id, spec.command);
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

fn provider(world: &World, plugin: Entity, node: u64) -> String {
    format!("plugin:{}:{}:{node}", name_of(world, plugin), world.resource::<ServerInstance>().nonce)
}

/// A surface close must succeed before its container can be removed. Its returned layout
/// generation prevents a replacement installed between those calls from being despawned.
fn cleanup_pane(world: &mut World, plugin: Entity, open: &OpenPane) {
    let state = open.cleanup_state.as_ref().unwrap_or(&open.state);
    let &PaneState::Open { node, pane, surface } = state else { return };
    if let Some(surface) = surface {
        let expected = provider(world, plugin, surface);
        fux_call(world, Pending::PaneClose { plugin, opening: open.id, node }, "fux/surface.close",
            json!({"surface": surface, "expected_provider": expected}));
    } else if let Some(pane) = pane {
        fux_call(world, Pending::PaneRemoved { plugin, opening: open.id }, "fux/pane.close", json!({"pane": pane}));
    } else if let Some(generation) = open.generation {
        fux_call(world, Pending::PaneRemoved { plugin, opening: open.id }, "fux/node.despawn",
            json!({"node": node, "generation": generation}));
    }
}

fn retire_pane(world: &mut World, plugin: Entity, opening: u64) {
    let token = world.get_mut::<Panes>(plugin).and_then(|mut panes| {
        let open = panes.0.iter_mut().find(|o| o.id == opening)?;
        if !matches!(open.state, PaneState::Failed { .. }) { open.state = PaneState::Closed; }
        open.cleanup_state = None;
        open.fux_token.take()
    });
    if let Some(token) = token {
        let path = world.resource::<PluginPaths>().fux_descriptor(&name_of(world, plugin), opening);
        fux_call(world, Pending::Retire { path }, "fux/token.revoke", json!({"revoke": token}));
    }
}

fn fail_pane(world: &mut World, plugin: Entity, opening: u64, problem: String) {
    if let Some(open) = world.get::<Panes>(plugin).and_then(|p| p.0.iter().find(|o| o.id == opening)).cloned() {
        cleanup_pane(world, plugin, &open);
    }
    let name = name_of(world, plugin);
    let path = world.resource::<PluginPaths>().fux_descriptor(&name, opening);
    let token = world.get_mut::<Panes>(plugin).and_then(|mut panes| panes.0.iter_mut().find(|o| o.id == opening).and_then(|o| o.fux_token.take()));
    if let Some(token) = token {
        fux_call(world, Pending::Retire { path }, "fux/token.revoke", json!({"revoke": token}));
    }
    if let Some(mut panes) = pane_mut(world, plugin, opening)
        && let Some(open) = panes.0.iter_mut().find(|o| o.id == opening)
    {
        if matches!(open.state, PaneState::Open { .. }) {
            open.cleanup_state = Some(open.state.clone());
        }
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
    if let Pending::PaneClose { plugin, opening, node } = pending {
        match result {
            Ok(value) => if let Some(generation) = value.get("generation").and_then(Value::as_u64) {
                if let Some(mut panes) = world.get_mut::<Panes>(plugin)
                    && let Some(open) = panes.0.iter_mut().find(|o| o.id == opening)
                {
                    let state = PaneState::Open { node, pane: None, surface: None };
                    if open.cleanup_state.is_some() { open.cleanup_state = Some(state); } else { open.state = state; }
                    open.generation = Some(generation);
                }
                fux_call(world, Pending::PaneRemoved { plugin, opening }, "fux/node.despawn",
                    json!({"node": node, "generation": generation}));
            },
            Err(e) => bevy_log::warn!("plugin surface closure unresolved; ownership retained: {e}"),
        }
        return;
    }
    if let Pending::PaneRemoved { plugin, opening } = pending {
        match result {
            Ok(_) => retire_pane(world, plugin, opening),
            Err(e) => bevy_log::warn!("plugin pane closure unresolved; ownership retained: {e}"),
        }
        return;
    }
    if let Pending::ReconcilePanes { openings } = pending {
        if let Ok(value) = result
            && let Ok(list) = serde_json::from_value::<WorkspaceList>(value)
        {
            let closed: Vec<_> = openings.into_iter().filter(|(plugin, opening)| {
                let Some(open) = world.get::<Panes>(*plugin).and_then(|panes| panes.0.iter().find(|p| p.id == *opening)) else { return false };
                let PaneState::Open { pane: Some(pane), .. } = open.state else { return false };
                !list.workspaces.iter().filter(|w| w.name == open.workspace)
                    .flat_map(|w| &w.roots).flat_map(|r| &r.panes).any(|p| p.id == pane)
            }).collect();
            for (plugin, opening) in closed { retire_pane(world, plugin, opening); }
        }
        return;
    }
    let (plugin, opening) = match pending {
        Pending::PaneMint { plugin, opening }
        | Pending::PaneList { plugin, opening }
        | Pending::PaneCreate { plugin, opening }
        | Pending::PaneViewers { plugin, opening }
        | Pending::PaneSurface { plugin, opening } => (plugin, opening),
        Pending::Mint { .. } | Pending::Ignore | Pending::PaneClose { .. } | Pending::PaneRemoved { .. } | Pending::ReconcilePanes { .. } | Pending::Retire { .. } => return,
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
    if matches!(open.state, PaneState::Failed { .. }) {
        if matches!(pending, Pending::PaneMint { .. })
            && let Some(token) = value.get("token").and_then(Value::as_str)
        {
            revoke_fux_token(world, token.to_owned());
        }
        if matches!(pending, Pending::PaneCreate { .. }) {
            if let Some(pane) = value.get("pane").and_then(Value::as_u64) {
                fux_call(world, Pending::Ignore, "fux/pane.close", json!({"pane": pane}));
            } else if let (Some(node), Some(generation)) = (value.get("node").and_then(Value::as_u64), value.get("generation").and_then(Value::as_u64)) {
                fux_call(world, Pending::Ignore, "fux/node.despawn", json!({"node": node, "generation": generation}));
            }
        }
        if matches!(pending, Pending::PaneSurface { .. })
            && let Some(surface) = value.get("surface").and_then(Value::as_u64)
        {
            let expected = provider(world, plugin, surface);
            fux_call(world, Pending::PaneClose { plugin, opening, node: surface }, "fux/surface.close",
                json!({"surface": surface, "expected_provider": expected}));
        }
        return;
    }
    let name = name_of(world, plugin);
    match pending {
        Pending::PaneMint { .. } => {
            let token = value
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if token.is_empty() { return fail_pane(world, plugin, opening, "fux returned no scoped token".into()); }
            match write_fux_descriptor(world, &name, opening, &token) {
                Ok(path) => {
                    if let Some(mut panes) = pane_mut(world, plugin, opening)
                        && let Some(o) = panes.0.iter_mut().find(|o| o.id == opening)
                    {
                        o.fux_token = Some(token);
                        o.fux_brp = Some(path);
                    }
                }
                Err(e) => {
                    revoke_fux_token(world, token);
                    return fail_pane(world, plugin, opening, e);
                }
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
            let selected = if let Some(target) = open.target {
                match ws.roots.iter().find(|r| r.panes.iter().any(|p| p.id == target)) {
                    Some(root) => Some(root),
                    None => return fail_pane(world, plugin, opening, "target pane is outside requested workspace".into()),
                }
            } else { ws.roots.first() };
            let (root, generation, first_pane) = selected
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
            if let Some(mut panes) = world.get_mut::<Panes>(plugin)
                && let Some(open) = panes.0.iter_mut().find(|o| o.id == opening)
            { open.generation = Some(generation); }
            match (open.kind, open.placement) {
                (PaneKind::Surface, _) => {
                    fux_call(
                        world,
                        Pending::PaneSurface { plugin, opening },
                        "fux/surface.open",
                        json!({ "workspace": open.workspace, "node": node, "provider": format!("plugin:{name}:{}:{node}", world.resource::<ServerInstance>().nonce), "generation": generation }),
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
        Pending::Mint { .. } | Pending::Ignore | Pending::PaneClose { .. } | Pending::PaneRemoved { .. } | Pending::ReconcilePanes { .. } | Pending::Retire { .. } => {}
    }
}

fn set_open(world: &mut World, plugin: Entity, opening: u64, node: u64, pane: Option<u64>, surface: Option<u64>) {
    if let Some(mut panes) = pane_mut(world, plugin, opening)
        && let Some(o) = panes.0.iter_mut().find(|o| o.id == opening)
    {
        o.state = PaneState::Open { node, pane, surface };
    }
    if pane.is_some() {
        fux_call(world, Pending::ReconcilePanes { openings: vec![(plugin, opening)] }, "fux/workspace.list", json!({}));
    }
}

// ---------------------------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------------------------

fn collect_inbound(mut inbound: MessageReader<Inbound>, mut host: ResMut<Host>) {
    for message in inbound.read() {
        match message {
            Inbound::FuxLink { instance } => host.inbox.push(Item::Link(instance.clone())),
            Inbound::FuxGap { .. } => host.inbox.push(Item::Gap),
            Inbound::FuxEvent { name, body, .. } if name == "PaneClosed" => {
                if let Some(pane) = body.get("pane").and_then(Value::as_u64) {
                    host.inbox.push(Item::PaneClosed(pane));
                }
            }
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
            Item::Link(instance) => {
                world.resource_mut::<Host>().observed_fux_instance = instance;
                reconcile_panes(world);
            }
            Item::Gap => reconcile_panes(world),
            Item::PaneClosed(pane) => {
                let host = world.resource::<Host>();
                if host.observed_fux_instance.is_none() || host.observed_fux_instance != host.fux_instance { continue; }
                let closed: Vec<_> = world.query::<(Entity, &Panes)>().iter(world)
                    .flat_map(|(plugin, panes)| panes.0.iter().filter_map(|open| {
                        matches!(open.state, PaneState::Open { pane: Some(id), .. } if id == pane)
                            .then_some((plugin, open.id))
                    }).collect::<Vec<_>>()).collect();
                for (plugin, opening) in closed { retire_pane(world, plugin, opening); }
            }
            Item::Exited { plugin, run, code } => on_exit(world, plugin, run, code),
            Item::Reply { call, result } => {
                let Some(pending) = world.resource_mut::<Host>().calls.remove(&call) else {
                    continue;
                };
                match pending {
                    Pending::Mint { plugin, run } => {
                        let wanted = world.get::<Runs>(plugin).is_some_and(|runs| runs.0.iter().any(|r| r.id == run && r.state == RunState::Minting));
                        if !wanted {
                            if let Ok(value) = result
                                && let Some(token) = value.get("token").and_then(Value::as_str)
                            { revoke_fux_token(world, token.to_owned()); }
                            continue;
                        }
                        let token = match result {
                            Ok(value) => value
                                .get("token")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            Err(e) => {
                                world.entity_mut(plugin).insert(PluginProblem(format!("fux token not minted: {e}")));
                                on_exit(world, plugin, run, None);
                                continue;
                            }
                        };
                        if token.is_none() {
                            on_exit(world, plugin, run, None);
                            continue;
                        }
                        spawn_run(world, plugin, run, token);
                    }
                    Pending::Retire { path } => match result {
                        Ok(_) => { let _ = std::fs::remove_file(path); }
                        Err(e) => bevy_log::warn!("plugin grant retirement unresolved; descriptor retained: {e}"),
                    },
                    Pending::Ignore => {
                        if let Err(e) = result { bevy_log::warn!("plugin cleanup or viewer update failed: {e}"); }
                    }
                    other => pane_step(world, other, result),
                }
            }
        }
    }
}

fn reconcile_panes(world: &mut World) {
    let host = world.resource::<Host>();
    if host.observed_fux_instance.is_none() || host.observed_fux_instance != host.fux_instance
        || host.calls.values().any(|p| matches!(p, Pending::ReconcilePanes { .. })) { return; }
    let openings: Vec<_> = world.query::<(Entity, &Panes)>().iter(world).flat_map(|(plugin, panes)| {
        panes.0.iter().filter_map(|open| matches!(open.state, PaneState::Open { pane: Some(_), .. }).then_some((plugin, open.id))).collect::<Vec<_>>()
    }).collect();
    if !openings.is_empty() {
        fux_call(world, Pending::ReconcilePanes { openings }, "fux/workspace.list", json!({}));
    }
}

fn on_exit(world: &mut World, plugin: Entity, run: u64, code: Option<i32>) {
    let canceled = world.get::<Runs>(plugin).is_some_and(|runs| runs.0.iter().any(|r| r.id == run && r.state == RunState::Stopping));
    retire_run_token(world, plugin, run);
    if world.get::<Runs>(plugin).is_none_or(|runs| runs.0.iter().all(|r| r.id != run || matches!(r.state, RunState::Exited { .. }))) {
        return;
    }
    let ms = now(world);
    let Some(record) = world.get_mut::<Runs>(plugin).and_then(|mut runs| {
        runs.get_mut(run).map(|r| {
            r.state = RunState::Exited { code, ms };
            r.clone()
        })
    }) else {
        return;
    };
    world.resource_mut::<Journal>().mark_dirty();
    if canceled { return; }
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
            if let Some(surface) = record.surface
                && let Some(open) = world.get::<Panes>(plugin).and_then(|panes| panes.0.iter()
                    .find(|o| matches!(o.state, PaneState::Open { surface: Some(id), .. } if id == surface))).cloned()
            {
                cleanup_pane(world, plugin, &open);
            }
        }
        RunKind::Action(_) | RunKind::Link(_) => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Cursors, bindings, records, logs
// ---------------------------------------------------------------------------------------------

fn read_cursors(path: &Path) -> Result<HookCursors, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HookCursors::default()),
        Err(e) => return Err(format!("hook cursors: {e}")),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 { return Err("hook cursor file exceeds bound".into()); }
    serde_json::from_slice(&bytes).map_err(|e| format!("hook cursors corrupt; replay refused: {e}"))
}

/// The hook process reports the last cursor it processed on either stream; persisted at once.
pub fn set_cursor(world: &mut World, plugin: Entity, zor: Option<u64>, fux: Option<u64>) -> Res<HookCursors> {
    set_cursor_for(world, plugin, zor, fux, None, None)
}

/// Cursors are scoped to server incarnation. Switching incarnation resets only that stream.
pub fn set_cursor_for(world: &mut World, plugin: Entity, zor: Option<u64>, fux: Option<u64>, zor_instance: Option<String>, fux_instance: Option<String>) -> Res<HookCursors> {
    check_plugin(world, plugin)?;
    let Some(hook) = world.get::<Hook>(plugin) else {
        return Err(PluginError::NotFound("hook".into()));
    };
    let mut cursors = hook.cursors.clone();
    if let Some(instance) = zor_instance
        && cursors.zor_instance != instance
    {
        cursors.zor = 0;
        cursors.zor_instance = instance;
    }
    if let Some(instance) = fux_instance
        && cursors.fux_instance != instance
    {
        cursors.fux = 0;
        cursors.fux_instance = instance;
    }
    if let Some(zor) = zor {
        cursors.zor = cursors.zor.max(zor);
    }
    if let Some(fux) = fux {
        cursors.fux = cursors.fux.max(fux);
    }
    let name = name_of(world, plugin);
    let path = world.resource::<PluginPaths>().cursors(&name);
    write_cursors(&path, &cursors).map_err(PluginError::Io)?;
    if let Some(mut hook) = world.get_mut::<Hook>(plugin) { hook.cursors = cursors.clone(); }
    Ok(cursors)
}

fn write_cursors(path: &Path, cursors: &HookCursors) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let document = serde_json::to_string(cursors).map_err(|e| e.to_string())?;
    crate::journal::write_atomic(path, &document, 0o600).map_err(|e| e.to_string())
}

/// Every enabled plugin's keybinding, with fux's canonical `plugin:<name>/<action>` action.
pub fn bindings(world: &mut World) -> Vec<Binding> {
    let mut out: Vec<Binding> = world
        .query_filtered::<(&PluginId, &Loaded), With<PluginEnabled>>()
        .iter(world)
        .flat_map(|(id, loaded)| {
            loaded.manifest.actions.iter().filter_map(|a| {
                a.keybinding.as_ref().map(|chord| Binding {
                    chord: chord.clone(),
                    plugin: id.0.clone(),
                    action: format!("plugin:{}/{}", id.0, a.id),
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
