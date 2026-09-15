//! Immutable public identities and the `Ids` index maintained by component hooks (prompt 3.3:
//! hooks only record; uniqueness is validated at the typed transition before insertion).

use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use bevy_platform::collections::HashMap;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};

macro_rules! numeric_id {
    ($(#[$m:meta])* $name:ident, $field:ident) => {
        $(#[$m])*
        #[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[reflect(Component, Serialize, Deserialize)]
        #[component(immutable, on_insert = Self::on_insert, on_discard = Self::on_discard)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            fn on_insert(world: DeferredWorld, ctx: HookContext) {
                on_insert_id::<Self>(world, ctx);
            }
            fn on_discard(world: DeferredWorld, ctx: HookContext) {
                on_discard_id::<Self>(world, ctx);
            }
        }

        impl Indexed for $name {
            fn map(ids: &mut Ids) -> &mut HashMap<Self, Entity> {
                &mut ids.$field
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

numeric_id!(
    /// Public id of a pane: the shared process entity.
    PaneId,
    panes
);
numeric_id!(
    /// Public id of any addressable template node (roots included). Instances carry the same
    /// `NodeId` as the template node they were cloned from.
    NodeId,
    nodes
);
numeric_id!(
    /// Public id of an attached viewer.
    ViewerId,
    viewers
);

/// Unique workspace name; validated for uniqueness before insertion.
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[reflect(Component, Serialize, Deserialize)]
#[component(immutable, on_insert = Self::on_insert, on_discard = Self::on_discard)]
#[serde(transparent)]
pub struct WorkspaceName(pub String);

impl WorkspaceName {
    fn on_insert(world: DeferredWorld, ctx: HookContext) {
        on_insert_id::<Self>(world, ctx);
    }
    fn on_discard(world: DeferredWorld, ctx: HookContext) {
        on_discard_id::<Self>(world, ctx);
    }
}

impl Indexed for WorkspaceName {
    fn map(ids: &mut Ids) -> &mut HashMap<Self, Entity> {
        &mut ids.workspaces
    }
}

impl core::fmt::Display for WorkspaceName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

pub trait Indexed: Component + Clone + Eq + core::hash::Hash + Sized {
    fn map(ids: &mut Ids) -> &mut HashMap<Self, Entity>;
}

/// Index from public ids to entities; equals the set of live id components (invariant 3.5).
#[derive(Resource, Default, Clone, Debug)]
pub struct Ids {
    pub panes: HashMap<PaneId, Entity>,
    pub nodes: HashMap<NodeId, Entity>,
    pub viewers: HashMap<ViewerId, Entity>,
    pub workspaces: HashMap<WorkspaceName, Entity>,
    next_pane: u64,
    next_node: u64,
    next_viewer: u64,
}

impl Ids {
    pub fn pane(&self, id: PaneId) -> Option<Entity> {
        self.panes.get(&id).copied()
    }
    pub fn node(&self, id: NodeId) -> Option<Entity> {
        self.nodes.get(&id).copied()
    }
    pub fn viewer(&self, id: ViewerId) -> Option<Entity> {
        self.viewers.get(&id).copied()
    }
    pub fn workspace(&self, name: &str) -> Option<Entity> {
        self.workspaces.get(name).copied()
    }
    pub fn allocate_pane(&mut self) -> PaneId {
        self.next_pane += 1;
        PaneId(self.next_pane)
    }
    pub fn allocate_node(&mut self) -> NodeId {
        self.next_node += 1;
        NodeId(self.next_node)
    }
    pub fn allocate_viewer(&mut self) -> ViewerId {
        self.next_viewer += 1;
        ViewerId(self.next_viewer)
    }
}

impl core::borrow::Borrow<str> for WorkspaceName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Records the id. An id already mapped to a *different live* entity keeps its mapping: the
/// template node registers first and its instances (cloned with the same `NodeId`) never
/// displace it, independent of component insertion order.
fn on_insert_id<T: Indexed>(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<T>(ctx.entity).cloned() else {
        return;
    };
    let existing = {
        let mut ids = world.resource_mut::<Ids>();
        T::map(&mut ids).get(&id).copied()
    };
    if let Some(other) = existing
        && other != ctx.entity
        && world.get_entity(other).is_ok()
    {
        return;
    }
    let mut ids = world.resource_mut::<Ids>();
    T::map(&mut ids).insert(id, ctx.entity);
}

fn on_discard_id<T: Indexed>(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<T>(ctx.entity).cloned() else {
        return;
    };
    let mut ids = world.resource_mut::<Ids>();
    let map = T::map(&mut ids);
    if map.get(&id) == Some(&ctx.entity) {
        map.remove(&id);
    }
}
