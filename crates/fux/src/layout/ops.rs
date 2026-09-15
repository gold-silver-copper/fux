//! Typed, validated layout transitions (prompt 3.4). Every function checks everything it needs
//! before touching the World and either commits completely or returns a [`LayoutError`].
//! Template edits bump the root's [`LayoutGeneration`]; instances follow on the next update
//! ([`super::instances`]). Instance-only state (zoom, scroll) is recorded per viewer by
//! [`NodeId`] and applied to the live instance immediately when one exists.

use bevy_asset::uuid::Uuid;
use bevy_camera::{Camera, RenderTarget};
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_input_focus::directional_navigation::{AutoNavigationConfig, FocusableArea};
use bevy_input_focus::navigator::find_best_candidate;
use bevy_math::{CompassOctant, UVec2, Vec2};
use bevy_picking::pointer::PointerId;
use bevy_ui::{
    BackgroundColor, BorderColor, ComputedNode, ComputedStackIndex, Display, FlexDirection,
    FlexWrap, Node, ScrollPosition, UiGlobalTransform, Val, ZIndex,
};

use super::{LayoutError, NavDirection, NodePatch, Side, ViewState, instances, size};
use crate::model::invariants::root_of_template;
use crate::model::*;

type R<T> = Result<T, LayoutError>;

// ---------------------------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------------------------

/// Spawns an open, empty workspace.
pub fn new_workspace(world: &mut World, name: &str) -> R<Entity> {
    check_name(name)?;
    if world.resource::<Ids>().workspace(name).is_some() {
        return Err(LayoutError::DuplicateWorkspace(name.to_owned()));
    }
    let count = world
        .query_filtered::<(), (With<Workspace>, Allow<Disabled>)>()
        .iter(world)
        .count();
    let max = world.resource::<Limits>().workspaces;
    if count + 1 > max {
        return Err(LayoutError::TooManyWorkspaces {
            count: count + 1,
            max,
        });
    }
    Ok(world
        .spawn((
            Workspace,
            WorkspaceName(name.to_owned()),
            Name::new(name.to_owned()),
            RootOrder::default(),
            Open,
        ))
        .id())
}

/// Marks the workspace `Retiring` + `Disabled`; the lifecycle closes its panes. Idempotent.
pub fn retire_workspace(world: &mut World, ws: Entity, now_ms: u64) -> R<()> {
    workspace(world, ws)?;
    if world.get::<Retiring>(ws).is_some() {
        return Ok(());
    }
    world
        .entity_mut(ws)
        .remove::<Open>()
        .insert((Retiring { since_ms: now_ms }, Disabled));
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Template roots
// ---------------------------------------------------------------------------------------------

/// Spawns an empty template root filling its viewport and appends it to the workspace's order.
pub fn new_root(world: &mut World, ws: Entity, name: &str) -> R<Entity> {
    open_workspace(world, ws)?;
    check_name(name)?;
    check_capacity(world, ws, 1, 0)?;
    let id = world.resource_mut::<Ids>().allocate_node();
    let root = world
        .spawn((
            TemplateRoot,
            TemplateNode,
            id,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..Node::default()
            },
            RootOf(ws),
            LayoutGeneration(0),
            Name::new(name.to_owned()),
        ))
        .id();
    if let Some(mut order) = world.get_mut::<RootOrder>(ws) {
        order.0.push(root);
    }
    Ok(root)
}

/// Despawns a root whose panes have all exited; returns the (exited) panes it despawned with it.
pub fn close_root(world: &mut World, root: Entity) -> R<Vec<Entity>> {
    template_root(world, root)?;
    let panes = placed_panes(world, root);
    if let Some(&live) = panes.iter().find(|&&p| pane_is_live(world, p)) {
        return Err(LayoutError::PaneLive(live));
    }
    let ws = world.get::<RootOf>(root).map(|r| r.0);
    if let Some(mut order) = ws.and_then(|ws| world.get_mut::<RootOrder>(ws)) {
        order.0.retain(|r| *r != root);
    }
    reshow_viewers(world, root);
    world.despawn(root);
    for &pane in &panes {
        world.despawn(pane);
    }
    Ok(panes)
}

/// Moves a root (and the panes it places) to another workspace, appended to its order.
pub fn move_root(world: &mut World, root: Entity, ws: Entity) -> R<()> {
    template_root(world, root)?;
    open_workspace(world, ws)?;
    let old = world
        .get::<RootOf>(root)
        .map(|r| r.0)
        .ok_or(LayoutError::NotATemplateRoot(root))?;
    if old == ws {
        return Ok(());
    }
    let (nodes, _) = subtree_stats(world, root);
    let panes = placed_panes(world, root);
    check_capacity(world, ws, nodes, panes.len())?;
    if let Some(mut order) = world.get_mut::<RootOrder>(old) {
        order.0.retain(|r| *r != root);
    }
    reshow_viewers(world, root);
    world.entity_mut(root).insert(RootOf(ws));
    if let Some(mut order) = world.get_mut::<RootOrder>(ws) {
        order.0.push(root);
    }
    for pane in panes {
        world.entity_mut(pane).insert(PaneIn(ws));
    }
    bump(world, root);
    Ok(())
}

/// Sets the user-visible order; `order` must be a permutation of the workspace's roots.
pub fn order_roots(world: &mut World, ws: Entity, order: &[Entity]) -> R<()> {
    workspace(world, ws)?;
    let roots: Vec<Entity> = world
        .get::<Roots>(ws)
        .map(|r| r.iter().collect())
        .unwrap_or_default();
    let permutation = roots.len() == order.len()
        && order.iter().all(|e| roots.contains(e))
        && order
            .iter()
            .enumerate()
            .all(|(i, e)| !order.get(..i).is_some_and(|prefix| prefix.contains(e)));
    if !permutation {
        return Err(LayoutError::RootOrderMismatch);
    }
    if let Some(mut current) = world.get_mut::<RootOrder>(ws) {
        current.set_if_neq(RootOrder(order.to_vec()));
    }
    Ok(())
}

