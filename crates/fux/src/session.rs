//! Session persistence and restart restoration (prompt 3.8, 3.4 "Scenes").
//!
//! * [`document`] extracts the session as one RON `DynamicWorld`: for every open workspace the
//!   allowlisted template subgraph of `scene::export` (`Node`, `ChildOf`/`Children`, `ZIndex`,
//!   colours, `Name`, `RootOrder`) plus, behind each placing leaf, a pane entity carrying
//!   `LaunchAttribution`, `Title`, [`Historical`] (its last screen, bounded) and [`LastFocus`]
//!   on the pane a viewer targeted. A pane launched with an environment also carries its
//!   `PaneTemplate`, so the variables survive a restart; a pane launched without one does not,
//!   so the file never holds an environment the launch did not put there. Public ids are not
//!   written: a restored session allocates fresh ones. Nothing about sockets, PTYs, viewers,
//!   tokens or the instance nonce is allowlisted, so it cannot leak into the file.
//! * [`SessionPlugin`] writes the document to `<state_dir>/session/<server>.scn.ron` (atomic:
//!   temp file + rename, 0600) at most once per [`SAVE_INTERVAL_MS`] while the layout, names,
//!   focus or titles change, and once more on `OnEnter(ServerMode::ShuttingDown)`.
//! * [`restore_or_bootstrap`] runs at `Startup`: with `--restore auto|ask` and an existing file
//!   the document is loaded into an inert World, validated (limits, references, `Node`-less
//!   roots), split per workspace and rebuilt through `scene::apply` with templates allowed, so
//!   every restored leaf launches through the same `Creation` path as any spawn. `auto`
//!   materialises immediately (the lifecycle feeds the `Historical` lines above a dim
//!   separator); `ask` parks every pane behind [`RestorePending`] until
//!   `fux/session.restore` or `fux/session.skip`. A file that fails validation is renamed
//!   `*.rejected-<ms>` and the default workspace is bootstrapped instead.

use std::path::{Path, PathBuf};

use bevy_app::prelude::*;
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_log::{info, warn};
use bevy_platform::collections::{HashMap, HashSet};
use bevy_reflect::prelude::*;
use bevy_state::prelude::*;
use bevy_ui::{BackgroundColor, BorderColor, Node, ZIndex};
use bevy_world_serialization::serde::WorldDeserializer;
use bevy_world_serialization::{DynamicEntity, DynamicWorld, DynamicWorldBuilder};
use serde::de::DeserializeSeed as _;
use serde::{Deserialize, Serialize};

use crate::layout::{LayoutError, ops};
use crate::lifecycle::{self, LifecycleSystems};
use crate::model::*;
use crate::scene::{self, ApplyOptions, SceneError};
use crate::terminal::Terminal;

/// Directory under the state directory holding `<server>.scn.ron`.
pub const SESSION_DIR: &str = "session";
/// Screen lines kept per pane (the tail of the visible screen).
pub const MAX_HISTORY_LINES: usize = 200;
/// Bytes kept per historical line.
pub const MAX_HISTORY_LINE_BYTES: usize = 4096;
/// Autosaves are at least this far apart; shutdown saves immediately.
pub const SAVE_INTERVAL_MS: u64 = 5_000;

// ---------------------------------------------------------------------------------------------
// Components and resources
// ---------------------------------------------------------------------------------------------

/// The last screen of a pane when the session was saved. On a restored pane the lifecycle feeds
/// the lines into the new `Terminal` above a dim separator before the process starts, then
/// removes the component.
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Historical {
    pub lines: Vec<String>,
}

