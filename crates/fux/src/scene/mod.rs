//! Scenes (prompt 3.4 "Scenes", 3.8 file discipline, 3.9 `fux/scene.*`): a workspace's layout
//! as a `DynamicWorld` of the allowlisted **template** subgraph, serialised as RON.
//!
//! * [`export`] extracts, for one workspace, a synthetic workspace entity (`WorkspaceName`,
//!   `RootOrder`), every template node of every root in order (`Node`, `ChildOf`/`Children`,
//!   `ZIndex`, `BackgroundColor`, `BorderColor`, `Name`, `NodeId`, `RootOf`) and, behind each
//!   placing leaf, a minimal pane entity (`PaneId`, `LaunchAttribution`) linked by `Places`.
//!   Entity ids are the server's; [`apply`] maps them.
//! * [`apply`] deserialises a document into an inert scratch `World` with the same
//!   `AppTypeRegistry`, validates everything (allowlist, hierarchy, depth and count limits, pane
//!   references, templates, generations), and only then commits: the workspace's roots are
//!   replaced by the document's, built through `layout::ops`, so instances re-clone on their
//!   own. A plain scene references existing panes by `PaneId` and never launches a process; a
//!   *template scene* (leaves carrying `PaneTemplate`, or pane entities whose `PaneId` no longer
//!   resolves but carry `LaunchAttribution`) launches through the same `Requests` path as any
//!   spawn.
//! * [`save`]/[`load`]/[`list`] keep documents under `<config_dir>/layouts/<name>.scn.ron`
//!   (atomic write: temp + rename); [`restore`] loads a user file or a [`builtin`] and applies
//!   it. Users write layouts as scene assets and select them by name.

pub mod builtin;

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use bevy_asset::AssetServer;
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_platform::collections::{HashMap, HashSet};
use bevy_ui::{BackgroundColor, BorderColor, Node, ZIndex};
use bevy_world_serialization::serde::WorldDeserializer;
use bevy_world_serialization::{DynamicWorld, DynamicWorldBuilder};
use serde::de::DeserializeSeed as _;

use crate::layout::{LayoutError, ops};
use crate::lifecycle;
use crate::model::invariants::root_of_template;
use crate::model::*;
use crate::remote::methods::{MAX_ARG_BYTES, MAX_ARGV_ENTRIES, MAX_ENV_BYTES, MAX_ENV_ENTRIES};

/// Bound on a document accepted by [`apply`] or read by [`load`].
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Layout file names: `[A-Za-z0-9_-]{1,64}`.
pub const MAX_NAME_LEN: usize = 64;
/// Layout files: `<name>.scn.ron`.
pub const EXTENSION: &str = ".scn.ron";
/// Directory under the config directory holding user layouts.
pub const LAYOUTS_DIR: &str = "layouts";

/// Where user layouts live (`<config_dir>/layouts`); inserted by the app from `Paths`.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct LayoutDir(pub PathBuf);