/// Renames a template node or root (`Name`, copied to instances).
pub fn rename(world: &mut World, node_or_root: Entity, name: &str) -> R<()> {
    template_node(world, node_or_root)?;
    check_name(name)?;
    let root = root_of(world, node_or_root)?;
    world
        .entity_mut(node_or_root)
        .insert(Name::new(name.to_owned()));
    bump(world, root);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Template nodes
// ---------------------------------------------------------------------------------------------

/// Spawns a template node under `parent` at `index` (default: last). With a `PaneTemplate` the
/// node becomes a placing leaf of a new `Disabled`, `Process::Starting` pane; the lifecycle
/// materialises the process.
pub fn spawn_node(
    world: &mut World,
    parent: Entity,
    index: Option<usize>,
    node: Node,
    template: Option<PaneTemplate>,
) -> R<Entity> {
    template_node(world, parent)?;
    editable_container(world, parent)?;
    let root = root_of(world, parent)?;
    let ws = workspace_of_root(world, root)?;
    open_workspace(world, ws)?;
    check_depth(depth_of(world, parent) + 1)?;
    check_capacity(world, ws, 1, usize::from(template.is_some()))?;
    let len = children(world, parent).len();
    let at = index.unwrap_or(len);
    if at > len {
        return Err(LayoutError::IndexOutOfRange { index: at, len });
    }
    let id = world.resource_mut::<Ids>().allocate_node();
    let child = world.spawn((TemplateNode, id, node)).id();
    if let Some(template) = template {
        let pane = spawn_pane(world, ws, template);
        world.entity_mut(child).insert(Places(pane));
    }
    world.entity_mut(parent).insert_children(at, &[child]);
    bump(world, root);
    Ok(child)
}

/// Despawns a non-root template subtree; refused while it places a pane that has not exited.
/// Exited panes it places are despawned with it.
pub fn despawn_node(world: &mut World, node: Entity) -> R<()> {
    template_node(world, node)?;
    if world.get::<TemplateRoot>(node).is_some() {
        return Err(LayoutError::IsATemplateRoot(node));
    }
    let root = root_of(world, node)?;
    let panes = placed_panes(world, node);
    if let Some(&live) = panes.iter().find(|&&p| pane_is_live(world, p)) {
        return Err(LayoutError::PaneLive(live));
    }
    world.despawn(node);
    for pane in panes {
        world.despawn(pane);
    }
    bump(world, root);
    Ok(())
}

/// Moves a non-root template subtree under `new_parent` (same workspace) at `index`.
pub fn reparent_node(
    world: &mut World,
    node: Entity,
    new_parent: Entity,
    index: Option<usize>,
) -> R<()> {
    template_node(world, node)?;
    template_node(world, new_parent)?;
    if world.get::<TemplateRoot>(node).is_some() {
        return Err(LayoutError::IsATemplateRoot(node));
    }
    editable_container(world, new_parent)?;
    editable_slot(world, node)?;
    if new_parent == node || is_descendant(world, node, new_parent) {
        return Err(LayoutError::Cycle(node));
    }
    let old_root = root_of(world, node)?;
    let new_root = root_of(world, new_parent)?;
    if workspace_of_root(world, old_root)? != workspace_of_root(world, new_root)? {
        return Err(LayoutError::CrossWorkspace);
    }
    let (_, height) = subtree_stats(world, node);
    check_depth(depth_of(world, new_parent) + 1 + height)?;
    let siblings = children(world, new_parent);
    let already_child = siblings.contains(&node);
    // A node already under `new_parent` is moved among its siblings, so the range is one
    // shorter (`Children::place` removes before it inserts).
    let len = siblings.len() - usize::from(already_child);
    if let Some(i) = index
        && i > len
    {
        return Err(LayoutError::IndexOutOfRange { index: i, len });
    }
    let at = index.unwrap_or(len);
    world.entity_mut(new_parent).insert_children(at, &[node]);
    bump(world, old_root);
    if new_root != old_root {
        bump(world, new_root);
        retarget_after_move(world, node, old_root, new_root);
    }
    Ok(())
}

/// Moves a node to `index` among its siblings.
pub fn reorder_node(world: &mut World, node: Entity, index: usize) -> R<()> {
    template_node(world, node)?;
    let parent = world
        .get::<ChildOf>(node)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(node))?;
    let len = children(world, parent).len();
    if index >= len {
        return Err(LayoutError::IndexOutOfRange { index, len });
    }
    let root = root_of(world, node)?;
    world.entity_mut(parent).insert_children(index, &[node]);
    bump(world, root);
    Ok(())
}