impl Historical {
    /// The tail of `lines` within the persisted bounds.
    pub fn bounded(mut lines: Vec<String>) -> Self {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        let excess = lines.len().saturating_sub(MAX_HISTORY_LINES);
        lines.drain(..excess);
        for line in &mut lines {
            if line.len() > MAX_HISTORY_LINE_BYTES {
                let mut cut = MAX_HISTORY_LINE_BYTES;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
        }
        Self { lines }
    }
}

/// The pane a viewer of its workspace targeted when the session was saved (one per
/// workspace); kept on the restored pane until the first viewer attaches to the workspace
/// (`ops::attach_viewer` targets it and removes the marker).
#[derive(Component, Reflect, Debug, Default)]
#[reflect(Component)]
pub struct LastFocus;

/// A restored pane whose process has not been decided on (`--restore ask`): it stays
/// `Disabled`/`Starting` and is not materialised until `fux/session.restore` (marker removed)
/// or `fux/session.skip` (closed; its leaf collapses through the lifecycle).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestorePending;

/// What `serve --restore` does with an existing session file.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum RestoreMode {
    /// Rebuild the session and launch every pane.
    #[default]
    Auto,
    /// Rebuild the session; each pane waits for `fux/session.restore` or `fux/session.skip`.
    Ask,
    /// Ignore the file; start with the default workspace.
    None,
}

/// Where this server's session file lives.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    pub dir: PathBuf,
    pub server: String,
}

impl SessionFile {
    pub fn path(&self) -> PathBuf {
        self.dir
            .join(format!("{}{}", self.server, scene::EXTENSION))
    }
}

/// Autosave bookkeeping; the runner sleeps no longer than `next_save_ms` when set.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionState {
    /// Something persisted changed since the last write.
    pub dirty: bool,
    /// Clock of the last attempted write (0: never).
    pub last_save_ms: u64,
    /// A throttled write is due at this clock.
    pub next_save_ms: Option<u64>,
    /// Successful writes since start.
    pub saves: u64,
    /// The last write failure, cleared by the next success.
    pub last_error: Option<String>,
}

