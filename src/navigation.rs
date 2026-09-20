//! Native hierarchy normalization and viewer-local navigation memory.
#[cfg(test)]
mod tests;
use crate::model::*;
use bevy_ecs::prelude::*;
use bevy_ui::{Node, Val};

pub fn tab_node() -> Node {
    Node {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        flex_grow: 1.0,
        min_width: Val::ZERO,
        min_height: Val::ZERO,
        ..Default::default()
    }
}

pub fn workspaces(world: &mut World) -> Vec<Entity> {
    let mut roots: Vec<_> = world
        .query_filtered::<Entity, With<Workspace>>()
        .iter(world)
        .collect();
    roots.sort_by_key(|e| {
        (
            world.get::<WorkspaceOrder>(*e).map_or(0, |o| o.0),
            e.to_bits(),
        )
    });
    roots
}

pub fn tabs(world: &World, workspace: Entity) -> Vec<Entity> {
    world
        .get::<Children>(workspace)
        .map(|children| {
            children
                .iter()
                .filter(|e| world.get::<Tab>(*e).is_some())
                .collect()
        })
        .unwrap_or_default()
}

pub fn leaves(world: &World, root: Entity) -> Vec<Entity> {
    let mut result = Vec::new();
    fn visit(world: &World, entity: Entity, result: &mut Vec<Entity>) {
        if world.get::<PaneView>(entity).is_some() {
            result.push(entity);
        }
        if let Some(children) = world.get::<Children>(entity) {
            for child in children.iter() {
                visit(world, child, result);
            }
        }
    }
    visit(world, root, &mut result);
    result
}

/// Wrap legacy root children without rewriting their native layout properties.
/// Also handles unrestricted ECS additions of non-tab children to a workspace.
pub fn normalize_workspace(world: &mut World, root: Entity) {
    let loose: Vec<_> = world
        .get::<Children>(root)
        .map(|children| {
            children
                .iter()
                .filter(|e| world.get::<Tab>(*e).is_none())
                .collect()
        })
        .unwrap_or_default();
    let legacy = tabs(world, root).is_empty();
    if !loose.is_empty() || legacy {
        // Move, rather than duplicate, the old root's complete layout box into
        // the first tab. A neutral workspace wrapper preserves grid tracks,
        // margins and padding exactly once.
        let node = if legacy {
            world.get::<Node>(root).cloned().unwrap_or_else(tab_node)
        } else {
            tab_node()
        };
        let tab = world
            .spawn((Tab, Name::new("main"), node, ChildOf(root)))
            .id();
        if legacy {
            world.entity_mut(root).insert(tab_node());
            if world.get::<Split>(root).is_some() {
                world.entity_mut(tab).insert(Split);
            }
        }
        world.entity_mut(tab).add_children(&loose);
    }
}

/// Exactly the actions `control` implements.
pub enum Scope {
    Tab,
    Workspace,
}
pub enum Pick {
    Entity(Entity),
    Next,
    Previous,
}

/// Opens a new tab with a fresh pane in the viewer's workspace and selects it.
pub fn tab_new(world: &mut World, id: Entity, name: Option<String>) -> Result<(), String> {
    repair(world);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let root = v.workspace;
    let title = name.unwrap_or_else(|| format!("tab-{}", tabs(world, root).len() + 1));
    let tab = world
        .spawn((Tab, Name::new(title), tab_node(), ChildOf(root)))
        .id();
    let settings = world.resource::<crate::assets::Settings>().clone();
    let leaf = crate::server::spawn_pane(&mut world.commands(), &settings, tab, None, None)?;
    world.flush();
    let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
    v.tab = Some(tab);
    v.focus = Some(leaf);
    v.zoom = false;
    repair(world);
    Ok(())
}

/// Returns focus to the pane this viewer focused before the current one.
pub fn focus_last(world: &mut World, id: Entity) -> Result<(), String> {
    repair(world);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let tab = v.tab.ok_or("no active tab")?;
    let previous = world
        .get::<Navigation>(id)
        .and_then(|memory| memory.previous.get(&tab).copied());
    if let Some(previous) = previous
        .filter(|e| leaves(world, tab).contains(e) && crate::frame::visible_leaf(world, id, *e))
    {
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.focus = Some(previous);
    }
    repair(world);
    Ok(())
}