/// Swaps the tree slots of two non-root template nodes of the same workspace (leaves or whole
/// subtrees, under the same or different parents and roots); each keeps its own `Node` style
/// and the pane it places. Refused for a node and its own ancestor.
pub fn exchange(world: &mut World, a: Entity, b: Entity) -> R<()> {
    template_node(world, a)?;
    template_node(world, b)?;
    if a == b {
        return Err(LayoutError::Cycle(a));
    }
    let parent_a = world
        .get::<ChildOf>(a)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(a))?;
    let parent_b = world
        .get::<ChildOf>(b)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(b))?;
    editable_container(world, parent_a)?;
    editable_container(world, parent_b)?;
    if is_descendant(world, a, b) {
        return Err(LayoutError::Cycle(b));
    }
    if is_descendant(world, b, a) {
        return Err(LayoutError::Cycle(a));
    }
    let root_a = root_of(world, a)?;
    let root_b = root_of(world, b)?;
    if workspace_of_root(world, root_a)? != workspace_of_root(world, root_b)? {
        return Err(LayoutError::CrossWorkspace);
    }
    let (_, height_a) = subtree_stats(world, a);
    let (_, height_b) = subtree_stats(world, b);
    check_depth(depth_of(world, parent_b) + 1 + height_a)?;
    check_depth(depth_of(world, parent_a) + 1 + height_b)?;
    let index_a = children(world, parent_a)
        .iter()
        .position(|c| *c == a)
        .ok_or(LayoutError::NotATemplateNode(a))?;
    let index_b = children(world, parent_b)
        .iter()
        .position(|c| *c == b)
        .ok_or(LayoutError::NotATemplateNode(b))?;
    if parent_a == parent_b {
        if let Some(mut kids) = world.get_mut::<Children>(parent_a) {
            kids.swap(index_a, index_b);
        }
    } else {
        // Each insert first detaches the node from its old parent, so the second slot index is
        // still the one read above: `a` left `parent_a` before `b` is placed there.
        world.entity_mut(parent_b).insert_children(index_b, &[a]);
        world.entity_mut(parent_a).insert_children(index_a, &[b]);
    }
    bump(world, root_a);
    if root_b != root_a {
        bump(world, root_b);
        retarget_after_move(world, a, root_a, root_b);
        retarget_after_move(world, b, root_b, root_a);
    }
    Ok(())
}

/// Applies a [`NodePatch`] to a template node's `Node`, `ZIndex`, `BackgroundColor` and
/// `BorderColor`; nothing is written when any field is invalid.
pub fn patch_node(world: &mut World, node: Entity, patch: &NodePatch) -> R<()> {
    template_node(world, node)?;
    let root = root_of(world, node)?;
    let entity = world.entity(node);
    let mut style = entity
        .get::<Node>()
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(node))?;
    let mut z = entity.get::<ZIndex>().copied().unwrap_or_default();
    let mut background = entity.get::<BackgroundColor>().copied().unwrap_or_default();
    let mut border = entity.get::<BorderColor>().copied().unwrap_or_default();
    patch.apply(&mut style, &mut z, &mut background, &mut border)?;
    let mut entity = world.entity_mut(node);
    if let Some(mut current) = entity.get_mut::<Node>() {
        current.set_if_neq(style);
    }
    if let Some(mut current) = entity.get_mut::<ZIndex>() {
        current.set_if_neq(z);
    }
    if let Some(mut current) = entity.get_mut::<BackgroundColor>() {
        current.set_if_neq(background);
    }
    if let Some(mut current) = entity.get_mut::<BorderColor>() {
        current.set_if_neq(border);
    }
    bump(world, root);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Conveniences over the primitives
// ---------------------------------------------------------------------------------------------

/// Splits the leaf placing `pane`: a new leaf with a new pane appears to its right or below it.
/// When the parent already flexes along that axis the new leaf becomes a sibling; otherwise the
/// leaf is wrapped in a Row/Column container that inherits its placement, and both leaves get
/// `flex_grow: 1`.
pub fn split(
    world: &mut World,
    pane: Entity,
    direction: SplitDirection,
    template: PaneTemplate,
) -> R<(Entity, Entity)> {
    let leaf = leaf_of(world, pane)?;
    let parent = world
        .get::<ChildOf>(leaf)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(leaf))?;
    let root = root_of(world, leaf)?;
    let ws = workspace_of_root(world, root)?;
    open_workspace(world, ws)?;
    let axis = match direction {
        SplitDirection::Right => FlexDirection::Row,
        SplitDirection::Below => FlexDirection::Column,
    };
    let leaf_node = world
        .get::<Node>(leaf)
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(leaf))?;
    let parent_node = world
        .get::<Node>(parent)
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(parent))?;
    let index = children(world, parent)
        .iter()
        .position(|c| *c == leaf)
        .ok_or(LayoutError::NotATemplateNode(leaf))?;
    let same_axis = parent_node.display == Display::Flex
        && parent_node.flex_direction == axis
        && parent_node.flex_wrap == FlexWrap::NoWrap
        && leaf_node.flex_grow > 0.0;
    let new_leaf = if same_axis {
        spawn_node(
            world,
            parent,
            Some(index + 1),
            Node {
                flex_grow: leaf_node.flex_grow,
                ..Node::default()
            },
            Some(template),
        )?
    } else {
        check_depth(depth_of(world, leaf) + 1)?;
        check_capacity(world, ws, 2, 1)?;
        let container = spawn_node(
            world,
            parent,
            Some(index),
            placement_of(&leaf_node, axis),
            None,
        )?;
        world.entity_mut(container).add_children(&[leaf]);
        if let Some(mut node) = world.get_mut::<Node>(leaf) {
            node.set_if_neq(without_placement(leaf_node));
        }
        spawn_node(
            world,
            container,
            None,
            Node {
                flex_grow: 1.0,
                ..Node::default()
            },
            Some(template),
        )?
    };
    let new_pane = world
        .get::<Places>(new_leaf)
        .map(|p| p.0)
        .ok_or(LayoutError::PaneNotPlaced(new_leaf))?;
    Ok((new_leaf, new_pane))
}