impl LayoutDir {
    pub fn new(paths: &crate::paths::Paths) -> Self {
        Self(paths.config_dir.join(LAYOUTS_DIR))
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Typed refusal of a scene operation. Every variant leaves the World untouched.
#[derive(Debug, Clone, PartialEq)]
pub enum SceneError {
    /// The RON or a reflected component could not be read (unknown type paths included).
    Parse(String),
    Serialize(String),
    /// The World lacks the `AssetServer` the reflect deserializer needs for handle paths.
    NoAssetServer,
    ResourcesNotAllowed,
    /// A registered component outside the layout allowlist.
    ComponentNotAllowed(String),
    /// Exactly one document entity carries `RootOrder`.
    NoWorkspace,
    MultipleWorkspaces,
    EmptyDocument,
    /// A document entity referenced as a root or child has no `Node`.
    NotANode(Entity),
    RootHasParent(Entity),
    /// `ChildOf` and `Children` disagree, or a node is listed twice.
    Hierarchy(String),
    /// A document entity that is neither the workspace, a node under a root nor a pane entity
    /// referenced by exactly one leaf.
    Dangling(Entity),
    /// A placing leaf with children, or a leaf carrying both `Places` and `PaneTemplate`.
    BadLeaf(Entity),
    /// `Places` names an entity that is neither a pane reference nor a template.
    BadPaneReference(Entity),
    DepthExceeded {
        depth: usize,
        max: usize,
    },
    TooManyNodes {
        count: usize,
        max: usize,
    },
    TooManyPanes {
        count: usize,
        max: usize,
    },
    TooManyLeaves {
        count: usize,
        max: usize,
    },
    PaneNotFound(PaneId),
    ForeignPane(PaneId),
    /// The pane has exited and is closing.
    PaneNotPlaceable(PaneId),
    DuplicatePane(PaneId),
    TemplatesNotAllowed,
    InvalidTemplate(String),
    InvalidName(String),
    StaleGeneration {
        expected: Vec<(NodeId, u64)>,
        current: Vec<(NodeId, u64)>,
    },
    /// Live panes the document does not place; pass `close_unplaced` to close them.
    UnplacedPanes(Vec<PaneId>),
    Layout(LayoutError),
    /// `[A-Za-z0-9_-]{1,64}`.
    InvalidFileName(String),
    NotFound(String),
    TooLarge {
        bytes: usize,
        max: usize,
    },
    NoLayoutDir,
    Io(String),
}

impl core::fmt::Display for SceneError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "scene does not parse: {e}"),
            Self::Serialize(e) => write!(f, "scene does not serialize: {e}"),
            Self::NoAssetServer => write!(f, "no AssetServer to deserialize with"),
            Self::ResourcesNotAllowed => write!(f, "a layout scene carries no resources"),
            Self::ComponentNotAllowed(path) => {
                write!(f, "component `{path}` is not allowed in a layout scene")
            }
            Self::NoWorkspace => write!(f, "the document has no workspace entity (RootOrder)"),
            Self::MultipleWorkspaces => {
                write!(f, "the document has more than one workspace entity")
            }
            Self::EmptyDocument => write!(f, "the document has no roots"),
            Self::NotANode(e) => write!(f, "document entity {e} is used as a node but has no Node"),
            Self::RootHasParent(e) => write!(f, "root {e} has a parent"),
            Self::Hierarchy(why) => write!(f, "inconsistent hierarchy: {why}"),
            Self::Dangling(e) => write!(f, "document entity {e} is not part of the layout"),
            Self::BadLeaf(e) => write!(
                f,
                "leaf {e} places a pane and has children, or places two things"
            ),
            Self::BadPaneReference(e) => {
                write!(f, "{e} is neither a pane reference nor a template")
            }
            Self::DepthExceeded { depth, max } => write!(f, "depth {depth} exceeds {max}"),
            Self::TooManyNodes { count, max } => {
                write!(f, "{count} nodes exceed the limit of {max}")
            }
            Self::TooManyPanes { count, max } => {
                write!(f, "{count} panes exceed the limit of {max}")
            }
            Self::TooManyLeaves { count, max } => {
                write!(f, "{count} leaves in one root exceed {max}")
            }
            Self::PaneNotFound(id) => write!(f, "pane {id} not found"),
            Self::ForeignPane(id) => write!(f, "pane {id} belongs to another workspace"),
            Self::PaneNotPlaceable(id) => write!(f, "pane {id} has exited"),
            Self::DuplicatePane(id) => write!(f, "pane {id} is placed twice"),
            Self::TemplatesNotAllowed => {
                write!(f, "the document launches panes; pass allow_templates")
            }
            Self::InvalidTemplate(why) => write!(f, "invalid pane template: {why}"),
            Self::InvalidName(name) => write!(f, "invalid name {name:?}"),
            Self::StaleGeneration { expected, current } => {
                write!(f, "stale roots {expected:?}; the workspace has {current:?}")
            }
            Self::UnplacedPanes(panes) => {
                write!(
                    f,
                    "the document leaves {} live pane(s) unplaced (",
                    panes.len()
                )?;
                for (i, p) in panes.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, "); pass close_unplaced to close them")
            }
            Self::Layout(e) => write!(f, "{e}"),
            Self::InvalidFileName(name) => write!(f, "invalid layout name {name:?}"),
            Self::NotFound(name) => write!(f, "no layout named {name:?}"),
            Self::TooLarge { bytes, max } => write!(f, "document of {bytes} bytes exceeds {max}"),
            Self::NoLayoutDir => write!(f, "no layout directory is configured"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SceneError {}

impl From<LayoutError> for SceneError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

impl From<std::io::Error> for SceneError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

type R<T> = Result<T, SceneError>;

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

/// An exported workspace layout with the state a later [`apply`] can be guarded by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// RON `DynamicWorld`.
    pub document: String,
    /// Template roots in `RootOrder` order with their `NodeId` and `LayoutGeneration`: what a
    /// later [`apply`] names in [`ApplyOptions::expected`].
    pub roots: Vec<(Entity, NodeId, u64)>,
}

/// `(root, NodeId, LayoutGeneration)` of every root of the workspace in `RootOrder` order.
pub fn root_states(world: &World, workspace: Entity) -> Vec<(Entity, NodeId, u64)> {
    world
        .get::<RootOrder>(workspace)
        .map(|o| o.0.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|&r| {
            (
                r,
                world.get::<NodeId>(r).copied().unwrap_or(NodeId(0)),
                world.get::<LayoutGeneration>(r).map_or(0, |g| g.0),
            )
        })
        .collect()
}