pub struct SessionPlugin {
    /// `<state_dir>/session`.
    pub dir: PathBuf,
    pub server: String,
}

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SessionFile {
            dir: self.dir.clone(),
            server: self.server.clone(),
        })
        .init_resource::<SessionState>()
        .init_resource::<RestoreMode>()
        .register_type::<Historical>()
        .register_type::<LastFocus>()
        .add_systems(
            PostUpdate,
            autosave
                .in_set(Phase::Projection)
                .after(LifecycleSystems::Materialize)
                .run_if(not(in_state(ServerMode::ShuttingDown))),
        )
        .add_systems(OnEnter(ServerMode::ShuttingDown), save_on_shutdown);
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum SessionError {
    Scene(SceneError),
    Layout(LayoutError),
    /// The document is well-formed RON but not a session.
    Invalid(String),
    /// The app has no [`SessionFile`] (headless without the plugin).
    NoSessionFile,
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Scene(e) => write!(f, "{e}"),
            Self::Layout(e) => write!(f, "{e}"),
            Self::Invalid(why) => write!(f, "invalid session: {why}"),
            Self::NoSessionFile => f.write_str("no session file configured"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<SceneError> for SessionError {
    fn from(e: SceneError) -> Self {
        Self::Scene(e)
    }
}

impl From<LayoutError> for SessionError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

type R<T> = Result<T, SessionError>;

fn invalid(why: impl Into<String>) -> SessionError {
    SessionError::Invalid(why.into())
}

// ---------------------------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------------------------

/// The session as a RON `DynamicWorld` (see the module doc). Retiring workspaces and exited
/// panes are not part of it.
pub fn document(world: &World) -> R<String> {
    let ids = world.resource::<Ids>();
    let mut workspaces: Vec<(Entity, &str)> = ids
        .workspaces
        .iter()
        .filter(|&(_, &ws)| world.get::<Open>(ws).is_some() && world.get::<Retiring>(ws).is_none())
        .map(|(name, &ws)| (ws, name.0.as_str()))
        .collect();
    workspaces.sort_unstable_by(|a, b| a.1.cmp(b.1));

    // One focus per workspace: the first viewer's target.
    let mut focus: HashMap<Entity, Entity> = HashMap::default();
    let mut viewers: Vec<Entity> = ids.viewers.values().copied().collect();
    viewers.sort_unstable();
    for viewer in viewers {
        if let (Some(viewing), Some(target)) =
            (world.get::<Viewing>(viewer), world.get::<Targets>(viewer))
        {
            focus.entry(viewing.0).or_insert(target.0);
        }
    }

    let mut extract: Vec<Entity> = Vec::new();
    let mut roots: HashSet<Entity> = HashSet::default();
    let mut panes: HashMap<Entity, (Historical, bool)> = HashMap::default();
    for &(ws, _) in &workspaces {
        extract.push(ws);
        let order: Vec<Entity> = world
            .get::<RootOrder>(ws)
            .map(|o| o.0.clone())
            .unwrap_or_default();
        for root in order {
            roots.insert(root);
            walk(world, root, &mut |node| {
                extract.push(node);
                let Some(pane) = world.get::<Places>(node).map(|p| p.0) else {
                    return;
                };
                if matches!(world.get::<Process>(pane), Some(Process::Exited { .. })) {
                    return;
                }
                extract.push(pane);
                let history = match world.get::<Terminal>(pane) {
                    Some(terminal) => Historical::bounded(terminal.screen_lines()),
                    None => world.get::<Historical>(pane).cloned().unwrap_or_default(),
                };
                panes.insert(pane, (history, focus.get(&ws) == Some(&pane)));
            });
        }
    }

    let registry = world.resource::<AppTypeRegistry>().read();
    let mut dynamic = DynamicWorldBuilder::from_world(world, &registry)
        .deny_all()
        .allow_component::<Workspace>()
        .allow_component::<WorkspaceName>()
        .allow_component::<RootOrder>()
        .allow_component::<TemplateRoot>()
        .allow_component::<TemplateNode>()
        .allow_component::<RootOf>()
        .allow_component::<Node>()
        .allow_component::<ChildOf>()
        .allow_component::<Children>()
        .allow_component::<ZIndex>()
        .allow_component::<BackgroundColor>()
        .allow_component::<BorderColor>()
        .allow_component::<Name>()
        .allow_component::<Places>()
        .allow_component::<Pane>()
        .allow_component::<LaunchAttribution>()
        .allow_component::<PaneTemplate>()
        .allow_component::<Title>()
        .extract_entities(extract.into_iter())
        .remove_empty_entities()
        .build();
    // As in `scene::export`: roots are parentless in the document and the workspace lists them
    // only through `RootOrder`. Panes gain their history and focus; an empty title is noise and
    // a template without an environment adds nothing to the attribution.
    for entity in &mut dynamic.entities {
        if workspaces.iter().any(|(ws, _)| *ws == entity.entity) {
            entity.components.retain(|c| !is::<Children>(c.as_ref()));
        } else if roots.contains(&entity.entity) {
            entity.components.retain(|c| !is::<ChildOf>(c.as_ref()));
        } else if let Some((history, focused)) = panes.get(&entity.entity) {
            entity.components.retain(|c| {
                if is::<Title>(c.as_ref()) {
                    Title::from_reflect(c.as_ref()).is_some_and(|t| !t.0.is_empty())
                } else if is::<PaneTemplate>(c.as_ref()) {
                    PaneTemplate::from_reflect(c.as_ref()).is_some_and(|t| !t.env.is_empty())
                } else {
                    true
                }
            });
            entity.components.push(Box::new(history.clone()));
            if *focused {
                entity.components.push(Box::new(LastFocus));
            }
        }
    }
    dynamic
        .serialize(&registry)
        .map_err(|e| SceneError::Serialize(e.to_string()).into())
}

fn is<T: 'static>(component: &dyn PartialReflect) -> bool {
    component
        .get_represented_type_info()
        .is_some_and(|info| info.type_id() == core::any::TypeId::of::<T>())
}

/// Pre-order walk of a template subtree, not descending into surface leaves.
fn walk(world: &World, root: Entity, f: &mut dyn FnMut(Entity)) {
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        f(entity);
        if world.get::<Surface>(entity).is_some() {
            continue;
        }
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter().rev());
        }
    }
}

