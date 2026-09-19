//! Surfaces (prompt 3.13): a template leaf whose subtree is not a terminal but a scene another
//! app streams. [`open`] marks the leaf, [`update`] validates a RON `DynamicWorld` restricted to
//! the surface vocabulary and writes it into the **template** subtree under the leaf through the
//! surface's provider-id → entity map (`DynamicWorld::write_to_world_with`), [`close`] despawns
//! the subtree. Every commit bumps the root's [`LayoutGeneration`], so instances re-clone and
//! viewers receive the subtree like any other nodes. Input travels back to the provider as
//! [`SurfaceInput`] events on `fux/events+watch`: [`pointer_input`] from the pointer policy
//! (`pointer.rs`) for presses, releases and the wheel on any node under the leaf,
//! [`key_input`] from `ViewerRequest::SurfaceKey` for keys typed while a viewer's focus is on
//! the leaf. Both are bounded per surface by [`MAX_INPUTS_PER_SECOND`]; the excess is dropped
//! and counted ([`SurfaceInputDrops`], `fux/server.info`).
//!
//! Delta contract: entity ids are the provider's and stable across updates; `ChildOf` names a
//! parent inside the delta (or, in a partial update, an entity already in the surface); an
//! entity without `ChildOf` is a direct child of the surface leaf. Sibling order is the parent's
//! `Children` list when the delta carries one, else the order entities appear in the delta; a
//! full update sets the order exactly and despawns every entity it omits, a partial update only
//! adds, reparents (appending) and rewrites components. Every new entity carries a `Node`.

use core::any::TypeId;

use bevy_app::prelude::*;
use bevy_asset::AssetServer;
use bevy_ecs::entity::{EntityHashMap, EntityHashSet};
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_reflect::prelude::*;
use bevy_reflect::{FromReflect, PartialReflect};
use bevy_ui::{BackgroundColor, BorderColor, Node, ScrollPosition, ZIndex};
use bevy_world_serialization::serde::WorldDeserializer;
use bevy_world_serialization::{DynamicEntity, DynamicWorld};
use serde::de::DeserializeSeed as _;

use crate::events::{SurfaceInput, SurfaceInputKind};
use crate::layout::instances;
use crate::lifecycle::now_ms;
use crate::model::invariants::root_of_template;
use crate::model::{
    Ids, LayoutGeneration, Limits, MAX_DEPTH, NodeId, Places, RootOf, Roots, Surface,
    TemplateNode, TemplateRoot, ViewerId, Viewing,
};

// Provider pacing bounds. They are not configuration (`Limits` is the user's `[limits]` table
// and stays the model's): a provider that needs more than a terminal-refresh rate of scene
// deltas is streaming pixels, which a surface is not for.
/// Updates one surface accepts per second.
pub const MAX_UPDATES_PER_SECOND: u32 = 60;
/// Delta bytes one surface accepts per second.
pub const MAX_BYTES_PER_SECOND: usize = 256 * 1024;
/// Longest `Text` a surface node may carry.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Per-surface node bound as a divisor of `Limits.nodes_per_workspace`.
pub const NODES_PER_WORKSPACE_DIVISOR: usize = 4;
/// `SurfaceInput` events one surface emits per second; the excess is dropped and counted.
pub const MAX_INPUTS_PER_SECOND: u32 = 200;
/// Bytes one `SurfaceInput { kind: Key }` carries; longer key runs are split into several.
pub const MAX_INPUT_BYTES: usize = 64;

/// `SurfaceInput` events dropped by the rate bound since start, server-wide.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceInputDrops(pub u64);

/// A single line of text a node paints, left-aligned and clipped to its content rect. fux
/// defines it because `bevy_text` is unused (prompt 3.13).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component, Default)]
pub struct Text(pub String);

/// Server-side state of a surface leaf, never replicated: who streams it, the last revision
/// applied, the provider-id → template entity map and the pacing window.
#[derive(Component, Debug)]
pub struct SurfaceState {
    pub provider: String,
    pub revision: u64,
    entities: EntityHashMap<Entity>,
    pacing: Pacing,
    inputs: InputPacing,
}