/// The template subgraph of `workspace` as a RON document.
pub fn export(world: &World, workspace: Entity) -> R<Export> {
    check_workspace(world, workspace)?;
    let states = root_states(world, workspace);
    let roots: Vec<Entity> = states.iter().map(|s| s.0).collect();
    let mut extract = vec![workspace];
    for &root in &roots {
        walk(world, root, &mut |entity| {
            extract.push(entity);
            if let Some(places) = world.get::<Places>(entity) {
                extract.push(places.0);
            }
        });
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
        .allow_component::<NodeId>()
        .allow_component::<Places>()
        .allow_component::<Pane>()
        .allow_component::<PaneId>()
        .allow_component::<LaunchAttribution>()
        .extract_entities(extract.into_iter())
        .remove_empty_entities()
        .build();
    // In the World a root is a child of its workspace (so `bevy_ui` never lays it out); in the
    // document roots are parentless and the workspace lists them only through `RootOrder`.
    fn is_type(component: &dyn bevy_reflect::PartialReflect, id: core::any::TypeId) -> bool {
        component
            .get_represented_type_info()
            .is_some_and(|info| info.type_id() == id)
    }
    for entity in &mut dynamic.entities {
        if entity.entity == workspace {
            entity
                .components
                .retain(|c| !is_type(c.as_ref(), core::any::TypeId::of::<Children>()));
        } else if roots.contains(&entity.entity) {
            entity
                .components
                .retain(|c| !is_type(c.as_ref(), core::any::TypeId::of::<ChildOf>()));
        }
    }
    let document = dynamic
        .serialize(&registry)
        .map_err(|e| SceneError::Serialize(e.to_string()))?;
    Ok(Export {
        document,
        roots: states,
    })
}

/// Pre-order walk of a template subtree, not descending into surface leaves (their subtrees
/// belong to their providers, prompt 3.13).
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

// ---------------------------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------------------------

/// How [`apply`] treats what the document does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOptions {
    /// `(NodeId, LayoutGeneration)` of every current root in `RootOrder` order, as the caller
    /// saw them (an [`Export`]); any difference, a replaced root included, refuses the apply.
    /// `None` skips the check.
    pub expected: Option<Vec<(NodeId, u64)>>,
    /// Leaves may launch processes (`PaneTemplate`, or unresolvable pane references carrying
    /// `LaunchAttribution`).
    pub allow_templates: bool,
    /// Live panes the document does not reference fill its launching leaves, in the order both
    /// appear, before anything is launched.
    pub adopt: bool,
    /// Live panes the document leaves unplaced are closed through the lifecycle instead of
    /// refusing the apply.
    pub close_unplaced: bool,
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// The new template roots in `RootOrder` order with their generations.
    pub roots: Vec<(Entity, u64)>,
    /// Panes created (`Disabled`, `Process::Starting`) for launching leaves.
    pub launched: Vec<Entity>,
    /// Existing panes that filled launching leaves.
    pub adopted: Vec<Entity>,
    /// Panes closed because the document did not place them.
    pub closed: Vec<Entity>,
}

/// One validated leaf.
#[derive(Debug, Clone)]
enum Leaf {
    Existing(Entity),
    Launch(PaneTemplate),
}

/// One validated node of the document, ready to build.
#[derive(Debug, Clone)]
struct PlanNode {
    node: Node,
    name: Option<String>,
    z_index: Option<ZIndex>,
    background: Option<BackgroundColor>,
    border: Option<BorderColor>,
    leaf: Option<Leaf>,
    children: Vec<PlanNode>,
}

/// Everything decided before the first write.
struct Plan {
    roots: Vec<PlanNode>,
    /// Live panes of the old roots that the document does not place (after adoption).
    unplaced: Vec<Entity>,
    /// Unplaced panes that fill launching leaves instead.
    adopted: Vec<Entity>,
}

/// Replaces the layout of `workspace` with `document`. Validates everything first; a refusal
/// leaves the World untouched.
pub fn apply(
    world: &mut World,
    workspace: Entity,
    document: &str,
    options: &ApplyOptions,
) -> R<ApplyReport> {
    check_workspace(world, workspace)?;
    if world.get::<Open>(workspace).is_none() {
        return Err(LayoutError::WorkspaceRetiring(workspace).into());
    }
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(SceneError::TooLarge {
            bytes: document.len(),
            max: MAX_DOCUMENT_BYTES,
        });
    }
    let old_roots: Vec<Entity> = world
        .get::<RootOrder>(workspace)
        .map(|o| o.0.clone())
        .unwrap_or_default();
    if let Some(expected) = &options.expected {
        let current: Vec<(NodeId, u64)> = root_states(world, workspace)
            .into_iter()
            .map(|(_, id, generation)| (id, generation))
            .collect();
        if *expected != current {
            return Err(SceneError::StaleGeneration {
                expected: expected.clone(),
                current,
            });
        }
    }

    let dynamic = parse(world, document)?;
    let (mut scratch, entities) = scratch_world(world, &dynamic)?;
    let plan = validate(
        world,
        &mut scratch,
        &entities,
        workspace,
        &old_roots,
        options,
    )?;
    Ok(commit(world, workspace, &old_roots, plan))
}

