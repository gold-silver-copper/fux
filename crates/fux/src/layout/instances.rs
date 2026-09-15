//! Instance sync (prompt 3.4): each viewer's instance tree is a clone of the template root it
//! shows, re-cloned with `EntityCloner` (linked cloning over `Children`) whenever the template's
//! `LayoutGeneration` changed since the last sync, and despawned when the viewer stops showing
//! the root. Per-viewer state ([`ViewState`]: zoom, scroll, display overrides) is re-applied by
//! `NodeId` after every clone.

use bevy_ecs::entity::{EntityCloner, EntityHashMap};
use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_ui::{
    BackgroundColor, BorderColor, Display, Node, PositionType, ScrollPosition, UiTargetCamera, Val,
    ZIndex,
};

use super::ViewState;
use crate::model::{
    InstanceNode, InstanceOf, Instances, LayoutGeneration, MAX_DEPTH, NodeId, Places, Showing,
    Shows, Surface, TemplateRoot, Viewer, ViewerCamera, Zoomed,
};
use crate::surface::Text;

#[derive(Default)]
pub struct Scratch {
    viewers: Vec<(Entity, Entity, Option<Entity>)>,
    roots: Vec<(Entity, Entity, Option<Entity>)>,
    edited: Vec<Entity>,
    satisfied: Vec<bool>,
}

/// `Update`/[`super::LayoutSystems::Instances`]: instance trees equal their templates in shape.
/// An instance survives while its viewer still shows its template root and that root's
/// [`LayoutGeneration`] did not change since the last run; everything else is despawned and a
/// viewer left without an instance of the root it shows gets a fresh clone.
pub fn sync_instances(
    world: &mut World,
    viewer_query: &mut QueryState<
        (Entity, &'static ViewerCamera, Option<&'static Showing>),
        With<Viewer>,
    >,
    root_query: &mut QueryState<
        (Entity, &'static UiTargetCamera, Option<&'static InstanceOf>),
        (With<InstanceNode>, Without<ChildOf>),
    >,
    edited_query: &mut QueryState<Entity, (With<TemplateRoot>, Changed<LayoutGeneration>)>,
    mut scratch: Local<Scratch>,
) {
    let Scratch {
        viewers,
        roots,
        edited,
        satisfied,
    } = &mut *scratch;
    viewers.clear();
    viewers.extend(
        viewer_query
            .iter(world)
            .map(|(viewer, camera, showing)| (viewer, camera.0, showing.map(|s| s.0))),
    );
    roots.clear();
    roots.extend(
        root_query
            .iter(world)
            .map(|(root, camera, of)| (root, camera.entity(), of.map(|o| o.0))),
    );
    edited.clear();
    edited.extend(edited_query.iter(world));
    satisfied.clear();
    satisfied.resize(viewers.len(), false);

    for &(instance, camera, template) in &*roots {
        let owner = viewers.iter().position(|v| v.1 == camera);
        let current = owner.and_then(|i| viewers.get(i)).and_then(|v| v.2);
        let wanted = template.is_some_and(|t| current == Some(t) && !edited.contains(&t));
        match (wanted, owner) {
            (true, Some(i)) => {
                if let Some(flag) = satisfied.get_mut(i) {
                    *flag = true;
                }
            }
            _ => {
                world.despawn(instance);
            }
        }
    }
    for (i, &(viewer, camera, showing)) in viewers.iter().enumerate() {
        if satisfied.get(i).copied().unwrap_or(true) {
            continue;
        }
        if let Some(root) = showing
            && world.get::<TemplateRoot>(root).is_some()
        {
            clone_instance(world, viewer, camera, root);
        }
    }
}

/// Clones `root` into a new instance tree for `viewer` and applies its view state. The
/// template root's `ChildOf` (its workspace) is cloned too and removed again: the instance
/// root must stay parentless so `bevy_ui` lays it out against `camera`.
fn clone_instance(world: &mut World, viewer: Entity, camera: Entity, root: Entity) -> Entity {
    let target = world.spawn_empty().id();
    let mut map = EntityHashMap::<Entity>::default();
    map.insert(root, target);
    let mut cloner = EntityCloner::build_opt_in(world);
    cloner
        .allow::<(
            Node,
            ZIndex,
            BackgroundColor,
            BorderColor,
            ScrollPosition,
            Name,
            NodeId,
            Surface,
            Text,
            Children,
            ChildOf,
        )>()
        .linked_cloning(true);
    let mut cloner = cloner.finish();
    cloner.clone_entity_mapped(world, root, &mut map);
    world.entity_mut(target).remove::<ChildOf>();
    for (&template, &instance) in &map {
        let places = world.get::<Places>(template).map(|p| p.0);
        let mut entity = world.entity_mut(instance);
        entity.insert((InstanceNode, InstanceOf(template)));
        if let Some(pane) = places {
            entity.insert(Shows(pane));
        }
    }
    world.entity_mut(target).insert(UiTargetCamera(camera));
    apply_view_state(world, viewer);
    target
}

