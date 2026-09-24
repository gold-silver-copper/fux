//! The structural rules fux's world keeps, as one checkable list.
//!
//! Every rule here must hold after any request fux has finished handling,
//! whoever sent it: fux's own commands and systems keep them, and the BRP
//! guard (`policy`) refuses what would break them. The in-process property
//! test checks them after every generated request, `fux.invariants` exposes
//! them to black-box harnesses, and debug builds check them after every
//! mutating request.
#[cfg(test)]
mod tests;

use crate::{assets::Settings, model::*};
use bevy_ecs::{prelude::*, reflect::AppTypeRegistry, resource::IsResource};

/// The largest viewer or PTY dimension fux accepts (`fux.attach` clamps to it).
pub const MAX_DIMENSION: u16 = 4096;

/// Every broken rule, one line each. Empty means the world is consistent.
pub fn violations(world: &mut World) -> Vec<String> {
    let mut v = Vec::new();
    if !world.contains_resource::<Settings>() {
        v.push("the Settings resource is missing".into());
    }
    hierarchy(world, &mut v);
    processes(world, &mut v);
    workspace_order(world, &mut v);
    projection(world, &mut v);
    viewers(world, &mut v);
    v
}

/// Every workspace can be projected into a viewer's presentation. When one
/// cannot, fux paints the reason in the bar but can act on nothing in it:
/// every command and key for its viewers is dropped (finding 029).
fn projection(world: &mut World, v: &mut Vec<String>) {
    if !world.contains_resource::<AppTypeRegistry>() {
        return;
    }
    // The cached projection is stale until invalidation has seen the latest
    // changes; fux runs the same system before every projection.
    if let Err(error) = world.run_system_cached(crate::layout::invalidate_layouts) {
        v.push(format!("layout invalidation could not run: {error}"));
        return;
    }
    let roots: Vec<Entity> = world
        .query_filtered::<Entity, With<Workspace>>()
        .iter(world)
        .collect();
    for root in roots {
        if let Err(error) = crate::layout::scene(world, root) {
            v.push(format!("workspace {root} cannot be projected: {error}"));
        }
    }
}

fn parent(world: &World, entity: Entity) -> Option<Entity> {
    world.get::<ChildOf>(entity).map(ChildOf::parent)
}

fn is_layout(world: &World, entity: Entity) -> bool {
    world.get::<Workspace>(entity).is_some()
        || world.get::<Tab>(entity).is_some()
        || world.get::<Split>(entity).is_some()
        || world.get::<PaneView>(entity).is_some()
}

/// Whether `entity` may hold panes and splits: a tab, or a split container.
fn is_container(world: &World, entity: Entity) -> bool {
    world.get::<Tab>(entity).is_some() || world.get::<Split>(entity).is_some()
}

fn hierarchy(world: &mut World, v: &mut Vec<String>) {
    // Every parent exists, on any entity: a relationship to an entity that
    // was never spawned dangles, and Bevy's hooks fail on it (finding 030).
    let parented: Vec<(Entity, Entity)> = world
        .query::<(Entity, &ChildOf)>()
        .iter(world)
        .map(|(e, c)| (e, c.parent()))
        .collect();
    for (entity, parent) in parented {
        if world.get_entity(parent).is_err() {
            v.push(format!(
                "{entity} has ChildOf {parent}, which does not exist"
            ));
        }
    }
    let layout: Vec<Entity> = world
        .query_filtered::<Entity, LayoutRole>()
        .iter(world)
        .collect();
    for entity in layout {
        if world.get::<IsResource>(entity).is_some() {
            v.push(format!("resource entity {entity} is a layout node"));
        }
        if world.get::<Viewer>(entity).is_some() {
            v.push(format!("layout node {entity} is also a viewer"));
        }
        // Walk up the parents; a cycle or a chain deeper than any real layout
        // is reported rather than followed forever.
        let mut cursor = parent(world, entity);
        let mut depth = 0u32;
        while let Some(p) = cursor {
            if p == entity || depth > 1024 {
                v.push(format!("layout node {entity} is inside its own subtree"));
                break;
            }
            depth += 1;
            cursor = parent(world, p);
        }
        let up = parent(world, entity);
        if world.get::<Workspace>(entity).is_some() {
            if let Some(p) = up {
                v.push(format!("workspace {entity} has a parent {p}"));
            }
        } else if world.get::<Tab>(entity).is_some() {
            // A tab with no parent is unplaced: spawned and not yet put in a
            // workspace, as clients add tabs in two requests.
            if up.is_some_and(|p| world.get::<Workspace>(p).is_none()) {
                v.push(format!("tab {entity} has parent {up:?}, not a workspace"));
            }
        } else if world.get::<PaneView>(entity).is_some() && up.is_none() {
            // An unplaced view: spawned and not yet put under a tab, as the
            // README's way of showing a custom process does in two requests.
        } else if !up.is_some_and(|p| is_container(world, p)) {
            let kind = if world.get::<PaneView>(entity).is_some() {
                "pane view"
            } else {
                "split"
            };
            v.push(format!(
                "{kind} {entity} has parent {up:?}, not a tab or a split"
            ));
        }
        if let Some(p) = up {
            let listed = world
                .get::<Children>(p)
                .is_some_and(|children| children.contains(&entity));
            if !listed {
                v.push(format!(
                    "{entity} has ChildOf {p} but is not in its Children"
                ));
            }
        }
        // A split container that is not also a tab or workspace holds at least
        // two children: one-child and empty splits collapse.
        if world.get::<Split>(entity).is_some()
            && world.get::<Tab>(entity).is_none()
            && world.get::<Workspace>(entity).is_none()
        {
            let count = world.get::<Children>(entity).map_or(0, |c| c.len());
            if count < 2 {
                v.push(format!(
                    "split container {entity} has {count} children and did not collapse"
                ));
            }
        }
    }
}