fn parse(world: &World, document: &str) -> R<DynamicWorld> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut assets = world
        .get_resource::<AssetServer>()
        .cloned()
        .ok_or(SceneError::NoAssetServer)?;
    let mut de =
        ron::de::Deserializer::from_str(document).map_err(|e| SceneError::Parse(e.to_string()))?;
    let seed = WorldDeserializer {
        type_registry: &registry.read(),
        load_from_path: &mut assets,
    };
    seed.deserialize(&mut de)
        .map_err(|e| SceneError::Parse(de.span_error(e).to_string()))
}

/// Registered components a document may carry; anything else registered is refused before it
/// touches a World.
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
        TypeId::of::<NodeId>(),
        TypeId::of::<Places>(),
        TypeId::of::<Pane>(),
        TypeId::of::<PaneId>(),
        TypeId::of::<LaunchAttribution>(),
        TypeId::of::<PaneTemplate>(),
    ]
    .contains(&type_id)
}

/// An inert World holding the document: same registry, an `Ids` index for the id hooks,
/// relationship hooks skipped (`Children` comes from the document as written). Returns the
/// scratch entities in document order (resources are entities too; they are not part of it).
fn scratch_world(world: &World, dynamic: &DynamicWorld) -> R<(World, Vec<Entity>)> {
    if !dynamic.resources.is_empty() {
        return Err(SceneError::ResourcesNotAllowed);
    }
    for entity in &dynamic.entities {
        for component in &entity.components {
            let info = component.get_represented_type_info().ok_or_else(|| {
                SceneError::ComponentNotAllowed(component.reflect_type_path().to_owned())
            })?;
            if !allowed(info.type_id()) {
                return Err(SceneError::ComponentNotAllowed(info.type_path().to_owned()));
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
    let entities = dynamic
        .entities
        .iter()
        .filter_map(|e| map.get(&e.entity).copied())
        .collect();
    Ok((scratch, entities))
}

fn validate(
    world: &World,
    scratch: &mut World,
    entities: &[Entity],
    workspace: Entity,
    old_roots: &[Entity],
    options: &ApplyOptions,
) -> R<Plan> {
    // The workspace entity and its root list.
    let workspaces: Vec<(Entity, Vec<Entity>)> = scratch
        .query::<(Entity, &RootOrder)>()
        .iter(scratch)
        .map(|(e, o)| (e, o.0.clone()))
        .collect();
    let (ws_entity, roots) = match workspaces.as_slice() {
        [] => return Err(SceneError::NoWorkspace),
        [one] => one.clone(),
        _ => return Err(SceneError::MultipleWorkspaces),
    };
    if roots.is_empty() {
        return Err(SceneError::EmptyDocument);
    }
    let limits = world.resource::<Limits>().clone();

    // Hierarchy: every Node entity is reachable from exactly one root; `Children` fixes sibling
    // order and must agree with `ChildOf`; without `Children`, `ChildOf` sources in entity order.
    let mut sources: Vec<(Entity, Entity)> = scratch
        .query::<(Entity, &ChildOf)>()
        .iter(scratch)
        .map(|(child, of)| (child, of.parent()))
        .collect();
    sources.sort_unstable();
    let mut children_of: HashMap<Entity, Vec<Entity>> = HashMap::default();
    for (child, parent) in sources {
        children_of.entry(parent).or_default().push(child);
    }
    let mut seen: HashSet<Entity> = HashSet::default();
    seen.insert(ws_entity);
    let mut plan_roots = Vec::with_capacity(roots.len());
    let mut refs: HashMap<Entity, usize> = HashMap::default();
    for &root in &roots {
        if !seen.insert(root) {
            return Err(SceneError::Hierarchy(format!("root {root} listed twice")));
        }
        if scratch.get::<ChildOf>(root).is_some() {
            return Err(SceneError::RootHasParent(root));
        }
        if let Some(of) = scratch.get::<RootOf>(root)
            && of.0 != ws_entity
        {
            return Err(SceneError::Hierarchy(format!(
                "root {root} belongs to another workspace entity"
            )));
        }
        let mut leaves = 0usize;
        let node = plan_node(
            scratch,
            root,
            0,
            &mut seen,
            &mut refs,
            &children_of,
            &mut leaves,
        )?;
        if leaves > MAX_LEAVES {
            return Err(SceneError::TooManyLeaves {
                count: leaves,
                max: MAX_LEAVES,
            });
        }
        plan_roots.push(node);
    }
    let node_count = seen.len() - 1;
    if node_count > limits.nodes_per_workspace {
        return Err(SceneError::TooManyNodes {
            count: node_count,
            max: limits.nodes_per_workspace,
        });
    }

    // Pane references: each referenced document entity is used by exactly one leaf and resolves
    // to a placeable pane of the workspace, or is a template.
    for (entity, count) in &refs {
        if *count > 1 {
            let id = scratch.get::<PaneId>(*entity).copied().unwrap_or(PaneId(0));
            return Err(SceneError::DuplicatePane(id));
        }
        if scratch.get::<Node>(*entity).is_some() {
            return Err(SceneError::BadPaneReference(*entity));
        }
        seen.insert(*entity);
    }
    for &entity in entities {
        if !seen.contains(&entity) {
            return Err(SceneError::Dangling(entity));
        }
    }
    let mut resolver = Resolver {
        world,
        scratch,
        workspace,
        options,
        default: lifecycle::default_template(world),
        referenced: Vec::new(),
        launches: 0,
    };
    resolver.leaves(&mut plan_roots)?;
    let Resolver {
        referenced,
        mut launches,
        ..
    } = resolver;
    let mut unique: HashSet<Entity> = HashSet::default();
    for &pane in &referenced {
        if !unique.insert(pane) {
            let id = world.get::<PaneId>(pane).copied().unwrap_or(PaneId(0));
            return Err(SceneError::DuplicatePane(id));
        }
    }

    // Panes of the old roots the document does not place: adopted into launching leaves, then
    // closed or refused.
    let mut unplaced: Vec<Entity> = Vec::new();
    for &root in old_roots {
        walk(world, root, &mut |entity| {
            if let Some(places) = world.get::<Places>(entity)
                && !unique.contains(&places.0)
                && !matches!(world.get::<Process>(places.0), Some(Process::Exited { .. }))
            {
                unplaced.push(places.0);
            }
        });
    }
    let mut adopted = Vec::new();
    if options.adopt {
        let mut pool = unplaced.into_iter();
        adopt(&mut plan_roots, &mut pool, &mut adopted);
        unplaced = pool.collect();
        launches -= adopted.len();
    }
    let closing = if options.close_unplaced {
        unplaced.len()
    } else if unplaced.is_empty() {
        0
    } else {
        return Err(SceneError::UnplacedPanes(
            unplaced
                .iter()
                .filter_map(|&p| world.get::<PaneId>(p).copied())
                .collect(),
        ));
    };
    // The net count after the commit: closed panes leave `WorkspacePanes` only when the
    // lifecycle despawns them, so `commit` cannot recheck this against the live World.
    let panes = world
        .get::<WorkspacePanes>(workspace)
        .map_or(0, |p| p.len())
        .saturating_sub(closing)
        + launches;
    if panes > limits.panes_per_workspace {
        return Err(SceneError::TooManyPanes {
            count: panes,
            max: limits.panes_per_workspace,
        });
    }
    Ok(Plan {
        roots: plan_roots,
        unplaced,
        adopted,
    })
}

/// Builds the plan for one document subtree, checking depth, names and leaf shape.
fn plan_node(
    scratch: &World,
    entity: Entity,
    depth: usize,
    seen: &mut HashSet<Entity>,
    refs: &mut HashMap<Entity, usize>,
    children_of: &HashMap<Entity, Vec<Entity>>,
    leaves: &mut usize,
) -> R<PlanNode> {
    if depth > MAX_DEPTH {
        return Err(SceneError::DepthExceeded {
            depth,
            max: MAX_DEPTH,
        });
    }
    let node = scratch
        .get::<Node>(entity)
        .cloned()
        .ok_or(SceneError::NotANode(entity))?;
    let name = scratch.get::<Name>(entity).map(|n| n.as_str().to_owned());
    if let Some(name) = &name {
        check_name(name)?;
    }
    let places = scratch.get::<Places>(entity).map(|p| p.0);
    let template = scratch.get::<PaneTemplate>(entity).cloned();
    let leaf = match (places, template) {
        (Some(_), Some(_)) => return Err(SceneError::BadLeaf(entity)),
        (Some(pane), None) => {
            *refs.entry(pane).or_default() += 1;
            Some(Leaf::Existing(pane))
        }
        (None, Some(template)) => Some(Leaf::Launch(template)),
        (None, None) => None,
    };
    // Children: the `Children` list when present, else `ChildOf` sources in entity order.
    let listed: Option<Vec<Entity>> = scratch.get::<Children>(entity).map(|c| c.iter().collect());
    let by_parent: Vec<Entity> = children_of.get(&entity).cloned().unwrap_or_default();
    let children_ids = match listed {
        Some(listed) => {
            for &child in &listed {
                let of = scratch.get::<ChildOf>(child);
                if of.is_none_or(|of| of.parent() != entity) {
                    return Err(SceneError::Hierarchy(format!(
                        "{child} is listed under {entity} but has another parent"
                    )));
                }
            }
            for &child in &by_parent {
                if !listed.contains(&child) {
                    return Err(SceneError::Hierarchy(format!(
                        "{child} names {entity} as parent but is not in its Children"
                    )));
                }
            }
            listed
        }
        None => by_parent,
    };
    if leaf.is_some() && !children_ids.is_empty() {
        return Err(SceneError::BadLeaf(entity));
    }
    if leaf.is_some() {
        *leaves += 1;
    }
    let mut children = Vec::with_capacity(children_ids.len());
    for child in children_ids {
        if !seen.insert(child) {
            return Err(SceneError::Hierarchy(format!("{child} appears twice")));
        }
        children.push(plan_node(
            scratch,
            child,
            depth + 1,
            seen,
            refs,
            children_of,
            leaves,
        )?);
    }
    Ok(PlanNode {
        node,
        name,
        z_index: scratch.get::<ZIndex>(entity).copied(),
        background: scratch.get::<BackgroundColor>(entity).copied(),
        border: scratch.get::<BorderColor>(entity).copied(),
        leaf,
        children,
    })
}

/// Turns pane references into target-World panes (or launch templates) and validates every
/// template; counts what it referenced and what it will launch.
struct Resolver<'a> {
    world: &'a World,
    scratch: &'a World,
    workspace: Entity,
    options: &'a ApplyOptions,
    default: PaneTemplate,
    referenced: Vec<Entity>,
    launches: usize,
}

impl Resolver<'_> {
    fn leaves(&mut self, nodes: &mut [PlanNode]) -> R<()> {
        for node in nodes {
            if let Some(leaf) = node.leaf.take() {
                node.leaf = Some(self.leaf(leaf)?);
            }
            self.leaves(&mut node.children)?;
        }
        Ok(())
    }

    fn leaf(&mut self, leaf: Leaf) -> R<Leaf> {
        let resolved = match leaf {
            Leaf::Launch(template) => Leaf::Launch(template),
            Leaf::Existing(doc_pane) => self.reference(doc_pane)?,
        };
        Ok(match resolved {
            Leaf::Existing(pane) => {
                self.referenced.push(pane);
                Leaf::Existing(pane)
            }
            Leaf::Launch(template) => {
                if !self.options.allow_templates {
                    return Err(SceneError::TemplatesNotAllowed);
                }
                self.launches += 1;
                Leaf::Launch(complete_template(template, &self.default)?)
            }
        })
    }

    /// A document pane entity: a `PaneId` of this workspace, or (templates allowed) the
    /// `LaunchAttribution` of a pane that no longer exists, or a bare `PaneTemplate`.
    fn reference(&self, doc_pane: Entity) -> R<Leaf> {
        let Some(id) = self.scratch.get::<PaneId>(doc_pane).copied() else {
            return match self.scratch.get::<PaneTemplate>(doc_pane) {
                Some(template) => Ok(Leaf::Launch(template.clone())),
                None => Err(SceneError::BadPaneReference(doc_pane)),
            };
        };
        match self.world.resource::<Ids>().pane(id) {
            Some(pane) => {
                if self.world.get::<PaneIn>(pane).map(|p| p.0) != Some(self.workspace) {
                    return Err(SceneError::ForeignPane(id));
                }
                if matches!(
                    self.world.get::<Process>(pane),
                    Some(Process::Exited { .. })
                ) {
                    return Err(SceneError::PaneNotPlaceable(id));
                }
                Ok(Leaf::Existing(pane))
            }
            None => match self.scratch.get::<LaunchAttribution>(doc_pane) {
                Some(a) if self.options.allow_templates => Ok(Leaf::Launch(PaneTemplate {
                    argv: a.argv.clone(),
                    cwd: a.cwd.clone(),
                    env: Vec::new(),
                    stream: a.stream.clone(),
                })),
                _ => Err(SceneError::PaneNotFound(id)),
            },
        }
    }
}

/// An empty `argv` means the configured default command; the rest is bounded like a
/// `fux/pane.new` template.
fn complete_template(mut template: PaneTemplate, default: &PaneTemplate) -> R<PaneTemplate> {
    if template.argv.is_empty() {
        template.argv.clone_from(&default.argv);
        if template.cwd.is_none() {
            template.cwd.clone_from(&default.cwd);
        }
        if template.env.is_empty() {
            template.env.clone_from(&default.env);
        }
    }
    if template.stream.is_empty() {
        template.stream.clone_from(&default.stream);
    }
    let invalid = |why: &str| SceneError::InvalidTemplate(why.to_owned());
    if template.argv.len() > MAX_ARGV_ENTRIES {
        return Err(invalid("too many argv entries"));
    }
    if template.argv.first().is_none_or(|first| first.is_empty()) {
        return Err(invalid("argv must start with a non-empty executable"));
    }
    if template
        .argv
        .iter()
        .any(|a| a.len() > MAX_ARG_BYTES || a.contains('\0'))
    {
        return Err(invalid("argv entries must be bounded and contain no NUL"));
    }
    if template.env.len() > MAX_ENV_ENTRIES {
        return Err(invalid("too many environment entries"));
    }
    let mut total = 0usize;
    for (name, value) in &template.env {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(invalid(
                "environment names are non-empty without `=` or NUL",
            ));
        }
        total = total.saturating_add(name.len()).saturating_add(value.len());
    }
    if total > MAX_ENV_BYTES {
        return Err(invalid("environment too large"));
    }
    if template.cwd.as_deref().is_some_and(|c| c.contains('\0')) {
        return Err(invalid("cwd contains NUL"));
    }
    Ok(template)
}