/// Moves an existing non-root template subtree `node` beside `target` (a non-root template
/// node of the same workspace, outside `node`'s subtree) the way [`split`] would place a new
/// leaf: when `target`'s parent already flexes along the side's axis, `node` becomes `target`'s
/// neighbour with `target`'s `flex_grow`; otherwise `target` is wrapped in a Row/Column
/// container that inherits its placement and both fill it equally.
pub fn place_beside(world: &mut World, node: Entity, target: Entity, side: Side) -> R<()> {
    template_node(world, node)?;
    template_node(world, target)?;
    if node == target || is_descendant(world, node, target) {
        return Err(LayoutError::Cycle(node));
    }
    let target_parent = world
        .get::<ChildOf>(target)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(target))?;
    if world.get::<ChildOf>(node).is_none() {
        return Err(LayoutError::IsATemplateRoot(node));
    }
    editable_container(world, target_parent)?;
    editable_slot(world, node)?;
    let old_root = root_of(world, node)?;
    let new_root = root_of(world, target)?;
    let ws = workspace_of_root(world, new_root)?;
    if workspace_of_root(world, old_root)? != ws {
        return Err(LayoutError::CrossWorkspace);
    }
    open_workspace(world, ws)?;
    let (axis, after) = match side {
        Side::Left => (FlexDirection::Row, false),
        Side::Right => (FlexDirection::Row, true),
        Side::Top => (FlexDirection::Column, false),
        Side::Bottom => (FlexDirection::Column, true),
    };
    let target_node = world
        .get::<Node>(target)
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(target))?;
    let parent_node = world
        .get::<Node>(target_parent)
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(target_parent))?;
    let node_style = world
        .get::<Node>(node)
        .cloned()
        .ok_or(LayoutError::NotATemplateNode(node))?;
    let (_, height) = subtree_stats(world, node);
    let same_axis = parent_node.display == Display::Flex
        && parent_node.flex_direction == axis
        && parent_node.flex_wrap == FlexWrap::NoWrap
        && target_node.flex_grow > 0.0;
    if same_axis {
        check_depth(depth_of(world, target_parent) + 1 + height)?;
        // `insert_children` detaches `node` first, so its index is taken after that removal.
        world.entity_mut(node).remove::<ChildOf>();
        let index = children(world, target_parent)
            .iter()
            .position(|c| *c == target)
            .ok_or(LayoutError::NotATemplateNode(target))?;
        world
            .entity_mut(target_parent)
            .insert_children(index + usize::from(after), &[node]);
        if let Some(mut style) = world.get_mut::<Node>(node) {
            style.set_if_neq(with_placement(
                node_style,
                &Node {
                    flex_grow: target_node.flex_grow,
                    ..Node::default()
                },
            ));
        }
    } else {
        check_depth(depth_of(world, target) + 1 + height)?;
        check_capacity(world, ws, 1, 0)?;
        let index = children(world, target_parent)
            .iter()
            .position(|c| *c == target)
            .ok_or(LayoutError::NotATemplateNode(target))?;
        let container = spawn_node(
            world,
            target_parent,
            Some(index),
            placement_of(&target_node, axis),
            None,
        )?;
        let pair = if after {
            [target, node]
        } else {
            [node, target]
        };
        world.entity_mut(container).add_children(&pair);
        if let Some(mut style) = world.get_mut::<Node>(target) {
            style.set_if_neq(without_placement(target_node));
        }
        if let Some(mut style) = world.get_mut::<Node>(node) {
            style.set_if_neq(without_placement(node_style));
        }
    }
    bump(world, old_root);
    if new_root != old_root {
        bump(world, new_root);
        retarget_after_move(world, node, old_root, new_root);
    }
    Ok(())
}

/// Swaps the leaf placing `pane` with its next sibling (the previous one when it is last).
pub fn swap(world: &mut World, pane: Entity, direction: SplitDirection) -> R<()> {
    let _ = direction;
    let leaf = leaf_of(world, pane)?;
    let parent = world
        .get::<ChildOf>(leaf)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(leaf))?;
    let root = root_of(world, leaf)?;
    let siblings = children(world, parent);
    let index = siblings
        .iter()
        .position(|c| *c == leaf)
        .ok_or(LayoutError::NotATemplateNode(leaf))?;
    let other = if index + 1 < siblings.len() {
        index + 1
    } else if index > 0 {
        index - 1
    } else {
        return Err(LayoutError::NoNeighbour);
    };
    if let Some(mut kids) = world.get_mut::<Children>(parent) {
        kids.swap(index, other);
    }
    bump(world, root);
    Ok(())
}