impl SurfaceState {
    /// Template nodes under the surface leaf.
    pub fn nodes(&self) -> usize {
        self.entities.len()
    }

    /// The template entity behind a provider id.
    pub fn entity(&self, provider_id: Entity) -> Option<Entity> {
        self.entities.get(&provider_id).copied()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Pacing {
    window_start_ms: u64,
    updates: u32,
    bytes: usize,
}

impl Pacing {
    fn admit(&mut self, now_ms: u64, bytes: usize) -> Result<(), SurfaceError> {
        if now_ms.saturating_sub(self.window_start_ms) >= 1000 {
            *self = Self {
                window_start_ms: now_ms,
                ..Default::default()
            };
        }
        if self.updates >= MAX_UPDATES_PER_SECOND
            || self.bytes.saturating_add(bytes) > MAX_BYTES_PER_SECOND
        {
            return Err(SurfaceError::RateLimited);
        }
        self.updates += 1;
        self.bytes += bytes;
        Ok(())
    }
}

/// The per-surface `SurfaceInput` window.
#[derive(Debug, Default, Clone, Copy)]
struct InputPacing {
    window_start_ms: u64,
    events: u32,
}

impl InputPacing {
    fn admit(&mut self, now_ms: u64) -> bool {
        if now_ms.saturating_sub(self.window_start_ms) >= 1000 {
            *self = Self {
                window_start_ms: now_ms,
                events: 0,
            };
        }
        if self.events >= MAX_INPUTS_PER_SECOND {
            return false;
        }
        self.events += 1;
        true
    }
}

/// Typed refusal; every variant leaves the World untouched (the pacing window excepted: refused
/// updates still spend it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    NoSuchEntity(Entity),
    NotATemplateNode(Entity),
    NotASurface(Entity),
    AlreadyASurface(Entity),
    PlacesAPane(Entity),
    HasChildren(Entity),
    StaleRevision {
        revision: u64,
        current: u64,
    },
    RateLimited,
    Malformed(String),
    CarriesResources,
    UnknownType(String),
    /// A new entity without a `Node`.
    MissingNode(Entity),
    /// `ChildOf` names an entity that is neither in the delta nor (partial updates) in the surface.
    UnknownParent {
        child: Entity,
        parent: Entity,
    },
    /// A `Children` entry whose `ChildOf` does not name that parent.
    InconsistentChildren {
        parent: Entity,
        child: Entity,
    },
    Cycle(Entity),
    DepthExceeded {
        depth: usize,
        max: usize,
    },
    TooManyNodes {
        count: usize,
        max: usize,
    },
    TextTooLong {
        bytes: usize,
        max: usize,
    },
    /// `SurfaceKey` named a node id no template node carries.
    UnknownNode(NodeId),
    /// The viewer views another workspace than the surface's.
    NotViewing(Entity),
}

impl core::fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSuchEntity(e) => write!(f, "{e} does not exist"),
            Self::NotATemplateNode(e) => write!(f, "{e} is not a template node"),
            Self::NotASurface(e) => write!(f, "{e} is not a surface"),
            Self::AlreadyASurface(e) => write!(f, "{e} is already a surface"),
            Self::PlacesAPane(e) => write!(f, "{e} places a pane"),
            Self::HasChildren(e) => write!(f, "{e} has children"),
            Self::StaleRevision { revision, current } => {
                write!(f, "stale revision {revision}; the surface is at {current}")
            }
            Self::RateLimited => write!(
                f,
                "surface rate bound: {MAX_UPDATES_PER_SECOND} updates and {MAX_BYTES_PER_SECOND} bytes per second"
            ),
            Self::Malformed(s) => write!(f, "malformed delta: {s}"),
            Self::CarriesResources => write!(f, "a surface delta carries no resources"),
            Self::UnknownType(t) => write!(f, "`{t}` is not in the surface vocabulary"),
            Self::MissingNode(e) => write!(f, "new entity {e} has no Node"),
            Self::UnknownParent { child, parent } => {
                write!(
                    f,
                    "{child} names parent {parent}, which is not in the surface"
                )
            }
            Self::InconsistentChildren { parent, child } => {
                write!(f, "{parent} lists child {child} whose ChildOf disagrees")
            }
            Self::Cycle(e) => write!(f, "{e} is its own ancestor"),
            Self::DepthExceeded { depth, max } => write!(f, "template depth {depth} exceeds {max}"),
            Self::TooManyNodes { count, max } => write!(f, "{count} surface nodes exceed {max}"),
            Self::TextTooLong { bytes, max } => write!(f, "text of {bytes} bytes exceeds {max}"),
            Self::UnknownNode(id) => write!(f, "no node {id}"),
            Self::NotViewing(v) => write!(f, "viewer {v} does not view the surface's workspace"),
        }
    }
}