/// Launching leaves take the unplaced panes in document order.
fn adopt(
    nodes: &mut [PlanNode],
    pool: &mut impl Iterator<Item = Entity>,
    adopted: &mut Vec<Entity>,
) {
    for node in nodes {
        if matches!(node.leaf, Some(Leaf::Launch(_)))
            && let Some(pane) = pool.next()
        {
            node.leaf = Some(Leaf::Existing(pane));
            adopted.push(pane);
        }
        adopt(&mut node.children, pool, adopted);
    }
}

/// Writes the plan: old roots go, new roots are built with the unchecked `layout::ops`
/// constructors (every check they skip was made against the plan, so nothing here can
/// refuse and leave the workspace half-written), viewers follow their panes.
fn commit(world: &mut World, workspace: Entity, old_roots: &[Entity], plan: Plan) -> ApplyReport {
    let viewers: Vec<(Entity, bool, Option<usize>, Option<Entity>)> = world
        .get::<ViewedBy>(workspace)
        .map(|v| v.iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|viewer| {
            let exact = world.get::<ExactTarget>(viewer).is_some();
            let index = world
                .get::<Showing>(viewer)
                .and_then(|s| old_roots.iter().position(|r| *r == s.0));
            let target = world.get::<Targets>(viewer).map(|t| t.0);
            (viewer, exact, index, target)
        })
        .collect();

    if let Some(mut order) = world.get_mut::<RootOrder>(workspace) {
        order.0.clear();
    }
    for &root in old_roots {
        world.despawn(root);
    }

    let mut report = ApplyReport {
        adopted: plan.adopted,
        ..ApplyReport::default()
    };
    for &pane in &plan.unplaced {
        if let Err(error) = lifecycle::close_pane(world, pane) {
            bevy_log::warn!("scene apply: close unplaced pane {pane}: {error}");
        }
        world.entity_mut(pane).insert(Disabled);
        report.closed.push(pane);
    }
    for plan_root in plan.roots {
        let name = plan_root.name.as_deref().unwrap_or("root");
        let root = ops::create_root(world, workspace, name);
        world.entity_mut(root).insert(plan_root.node.clone());
        decorate(world, root, &plan_root);
        build_children(world, workspace, root, plan_root.children, &mut report);
        let generation = world.get::<LayoutGeneration>(root).map_or(0, |g| g.0);
        report.roots.push((root, generation));
    }

    let new_roots: Vec<Entity> = report.roots.iter().map(|(r, _)| *r).collect();
    for (viewer, exact, index, target) in viewers {
        let placed_root = target
            .and_then(|pane| world.get::<PlacedIn>(pane))
            .and_then(|leaves| leaves.iter().next())
            .and_then(|leaf| root_of_template(world, leaf));
        let next = placed_root.or_else(|| {
            if exact {
                None
            } else {
                index
                    .and_then(|i| new_roots.get(i))
                    .or(new_roots.last())
                    .copied()
            }
        });
        match next {
            Some(root) => {
                if let Err(error) = ops::show_root(world, viewer, root) {
                    bevy_log::warn!("scene apply: reshow viewer {viewer}: {error}");
                }
            }
            // Its pane is no longer placed anywhere (closed above): the viewer shows and
            // targets nothing rather than a `Disabled` pane (prompt 3.5), and an exact
            // attachment leaves as it would had the pane exited (`ExactTargetLost`).
            None => {
                let mut entity = world.entity_mut(viewer);
                entity.remove::<(Showing, Targets)>();
                if exact {
                    entity.insert(Detaching);
                }
            }
        }
    }
    report
}

