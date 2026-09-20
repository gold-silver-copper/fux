use bevy_ecs::{entity::MapEntities, prelude::*, reflect::ReflectMapEntities};
use bevy_reflect::{Reflect, ReflectDeserialize, ReflectSerialize, std_traits::ReflectDefault};
use bevy_ui::Node;
use serde::{Deserialize, Serialize};

#[derive(Component, Reflect, Default, Clone)]
#[reflect(Component, Default)]
#[require(Node, LayoutCache)]
pub struct Workspace;

/// Derived scene data belongs to its workspace and dies with that entity.
/// Not reflected: scene persistence never serializes caches.
#[derive(Component, Default)]
pub struct LayoutCache {
    pub scene: Option<std::sync::Arc<bevy_world_serialization::DynamicWorld>>,
    pub built_at: bevy_ecs::change_detection::Tick,
    /// Each member's component set, minus viewer bookkeeping (see `Shape`).
    pub members: bevy_ecs::entity::EntityHashMap<Shape>,
}

/// The components of a layout entity that are scene content. Viewer
/// relationship targets come and go with focus and must not count as edits.
pub type Shape = Box<[bevy_ecs::component::ComponentId]>;

pub(crate) fn shape(world: &World, entity: Entity) -> Shape {
    let ignored = [
        world.component_id::<Viewers>(),
        world.component_id::<TabViewers>(),
        world.component_id::<FocusedBy>(),
    ];
    let mut ids: Vec<_> = world
        .entity(entity)
        .archetype()
        .components()
        .iter()
        .copied()
        .filter(|id| !ignored.contains(&Some(*id)))
        .collect();
    ids.sort();
    ids.into_boxed_slice()
}

#[derive(Component, Reflect, Default, Clone)]
#[reflect(Component, Default)]
#[require(Node)]
pub struct Split;

/// A workspace's ordered children are tabs; processes remain external references.
#[derive(Component, Reflect, Default, Clone)]
#[reflect(Component, Default)]
#[require(Node)]
pub struct Tab;

#[derive(Component, Reflect, Default, Clone, Copy)]
#[reflect(Component, Default)]
pub struct WorkspaceOrder(pub i64);

/// The workspace a viewer looks at. Bevy removes it when the workspace dies,
/// and `navigation::repair` then picks another.
#[derive(Component, Reflect, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component, Serialize, Deserialize)]
#[serde(transparent)]
#[relationship(relationship_target = Viewers)]
pub struct Viewing(pub Entity);
/// Unreflected on purpose: it lives on layout entities and must stay out of scenes.
#[derive(Component, Default)]
#[relationship_target(relationship = Viewing)]
pub struct Viewers(Vec<Entity>);

/// The tab a viewer shows within its workspace.
#[derive(Component, Reflect, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component, Serialize, Deserialize)]
#[serde(transparent)]
#[relationship(relationship_target = TabViewers)]
pub struct OnTab(pub Entity);
#[derive(Component, Default)]
#[relationship_target(relationship = OnTab)]
pub struct TabViewers(Vec<Entity>);

/// The pane view a viewer's input goes to.
#[derive(Component, Reflect, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component, Serialize, Deserialize)]
#[serde(transparent)]
#[relationship(relationship_target = FocusedBy)]
pub struct Focused(pub Entity);
#[derive(Component, Default)]
#[relationship_target(relationship = Focused)]
pub struct FocusedBy(Vec<Entity>);

/// Viewer-local memory of where it was: the tab per workspace, the focused pane
/// per tab and the previously focused pane per tab. Observers record entries
/// as the relationships change and prune them as their entities die.
#[derive(Component, Default)]
pub struct Memory {
    pub tabs: bevy_ecs::entity::EntityHashMap<Entity>,
    pub focus: bevy_ecs::entity::EntityHashMap<Entity>,
    pub previous: bevy_ecs::entity::EntityHashMap<Entity>,
}

#[derive(Component, Reflect, Clone)]
#[reflect(Component)]
#[require(ProcessState)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: String,
    pub history_lines: usize,
}

/// One process lifecycle. A running process may carry its last I/O error
/// without leaving `Running`; only spawn and termination failures are `Failed`.
#[derive(Reflect, Clone, PartialEq, Default, Debug, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Status {
    #[default]
    Starting,
    Running {
        pid: u32,
        error: Option<String>,
    },
    Exited {
        code: i32,
    },
    Failed {
        error: String,
    },
}

#[derive(Component, Reflect, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct ProcessState {
    pub status: Status,
    pub rows: u16,
    pub cols: u16,
    pub revision: u64,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self {
            status: Status::Starting,
            rows: 24,
            cols: 80,
            revision: 0,
        }
    }
}

/// A layout leaf refers to a live process, not a serialized process recipe.
#[derive(Component, Reflect, Clone, MapEntities)]
#[reflect(Component, MapEntities)]
#[require(Node)]
#[relationship(relationship_target = PaneViews)]
pub struct PaneView {
    #[entities]
    pub pane: Entity,
}

/// Bevy maintains the inverse relation; no independent pane-view index.
#[derive(Component, Reflect, Default)]
#[reflect(Component, Default)]
#[relationship_target(relationship = PaneView)]
pub struct PaneViews(Vec<Entity>);

#[derive(Component, Reflect, Clone)]
#[reflect(Component)]
#[require(Memory, crate::paste::Ownership)]
pub struct Viewer {
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub scrollback: usize,
    pub notice: Option<Notice>,
}

/// The bar's right zone shows the latest notice until further input clears it.
#[derive(Reflect, Clone, PartialEq, Debug, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}
impl Notice {
    pub fn info(text: impl Into<String>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            error: false,
        })
    }
    pub fn error(text: impl Into<String>) -> Option<Self> {
        Some(Self {
            text: text.into(),
            error: true,
        })
    }
}

pub const DETACHED: &str = "viewer no longer attached";

/// The workspace, tab and pane view a viewer currently relates to.
pub(crate) fn viewing(world: &World, id: Entity) -> Option<Entity> {
    world.get::<Viewing>(id).map(|v| v.0)
}
pub(crate) fn on_tab(world: &World, id: Entity) -> Option<Entity> {
    world.get::<OnTab>(id).map(|t| t.0)
}
pub(crate) fn focused(world: &World, id: Entity) -> Option<Entity> {
    world.get::<Focused>(id).map(|f| f.0)
}

/// Notify a viewer that may already be gone; a missing viewer has nobody to tell.
/// `None` clears the bar.
pub(crate) fn notify(world: &mut World, id: Entity, notice: Option<Notice>) {
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.notice = notice;
    }
}

#[derive(Resource, Clone)]
pub struct Wake(pub std::thread::Thread);

impl Wake {
    pub fn notify(&self) {
        self.0.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    #[test]
    fn layout_relationships_do_not_own_shared_processes() -> crate::testing::Outcome {
        let mut world = World::new();
        let process = world
            .spawn(Launch {
                argv: vec!["/bin/sh".into()],
                cwd: String::new(),
                history_lines: 0,
            })
            .id();
        let left = world.spawn(Workspace).id();
        let right = world.spawn(Workspace).id();
        let removed = world
            .spawn((PaneView { pane: process }, ChildOf(left)))
            .id();
        let retained = world
            .spawn((PaneView { pane: process }, ChildOf(right)))
            .id();
        world.despawn(left);
        assert!(world.get_entity(removed).is_err());
        assert_eq!(world.get::<PaneView>(retained).need()?.pane, process);
        assert!(world.get::<Launch>(process).is_some());
        world.despawn(right);
        assert!(world.get::<Launch>(process).is_some());
        Ok(())
    }
}