impl std::error::Error for SurfaceError {}

type R<T> = Result<T, SurfaceError>;

// ---------------------------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------------------------

/// Marks a childless, pane-less, non-root template node as a surface streamed by `provider`.
pub fn open(world: &mut World, node: Entity, provider: &str) -> R<()> {
    let entity = world
        .get_entity(node)
        .map_err(|_| SurfaceError::NoSuchEntity(node))?;
    if !entity.contains::<TemplateNode>() || entity.contains::<TemplateRoot>() {
        return Err(SurfaceError::NotATemplateNode(node));
    }
    if entity.contains::<Surface>() {
        return Err(SurfaceError::AlreadyASurface(node));
    }
    if entity.contains::<Places>() {
        return Err(SurfaceError::PlacesAPane(node));
    }
    if entity.get::<Children>().is_some_and(|c| !c.is_empty()) {
        return Err(SurfaceError::HasChildren(node));
    }
    let root = root_of_template(world, node).ok_or(SurfaceError::NotATemplateNode(node))?;
    world.entity_mut(node).insert((
        Surface,
        SurfaceState {
            provider: provider.to_owned(),
            revision: 0,
            entities: EntityHashMap::default(),
            pacing: Pacing::default(),
            inputs: InputPacing::default(),
        },
    ));
    bump(world, root);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Input back to the provider
// ---------------------------------------------------------------------------------------------

/// The surface leaf a template node is or lies under.
pub fn surface_of(world: &World, mut node: Entity) -> Option<Entity> {
    for _ in 0..=MAX_DEPTH {
        if world.get::<Surface>(node).is_some() {
            return Some(node);
        }
        node = world.get::<ChildOf>(node)?.parent();
    }
    None
}

/// A viewer's pointer press, release or wheel on the template node `node` (the leaf or one
/// under it) at cell (`col`, `row`) of the leaf's content box.
pub fn pointer_input(
    world: &mut World,
    viewer: Entity,
    node: Entity,
    kind: SurfaceInputKind,
    col: u16,
    row: u16,
) -> R<()> {
    let surface = surface_of(world, node).ok_or(SurfaceError::NotASurface(node))?;
    emit(world, viewer, surface, node, kind, col, row, &[])
}

/// Keys a viewer typed while its focus was on the surface leaf `node` (or a node under it):
/// one `Key` event per [`MAX_INPUT_BYTES`] chunk, each spending the rate budget.
pub fn key_input(world: &mut World, viewer: Entity, node: NodeId, bytes: &[u8]) -> R<()> {
    let entity = world
        .resource::<Ids>()
        .node(node)
        .ok_or(SurfaceError::UnknownNode(node))?;
    let surface = surface_of(world, entity).ok_or(SurfaceError::NotASurface(entity))?;
    for chunk in bytes.chunks(MAX_INPUT_BYTES.max(1)) {
        emit(world, viewer, surface, entity, SurfaceInputKind::Key, 0, 0, chunk)?;
    }
    Ok(())
}

/// Triggers one `SurfaceInput` for `viewer` on `node` under `surface`, or drops it (counted)
/// when the surface's window is spent.
fn emit(
    world: &mut World,
    viewer: Entity,
    surface: Entity,
    node: Entity,
    kind: SurfaceInputKind,
    col: u16,
    row: u16,
    bytes: &[u8],
) -> R<()> {
    let root = root_of_template(world, surface).ok_or(SurfaceError::NotATemplateNode(surface))?;
    let scope = world
        .get::<RootOf>(root)
        .map(|r| r.0)
        .ok_or(SurfaceError::NotATemplateNode(surface))?;
    if world.get::<Viewing>(viewer).map(|v| v.0) != Some(scope) {
        return Err(SurfaceError::NotViewing(viewer));
    }
    let (Some(&viewer_id), Some(&surface_id), Some(&node_id)) = (
        world.get::<ViewerId>(viewer),
        world.get::<NodeId>(surface),
        world.get::<NodeId>(node),
    ) else {
        return Err(SurfaceError::NotViewing(viewer));
    };
    let now = now_ms(world);
    let admitted = world
        .get_mut::<SurfaceState>(surface)
        .ok_or(SurfaceError::NotASurface(surface))?
        .inputs
        .admit(now);
    if !admitted {
        world.resource_mut::<SurfaceInputDrops>().0 += 1;
        return Ok(());
    }
    let state = world
        .get::<SurfaceState>(surface)
        .ok_or(SurfaceError::NotASurface(surface))?;
    let provider = state.provider.clone();
    let revision = state.revision;
    let provider_node = state
        .entities
        .iter()
        .find_map(|(source, target)| (*target == node).then_some(source.to_bits()));
    world.trigger(SurfaceInput {
        entity: surface,
        scope,
        surface: surface_id,
        node: node_id,
        provider,
        provider_node,
        revision,
        viewer: viewer_id,
        kind,
        col,
        row,
        bytes: bytes.to_vec(),
    });
    Ok(())
}

/// Applies a provider delta at `revision` (strictly above the surface's) and returns the node
/// count under the surface afterwards. Validation is complete before anything is written.
pub fn update(
    world: &mut World,
    surface: Entity,
    revision: u64,
    full: bool,
    ron: &str,
) -> R<usize> {
    let state = world
        .get::<SurfaceState>(surface)
        .ok_or(SurfaceError::NotASurface(surface))?;
    if revision <= state.revision {
        return Err(SurfaceError::StaleRevision {
            revision,
            current: state.revision,
        });
    }
    let now = crate::lifecycle::now_ms(world);
    if let Some(mut state) = world.get_mut::<SurfaceState>(surface) {
        state.pacing.admit(now, ron.len())?;
    }
    let root = root_of_template(world, surface).ok_or(SurfaceError::NotATemplateNode(surface))?;

    let registry = world.resource::<AppTypeRegistry>().clone();
    let dynamic = parse(world, &registry, ron)?;
    let plan = validate(world, surface, full, &dynamic)?;

    // Components first, through the surface's map so ids stay stable across updates. The write
    // cannot refuse what `validate` admitted (the vocabulary is registered with the same
    // registry the delta was parsed against); should it, the map goes back as it was and the
    // revision stays, so the provider can retry.
    let Some(mut state) = world.get_mut::<SurfaceState>(surface) else {
        return Err(SurfaceError::NotASurface(surface));
    };
    let mut map = core::mem::take(&mut state.entities);
    if let Err(error) = plan
        .components
        .write_to_world_with(world, &mut map, &registry.read())
    {
        for provider_id in &plan.fresh {
            if let Some(template) = map.remove(provider_id) {
                let _ = world.try_despawn(template);
            }
        }
        if let Some(mut state) = world.get_mut::<SurfaceState>(surface) {
            state.entities = map;
        }
        return Err(SurfaceError::Malformed(error.to_string()));
    }
    // Fresh nodes are template nodes with their own `NodeId`, so viewer state keyed by id
    // (scroll, display overrides) and `fux/node.*` addressing reach them like any other.
    for provider_id in &plan.fresh {
        if let Some(&template) = map.get(provider_id) {
            let id = world.resource_mut::<Ids>().allocate_node();
            world.entity_mut(template).insert((TemplateNode, id));
        }
    }

    // Hierarchy. Moves go through `add_child` so relationship hooks keep every `Children`
    // list honest (`replace_children` inserts with hooks skipped, which would leave a moved
    // entity listed under its old parent); a full update then despawns what it omitted and
    // sets each parent's order over the now-exact set.
    let resolve = |map: &EntityHashMap<Entity>, id: Option<Entity>| -> Option<Entity> {
        match id {
            None => Some(surface),
            Some(id) => map.get(&id).copied(),
        }
    };
    for (child, parent) in &plan.parents {
        let (Some(child), Some(parent)) = (map.get(child).copied(), resolve(&map, *parent)) else {
            continue;
        };
        let current = world.get::<ChildOf>(child).map(ChildOf::parent);
        if current != Some(parent) {
            world.entity_mut(parent).add_child(child);
        }
    }
    if full {
        for provider_id in &plan.absent {
            if let Some(template) = map.remove(provider_id) {
                let _ = world.try_despawn(template);
            }
        }
        // Cascades under despawned parents.
        map.retain(|_, template| world.get_entity(*template).is_ok());
        let mut ordered: Vec<Entity> = Vec::new();
        for (parent, children) in &plan.order {
            let Some(parent) = resolve(&map, *parent) else {
                continue;
            };
            ordered.clear();
            ordered.extend(children.iter().filter_map(|c| map.get(c).copied()));
            world.entity_mut(parent).replace_children(&ordered);
        }
    }

    let nodes = map.len();
    if let Some(mut state) = world.get_mut::<SurfaceState>(surface) {
        state.entities = map;
        state.revision = revision;
    }
    bump(world, root);
    Ok(nodes)
}

/// Despawns the surface's subtree and removes the marker.
pub fn close(world: &mut World, surface: Entity) -> R<()> {
    if world.get::<SurfaceState>(surface).is_none() {
        return Err(SurfaceError::NotASurface(surface));
    }
    let root = root_of_template(world, surface).ok_or(SurfaceError::NotATemplateNode(surface))?;
    world.entity_mut(surface).despawn_children();
    world
        .entity_mut(surface)
        .remove::<(Surface, SurfaceState)>();
    bump(world, root);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

/// The surface vocabulary; anything else in a delta is refused before it is written.
fn vocabulary() -> [TypeId; 9] {
    [
        TypeId::of::<Node>(),
        TypeId::of::<Name>(),
        TypeId::of::<ZIndex>(),
        TypeId::of::<BackgroundColor>(),
        TypeId::of::<BorderColor>(),
        TypeId::of::<ScrollPosition>(),
        TypeId::of::<Text>(),
        TypeId::of::<ChildOf>(),
        TypeId::of::<Children>(),
    ]
}

/// What an update commits once validated: components to write (relationships stripped), the
/// parent of every delta entity, per-parent order, new ids and (full updates) ids to despawn.
struct Plan {
    components: DynamicWorld,
    parents: Vec<(Entity, Option<Entity>)>,
    order: Vec<(Option<Entity>, Vec<Entity>)>,
    fresh: EntityHashSet,
    absent: Vec<Entity>,
}

fn parse(world: &World, registry: &AppTypeRegistry, ron: &str) -> R<DynamicWorld> {
    let mut asset_server = world.resource::<AssetServer>().clone();
    let mut de =
        ron::de::Deserializer::from_str(ron).map_err(|e| SurfaceError::Malformed(e.to_string()))?;
    let seed = WorldDeserializer {
        type_registry: &registry.read(),
        load_from_path: &mut asset_server,
    };
    seed.deserialize(&mut de)
        .map_err(|e| SurfaceError::Malformed(de.span_error(e).to_string()))
}

fn type_id_of(component: &dyn PartialReflect) -> R<(TypeId, &str)> {
    let info = component
        .get_represented_type_info()
        .ok_or_else(|| SurfaceError::UnknownType(component.reflect_type_path().to_owned()))?;
    Ok((info.type_id(), info.type_path()))
}

fn validate(world: &World, surface: Entity, full: bool, delta: &DynamicWorld) -> R<Plan> {
    if !delta.resources.is_empty() {
        return Err(SurfaceError::CarriesResources);
    }
    let state = world
        .get::<SurfaceState>(surface)
        .ok_or(SurfaceError::NotASurface(surface))?;
    let vocabulary = vocabulary();
    let text = TypeId::of::<Text>();
    let child_of = TypeId::of::<ChildOf>();
    let children = TypeId::of::<Children>();

    // Pass 1: vocabulary, sizes, relationships out of the component lists.
    let mut in_delta = EntityHashSet::default();
    let mut parents: Vec<(Entity, Option<Entity>)> = Vec::with_capacity(delta.entities.len());
    let mut listed: Vec<(Entity, Vec<Entity>)> = Vec::new();
    let mut components = DynamicWorld::default();
    for entity in &delta.entities {
        if !in_delta.insert(entity.entity) {
            return Err(SurfaceError::Malformed(format!(
                "{} appears twice",
                entity.entity
            )));
        }
        let mut parent = None;
        let mut has_node = false;
        let mut kept: Vec<Box<dyn PartialReflect>> = Vec::with_capacity(entity.components.len());
        for component in &entity.components {
            let (id, path) = type_id_of(component.as_ref())?;
            if !vocabulary.contains(&id) {
                return Err(SurfaceError::UnknownType(path.to_owned()));
            }
            if id == child_of {
                let value = ChildOf::from_reflect(component.as_ref()).ok_or_else(|| {
                    SurfaceError::Malformed(format!("ChildOf on {}", entity.entity))
                })?;
                parent = Some(value.parent());
                continue;
            }
            if id == children {
                let value = Children::from_reflect(component.as_ref()).ok_or_else(|| {
                    SurfaceError::Malformed(format!("Children on {}", entity.entity))
                })?;
                listed.push((entity.entity, value.iter().collect()));
                continue;
            }
            if id == text {
                let value = Text::from_reflect(component.as_ref())
                    .ok_or_else(|| SurfaceError::Malformed(format!("Text on {}", entity.entity)))?;
                if value.0.len() > MAX_TEXT_BYTES {
                    return Err(SurfaceError::TextTooLong {
                        bytes: value.0.len(),
                        max: MAX_TEXT_BYTES,
                    });
                }
            }
            has_node |= id == TypeId::of::<Node>();
            kept.push(component.to_dynamic());
        }
        let known = state.entities.contains_key(&entity.entity);
        if !has_node && (full || !known) {
            return Err(SurfaceError::MissingNode(entity.entity));
        }
        parents.push((entity.entity, parent));
        components.entities.push(DynamicEntity {
            entity: entity.entity,
            components: kept,
        });
    }

    // Pass 2: the final parent of every node that will exist, delta or kept.
    let mut final_parent: EntityHashMap<Option<Entity>> = EntityHashMap::default();
    for &(child, parent) in &parents {
        final_parent.insert(child, parent);
    }
    let mut absent = Vec::new();
    if full {
        absent.extend(
            state
                .entities
                .keys()
                .copied()
                .filter(|id| !in_delta.contains(id)),
        );
    } else {
        let inverse: EntityHashMap<Entity> = state
            .entities
            .iter()
            .map(|(&provider_id, &template)| (template, provider_id))
            .collect();
        for (&provider_id, &template) in &state.entities {
            if in_delta.contains(&provider_id) {
                continue;
            }
            let parent = match world.get::<ChildOf>(template).map(ChildOf::parent) {
                Some(p) if p == surface => None,
                Some(p) => Some(*inverse.get(&p).ok_or(SurfaceError::Cycle(provider_id))?),
                None => None,
            };
            final_parent.insert(provider_id, parent);
        }
    }
    for &(child, parent) in &parents {
        if let Some(parent) = parent
            && !final_parent.contains_key(&parent)
        {
            return Err(SurfaceError::UnknownParent { child, parent });
        }
    }
    for (parent, entries) in &listed {
        for &child in entries {
            if final_parent.get(&child).copied().flatten() != Some(*parent) {
                return Err(SurfaceError::InconsistentChildren {
                    parent: *parent,
                    child,
                });
            }
        }
    }

    // Limits: node count per surface and per workspace, depth from the surface's own.
    let count = final_parent.len();
    let limits = world.resource::<Limits>();
    let per_surface = (limits.nodes_per_workspace / NODES_PER_WORKSPACE_DIVISOR).max(1);
    if count > per_surface {
        return Err(SurfaceError::TooManyNodes {
            count,
            max: per_surface,
        });
    }
    let workspace_nodes =
        workspace_nodes(world, surface).saturating_sub(state.entities.len()) + count;
    if workspace_nodes > limits.nodes_per_workspace {
        return Err(SurfaceError::TooManyNodes {
            count: workspace_nodes,
            max: limits.nodes_per_workspace,
        });
    }
    let base = depth_of(world, surface);
    for &start in final_parent.keys() {
        let mut depth = 1;
        let mut at = start;
        while let Some(parent) = final_parent.get(&at).copied().flatten() {
            depth += 1;
            if depth > count {
                return Err(SurfaceError::Cycle(start));
            }
            at = parent;
        }
        if base + depth > MAX_DEPTH {
            return Err(SurfaceError::DepthExceeded {
                depth: base + depth,
                max: MAX_DEPTH,
            });
        }
    }

    // Order per parent: the parent's `Children` first, then the rest in delta order.
    let mut order: Vec<(Option<Entity>, Vec<Entity>)> = Vec::new();
    if full {
        let mut by_parent: Vec<(Option<Entity>, Vec<Entity>)> = Vec::new();
        for &(child, parent) in &parents {
            match by_parent.iter_mut().find(|(p, _)| *p == parent) {
                Some((_, list)) => list.push(child),
                None => by_parent.push((parent, vec![child])),
            }
        }
        for (parent, mut rest) in by_parent {
            let mut list: Vec<Entity> = Vec::with_capacity(rest.len());
            if let Some(p) = parent
                && let Some((_, entries)) = listed.iter().find(|(l, _)| *l == p)
            {
                for &entry in entries {
                    if let Some(at) = rest.iter().position(|e| *e == entry) {
                        list.push(rest.remove(at));
                    }
                }
            }
            list.append(&mut rest);
            order.push((parent, list));
        }
        // Parents that lost every child in this update: emptied explicitly.
        for provider_id in &in_delta {
            if !order.iter().any(|(p, _)| *p == Some(*provider_id)) {
                order.push((Some(*provider_id), Vec::new()));
            }
        }
        if !order.iter().any(|(p, _)| p.is_none()) {
            order.push((None, Vec::new()));
        }
    }

    let fresh: EntityHashSet = in_delta
        .iter()
        .copied()
        .filter(|id| !state.entities.contains_key(id))
        .collect();
    Ok(Plan {
        components,
        parents,
        order,
        fresh,
        absent,
    })
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn bump(world: &mut World, root: Entity) {
    if let Some(mut generation) = world.get_mut::<LayoutGeneration>(root) {
        generation.0 += 1;
    }
}

fn depth_of(world: &World, mut node: Entity) -> usize {
    let mut depth = 0;
    while let Some(parent) = world.get::<ChildOf>(node) {
        node = parent.parent();
        depth += 1;
        if depth > MAX_DEPTH {
            break;
        }
    }
    depth
}

/// Template nodes in the workspace the surface belongs to.
fn workspace_nodes(world: &World, surface: Entity) -> usize {
    let Some(root) = root_of_template(world, surface) else {
        return 0;
    };
    let Some(workspace) = world.get::<RootOf>(root) else {
        return 0;
    };
    let mut count = 0;
    if let Some(roots) = world.get::<Roots>(workspace.0) {
        for root in roots.iter() {
            instances::walk(world, root, &mut |_, _| count += 1);
        }
    }
    count
}

/// Registers the surface vocabulary fux defines.
pub struct SurfacePlugin;

impl Plugin for SurfacePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SurfaceInputDrops>()
            .register_type::<Text>();
    }
}