/// Removes the leaf placing `pane` (the pane itself is the lifecycle's to despawn), collapses a
/// container left with one child into its slot, despawns emptied containers and closes the root
/// when nothing is left in it.
pub fn remove_leaf(world: &mut World, pane: Entity) -> R<()> {
    let leaf = leaf_of(world, pane)?;
    let root = root_of(world, leaf)?;
    let mut parent = world
        .get::<ChildOf>(leaf)
        .map(ChildOf::parent)
        .ok_or(LayoutError::IsATemplateRoot(leaf))?;
    world.despawn(leaf);
    loop {
        let (len, only) = {
            let kids = children(world, parent);
            (kids.len(), kids.first().copied())
        };
        if parent == root {
            if len == 0 {
                close_root(world, root)?;
                return Ok(());
            }
            break;
        }
        let grandparent = world
            .get::<ChildOf>(parent)
            .map(ChildOf::parent)
            .ok_or(LayoutError::NotATemplateNode(parent))?;
        match (len, only) {
            (0, _) => {
                world.despawn(parent);
                parent = grandparent;
            }
            (1, Some(only)) => {
                let slot = children(world, grandparent)
                    .iter()
                    .position(|c| *c == parent)
                    .unwrap_or(0);
                let container_node = world.get::<Node>(parent).cloned().unwrap_or_default();
                if let Some(mut node) = world.get_mut::<Node>(only) {
                    let merged = with_placement(node.clone(), &container_node);
                    node.set_if_neq(merged);
                }
                world.entity_mut(grandparent).insert_children(slot, &[only]);
                world.despawn(parent);
                break;
            }
            _ => break,
        }
    }
    bump(world, root);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Viewers
// ---------------------------------------------------------------------------------------------

/// Spawns a viewer with its layout camera and picking pointer, showing the first root in order
/// (or the root placing `exact`) and targeting the first pane in it (or `exact`).
pub fn attach_viewer(
    world: &mut World,
    ws: Entity,
    viewport: Viewport,
    exact: Option<Entity>,
) -> R<Entity> {
    open_workspace(world, ws)?;
    let count = world
        .query_filtered::<(), With<Viewer>>()
        .iter(world)
        .count();
    let max = world.resource::<Limits>().viewers;
    if count + 1 > max {
        return Err(LayoutError::TooManyViewers {
            count: count + 1,
            max,
        });
    }
    let root = match exact {
        Some(pane) => {
            pane_entity(world, pane)?;
            if world.get::<PaneIn>(pane).map(|p| p.0) != Some(ws) {
                return Err(LayoutError::CrossWorkspace);
            }
            Some(root_of(world, leaf_of(world, pane)?)?)
        }
        None => world
            .get::<RootOrder>(ws)
            .and_then(|o| o.0.first().copied()),
    };
    let target = exact.or_else(|| root.and_then(|r| first_pane_in(world, r)));

    let mut camera = Camera::default();
    let mut render_target = RenderTarget::None { size: UVec2::ZERO };
    size::apply_viewport(&mut camera, &mut render_target, viewport);
    let camera = world.spawn((camera, render_target)).id();
    let id = world.resource_mut::<Ids>().allocate_viewer();
    // One custom pointer per attachment; ids never repeat within a server instance.
    let pointer = world
        .spawn(PointerId::Custom(Uuid::from_u64_pair(0x6675_7800, id.0)))
        .id();
    let viewer = world
        .spawn((
            Viewer,
            id,
            Viewing(ws),
            viewport,
            ViewerCamera(camera),
            ViewerPointer(pointer),
            RequestQueue::default(),
            CreationBarrier::default(),
            ProjectionBaseline::default(),
            ViewState::default(),
        ))
        .id();
    let mut entity = world.entity_mut(viewer);
    if let Some(root) = root {
        entity.insert(Showing(root));
    }
    if let Some(target) = target {
        entity.insert(Targets(target));
    }
    if exact.is_some() {
        entity.insert(ExactTarget);
    }
    Ok(viewer)
}

/// Despawns the viewer's instances, camera, pointer and the viewer itself.
pub fn detach_viewer(world: &mut World, viewer: Entity) -> R<()> {
    viewer_entity(world, viewer)?;
    let camera = world.get::<ViewerCamera>(viewer).map(|c| c.0);
    let pointer = world.get::<ViewerPointer>(viewer).map(|p| p.0);
    if let Some(camera) = camera {
        for instance in instances::instance_roots_of_camera(world, camera) {
            world.despawn(instance);
        }
        world.despawn(camera);
    }
    if let Some(pointer) = pointer {
        world.despawn(pointer);
    }
    world.despawn(viewer);
    Ok(())
}

/// Updates the viewport and the camera's target size; layout follows on the next update.
pub fn resize_viewer(world: &mut World, viewer: Entity, viewport: Viewport) -> R<()> {
    viewer_entity(world, viewer)?;
    if let Some(mut current) = world.get_mut::<Viewport>(viewer) {
        current.set_if_neq(viewport);
    }
    if let Some(camera) = world.get::<ViewerCamera>(viewer).map(|c| c.0) {
        let size = size::target_size(viewport);
        if let Some(mut camera) = world.get_mut::<Camera>(camera) {
            size::set_camera_size(&mut camera, size);
        }
        if let Some(mut target) = world.get_mut::<RenderTarget>(camera) {
            size::set_render_target(&mut target, size);
        }
    }
    Ok(())
}

/// Shows a root of the viewer's workspace; keeps the target when it is in that root, otherwise
/// targets the root's first pane.
pub fn show_root(world: &mut World, viewer: Entity, root: Entity) -> R<()> {
    viewer_entity(world, viewer)?;
    template_root(world, root)?;
    let ws = world
        .get::<Viewing>(viewer)
        .map(|v| v.0)
        .ok_or(LayoutError::NotAViewer(viewer))?;
    if workspace_of_root(world, root)? != ws {
        return Err(LayoutError::CrossWorkspace);
    }
    let target = world.get::<Targets>(viewer).map(|t| t.0);
    let target_in_root = target.is_some_and(|t| pane_in_root(world, t, root));
    if world.get::<ExactTarget>(viewer).is_some() && !target_in_root {
        return Err(LayoutError::ExactTarget(viewer));
    }
    let previous = world.get::<Showing>(viewer).map(|s| s.0);
    if previous != Some(root) {
        world.entity_mut(viewer).insert(Showing(root));
        if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
            state.zoom = None;
        }
    }
    if !target_in_root {
        match first_pane_in(world, root) {
            Some(pane) => {
                world.entity_mut(viewer).insert(Targets(pane));
            }
            None => {
                world.entity_mut(viewer).remove::<Targets>();
            }
        }
    }
    Ok(())
}

/// Retargets input to `pane`, showing its root first when it lives in another root.
pub fn target(world: &mut World, viewer: Entity, pane: Entity) -> R<()> {
    viewer_entity(world, viewer)?;
    pane_entity(world, pane)?;
    if world.get::<ExactTarget>(viewer).is_some() {
        return Err(LayoutError::ExactTarget(viewer));
    }
    let ws = world.get::<Viewing>(viewer).map(|v| v.0);
    if world.get::<PaneIn>(pane).map(|p| p.0) != ws {
        return Err(LayoutError::CrossWorkspace);
    }
    let root = root_of(world, leaf_of(world, pane)?)?;
    if world.get::<Showing>(viewer).map(|s| s.0) != Some(root) {
        show_root(world, viewer, root)?;
    }
    world.entity_mut(viewer).insert(Targets(pane));
    Ok(())
}

