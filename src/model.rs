use bevy_ecs::{entity::MapEntities, prelude::*, reflect::ReflectMapEntities};
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use bevy_ui::Node;

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
    pub members: bevy_ecs::entity::EntityHashMap<bevy_ecs::archetype::ArchetypeId>,
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

/// Viewer-local runtime memory, deliberately absent from scene reflection.
#[derive(Component, Default)]
pub struct Navigation {
    pub tabs: bevy_ecs::entity::EntityHashMap<Entity>,
    pub focus: bevy_ecs::entity::EntityHashMap<Entity>,
    pub previous: bevy_ecs::entity::EntityHashMap<Entity>,
    pub observed: Option<(Entity, Entity, Option<Entity>)>,
}

#[derive(Component, Reflect, Clone)]
#[reflect(Component)]
#[require(ProcessState)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: String,
    pub history_lines: usize,
}

#[derive(Component, Reflect, Clone, PartialEq)]
#[reflect(Component, Default)]
pub struct ProcessState {
    pub pid: Option<u32>,
    pub exit: Option<i32>,
    pub error: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub revision: u64,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self {
            pid: None,
            exit: None,
            error: None,
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
#[require(Navigation, crate::paste::Ownership)]
pub struct Viewer {
    pub workspace: Entity,
    pub tab: Option<Entity>,
    pub focus: Option<Entity>,
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub scrollback: usize,
    pub notice: String,
    pub notice_error: bool,
    pub help_scroll: usize,
    pub prefix: bool,
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

    #[test]
    fn layout_relationships_do_not_own_shared_processes() {
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
        assert_eq!(world.get::<PaneView>(retained).unwrap().pane, process);
        assert!(world.get::<Launch>(process).is_some());
        world.despawn(right);
        assert!(world.get::<Launch>(process).is_some());
    }
}