/// The optional decorations of a planned node; its `Node` is part of its spawn bundle.
fn decorate(world: &mut World, entity: Entity, plan: &PlanNode) {
    let mut e = world.entity_mut(entity);
    if let Some(name) = &plan.name {
        e.insert(Name::new(name.clone()));
    }
    if let Some(z) = plan.z_index {
        e.insert(z);
    }
    if let Some(bg) = plan.background {
        e.insert(bg);
    }
    if let Some(border) = plan.border {
        e.insert(border);
    }
}

fn build_children(
    world: &mut World,
    workspace: Entity,
    parent: Entity,
    children: Vec<PlanNode>,
    report: &mut ApplyReport,
) {
    for mut child in children {
        let template = match &mut child.leaf {
            Some(Leaf::Launch(template)) => Some(core::mem::take(template)),
            _ => None,
        };
        let node = core::mem::take(&mut child.node);
        let at = world.get::<Children>(parent).map_or(0, |c| c.len());
        let node = ops::create_node(world, workspace, parent, at, node, template);
        decorate(world, node, &child);
        match child.leaf {
            Some(Leaf::Existing(pane)) => {
                world.entity_mut(node).insert(Places(pane));
            }
            Some(Leaf::Launch(_)) => {
                if let Some(pane) = world.get::<Places>(node).map(|p| p.0) {
                    world.entity_mut(pane).insert(Creation {
                        requesters: vec![Requester::Server],
                        kind: CreationKind::Restore,
                    });
                    report.launched.push(pane);
                }
            }
            None => {}
        }
        build_children(world, workspace, node, child.children, report);
    }
}