/// Selects a tab or workspace for this viewer, by identity or by cycling.
pub fn select(world: &mut World, id: Entity, scope: Scope, pick: Pick) -> Result<(), String> {
    repair(world);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let root = v.workspace;
    let tab = v.tab.ok_or("no active tab")?;
    let (all, current) = match scope {
        Scope::Workspace => (workspaces(world), root),
        Scope::Tab => (tabs(world, root), tab),
    };
    let index = all
        .iter()
        .position(|e| *e == current)
        .ok_or("target disappeared")?;
    let selected = match pick {
        Pick::Entity(entity) => all
            .iter()
            .copied()
            .find(|e| *e == entity)
            .ok_or("selection target no longer exists")?,
        // `index` was found in `all`, so it is nonempty.
        Pick::Previous => all
            .get((index + all.len() - 1) % all.len())
            .copied()
            .ok_or("target disappeared")?,
        Pick::Next => all
            .get((index + 1) % all.len())
            .copied()
            .ok_or("target disappeared")?,
    };
    let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
    match scope {
        Scope::Workspace => v.workspace = selected,
        Scope::Tab => v.tab = Some(selected),
    }
    v.zoom = false;
    v.scrollback = 0;
    repair(world);
    Ok(())
}

/// Hierarchy removals repair every viewer as they happen, from any code path.
/// Removal observers see the entity still present, so the repair is queued and
/// runs once the removal has completed.
pub(crate) fn repair_on_remove<C: Component>(_: On<Remove, C>, mut commands: Commands) {
    commands.queue(repair);
}

pub fn repair(world: &mut World) {
    let roots = workspaces(world);
    for &root in &roots {
        normalize_workspace(world, root);
    }
    let viewers: Vec<_> = world
        .query_filtered::<Entity, With<Viewer>>()
        .iter(world)
        .collect();
    for id in viewers {
        let Some(v) = world.get::<Viewer>(id) else {
            continue;
        };
        let workspace = if roots.contains(&v.workspace) {
            v.workspace
        } else if let Some(&root) = roots.first() {
            root
        } else {
            world.despawn(id);
            continue;
        };
        let available = tabs(world, workspace);
        let Some(memory) = world.get::<Navigation>(id) else {
            continue;
        };
        let changed_workspace = memory.observed.is_some_and(|old| old.0 != workspace);
        let requested = if changed_workspace {
            memory.tabs.get(&workspace).copied()
        } else {
            v.tab
        };
        // normalize_workspace above guarantees every workspace has a tab.
        let Some(tab) = requested
            .filter(|t| available.contains(t))
            .or_else(|| available.first().copied())
        else {
            continue;
        };
        let visible = leaves(world, tab);
        let changed_tab = memory.observed.is_some_and(|old| old.1 != tab);
        let requested = if changed_tab {
            memory.focus.get(&tab).copied()
        } else {
            v.focus
        };
        let focus = requested
            .filter(|e| visible.contains(e))
            .or_else(|| visible.first().copied());
        let observed = memory.observed;
        let stale_tabs: Vec<_> = memory
            .tabs
            .iter()
            .filter(|(workspace, tab)| !tabs(world, **workspace).contains(tab))
            .map(|(workspace, _)| *workspace)
            .collect();
        let stale_focus: Vec<_> = memory
            .focus
            .iter()
            .filter(|(tab, leaf)| {
                world.get::<Tab>(**tab).is_none() || !leaves(world, **tab).contains(leaf)
            })
            .map(|(tab, _)| *tab)
            .collect();
        let stale_previous: Vec<_> = memory
            .previous
            .iter()
            .filter(|(tab, leaf)| {
                world.get::<Tab>(**tab).is_none() || !leaves(world, **tab).contains(leaf)
            })
            .map(|(tab, _)| *tab)
            .collect();
        let Some(mut memory) = world.get_mut::<Navigation>(id) else {
            continue;
        };
        for workspace in stale_tabs {
            memory.tabs.remove(&workspace);
        }
        for tab in stale_focus {
            memory.focus.remove(&tab);
        }
        for tab in stale_previous {
            memory.previous.remove(&tab);
        }
        if let Some((_, old_tab, Some(old_focus))) = observed
            && old_tab == tab
            && Some(old_focus) != focus
            && visible.contains(&old_focus)
        {
            memory.previous.insert(tab, old_focus);
        }
        memory.tabs.insert(workspace, tab);
        if let Some(focus) = focus {
            memory.focus.insert(tab, focus);
        }
        memory.observed = Some((workspace, tab, focus));
        let Some(mut v) = world.get_mut::<Viewer>(id) else {
            continue;
        };
        if v.workspace != workspace || v.tab != Some(tab) || v.focus != focus {
            v.workspace = workspace;
            v.tab = Some(tab);
            v.focus = focus;
            v.scrollback = 0;
            v.zoom = false;
        }
    }
}
