use bevy_ecs::{entity::MapEntities, prelude::*, reflect::ReflectMapEntities};
use bevy_reflect::{Reflect, std_traits::ReflectDefault};
use bevy_ui::Node;

#[derive(Component, Reflect, Default, Clone)]
#[reflect(Component, Default)]
#[require(Node)]
pub struct Workspace;

#[derive(Component, Reflect, Default, Clone)]
#[reflect(Component, Default)]
#[require(Node)]
pub struct Split;

#[derive(Component, Reflect, Clone)]
#[reflect(Component)]
#[require(ProcessState)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: String,
    pub history_lines: usize,
}

#[derive(Component, Reflect, Clone)]
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
pub struct Viewer {
    pub workspace: Entity,
    pub focus: Option<Entity>,
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub scrollback: usize,
    pub notice: String,
    pub prefix: bool,
    pub prompt: Option<String>,
    pub buffer: String,
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
