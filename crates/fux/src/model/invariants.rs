//! Structural invariants that must hold after every `update` (prompt 3.5). Called by tests
//! after every step and by the app in debug builds; a violation is an invariant error the
//! global error handler treats as fatal.

use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_platform::collections::HashSet;
use bevy_ui::{Node, UiTargetCamera};

use super::*;

/// Returns the first violated invariant as text.
pub fn check_invariants(world: &mut World) -> Result<(), String> {
    // Every `Disabled` entity carries no `Node`.
    if let Some(entity) = world
        .query_filtered::<Entity, (With<Disabled>, With<Node>)>()
        .iter(world)
        .next()
    {
        return Err(format!("{entity} is Disabled but has a Node"));
    }
    let ids = world.resource::<Ids>().clone();
    // Ids maps equal the set of live immutable id components.
    for (entity, id) in world
        .query_filtered::<(Entity, &PaneId), Allow<Disabled>>()
        .iter(world)
    {
        if ids.pane(*id) != Some(entity) {
            return Err(format!("pane {id} is not registered"));
        }
    }
    if ids.panes.len()
        != world
            .query_filtered::<&PaneId, Allow<Disabled>>()
            .iter(world)
            .count()
    {
        return Err("Ids.panes has stale entries".into());
    }
    for (entity, id) in world
        .query_filtered::<(Entity, &NodeId), With<TemplateNode>>()
        .iter(world)
    {
        if ids.node(*id) != Some(entity) {
            return Err(format!("template node {id} is not registered"));
        }
    }
    for (entity, id) in world
        .query_filtered::<(Entity, &ViewerId), Allow<Disabled>>()
        .iter(world)
    {
        if ids.viewer(*id) != Some(entity) {
            return Err(format!("viewer {id} is not registered"));
        }
    }
    for (entity, name) in world
        .query_filtered::<(Entity, &WorkspaceName), Allow<Disabled>>()
        .iter(world)
    {
        if ids.workspace(&name.0) != Some(entity) {
            return Err(format!("workspace {name} is not registered"));
        }
    }
    // Panes: exactly one PaneIn and exactly one placing template leaf, or Disabled.
    let mut placed: HashSet<Entity> = HashSet::default();
    for (entity, placed_in, pane_in, disabled) in world
        .query_filtered::<(Entity, Option<&PlacedIn>, Option<&PaneIn>, Has<Disabled>), (With<Pane>, Allow<Disabled>)>()
        .iter(world)
    {
        let leaves = placed_in.map_or(0, |p| p.len());
        if !disabled && (leaves != 1 || pane_in.is_none()) {
            return Err(format!(
                "pane {entity} has {leaves} placing leaves and pane_in={}",
                pane_in.is_some()
            ));
        }
        if leaves > 1 {
            return Err(format!("pane {entity} is placed by {leaves} leaves"));
        }
        placed.insert(entity);
    }
    // Template nodes: placing leaves have no children; no template node has a camera; every
    // template node is reachable from a root that has RootOf and is in RootOrder once.
    for (entity, places, children, camera) in world
        .query_filtered::<(
            Entity,
            Option<&Places>,
            Option<&Children>,
            Has<UiTargetCamera>,
        ), With<TemplateNode>>()
        .iter(world)
    {
        if camera {
            return Err(format!("template node {entity} has a UiTargetCamera"));
        }
        if places.is_some() && children.is_some_and(|c| !c.is_empty()) {
            return Err(format!("placing template leaf {entity} has children"));
        }
    }
    for (root, root_of, child_of) in world
        .query_filtered::<(Entity, Option<&RootOf>, Option<&ChildOf>), With<TemplateRoot>>()
        .iter(world)
    {
        let Some(root_of) = root_of else {
            return Err(format!("template root {root} has no RootOf"));
        };
        if child_of.is_some() {
            return Err(format!("template root {root} has a parent"));
        }
        let order = world
            .get::<RootOrder>(root_of.0)
            .ok_or_else(|| format!("workspace {} has no RootOrder", root_of.0))?;
        if order.0.iter().filter(|e| **e == root).count() != 1 {
            return Err(format!(
                "template root {root} is not exactly once in RootOrder"
            ));
        }
    }
    for (workspace, order, roots) in world
        .query_filtered::<(Entity, &RootOrder, Option<&Roots>), With<Workspace>>()
        .iter(world)
    {
        let set: HashSet<Entity> = roots.map(|r| r.iter().collect()).unwrap_or_default();
        if order.0.len() != set.len() || order.0.iter().any(|e| !set.contains(e)) {
            return Err(format!(
                "workspace {workspace} RootOrder disagrees with Roots"
            ));
        }
    }
    // Instances: every instance node has InstanceOf to a live template node; instance roots
    // have UiTargetCamera naming their viewer's camera; a viewer has at most one instance per
    // template root; every instance leaf Shows the pane its template leaf Places.
    let mut per_viewer_root: HashSet<(Entity, Entity)> = HashSet::default();
    let camera_owner: bevy_platform::collections::HashMap<Entity, Entity> = world
        .query_filtered::<(Entity, &ViewerCamera), With<Viewer>>()
        .iter(world)
        .map(|(v, c)| (c.0, v))
        .collect();
    for (entity, instance_of, child_of, camera, shows, disabled) in world
        .query_filtered::<(
            Entity,
            &InstanceOf,
            Option<&ChildOf>,
            Option<&UiTargetCamera>,
            Option<&Shows>,
            Has<Disabled>,
        ), With<InstanceNode>>()
        .iter(world)
    {
        if disabled {
            return Err(format!("instance node {entity} is Disabled"));
        }
        let Some(template) = world.get_entity(instance_of.0).ok() else {
            return Err(format!(
                "instance node {entity} points at a dead template node"
            ));
        };
        if !template.contains::<TemplateNode>() {
            return Err(format!(
                "instance node {entity} points at a non-template entity"
            ));
        }
        match (child_of, camera) {
            (None, Some(camera)) => {
                let Some(&viewer) = camera_owner.get(&camera.entity()) else {
                    return Err(format!(
                        "instance root {entity} has a camera no viewer owns"
                    ));
                };
                if !per_viewer_root.insert((viewer, instance_of.0)) {
                    return Err(format!("viewer {viewer} has two instances of one root"));
                }
            }
            (None, None) => return Err(format!("instance root {entity} has no UiTargetCamera")),
            (Some(_), Some(_)) => {
                return Err(format!(
                    "non-root instance node {entity} has a UiTargetCamera"
                ));
            }
            (Some(_), None) => {}
        }
        let places = template.get::<Places>().map(|p| p.0);
        let shown = shows.map(|s| s.0);
        if places != shown {
            return Err(format!(
                "instance {entity} shows {shown:?} but its template places {places:?}"
            ));
        }
    }
    // Viewers: Targets pane is placed in a shown root unless exact; barrier points at a Creation.
    for (viewer, targets, showing, exact, barrier) in world
        .query_filtered::<(
            Entity,
            Option<&Targets>,
            Option<&Showing>,
            Has<ExactTarget>,
            Option<&CreationBarrier>,
        ), With<Viewer>>()
        .iter(world)
    {
        if let Some(CreationBarrier(Some(b))) = barrier
            && world.get::<Creation>(*b).is_none()
        {
            return Err(format!("viewer {viewer} waits on a finished creation"));
        }
        let (Some(target), Some(showing)) = (targets, showing) else {
            continue;
        };
        if exact {
            continue;
        }
        let Some(placed_in) = world.get::<PlacedIn>(target.0) else {
            continue;
        };
        let in_shown_root = placed_in
            .iter()
            .any(|leaf| root_of_template(world, leaf) == Some(showing.0));
        if !in_shown_root {
            return Err(format!(
                "viewer {viewer} targets a pane outside the root it shows"
            ));
        }
    }
    Ok(())
}

/// Walks `ChildOf` up to the template root.
pub fn root_of_template(world: &World, mut node: Entity) -> Option<Entity> {
    for _ in 0..=MAX_DEPTH {
        match world.get::<ChildOf>(node) {
            Some(parent) => node = parent.parent(),
            None => return world.get::<TemplateRoot>(node).map(|_| node),
        }
    }
    None
}