/// Zooms the viewer's instance of `node` (a template or instance node in the shown root).
pub fn zoom(world: &mut World, viewer: Entity, node: Entity) -> R<()> {
    let id = shown_node_id(world, viewer, node)?;
    if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
        state.zoom = Some(id);
    }
    instances::apply_view_state(world, viewer);
    Ok(())
}

/// Restores the viewer's instance to the template's layout.
pub fn unzoom(world: &mut World, viewer: Entity) -> R<()> {
    viewer_entity(world, viewer)?;
    if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
        state.zoom = None;
    }
    instances::apply_view_state(world, viewer);
    Ok(())
}

/// Scrolls the viewer's instance of `node` by `rows` (clamped at zero; layout clamps the far end).
pub fn scroll(world: &mut World, viewer: Entity, node: Entity, rows: i32) -> R<()> {
    let id = shown_node_id(world, viewer, node)?;
    let mut offset = 0.0;
    if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
        let current = state.scroll.get(&id).copied().unwrap_or(0.0);
        offset = (current + rows as f32).max(0.0);
        if offset > 0.0 {
            state.scroll.insert(id, offset);
        } else {
            state.scroll.remove(&id);
        }
    }
    if let Some(instance) = instances::instance_root(world, viewer)
        .and_then(|root| instances::find_by_node_id(world, root, id))
        && let Some(mut position) = world.get_mut::<ScrollPosition>(instance)
        && position.0.y.to_bits() != offset.to_bits()
    {
        position.0 = Vec2::new(0.0, offset);
    }
    Ok(())
}

/// Overrides the `Display` of the viewer's instance of `node` (a template or instance node in
/// the shown root); `None` restores the template's value. Transient: kept in [`ViewState`] and
/// re-applied on re-clone, never written to the template.
pub fn set_display(
    world: &mut World,
    viewer: Entity,
    node: Entity,
    display: Option<Display>,
) -> R<()> {
    let id = shown_node_id(world, viewer, node)?;
    if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
        match display {
            Some(display) => {
                state.display.insert(id, display);
            }
            None => {
                state.display.remove(&id);
            }
        }
    }
    instances::apply_view_state(world, viewer);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Read-only geometry queries
// ---------------------------------------------------------------------------------------------

/// The pane the viewer would reach from its target in `direction`, from the laid-out instance
/// leaves: rect-edge distance with at least half overlap for the cardinal directions
/// (`bevy_input_focus::navigator`), tree order for `Next`/`Prev`.
pub fn navigate(world: &World, viewer: Entity, direction: NavDirection) -> Option<Entity> {
    let target = world.get::<Targets>(viewer)?.0;
    let root = instances::instance_root(world, viewer)?;
    let mut leaves: Vec<FocusableArea> = Vec::new();
    let mut panes: Vec<Entity> = Vec::new();
    instances::walk(world, root, &mut |world, entity| {
        let (Some(shows), Some(computed), Some(transform)) = (
            world.get::<Shows>(entity),
            world.get::<ComputedNode>(entity),
            world.get::<UiGlobalTransform>(entity),
        ) else {
            return;
        };
        if computed.is_empty() {
            return;
        }
        leaves.push(FocusableArea {
            entity,
            position: transform.translation,
            size: computed.size,
        });
        panes.push(shows.0);
    });
    let origin = panes.iter().position(|p| *p == target)?;
    let index = match direction {
        NavDirection::Next => (origin + 1) % panes.len(),
        NavDirection::Prev => (origin + panes.len() - 1) % panes.len(),
        NavDirection::Left | NavDirection::Right | NavDirection::Up | NavDirection::Down => {
            let octant = match direction {
                NavDirection::Left => CompassOctant::West,
                NavDirection::Right => CompassOctant::East,
                NavDirection::Up => CompassOctant::North,
                _ => CompassOctant::South,
            };
            let config = AutoNavigationConfig {
                min_alignment_factor: 0.5,
                max_search_distance: None,
                prefer_aligned: true,
            };
            let best = find_best_candidate(leaves.get(origin)?, octant, &leaves, &config)?;
            leaves.iter().position(|l| l.entity == best)?
        }
    };
    panes.get(index).copied()
}