/// Writes the current document to the session file. Returns the path and the byte count.
pub fn save(world: &World) -> R<(PathBuf, usize)> {
    let file = world
        .get_resource::<SessionFile>()
        .ok_or(SessionError::NoSessionFile)?;
    let document = document(world)?;
    let path = scene::save(&file.dir, &file.server, &document)?;
    Ok((path, document.len()))
}

/// The bookkeeping a successful write does; `fux/session.save` and the autosave share it.
pub fn note_save(world: &mut World, now: u64) {
    let mut state = world.resource_mut::<SessionState>();
    state.dirty = false;
    state.last_save_ms = now;
    state.next_save_ms = None;
    state.saves += 1;
    state.last_error = None;
}

fn save_now(world: &mut World, now: u64) {
    match save(world) {
        Ok((path, bytes)) => {
            note_save(world, now);
            bevy_log::debug!("session saved: {} ({bytes} bytes)", path.display());
        }
        Err(error) => {
            warn!("session save failed: {error}");
            let mut state = world.resource_mut::<SessionState>();
            state.dirty = false;
            state.last_save_ms = now;
            state.next_save_ms = None;
            state.last_error = Some(error.to_string());
        }
    }
}

/// `PostUpdate`/`Projection`: when anything persisted changed, write now if the last write is
/// older than [`SAVE_INTERVAL_MS`], else schedule the write (the runner wakes for it).
fn autosave(
    world: &mut World,
    changed: &mut QueryState<
        (),
        (
            Or<(
                Changed<LayoutGeneration>,
                Changed<RootOrder>,
                Changed<Name>,
                Changed<LaunchAttribution>,
                Changed<Targets>,
                Changed<Title>,
                Added<Retiring>,
            )>,
            Allow<Disabled>,
        ),
    >,
) {
    if world.get_resource::<SessionFile>().is_none() {
        return;
    }
    let dirty = changed.iter(world).next().is_some();
    let now = lifecycle::now_ms(world);
    let write = {
        let mut state = world.resource_mut::<SessionState>();
        state.dirty |= dirty;
        if !state.dirty {
            return;
        }
        let due =
            (state.last_save_ms != 0).then(|| state.last_save_ms.saturating_add(SAVE_INTERVAL_MS));
        match due {
            Some(at) if now < at => {
                state.next_save_ms = Some(at);
                false
            }
            _ => true,
        }
    };
    if write {
        save_now(world, now);
    }
}

fn save_on_shutdown(world: &mut World) {
    if world.get_resource::<SessionFile>().is_none() {
        return;
    }
    let now = lifecycle::now_ms(world);
    save_now(world, now);
}

// ---------------------------------------------------------------------------------------------
// Materialisation helper (called by the lifecycle)
// ---------------------------------------------------------------------------------------------

/// Feeds the historical screen into a fresh terminal, then a dim full-width separator, so the
/// old screen sits above the new prompt. Control characters in the stored lines are dropped:
/// history is text, never terminal input.
pub fn replay(terminal: &mut Terminal, history: &Historical, cols: u16) {
    let mut text = String::with_capacity(
        history.lines.iter().map(|l| l.len() + 2).sum::<usize>() + usize::from(cols) * 3 + 16,
    );
    for line in &history.lines {
        text.extend(line.chars().filter(|c| !c.is_control()));
        text.push_str("\r\n");
    }
    text.push_str("\x1b[2m");
    for _ in 0..cols {
        text.push('─');
    }
    text.push_str("\x1b[0m\r\n");
    terminal.feed(text.as_bytes());
}

// ---------------------------------------------------------------------------------------------
// Restoration
// ---------------------------------------------------------------------------------------------

/// What [`restore`] built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Restored workspaces in document order.
    pub workspaces: Vec<Entity>,
    /// Restored panes (`Disabled`, `Process::Starting`) in document order.
    pub panes: Vec<Entity>,
}

