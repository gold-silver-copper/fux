//! Native hierarchy normalization and viewer-local navigation memory.
#[cfg(test)]
mod tests;
use crate::{control::Scope, model::*};
use bevy_ecs::{lifecycle::HookContext, prelude::*, world::DeferredWorld};
use bevy_ui::Node;

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

pub enum Pick {
    Entity(Entity),
    Next,
    Previous,
}

/// Opens a new tab with a fresh pane in the viewer's workspace and selects it.
pub fn tab_new(world: &mut World, id: Entity, name: Option<String>) -> Result<(), String> {
    let root = viewing(world, id).ok_or(DETACHED)?;
    let title = name.unwrap_or_else(|| format!("tab-{}", tabs(world, root).len() + 1));
    let tab = world.spawn((Tab, Name::new(title), ChildOf(root))).id();
    let settings = world.resource::<crate::assets::Settings>().clone();
    let leaf = crate::server::spawn_pane(world, &settings, tab, None, None)?;
    world
        .get_entity_mut(id)
        .map_err(|_| DETACHED)?
        .insert((OnTab(tab), Focused(leaf)));
    world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
    Ok(())
}

/// Returns focus to the pane this viewer focused before the current one.
pub fn focus_last(world: &mut World, id: Entity) -> Result<(), String> {
    let tab = on_tab(world, id).ok_or("no active tab")?;
    let previous = world
        .get::<Memory>(id)
        .and_then(|memory| memory.previous.get(&tab).copied());
    if let Some(previous) = previous
        .filter(|e| leaves(world, tab).contains(e) && crate::frame::visible_leaf(world, id, *e))
    {
        world
            .get_entity_mut(id)
            .map_err(|_| DETACHED)?
            .insert(Focused(previous));
    }
    Ok(())
}

/// Selects a tab or workspace for this viewer, by identity or by cycling. The
/// dependent relationships are dropped; `repair` restores them from memory.
pub fn select(world: &mut World, id: Entity, scope: Scope, pick: Pick) -> Result<(), String> {
    let root = viewing(world, id).ok_or(DETACHED)?;
    let tab = on_tab(world, id).ok_or("no active tab")?;
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
    let mut entity = world.get_entity_mut(id).map_err(|_| DETACHED)?;
    match scope {
        Scope::Workspace => {
            entity
                .remove::<(OnTab, Focused)>()
                .insert(Viewing(selected));
        }
        Scope::Tab => {
            entity.remove::<Focused>().insert(OnTab(selected));
        }
    }
    world.get_mut::<Viewer>(id).ok_or(DETACHED)?.reset_view();
    Ok(())
}

/// Every workspace keeps at least one tab, whichever code path emptied it.
pub(crate) fn normalize_on_tab_removed(
    removed: On<Remove, Tab>,
    parents: Query<&ChildOf>,
    mut commands: Commands,
) {
    if let Ok(parent) = parents.get(removed.entity) {
        let workspace = parent.parent();
        commands.queue(move |world: &mut World| {
            if world.get::<Workspace>(workspace).is_some() {
                normalize_workspace(world, workspace);
            }
        });
    }
}

/// A child placed directly under a workspace by any caller is wrapped into a tab.
pub(crate) fn normalize_on_child_added(
    added: On<Insert, ChildOf>,
    parents: Query<&ChildOf>,
    workspaces: Query<(), With<Workspace>>,
    tabs: Query<(), With<Tab>>,
    mut commands: Commands,
) {
    let entity = added.entity;
    if let Ok(parent) = parents.get(entity)
        && workspaces.contains(parent.parent())
        && !tabs.contains(entity)
    {
        let workspace = parent.parent();
        commands.queue(move |world: &mut World| {
            if world.get::<Workspace>(workspace).is_some() {
                normalize_workspace(world, workspace);
            }
        });
    }
}

/// Insertion or removal of a viewer relationship, by any code path, repairs
/// every viewer once the change has completed.
pub(crate) fn repair_later(mut world: DeferredWorld, _: HookContext) {
    world.commands().queue(repair);
}

/// A viewer that changes tab remembers it for the workspace it is looking at.
pub(crate) fn remember_tab(mut world: DeferredWorld, context: HookContext) {
    if let (Some(viewing), Some(tab)) = (
        viewing(&world, context.entity),
        on_tab(&world, context.entity),
    ) && let Some(mut memory) = world.get_mut::<Memory>(context.entity)
    {
        memory.tabs.insert(viewing, tab);
    }
    repair_later(world, context);
}