// ---------------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------------

/// `[A-Za-z0-9_-]{1,64}`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn file_of(dir: &Path, name: &str) -> R<PathBuf> {
    if !valid_name(name) {
        return Err(SceneError::InvalidFileName(name.to_owned()));
    }
    Ok(dir.join(format!("{name}{EXTENSION}")))
}

/// Writes `document` to `<dir>/<name>.scn.ron` atomically (0600 temp file in the same
/// directory, fsync, rename); a failure leaves no partial file behind.
pub fn save(dir: &Path, name: &str, document: &str) -> R<PathBuf> {
    let path = file_of(dir, name)?;
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(SceneError::TooLarge {
            bytes: document.len(),
            max: MAX_DOCUMENT_BYTES,
        });
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let temp = dir.join(format!(
        ".{name}{EXTENSION}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(document.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(path)
}

/// Reads `<dir>/<name>.scn.ron`.
pub fn load(dir: &Path, name: &str) -> R<String> {
    let path = file_of(dir, name)?;
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SceneError::NotFound(name.to_owned()));
        }
        Err(e) => return Err(e.into()),
    };
    let bytes = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    if bytes > MAX_DOCUMENT_BYTES {
        return Err(SceneError::TooLarge {
            bytes,
            max: MAX_DOCUMENT_BYTES,
        });
    }
    Ok(std::fs::read_to_string(&path)?)
}