/// `Startup`: restores the session file when the mode allows and the file exists, else
/// bootstraps `workspace`. A file that fails validation is renamed `*.rejected-<ms>`.
pub fn restore_or_bootstrap(world: &mut World, workspace: &str) -> Result<(), BevyError> {
    let mode = world
        .get_resource::<RestoreMode>()
        .copied()
        .unwrap_or_default();
    let path = world.get_resource::<SessionFile>().map(SessionFile::path);
    if let Some(path) = path
        && mode != RestoreMode::None
        && path.is_file()
    {
        match restore_file(world, &path, mode) {
            Ok(report) => {
                info!(
                    "session restored from {}: {} workspace(s), {} pane(s){}",
                    path.display(),
                    report.workspaces.len(),
                    report.panes.len(),
                    if mode == RestoreMode::Ask {
                        " awaiting decisions"
                    } else {
                        ""
                    }
                );
                return Ok(());
            }
            Err(error) => {
                let rejected = reject(&path, lifecycle::now_ms(world));
                warn!(
                    "session file {} rejected ({error}); moved to {}",
                    path.display(),
                    rejected.display()
                );
            }
        }
    }
    lifecycle::bootstrap(world, workspace, &[])
}

/// Renames a bad session file out of the way; returns where it went (or the original path when
/// even that failed).
fn reject(path: &Path, now_ms: u64) -> PathBuf {
    let mut rejected = path.as_os_str().to_owned();
    rejected.push(format!(".rejected-{now_ms}"));
    let rejected = PathBuf::from(rejected);
    match std::fs::rename(path, &rejected) {
        Ok(()) => rejected,
        Err(error) => {
            warn!("could not move rejected session file: {error}");
            path.to_path_buf()
        }
    }
}

fn restore_file(world: &mut World, path: &Path, mode: RestoreMode) -> R<RestoreReport> {
    let dir = path
        .parent()
        .ok_or_else(|| invalid("session path has no directory"))?;
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(scene::EXTENSION))
        .ok_or_else(|| invalid("session path is not `<server>.scn.ron`"))?;
    let document = scene::load(dir, stem)?;
    restore(world, &document, mode)
}

/// One pane of the document, in pre-order within its workspace.
struct DocPane {
    template: PaneTemplate,
    history: Historical,
    title: Option<Title>,
    focus: bool,
}

/// One workspace of the document, ready to build.
struct DocWorkspace {
    name: String,
    /// Document entity ids of the workspace, its nodes and its panes.
    members: Vec<Entity>,
    panes: Vec<DocPane>,
}

/// Rebuilds the workspaces of `document` (see the module doc). Validates the whole document
/// first; a workspace that still fails to build undoes everything built so far, so a refusal
/// leaves the World as it was.
pub fn restore(world: &mut World, document: &str, mode: RestoreMode) -> R<RestoreReport> {
    if document.len() > scene::MAX_DOCUMENT_BYTES {
        return Err(SceneError::TooLarge {
            bytes: document.len(),
            max: scene::MAX_DOCUMENT_BYTES,
        }
        .into());
    }
    let dynamic = parse(world, document)?;
    let plan = validate(world, &dynamic)?;

    // Move the document's entities into per-workspace documents; the pane entities are rebuilt
    // as launch templates (fresh ids, no session-only components).
    let mut by_doc: HashMap<Entity, DynamicEntity> = dynamic
        .entities
        .into_iter()
        .map(|e| (e.entity, e))
        .collect();
    let mut report = RestoreReport::default();
    for ws in plan {
        let mut entities = Vec::with_capacity(ws.members.len());
        let mut templates = ws.panes.iter().map(|p| p.template.clone());
        for member in &ws.members {
            let Some(mut entity) = by_doc.remove(member) else {
                continue;
            };
            if entity.components.iter().any(|c| is::<Pane>(c.as_ref())) {
                let Some(template) = templates.next() else {
                    continue;
                };
                entity.components = vec![Box::new(Pane), Box::new(template)];
            }
            entities.push(entity);
        }
        let partial = DynamicWorld {
            resources: Vec::new(),
            entities,
        };
        let outcome = build_workspace(world, &ws, &partial, mode);
        match outcome {
            Ok((workspace, panes)) => {
                report.workspaces.push(workspace);
                report.panes.extend(panes);
            }
            Err(error) => {
                undo(world, &report);
                return Err(error);
            }
        }
    }
    Ok(report)
}