/// Instance roots laid out against `camera`.
pub fn instance_roots_of_camera(world: &mut World, camera: Entity) -> Vec<Entity> {
    world
        .query_filtered::<(Entity, &UiTargetCamera), (With<InstanceNode>, Without<ChildOf>)>()
        .iter(world)
        .filter(|(_, target)| target.entity() == camera)
        .map(|(root, _)| root)
        .collect()
}

/// The viewer's instance of the root it shows, if cloned yet.
pub fn instance_root(world: &World, viewer: Entity) -> Option<Entity> {
    let camera = world.get::<ViewerCamera>(viewer)?.0;
    let showing = world.get::<Showing>(viewer)?.0;
    world.get::<Instances>(showing)?.iter().find(|&instance| {
        world
            .get::<UiTargetCamera>(instance)
            .is_some_and(|target| target.entity() == camera)
    })
}

/// Pre-order walk of a template or instance subtree, with each node's depth below `root`;
/// never descends past [`MAX_DEPTH`].
pub fn walk(world: &World, root: Entity, f: &mut dyn FnMut(Entity, usize)) {
    let mut stack = vec![(root, 0usize)];
    while let Some((entity, depth)) = stack.pop() {
        f(entity, depth);
        if depth >= MAX_DEPTH {
            continue;
        }
        if let Some(children) = world.get::<Children>(entity) {
            for child in children.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }
}

/// The node in the subtree of `root` carrying `id`.
pub fn find_by_node_id(world: &World, root: Entity, id: NodeId) -> Option<Entity> {
    let mut found = None;
    walk(world, root, &mut |entity, _| {
        if found.is_none() && world.get::<NodeId>(entity) == Some(&id) {
            found = Some(entity);
        }
    });
    found
}

/// Re-derives the viewer's instance nodes from their templates and applies [`ViewState`]:
/// transient `Display` overrides replace the template's; the zoomed node fills the root and
/// everything off its path is `Display::None`; scrolled nodes get their `ScrollPosition`.
/// No-op without an instance.
pub fn apply_view_state(world: &mut World, viewer: Entity) {
    let Some(root) = instance_root(world, viewer) else {
        return;
    };
    let mut nodes: Vec<Entity> = Vec::new();
    walk(world, root, &mut |entity, _| nodes.push(entity));
    for &instance in &nodes {
        let Some(template) = world.get::<InstanceOf>(instance).map(|o| o.0) else {
            continue;
        };
        let Some(mut node) = world.get::<Node>(template).cloned() else {
            continue;
        };
        if let Some(id) = world.get::<NodeId>(template)
            && let Some(display) = world
                .get::<ViewState>(viewer)
                .and_then(|s| s.display.get(id))
        {
            node.display = *display;
        }
        if let Some(mut current) = world.get_mut::<Node>(instance) {
            current.set_if_neq(node);
        }
    }
    let zoom = world.get::<ViewState>(viewer).and_then(|s| s.zoom);
    let zoomed = zoom.and_then(|id| find_by_node_id(world, root, id));
    match zoomed {
        Some(zoomed) => {
            hide_off_path(world, root, zoomed);
            if let Some(mut node) = world.get_mut::<Node>(zoomed) {
                node.set_if_neq(Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    margin: bevy_ui::UiRect::ZERO,
                    ..node.clone()
                });
            }
            let mut entity = world.entity_mut(root);
            if entity.get::<Zoomed>() != Some(&Zoomed(zoomed)) {
                entity.insert(Zoomed(zoomed));
            }
        }
        None => {
            world.entity_mut(root).remove::<Zoomed>();
        }
    }
    let mut scrolled: Vec<(NodeId, f32)> = Vec::new();
    if let Some(state) = world.get::<ViewState>(viewer) {
        scrolled.extend(state.scroll.iter().map(|(id, y)| (*id, *y)));
    }
    for (id, y) in scrolled {
        if let Some(instance) = find_by_node_id(world, root, id)
            && let Some(mut position) = world.get_mut::<ScrollPosition>(instance)
            && position.0.y.to_bits() != y.to_bits()
        {
            position.0 = Vec2::new(0.0, y);
        }
    }
}

/// Every sibling of the path `root -> zoomed` becomes `Display::None`; their subtrees follow.
fn hide_off_path(world: &mut World, root: Entity, zoomed: Entity) {
    let mut path: Vec<Entity> = vec![zoomed];
    let mut node = zoomed;
    while node != root {
        let Some(parent) = world.get::<ChildOf>(node).map(ChildOf::parent) else {
            return;
        };
        path.push(parent);
        node = parent;
        if path.len() > MAX_DEPTH + 1 {
            return;
        }
    }
    let mut hidden: Vec<Entity> = Vec::new();
    for &on_path in &path {
        if on_path == zoomed {
            continue;
        }
        if let Some(children) = world.get::<Children>(on_path) {
            hidden.extend(children.iter().filter(|c| !path.contains(c)));
        }
    }
    for entity in hidden {
        if let Some(mut node) = world.get_mut::<Node>(entity)
            && node.display != Display::None
        {
            node.display = Display::None;
        }
    }
}