fn processes(world: &mut World, v: &mut Vec<String>) {
    let views: Vec<(Entity, Entity)> = world
        .query::<(Entity, &PaneView)>()
        .iter(world)
        .map(|(leaf, view)| (leaf, view.pane))
        .collect();
    for (leaf, pane) in views {
        match world.get_entity(pane) {
            Err(_) => v.push(format!("view {leaf} refers to missing process {pane}")),
            Ok(entity) => {
                if entity.contains::<IsResource>() || is_layout(world, pane) {
                    v.push(format!(
                        "view {leaf} refers to {pane}, which is not a process"
                    ));
                } else if !entity.contains::<ProcessState>() {
                    v.push(format!(
                        "view {leaf} refers to pane {pane} with no process state"
                    ));
                }
            }
        }
    }
    let states: Vec<(Entity, ProcessState)> = world
        .query::<(Entity, &ProcessState)>()
        .iter(world)
        .map(|(e, s)| (e, s.clone()))
        .collect();
    for (pane, state) in states {
        if state.rows == 0
            || state.cols == 0
            || state.rows > MAX_DIMENSION
            || state.cols > MAX_DIMENSION
        {
            v.push(format!(
                "process {pane} has size {}x{}, outside 1..={MAX_DIMENSION}",
                state.rows, state.cols
            ));
        }
        if let Status::Running { pid, .. } = state.status
            && !alive(pid)
        {
            v.push(format!(
                "process {pane} reports running pid {pid}, which is dead"
            ));
        }
    }
}

/// Whether a process with this ID exists. `kill(pid, 0)` delivers nothing.
fn alive(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(|pid| {
        pid > 0 && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    })
}

fn workspace_order(world: &mut World, v: &mut Vec<String>) {
    let workspaces: Vec<(Entity, Option<i64>)> = world
        .query_filtered::<(Entity, Option<&WorkspaceOrder>), With<Workspace>>()
        .iter(world)
        .map(|(e, o)| (e, o.map(|o| o.0)))
        .collect();
    let mut orders = Vec::new();
    for (workspace, order) in workspaces {
        match order {
            Some(order) => orders.push(order),
            None => v.push(format!("workspace {workspace} has no WorkspaceOrder")),
        }
    }
    let count = orders.len();
    orders.sort_unstable();
    orders.dedup();
    if orders.len() != count {
        v.push("two workspaces share a WorkspaceOrder".into());
    }
}

fn leaves(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    // Bounded, so a cycle reported by `hierarchy` cannot hang this pass.
    let mut budget = 1_000_000u32;
    while let Some(entity) = stack.pop() {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            break;
        }
        if world.get::<PaneView>(entity).is_some() {
            out.push(entity);
        }
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter().rev());
        }
    }
    out
}

fn viewers(world: &mut World, v: &mut Vec<String>) {
    let all: Vec<Entity> = world
        .query_filtered::<Entity, With<Viewer>>()
        .iter(world)
        .collect();
    for viewer in all {
        if world.get::<IsResource>(viewer).is_some() {
            v.push(format!("resource entity {viewer} is a viewer"));
            continue;
        }
        if is_layout(world, viewer) {
            // Reported by `hierarchy`.
            continue;
        }
        if let Some(size) = world.get::<Viewer>(viewer)
            && (size.rows > MAX_DIMENSION || size.cols > MAX_DIMENSION)
        {
            v.push(format!(
                "viewer {viewer} is {}x{}, over {MAX_DIMENSION}",
                size.rows, size.cols
            ));
        }
        let Some(workspace) = world.get::<Viewing>(viewer).map(|w| w.0) else {
            v.push(format!("viewer {viewer} has no workspace"));
            continue;
        };
        if world.get::<Workspace>(workspace).is_none() {
            v.push(format!(
                "viewer {viewer} views {workspace}, which is not a workspace"
            ));
            continue;
        }
        let Some(tab) = world.get::<OnTab>(viewer).map(|t| t.0) else {
            v.push(format!("viewer {viewer} has no tab"));
            continue;
        };
        if world.get::<Tab>(tab).is_none() {
            v.push(format!("viewer {viewer} is on {tab}, which is not a tab"));
            continue;
        }
        if parent(world, tab) != Some(workspace) {
            v.push(format!(
                "viewer {viewer} is on tab {tab} outside its workspace {workspace}"
            ));
            continue;
        }
        let tab_leaves = leaves(world, tab);
        match world.get::<Focused>(viewer).map(|f| f.0) {
            Some(f) if tab_leaves.contains(&f) => {}
            Some(f) => v.push(format!(
                "viewer {viewer} focuses {f}, which is not a pane of its tab {tab}"
            )),
            None if tab_leaves.is_empty() => {}
            None => v.push(format!(
                "viewer {viewer} has no focus although tab {tab} has panes"
            )),
        }
    }
}