fn parse(world: &World, document: &str) -> R<DynamicWorld> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut assets = world
        .get_resource::<bevy_asset::AssetServer>()
        .cloned()
        .ok_or(SceneError::NoAssetServer)?;
    let mut de =
        ron::de::Deserializer::from_str(document).map_err(|e| SceneError::Parse(e.to_string()))?;
    let seed = WorldDeserializer {
        type_registry: &registry.read(),
        load_from_path: &mut assets,
    };
    seed.deserialize(&mut de)
        .map_err(|e| SceneError::Parse(de.span_error(e).to_string()).into())
}

/// Components a session document may carry: `scene::apply`'s allowlist without the public ids,
/// plus the pane's title, history and focus.
fn allowed(type_id: core::any::TypeId) -> bool {
    use core::any::TypeId;
    [
        TypeId::of::<Workspace>(),
        TypeId::of::<WorkspaceName>(),
        TypeId::of::<RootOrder>(),
        TypeId::of::<TemplateRoot>(),
        TypeId::of::<TemplateNode>(),
        TypeId::of::<RootOf>(),
        TypeId::of::<Node>(),
        TypeId::of::<ChildOf>(),
        TypeId::of::<Children>(),
        TypeId::of::<ZIndex>(),
        TypeId::of::<BackgroundColor>(),
        TypeId::of::<BorderColor>(),
        TypeId::of::<Name>(),
        TypeId::of::<Places>(),
        TypeId::of::<Pane>(),
        TypeId::of::<LaunchAttribution>(),
        TypeId::of::<PaneTemplate>(),
        TypeId::of::<Title>(),
        TypeId::of::<Historical>(),
        TypeId::of::<LastFocus>(),
    ]
    .contains(&type_id)
}