/// Names of the saved layouts in `dir`, sorted; a missing directory lists nothing.
pub fn list(dir: &Path) -> R<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file = entry.file_name();
        let Some(file) = file.to_str() else {
            continue;
        };
        if let Some(name) = file.strip_suffix(EXTENSION)
            && valid_name(name)
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            names.push(name.to_owned());
        }
    }
    names.sort_unstable();
    Ok(names)
}

/// The saved layout `name`, or the built-in of that name when no file exists.
pub fn document_named(dir: &Path, name: &str) -> R<String> {
    match load(dir, name) {
        Err(SceneError::NotFound(_)) => builtin::document(name)
            .map(str::to_owned)
            .ok_or_else(|| SceneError::NotFound(name.to_owned())),
        other => other,
    }
}

/// [`document_named`] + [`apply`] with templates allowed (restoring a layout launches what it
/// needs).
pub fn restore(
    world: &mut World,
    workspace: Entity,
    dir: &Path,
    name: &str,
    options: &ApplyOptions,
) -> R<ApplyReport> {
    let document = document_named(dir, name)?;
    let options = ApplyOptions {
        allow_templates: true,
        ..options.clone()
    };
    apply(world, workspace, &document, &options)
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn check_workspace(world: &World, ws: Entity) -> R<()> {
    match world.get_entity(ws) {
        Ok(entity) if entity.contains::<Workspace>() => Ok(()),
        Ok(_) => Err(LayoutError::NotAWorkspace(ws).into()),
        Err(_) => Err(LayoutError::NoSuchEntity(ws).into()),
    }
}

/// The rule `layout::ops` applies to root and node names.
fn check_name(name: &str) -> R<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
        && name.trim() == name;
    if ok {
        Ok(())
    } else {
        Err(SceneError::InvalidName(name.to_owned()))
    }
}