/// A viewer that changes focus remembers it for its tab, and keeps the pane it
/// left as the tab's previous focus for `focus_last`. Restoring a remembered
/// focus after a tab switch is not a change and records nothing.
pub(crate) fn remember_focus(mut world: DeferredWorld, context: HookContext) {
    if let (Some(tab), Some(focus)) = (
        on_tab(&world, context.entity),
        focused(&world, context.entity),
    ) && let Some(mut memory) = world.get_mut::<Memory>(context.entity)
        && let Some(old) = memory.focus.insert(tab, focus)
        && old != focus
        && std::iter::successors(world.get::<ChildOf>(old), |parent| {
            world.get::<ChildOf>(parent.parent())
        })
        .any(|parent| parent.parent() == tab)
        && let Some(mut memory) = world.get_mut::<Memory>(context.entity)
    {
        memory.previous.insert(tab, old);
    }
    repair_later(world, context);
}

/// Memory entries die with the entities they name.
pub(crate) fn forget<C: Component>(removed: On<Remove, C>, mut viewers: Query<&mut Memory>) {
    let gone = removed.entity;
    for mut memory in &mut viewers {
        memory
            .tabs
            .retain(|key, value| *key != gone && *value != gone);
        memory
            .focus
            .retain(|key, value| *key != gone && *value != gone);
        memory
            .previous
            .retain(|key, value| *key != gone && *value != gone);
    }
}

/// Registers the observers that keep viewer relationships and memory valid.
pub(crate) fn observe(world: &mut World) {
    world.add_observer(normalize_on_tab_removed);
    world.add_observer(normalize_on_child_added);
    world.add_observer(forget::<Workspace>);
    world.add_observer(forget::<Tab>);
    world.add_observer(forget::<PaneView>);
}

/// Gives every viewer a workspace, a tab in it and a pane in that, filling
/// what is missing from memory or the first available. Consistent viewers are
/// left untouched, so the insertions here cannot trigger another repair.
pub fn repair(world: &mut World) {
    let roots = workspaces(world);
    let viewers: Vec<_> = world
        .query_filtered::<Entity, With<Viewer>>()
        .iter(world)
        .collect();
    for id in viewers {
        let workspace = match viewing(world, id).filter(|w| roots.contains(w)) {
            Some(workspace) => workspace,
            None => {
                let Some(&root) = roots.first() else {
                    world.despawn(id);
                    continue;
                };
                world.entity_mut(id).insert(Viewing(root));
                root
            }
        };
        let mut available = tabs(world, workspace);
        if available.is_empty() {
            // The tab removal that emptied this workspace has queued its
            // replacement; do not wait for it when a viewer needs a tab now.
            normalize_workspace(world, workspace);
            available = tabs(world, workspace);
        }
        let remembered =
            |world: &World,
             key: Entity,
             which: fn(&Memory) -> &bevy_ecs::entity::EntityHashMap<Entity>| {
                world
                    .get::<Memory>(id)
                    .and_then(|m| which(m).get(&key).copied())
            };
        let tab = match on_tab(world, id).filter(|t| available.contains(t)) {
            Some(tab) => tab,
            None => {
                // normalize_workspace guarantees every workspace has a tab.
                let Some(tab) = remembered(world, workspace, |m| &m.tabs)
                    .filter(|t| available.contains(t))
                    .or_else(|| available.first().copied())
                else {
                    continue;
                };
                world.entity_mut(id).insert(OnTab(tab));
                tab
            }
        };
        let visible = leaves(world, tab);
        if focused(world, id).is_none_or(|f| !visible.contains(&f)) {
            let focus = remembered(world, tab, |m| &m.focus)
                .filter(|e| visible.contains(e))
                .or_else(|| visible.first().copied());
            let mut entity = world.entity_mut(id);
            match focus {
                Some(focus) => {
                    entity.insert(Focused(focus));
                }
                None => {
                    entity.remove::<Focused>();
                }
            }
            if let Some(mut v) = world.get_mut::<Viewer>(id) {
                v.reset_view();
            }
        }
    }
}