/// Loads the document into an inert World (same registry, relationship hooks skipped) and
/// checks what `scene::apply` cannot see across workspaces: every entity belongs to exactly
/// one workspace, roots are `Node`s without parents, pane references are panes with an
/// attribution, and the workspace, node and pane counts respect the limits. Returns the
/// workspaces in document order with their panes in pre-order.
fn validate(world: &World, dynamic: &DynamicWorld) -> R<Vec<DocWorkspace>> {
    if !dynamic.resources.is_empty() {
        return Err(SceneError::ResourcesNotAllowed.into());
    }
    for entity in &dynamic.entities {
        for component in &entity.components {
            let info = component.get_represented_type_info().ok_or_else(|| {
                SceneError::ComponentNotAllowed(component.reflect_type_path().to_owned())
            })?;
            if !allowed(info.type_id()) {
                return Err(SceneError::ComponentNotAllowed(info.type_path().to_owned()).into());
            }
        }
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut scratch = World::new();
    scratch.insert_resource(registry.clone());
    scratch.init_resource::<Ids>();
    let mut map = EntityHashMap::<Entity>::default();
    dynamic
        .write_to_world_with(&mut scratch, &mut map, &registry.read())
        .map_err(|e| SceneError::Parse(e.to_string()))?;
    let doc_of: EntityHashMap<Entity> = map.iter().map(|(doc, live)| (*live, *doc)).collect();
    let limits = world.resource::<Limits>().clone();

    // Workspaces in document order.
    let order: HashMap<Entity, usize> = dynamic
        .entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.entity, i))
        .collect();
    let mut workspaces: Vec<(Entity, String, Vec<Entity>)> = scratch
        .query::<(Entity, &WorkspaceName, &RootOrder)>()
        .iter(&scratch)
        .map(|(e, name, roots)| (e, name.0.clone(), roots.0.clone()))
        .collect();
    workspaces.sort_by_key(|(e, ..)| doc_of.get(e).and_then(|d| order.get(d)).copied());
    if workspaces.is_empty() {
        return Err(SceneError::NoWorkspace.into());
    }
    if workspaces.len() > limits.workspaces {
        return Err(LayoutError::TooManyWorkspaces {
            count: workspaces.len(),
            max: limits.workspaces,
        }
        .into());
    }
    let mut names: HashSet<String> = HashSet::default();
    for (_, name, _) in &workspaces {
        if !names.insert(name.clone()) || world.resource::<Ids>().workspace(name).is_some() {
            return Err(LayoutError::DuplicateWorkspace(name.clone()).into());
        }
    }

    // `ChildOf` sources per parent, in scratch (= document) order, for nodes without
    // `Children`; the same rule `scene::apply` uses.
    let mut sources: Vec<(Entity, Entity)> = scratch
        .query::<(Entity, &ChildOf)>()
        .iter(&scratch)
        .map(|(child, of)| (child, of.parent()))
        .collect();
    sources.sort_unstable();
    let mut children_of: HashMap<Entity, Vec<Entity>> = HashMap::default();
    for (child, parent) in sources {
        children_of.entry(parent).or_default().push(child);
    }

    let mut seen: HashSet<Entity> = HashSet::default();
    let mut plan = Vec::with_capacity(workspaces.len());
    for (ws, name, roots) in workspaces {
        if roots.is_empty() {
            return Err(SceneError::EmptyDocument.into());
        }
        seen.insert(ws);
        let mut members = vec![ws];
        let mut panes = Vec::new();
        let mut nodes = 0usize;
        for root in roots {
            if scratch.get::<ChildOf>(root).is_some() {
                return Err(SceneError::RootHasParent(root).into());
            }
            let mut stack = vec![(root, 0usize)];
            while let Some((node, depth)) = stack.pop() {
                if depth > MAX_DEPTH {
                    return Err(SceneError::DepthExceeded {
                        depth,
                        max: MAX_DEPTH,
                    }
                    .into());
                }
                if scratch.get::<Node>(node).is_none() {
                    return Err(SceneError::NotANode(node).into());
                }
                if !seen.insert(node) {
                    return Err(SceneError::Hierarchy(format!("{node} appears twice")).into());
                }
                nodes += 1;
                members.push(node);
                if let Some(pane) = scratch.get::<Places>(node).map(|p| p.0) {
                    if !seen.insert(pane) {
                        return Err(
                            SceneError::Hierarchy(format!("pane {pane} is placed twice")).into(),
                        );
                    }
                    if scratch.get::<Node>(pane).is_some() {
                        return Err(SceneError::BadPaneReference(pane).into());
                    }
                    // The attribution is the resolved launch; only the environment comes from
                    // the template (written only when the launch carried one).
                    let template = match (
                        scratch.get::<LaunchAttribution>(pane),
                        scratch.get::<PaneTemplate>(pane),
                    ) {
                        (Some(a), t) => PaneTemplate {
                            argv: a.argv.clone(),
                            cwd: a.cwd.clone(),
                            env: t.map(|t| t.env.clone()).unwrap_or_default(),
                            stream: a.stream.clone(),
                        },
                        (None, Some(t)) => t.clone(),
                        (None, None) => return Err(SceneError::BadPaneReference(pane).into()),
                    };
                    members.push(pane);
                    panes.push(DocPane {
                        template,
                        history: Historical::bounded(
                            scratch
                                .get::<Historical>(pane)
                                .map(|h| h.lines.clone())
                                .unwrap_or_default(),
                        ),
                        title: scratch.get::<Title>(pane).cloned(),
                        focus: scratch.get::<LastFocus>(pane).is_some(),
                    });
                }
                let listed: Option<Vec<Entity>> =
                    scratch.get::<Children>(node).map(|c| c.iter().collect());
                let children =
                    listed.unwrap_or_else(|| children_of.get(&node).cloned().unwrap_or_default());
                stack.extend(children.into_iter().rev().map(|c| (c, depth + 1)));
            }
        }
        if nodes > limits.nodes_per_workspace {
            return Err(SceneError::TooManyNodes {
                count: nodes,
                max: limits.nodes_per_workspace,
            }
            .into());
        }
        if panes.len() > limits.panes_per_workspace {
            return Err(SceneError::TooManyPanes {
                count: panes.len(),
                max: limits.panes_per_workspace,
            }
            .into());
        }
        let members = members
            .iter()
            .filter_map(|m| doc_of.get(m).copied())
            .collect();
        plan.push(DocWorkspace {
            name,
            members,
            panes,
        });
    }
    if let Some(dangling) = scratch
        .iter_entities()
        .map(|e| e.id())
        .find(|e| !seen.contains(e) && doc_of.contains_key(e))
    {
        return Err(SceneError::Dangling(dangling).into());
    }
    Ok(plan)
}

