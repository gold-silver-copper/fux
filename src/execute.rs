//! One match over `Command`: each arm changes the world or hands off to the
//! module that owns that part of the model.
#[cfg(test)]
mod tests;
use crate::{
    assets::Settings,
    control::{Axis, Command, Order, Scope, Subject},
    frame,
    interaction::Prefix,
    layout::scene_io,
    model::*,
    presentation::Presentation,
    server::{self, first_leaf, spawn_pane},
    terminal::Terminal,
};
use bevy_ecs::prelude::*;
use bevy_ui::{FlexDirection, Node};

/// Runs one command for a viewer. Every arm either changes the world here or
/// hands off to the module that owns that part of the model.
pub(crate) fn execute(world: &mut World, id: Entity, command: Command) -> Result<(), String> {
    use crate::interaction::{self, check};
    use crate::navigation::{self as nav, Pick};
    use Command::*;
    if !matches!(command, CopyMode | Scroll { .. })
        && let Ok(mut entity) = world.get_entity_mut(id)
    {
        entity.remove::<crate::selection::Selection>();
    }
    interaction::close_prefix(world, id);
    notify(world, id, None);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    let (rows, scrollback) = (v.rows, v.scrollback);
    let target = crate::actions::Target::of(world, id).ok_or(DETACHED)?;
    let (workspace, tab) = (target.workspace, target.tab);
    let focus = target
        .leaf
        .filter(|leaf| world.get::<PaneView>(*leaf).is_some());
    let focus_on = |world: &mut World, leaf: Entity| -> Result<(), String> {
        world
            .get_entity_mut(id)
            .map_err(|_| DETACHED)?
            .insert(Focused(leaf));
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
        Ok(())
    };
    let pane_of = |world: &World, leaf: Entity| world.get::<PaneView>(leaf).map(|p| p.pane);
    if matches!(
        command,
        Select {
            scope: Scope::Tab,
            ..
        } | Next { scope: Scope::Tab }
            | Previous { scope: Scope::Tab }
            | Reorder {
                scope: Scope::Tab,
                ..
            }
    ) {
        tab.ok_or("no tab")?;
        if !matches!(command, Select { .. }) {
            target.multiple_tabs(world)?;
        }
    }
    match command {
        Split { axis, program } => {
            let settings = world.resource::<Settings>().clone();
            let container_of = tab.unwrap_or(workspace);
            let leaf = focus.or_else(|| first_leaf(world, container_of));
            let cwd = leaf
                .and_then(|leaf| pane_of(world, leaf))
                .and_then(|pane| world.get::<Launch>(pane))
                .map(|launch| launch.cwd.clone());
            let argv = program.map(|program| vec!["/bin/sh".into(), "-lc".into(), program]);
            // A split must leave both panes at least the 2x2 backing minimum
            // with a one-cell separator between them; otherwise one pane would
            // exist, take focus and input, and paint nothing at all.
            if let Some(leaf) = leaf
                && let Some(rect) = frame::rect(world, id, leaf)
            {
                let room = match axis {
                    Axis::Vertical => rect.height(),
                    Axis::Horizontal => rect.width(),
                };
                if room < 5 {
                    return Err("pane too small to split".into());
                }
            }
            let new = match leaf {
                Some(leaf) => {
                    let parent = world
                        .get::<ChildOf>(leaf)
                        .ok_or("pane has no parent")?
                        .parent();
                    let index = world
                        .get::<Children>(parent)
                        .and_then(|siblings| siblings.iter().position(|e| e == leaf))
                        .ok_or("missing child")?;
                    let mut container = world.spawn(Split);
                    if axis == Axis::Vertical {
                        container.insert(split_node(FlexDirection::Column));
                    }
                    let container = container.id();
                    let new = spawn_pane(world, &settings, container, argv, cwd)?;
                    world.entity_mut(container).insert_children(0, &[leaf]);
                    world
                        .entity_mut(parent)
                        .insert_children(index, &[container]);
                    new
                }
                None => spawn_pane(world, &settings, container_of, argv, cwd)?,
            };
            focus_on(world, new)?;
            world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
        }
        Close { subject } => {
            let entity = check(world, subject)?;
            // Removal observers repair viewer navigation as the hierarchy goes.
            interaction::close(world, entity);
        }
        Terminate => {
            let pane = focus
                .and_then(|leaf| pane_of(world, leaf))
                .ok_or("no pane")?;
            world
                .get_mut::<Terminal>(pane)
                .ok_or("process is not running")?
                .stop()?;
        }
        Zoom => {
            focus.ok_or("no pane")?;
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.zoom = !v.zoom;
        }
        Rename { subject, name } => {
            check(world, subject)?;
            interaction::rename(world, subject, name)?;
        }
        Resize { axis, grow } => {
            let mut child = focus.ok_or("no pane")?;
            let width = axis == Axis::Horizontal;
            while let Some(parent) = world.get::<ChildOf>(child).map(|c| c.parent()) {
                let direction = world
                    .get::<Node>(parent)
                    .ok_or("container has no node")?
                    .flex_direction;
                if width == matches!(direction, FlexDirection::Row | FlexDirection::RowReverse) {
                    let mut node = world.get_mut::<Node>(child).ok_or("pane has no node")?;
                    node.flex_grow = (node.flex_grow + if grow { 0.25 } else { -0.25 }).max(0.1);
                    break;
                }
                child = parent;
            }
        }
        ReorderPane { order } => {
            let leaf = focus.ok_or("no pane")?;
            target.multiple_panes(world)?;
            let parent = world
                .get::<ChildOf>(leaf)
                .ok_or("pane has no parent")?
                .parent();
            let mut siblings = world
                .get_mut::<Children>(parent)
                .ok_or("pane has no siblings")?;
            let index = siblings
                .iter()
                .position(|e| e == leaf)
                .ok_or("missing child")?;
            let other = match order {
                Order::Previous => index.saturating_sub(1),
                Order::Next => (index + 1).min(siblings.len() - 1),
            };
            siblings.swap(index, other);
        }
        Swap { with } => {
            let leaf = focus.ok_or("no pane")?;
            interaction::swap(world, leaf, with)?;
        }
        SwapDirection { direction } => {
            target.multiple_panes(world)?;
            interaction::beside(world, id, target, direction, true)?;
        }
        MoveDirection { direction } => {
            target.multiple_panes(world)?;
            interaction::beside(world, id, target, direction, false)?;
        }
        Move { to } => interaction::move_pane(world, id, target, to)?,
        CopyMode => crate::selection::start(world, id, focus.ok_or("no pane")?)?,
        Scroll { order } => {
            let pane = focus
                .and_then(|leaf| pane_of(world, leaf))
                .ok_or("no pane")?;
            let step = usize::from(rows / 2).max(1);
            let requested = match order {
                Order::Previous => scrollback.saturating_add(step),
                Order::Next => scrollback.saturating_sub(step),
            };
            // Clamp to retained history so scrolling back toward live output
            // moves immediately instead of first unwinding an invisible excess.
            let offset = match world.get_mut::<Terminal>(pane) {
                Some(terminal) => terminal.clamp_scrollback(requested),
                None => 0,
            };
            world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = offset;
        }
        Copy => {
            let settings = world.resource::<Settings>();
            crate::selection::validate_clipboard(settings, "")?;
            let pane = focus
                .and_then(|leaf| pane_of(world, leaf))
                .ok_or("no pane")?;
            if world
                .get::<Presentation>(id)
                .ok_or("presentation not initialized")?
                .clipboard
                .len()
                == 16
            {
                return Err("clipboard delivery queue is full".into());
            }
            let text = world
                .get_mut::<Terminal>(pane)
                .ok_or("terminal not found")?
                .copy_text(scrollback)?;
            crate::selection::validate_clipboard(world.resource::<Settings>(), &text)?;
            if let Some(mut view) = world.get_mut::<Presentation>(id) {
                view.clipboard.push(text);
            }
            notify(world, id, Notice::info("visible pane copied via OSC52"));
        }
        Focus { pane } => {
            check(world, Subject::Pane(pane))?;
            let shown = frame::rect(world, id, pane).is_some();
            let in_tab = tab.is_some_and(|tab| nav::leaves(world, tab).contains(&pane));
            if !shown || !in_tab {
                return Err("target is not a pane in this workspace".into());
            }
            focus_on(world, pane)?;
        }
        FocusNext | FocusPrevious => {
            focus.ok_or("no pane")?;
            target.multiple_panes(world)?;
            let next = world
                .get_mut::<Presentation>(id)
                .ok_or("presentation not initialized")?
                .focus_step(command == FocusPrevious);
            if let Some(next) = next {
                focus_on(world, next)?;
            }
        }
        FocusLast => {
            focus.ok_or("no pane")?;
            target.multiple_panes(world)?;
            nav::focus_last(world, id)?;
        }
        FocusDirection { direction } => {
            let leaf = focus.ok_or("no pane")?;
            if let Some(next) = crate::frame::neighbor(world, id, leaf, direction) {
                focus_on(world, next)?;
            }
        }
        TabNew { name } => {
            tab.ok_or("no tab")?;
            nav::tab_new(world, id, name)?;
        }
        Select { scope, entity } => nav::select(world, id, scope, Pick::Entity(entity))?,
        Next { scope } | Previous { scope } => {
            let pick = if matches!(command, Next { .. }) {
                Pick::Next
            } else {
                Pick::Previous
            };
            nav::select(world, id, scope, pick)?;
        }
        Reorder { scope, order } => {
            let subject = match scope {
                Scope::Tab => Subject::Tab(tab.ok_or("no tab")?),
                Scope::Workspace => Subject::Workspace(workspace),
            };
            interaction::reorder(world, subject, order)?;
        }
        WorkspaceNew { name } => {
            let settings = world.resource::<Settings>().clone();
            let roots = nav::workspaces(world);
            let title = name.unwrap_or_else(|| format!("workspace-{}", roots.len() + 1));
            let order = roots
                .iter()
                .filter_map(|e| world.get::<WorkspaceOrder>(*e).map(|o| o.0))
                .max()
                .unwrap_or(-1)
                .saturating_add(1);
            let root = server::workspace(world, &title);
            world.entity_mut(root).insert(WorkspaceOrder(order));
            let tab = world.spawn((Tab, Name::new("main"), ChildOf(root))).id();
            let leaf = spawn_pane(world, &settings, tab, None, None)?;
            world.get_entity_mut(id).map_err(|_| DETACHED)?.insert((
                Viewing(root),
                OnTab(tab),
                Focused(leaf),
            ));
            world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
        }
        SaveLayout { workspace, path } => {
            check(world, Subject::Workspace(workspace))?;
            notify(world, id, Notice::info("saving layout..."));
            scene_io(world, id, workspace, path, None);
        }
        LoadLayout {
            workspace,
            path,
            mapping,
        } => {
            check(world, Subject::Workspace(workspace))?;
            notify(world, id, Notice::info("loading layout..."));
            scene_io(world, id, workspace, path, Some(mapping));
        }
        // Help is the prefix command column itself; there is no second surface.
        Help => {
            world.entity_mut(id).insert(Prefix::default());
        }
        Detach => {
            world.despawn(id);
        }
        Menu { subject } => interaction::menu(world, id, target, subject)?,
        Choose { chooser } => interaction::choose(world, id, target, chooser)?,
    }
    Ok(())
}