/// The pane shown at viewport cell (`col`, `row`) of the viewer, topmost first.
pub fn pane_at(world: &World, viewer: Entity, col: u16, row: u16) -> Option<Entity> {
    let root = instances::instance_root(world, viewer)?;
    let point = Vec2::new(f32::from(col) + 0.5, f32::from(row) + 0.5);
    let mut best: Option<(u32, Entity)> = None;
    instances::walk(world, root, &mut |world, entity| {
        let (Some(shows), Some(computed), Some(transform), Some(stack)) = (
            world.get::<Shows>(entity),
            world.get::<ComputedNode>(entity),
            world.get::<UiGlobalTransform>(entity),
            world.get::<ComputedStackIndex>(entity),
        ) else {
            return;
        };
        if computed.contains_point(*transform, point) && best.is_none_or(|(s, _)| stack.0 > s) {
            best = Some((stack.0, shows.0));
        }
    });
    best.map(|(_, pane)| pane)
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn check_name(name: &str) -> R<()> {
    let ok = !name.is_empty()
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
        && name.trim() == name;
    if ok {
        Ok(())
    } else {
        Err(LayoutError::InvalidName(name.to_owned()))
    }
}

fn workspace(world: &World, ws: Entity) -> R<()> {
    match world.get_entity(ws) {
        Ok(entity) if entity.contains::<Workspace>() => Ok(()),
        Ok(_) => Err(LayoutError::NotAWorkspace(ws)),
        Err(_) => Err(LayoutError::NoSuchEntity(ws)),
    }
}

fn open_workspace(world: &World, ws: Entity) -> R<()> {
    workspace(world, ws)?;
    if world.get::<Open>(ws).is_some() {
        Ok(())
    } else {
        Err(LayoutError::WorkspaceRetiring(ws))
    }
}

fn template_node(world: &World, node: Entity) -> R<()> {
    match world.get_entity(node) {
        Ok(entity) if entity.contains::<TemplateNode>() => Ok(()),
        Ok(_) => Err(LayoutError::NotATemplateNode(node)),
        Err(_) => Err(LayoutError::NoSuchEntity(node)),
    }
}

/// A template node that may take children: not a placing leaf, not a surface leaf.
fn editable_container(world: &World, node: Entity) -> R<()> {
    if world.get::<Places>(node).is_some() {
        return Err(LayoutError::LeafHasChildren(node));
    }
    if world.get::<Surface>(node).is_some() {
        return Err(LayoutError::SurfaceSubtree(node));
    }
    Ok(())
}

/// A template node that may be moved out of its slot: its parent is not a surface leaf.
fn editable_slot(world: &World, node: Entity) -> R<()> {
    match world.get::<ChildOf>(node).map(ChildOf::parent) {
        Some(parent) if world.get::<Surface>(parent).is_some() => {
            Err(LayoutError::SurfaceSubtree(parent))
        }
        _ => Ok(()),
    }
}

fn template_root(world: &World, root: Entity) -> R<()> {
    match world.get_entity(root) {
        Ok(entity) if entity.contains::<TemplateRoot>() => Ok(()),
        Ok(_) => Err(LayoutError::NotATemplateRoot(root)),
        Err(_) => Err(LayoutError::NoSuchEntity(root)),
    }
}

fn pane_entity(world: &World, pane: Entity) -> R<()> {
    match world.get_entity(pane) {
        Ok(entity) if entity.contains::<Pane>() => Ok(()),
        Ok(_) => Err(LayoutError::NotAPane(pane)),
        Err(_) => Err(LayoutError::NoSuchEntity(pane)),
    }
}

fn viewer_entity(world: &World, viewer: Entity) -> R<()> {
    match world.get_entity(viewer) {
        Ok(entity) if entity.contains::<Viewer>() => Ok(()),
        Ok(_) => Err(LayoutError::NotAViewer(viewer)),
        Err(_) => Err(LayoutError::NoSuchEntity(viewer)),
    }
}

fn root_of(world: &World, node: Entity) -> R<Entity> {
    root_of_template(world, node).ok_or(LayoutError::NotATemplateNode(node))
}

fn workspace_of_root(world: &World, root: Entity) -> R<Entity> {
    world
        .get::<RootOf>(root)
        .map(|r| r.0)
        .ok_or(LayoutError::NotATemplateRoot(root))
}

fn children(world: &World, node: Entity) -> &[Entity] {
    world.get::<Children>(node).map_or(&[], |c| &**c)
}

fn leaf_of(world: &World, pane: Entity) -> R<Entity> {
    pane_entity(world, pane)?;
    world
        .get::<PlacedIn>(pane)
        .and_then(|p| p.iter().next())
        .ok_or(LayoutError::PaneNotPlaced(pane))
}

fn pane_is_live(world: &World, pane: Entity) -> bool {
    !matches!(world.get::<Process>(pane), Some(Process::Exited { .. }))
}

fn pane_in_root(world: &World, pane: Entity, root: Entity) -> bool {
    world.get::<PlacedIn>(pane).is_some_and(|p| {
        p.iter()
            .any(|leaf| root_of_template(world, leaf) == Some(root))
    })
}

/// Pre-order walk of a template subtree.
fn walk_template(world: &World, root: Entity, mut f: impl FnMut(Entity, usize)) {
    let mut stack = vec![(root, 0usize)];
    while let Some((entity, depth)) = stack.pop() {
        f(entity, depth);
        for &child in children(world, entity).iter().rev() {
            stack.push((child, depth + 1));
        }
    }
}

/// `(node count, height in edges)` of a subtree.
fn subtree_stats(world: &World, root: Entity) -> (usize, usize) {
    let (mut count, mut height) = (0, 0);
    walk_template(world, root, |_, depth| {
        count += 1;
        height = height.max(depth);
    });
    (count, height)
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

fn is_descendant(world: &World, ancestor: Entity, node: Entity) -> bool {
    let mut found = false;
    walk_template(world, ancestor, |entity, _| found |= entity == node);
    found
}

fn placed_panes(world: &World, root: Entity) -> Vec<Entity> {
    let mut panes = Vec::new();
    walk_template(world, root, |entity, _| {
        if let Some(places) = world.get::<Places>(entity) {
            panes.push(places.0);
        }
    });
    panes
}

fn first_pane_in(world: &World, root: Entity) -> Option<Entity> {
    let mut first = None;
    walk_template(world, root, |entity, _| {
        if first.is_none()
            && let Some(places) = world.get::<Places>(entity)
        {
            first = Some(places.0);
        }
    });
    first
}

fn check_depth(depth: usize) -> R<()> {
    if depth > MAX_DEPTH {
        Err(LayoutError::DepthExceeded {
            depth,
            max: MAX_DEPTH,
        })
    } else {
        Ok(())
    }
}

fn check_capacity(world: &World, ws: Entity, extra_nodes: usize, extra_panes: usize) -> R<()> {
    let limits = world.resource::<Limits>();
    let mut nodes = 0;
    if let Some(roots) = world.get::<Roots>(ws) {
        for root in roots.iter() {
            nodes += subtree_stats(world, root).0;
        }
    }
    let nodes = nodes + extra_nodes;
    if nodes > limits.nodes_per_workspace {
        return Err(LayoutError::TooManyNodes {
            count: nodes,
            max: limits.nodes_per_workspace,
        });
    }
    let panes = world.get::<WorkspacePanes>(ws).map_or(0, |p| p.len()) + extra_panes;
    if panes > limits.panes_per_workspace {
        return Err(LayoutError::TooManyPanes {
            count: panes,
            max: limits.panes_per_workspace,
        });
    }
    Ok(())
}

fn bump(world: &mut World, root: Entity) {
    if let Some(mut generation) = world.get_mut::<LayoutGeneration>(root) {
        generation.0 += 1;
    }
}

fn spawn_pane(world: &mut World, ws: Entity, template: PaneTemplate) -> Entity {
    let id = world.resource_mut::<Ids>().allocate_pane();
    let retain = world.resource::<Limits>().final_retain_ms;
    let workspace_name = world
        .get::<WorkspaceName>(ws)
        .map(|n| n.0.clone())
        .unwrap_or_default();
    let attribution = LaunchAttribution {
        workspace_name,
        stream: template.stream.clone(),
        argv: template.argv.clone(),
        cwd: template.cwd.clone(),
    };
    world
        .spawn((
            (
                Pane,
                id,
                PaneIn(ws),
                Process::Starting,
                WorkspaceStream(template.stream.clone()),
                template,
                attribution,
            ),
            (
                PaneSize::default(),
                FinalRetention(retain),
                OutputPacing::default(),
                RightClickPolicy::default(),
                Title::default(),
                Disabled,
            ),
        ))
        .id()
}

/// Viewers showing `root` (which is going away or leaving their workspace) show the first other
/// root of their workspace, or nothing. Exact attachments keep following their pane.
fn reshow_viewers(world: &mut World, root: Entity) {
    let viewers: Vec<(Entity, Entity)> = world
        .query_filtered::<(Entity, &Viewing, &Showing), (With<Viewer>, Without<ExactTarget>)>()
        .iter(world)
        .filter(|(_, _, showing)| showing.0 == root)
        .map(|(viewer, viewing, _)| (viewer, viewing.0))
        .collect();
    for (viewer, ws) in viewers {
        let next = world
            .get::<RootOrder>(ws)
            .and_then(|o| o.0.iter().copied().find(|r| *r != root));
        match next {
            Some(next) => {
                // `next` is a live root of the viewer's workspace, so this cannot fail.
                let _ = show_root(world, viewer, next);
            }
            None => {
                world.entity_mut(viewer).remove::<(Showing, Targets)>();
            }
        }
    }
}

/// After the subtree `moved` left `old_root` for `new_root`: viewers showing `old_root` whose
/// target went with it retarget to the first pane still in `old_root`; exact attachments follow
/// their pane and show `new_root`.
fn retarget_after_move(world: &mut World, moved: Entity, old_root: Entity, new_root: Entity) {
    let panes = placed_panes(world, moved);
    if panes.is_empty() {
        return;
    }
    let viewers: Vec<(Entity, bool)> = world
        .query_filtered::<(Entity, &Targets, &Showing, Has<ExactTarget>), With<Viewer>>()
        .iter(world)
        .filter(|(_, targets, showing, _)| showing.0 == old_root && panes.contains(&targets.0))
        .map(|(viewer, _, _, exact)| (viewer, exact))
        .collect();
    for (viewer, exact) in viewers {
        if exact {
            world.entity_mut(viewer).insert(Showing(new_root));
            if let Some(mut state) = world.get_mut::<ViewState>(viewer) {
                state.zoom = None;
            }
        } else {
            match first_pane_in(world, old_root) {
                Some(pane) => {
                    world.entity_mut(viewer).insert(Targets(pane));
                }
                None => {
                    world.entity_mut(viewer).remove::<Targets>();
                }
            }
        }
    }
}

/// Resolves a template or instance node the viewer shows to its template `NodeId`.
fn shown_node_id(world: &World, viewer: Entity, node: Entity) -> R<NodeId> {
    viewer_entity(world, viewer)?;
    let template = match world.get::<InstanceOf>(node) {
        Some(instance_of) => instance_of.0,
        None => node,
    };
    template_node(world, template)?;
    let shown = world.get::<Showing>(viewer).map(|s| s.0);
    if root_of_template(world, template) != shown {
        return Err(LayoutError::NotShown(node));
    }
    world
        .get::<NodeId>(template)
        .copied()
        .ok_or(LayoutError::NotATemplateNode(template))
}

/// A container taking over a leaf's slot: its placement properties, flexing along `axis`.
fn placement_of(leaf: &Node, axis: FlexDirection) -> Node {
    with_placement(
        Node {
            display: Display::Flex,
            flex_direction: axis,
            ..Node::default()
        },
        leaf,
    )
}

/// `node` with the placement properties (how the parent positions it) taken from `from`.
fn with_placement(node: Node, from: &Node) -> Node {
    Node {
        position_type: from.position_type,
        left: from.left,
        right: from.right,
        top: from.top,
        bottom: from.bottom,
        width: from.width,
        height: from.height,
        min_width: from.min_width,
        min_height: from.min_height,
        max_width: from.max_width,
        max_height: from.max_height,
        aspect_ratio: from.aspect_ratio,
        align_self: from.align_self,
        justify_self: from.justify_self,
        margin: from.margin,
        flex_grow: from.flex_grow,
        flex_shrink: from.flex_shrink,
        flex_basis: from.flex_basis,
        grid_row: from.grid_row,
        grid_column: from.grid_column,
        ..node
    }
}

/// `node` filling one slot of a flex container: placement reset, `flex_grow: 1`.
fn without_placement(node: Node) -> Node {
    with_placement(
        node,
        &Node {
            flex_grow: 1.0,
            ..Node::default()
        },
    )
}