/// Creates the workspace and applies its document; decorates the launched panes with the
/// session-only state in pre-order (the order `scene::apply` reports launches in).
fn build_workspace(
    world: &mut World,
    ws: &DocWorkspace,
    partial: &DynamicWorld,
    mode: RestoreMode,
) -> R<(Entity, Vec<Entity>)> {
    let workspace = ops::new_workspace(world, &ws.name)?;
    let options = ApplyOptions {
        allow_templates: true,
        ..ApplyOptions::default()
    };
    let report = match scene::apply_dynamic(world, workspace, partial, &options) {
        Ok(report) => report,
        Err(error) => {
            world.despawn(workspace);
            return Err(error.into());
        }
    };
    if report.launched.len() != ws.panes.len() {
        undo_workspace(world, workspace);
        return Err(invalid(format!(
            "workspace `{}` launched {} panes for {} records",
            ws.name,
            report.launched.len(),
            ws.panes.len()
        )));
    }
    for (&pane, record) in report.launched.iter().zip(&ws.panes) {
        let mut entity = world.entity_mut(pane);
        entity.insert(record.history.clone());
        if let Some(title) = &record.title {
            entity.insert(title.clone());
        }
        if record.focus {
            entity.insert(LastFocus);
        }
        if mode == RestoreMode::Ask {
            entity.insert(RestorePending);
        }
    }
    Ok((workspace, report.launched))
}

/// Removes what a failed restoration built: nothing was materialised yet (no `Terminal`, no
/// effect), so a plain despawn is complete.
fn undo(world: &mut World, report: &RestoreReport) {
    for &ws in &report.workspaces {
        undo_workspace(world, ws);
    }
}

fn undo_workspace(world: &mut World, ws: Entity) {
    let panes: Vec<Entity> = world
        .get::<WorkspacePanes>(ws)
        .map(|p| p.iter().collect())
        .unwrap_or_default();
    let roots: Vec<Entity> = world
        .get::<RootOrder>(ws)
        .map(|o| o.0.clone())
        .unwrap_or_default();
    for root in roots {
        world.despawn(root);
    }
    for pane in panes {
        world.despawn(pane);
    }
    world.despawn(ws);
}

// ---------------------------------------------------------------------------------------------
// Decisions (`fux/session.restore` / `fux/session.skip`)
// ---------------------------------------------------------------------------------------------

/// Panes awaiting a decision, in `PaneId` order.
pub fn pending(world: &mut World) -> Vec<Entity> {
    let mut panes: Vec<(PaneId, Entity)> = world
        .query_filtered::<(Entity, &PaneId), (With<RestorePending>, Allow<Disabled>)>()
        .iter(world)
        .map(|(e, id)| (*id, e))
        .collect();
    panes.sort_unstable();
    panes.into_iter().map(|(_, e)| e).collect()
}

/// Lets the lifecycle materialise a pending pane. `Err` when the pane is not pending.
pub fn decide_restore(world: &mut World, pane: Entity) -> R<()> {
    let mut entity = world
        .get_entity_mut(pane)
        .map_err(|_| invalid(format!("{pane} is not a pane")))?;
    if entity.take::<RestorePending>().is_none() {
        return Err(invalid(format!("{pane} is not awaiting a decision")));
    }
    Ok(())
}

/// Closes a pending pane without launching it; its leaf collapses through the lifecycle.
pub fn decide_skip(world: &mut World, pane: Entity) -> R<()> {
    decide_restore(world, pane)?;
    world.entity_mut(pane).remove::<Historical>();
    lifecycle::close_pane(world, pane).map_err(|e| invalid(e.to_string()))
}
