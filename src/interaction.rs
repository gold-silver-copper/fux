//! Viewer-local corner overlays. Captured entities are validated again on execution.
#[cfg(test)]
mod tests;
use crate::{
    actions::{self, Target},
    control::Control,
    model::*,
    navigation,
    protocol::Input,
};
use bevy_ecs::prelude::*;

#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub action: String,
    pub destination: Option<Entity>,
}
#[derive(Clone)]
pub enum Mode {
    List {
        title: String,
        entries: Vec<Entry>,
        selected: usize,
    },
    Confirm {
        action: String,
    },
    Text {
        action: String,
        buffer: String,
    },
}
#[derive(Component, Clone)]
pub struct Overlay {
    pub serial: u64,
    pub target: Target,
    pub mode: Mode,
}

fn label(world: &World, entity: Entity) -> String {
    let entity = world.get::<PaneView>(entity).map_or(entity, |p| p.pane);
    world
        .get::<Name>(entity)
        .map_or_else(|| entity.to_bits().to_string(), |n| n.to_string())
}
use crate::navigation::workspaces as roots;
fn open(world: &mut World, id: Entity, target: Target, mode: Mode) {
    let mut ownership = world.get_mut::<crate::paste::Ownership>(id).unwrap();
    ownership.serial = ownership.serial.wrapping_add(1);
    let serial = ownership.serial;
    world.entity_mut(id).insert(Overlay {
        serial,
        target,
        mode,
    });
    let mut v = world.get_mut::<Viewer>(id).unwrap();
    v.prefix = false;
    v.prompt = None;
    v.buffer.clear();
}
fn error(world: &mut World, id: Entity, message: impl Into<String>) {
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.notice = message.into();
        v.notice_error = true;
    }
}

pub fn invoke(
    world: &mut World,
    id: Entity,
    target: Target,
    action: &str,
    destination: Option<Entity>,
    value: &str,
    interactive: bool,
) -> Result<bool, String> {
    if let Some(reason) = actions::unavailable(world, target, action) {
        return Err(reason.into());
    }
    if !matches!(action, "copy_mode" | "scroll_up" | "scroll_down") {
        world.entity_mut(id).remove::<crate::selection::Selection>();
    }
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.prefix = false;
        v.notice.clear();
        v.notice_error = false;
    }
    if interactive && matches!(action, "close" | "tab_close" | "workspace_close") {
        open(
            world,
            id,
            target,
            Mode::Confirm {
                action: action.into(),
            },
        );
        return Ok(true);
    }
    if value.is_empty()
        && matches!(
            action,
            "rename_pane" | "rename_tab" | "rename_workspace" | "save_layout" | "load_layout"
        )
    {
        open(
            world,
            id,
            target,
            Mode::Text {
                action: action.into(),
                buffer: String::new(),
            },
        );
        return Ok(true);
    }
    match action {
        "tab_new" => {
            world.get_mut::<Viewer>(id).unwrap().workspace = target.workspace;
            navigation::control(world, id, action, None, value)?;
        }
        "copy_mode" => crate::selection::start(world, id, target.leaf.ok_or("no pane")?)?,
        "tab_choose" | "workspace_choose" | "swap_choose" | "move_tab" | "move_workspace"
            if destination.is_none() && value.is_empty() =>
        {
            let candidates = match action {
                "workspace_choose" | "move_workspace" => roots(world),
                "swap_choose" => navigation::leaves(world, target.tab.ok_or("no tab")?)
                    .into_iter()
                    .filter(|e| Some(*e) != target.leaf)
                    .collect(),
                _ => navigation::tabs(world, target.workspace),
            };
            let execute = match action {
                "tab_choose" => "tab_select",
                "workspace_choose" => "workspace_select",
                "swap_choose" => "swap",
                _ => action,
            };
            let entries = candidates
                .into_iter()
                .map(|entity| Entry {
                    label: label(world, entity),
                    action: execute.into(),
                    destination: Some(entity),
                })
                .collect();
            open(
                world,
                id,
                target,
                Mode::List {
                    title: actions::metadata(action).unwrap().label.into(),
                    entries,
                    selected: 0,
                },
            );
        }
        "pane_menu" | "tab_menu" | "workspace_menu" => {
            let group = match action {
                "pane_menu" => "Panes",
                "tab_menu" => "Tabs",
                _ => "Workspaces",
            };
            let entries = actions::ALL
                .iter()
                .filter(|a| {
                    a.group == group
                        && !a.id.ends_with("_menu")
                        && !matches!(
                            a.id,
                            "tab_next"
                                | "tab_previous"
                                | "tab_choose"
                                | "workspace_next"
                                | "workspace_previous"
                        )
                })
                .map(|a| Entry {
                    label: a.label.into(),
                    action: a.id.into(),
                    destination: None,
                })
                .collect();
            open(
                world,
                id,
                target,
                Mode::List {
                    title: format!(
                        "{group}: {}",
                        label(
                            world,
                            match action {
                                "pane_menu" => target.leaf.ok_or("no pane")?,
                                "tab_menu" => target.tab.ok_or("no tab")?,
                                _ => target.workspace,
                            }
                        )
                    ),
                    entries,
                    selected: 0,
                },
            );
        }
        "tab_reorder_previous"
        | "tab_reorder_next"
        | "workspace_reorder_previous"
        | "workspace_reorder_next" => {
            let workspace_action = action.starts_with("workspace_");
            let mut entities = if workspace_action {
                roots(world)
            } else {
                navigation::tabs(world, target.workspace)
            };
            let entity = if workspace_action {
                target.workspace
            } else {
                target.tab.ok_or("no tab")?
            };
            let index = entities
                .iter()
                .position(|e| *e == entity)
                .ok_or("target removed")?;
            let other = if action.ends_with("previous") {
                index.saturating_sub(1)
            } else {
                (index + 1).min(entities.len() - 1)
            };
            entities.swap(index, other);
            if workspace_action {
                for (index, entity) in entities.into_iter().enumerate() {
                    world
                        .entity_mut(entity)
                        .insert(WorkspaceOrder(index as i64));
                }
            } else {
                world
                    .entity_mut(target.workspace)
                    .replace_children(&entities);
            }
        }
        "close" | "tab_close" | "workspace_close" => {
            let entity = match action {
                "close" => target.leaf.ok_or("no pane")?,
                "tab_close" => target.tab.ok_or("no tab")?,
                _ => target.workspace,
            };
            close(world, entity);
            navigation::repair(world);
        }
        "rename_pane" | "rename_tab" | "rename_workspace" if !value.is_empty() => {
            let entity = match action {
                "rename_pane" => {
                    world
                        .get::<PaneView>(target.leaf.ok_or("no pane")?)
                        .ok_or("pane removed")?
                        .pane
                }
                "rename_tab" => target.tab.ok_or("no tab")?,
                _ => target.workspace,
            };
            world.entity_mut(entity).insert(Name::new(value.to_owned()));
        }
        "swap_left" | "swap_right" | "swap_up" | "swap_down" | "move_left" | "move_right"
        | "move_up" | "move_down" => {
            let source = target.leaf.ok_or("no pane")?;
            let direction = action.rsplit('_').next().unwrap();
            let destination = crate::server::neighbor(world, id, source, direction)
                .ok_or("no pane in that direction")?;
            if action.starts_with("swap_") {
                swap(world, source, destination)?;
            } else {
                move_beside(world, source, destination, direction)?;
            }
            world.get_mut::<Viewer>(id).unwrap().zoom = false;
        }
        "swap" => swap(
            world,
            target.leaf.ok_or("no pane")?,
            destination.ok_or("swap destination required")?,
        )?,
        "move_tab" | "move_workspace" | "move_new_tab" | "move_new_workspace" => {
            let leaf = target.leaf.ok_or("no pane")?;
            let (workspace, tab) = match action {
                "move_new_workspace" => {
                    let order = roots(world)
                        .into_iter()
                        .filter_map(|e| world.get::<WorkspaceOrder>(e).map(|o| o.0))
                        .max()
                        .unwrap_or(-1)
                        .saturating_add(1);
                    let root = world
                        .spawn((
                            Workspace,
                            WorkspaceOrder(order),
                            Name::new(
                                if value.is_empty() { "workspace" } else { value }.to_owned(),
                            ),
                            navigation::tab_node(),
                        ))
                        .id();
                    let tab = world
                        .spawn((
                            Tab,
                            Name::new("main"),
                            navigation::tab_node(),
                            ChildOf(root),
                        ))
                        .id();
                    (root, tab)
                }
                "move_new_tab" => {
                    let tab = world
                        .spawn((
                            Tab,
                            Name::new(if value.is_empty() { "tab" } else { value }.to_owned()),
                            navigation::tab_node(),
                            ChildOf(target.workspace),
                        ))
                        .id();
                    (target.workspace, tab)
                }
                "move_workspace" => {
                    let root = destination
                        .or_else(|| {
                            roots(world).into_iter().find(|e| {
                                world.get::<Name>(*e).is_some_and(|n| n.as_str() == value)
                            })
                        })
                        .filter(|e| world.get::<Workspace>(*e).is_some())
                        .ok_or("destination workspace removed")?;
                    navigation::normalize_workspace(world, root);
                    (root, navigation::tabs(world, root)[0])
                }
                _ => {
                    let tab = destination
                        .filter(|e| world.get::<Tab>(*e).is_some())
                        .ok_or("destination tab removed")?;
                    let root = world
                        .get::<ChildOf>(tab)
                        .ok_or("tab has no workspace")?
                        .parent();
                    (root, tab)
                }
            };
            if Some(tab) == target.tab {
                return Ok(true);
            }
            if world
                .get::<Children>(tab)
                .is_some_and(|children| !children.is_empty())
            {
                // A tab may itself be a native split container. Preserve custom
                // nonzero spacing, adding shared separators to default layouts.
                world.entity_mut(tab).insert(Split);
                if let Some(mut node) = world.get_mut::<bevy_ui::Node>(tab) {
                    if node.column_gap == bevy_ui::Val::ZERO {
                        node.column_gap = bevy_ui::Val::Px(1.0);
                    }
                    if node.row_gap == bevy_ui::Val::ZERO {
                        node.row_gap = bevy_ui::Val::Px(1.0);
                    }
                }
            }
            world.entity_mut(leaf).insert(ChildOf(tab));
            // Select the moved pane without overwriting other viewers' memory.
            let mut memory = world.get_mut::<Navigation>(id).unwrap();
            memory.tabs.insert(workspace, tab);
            memory.focus.insert(tab, leaf);
            let mut v = world.get_mut::<Viewer>(id).unwrap();
            v.workspace = workspace;
            v.tab = Some(tab);
            v.focus = Some(leaf);
            v.zoom = false;
            v.scrollback = 0;
            navigation::repair(world);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

pub fn close(world: &mut World, entity: Entity) {
    let processes: Vec<_> = navigation::leaves(world, entity)
        .into_iter()
        .filter_map(|leaf| world.get::<PaneView>(leaf).map(|p| p.pane))
        .collect();
    world.despawn(entity);
    use bevy_ecs::relationship::RelationshipTarget;
    for process in processes {
        if world
            .get::<PaneViews>(process)
            .is_none_or(|views| views.len() == 0)
        {
            world.despawn(process);
        }
    }
}

fn move_beside(
    world: &mut World,
    source: Entity,
    destination: Entity,
    direction: &str,
) -> Result<(), String> {
    use bevy_ui::{FlexDirection, Val};
    let parent = world
        .get::<ChildOf>(destination)
        .ok_or("destination removed")?
        .parent();
    let index = world
        .get::<Children>(parent)
        .unwrap()
        .iter()
        .position(|e| e == destination)
        .unwrap();
    let mut node = navigation::tab_node();
    node.width = Val::Auto;
    node.height = Val::Auto;
    node.flex_basis = Val::ZERO;
    node.flex_direction = if matches!(direction, "up" | "down") {
        FlexDirection::Column
    } else {
        FlexDirection::Row
    };
    node.row_gap = Val::Px(1.0);
    node.column_gap = Val::Px(1.0);
    let split = world.spawn((Split, node)).id();
    let children = if matches!(direction, "left" | "up") {
        [source, destination]
    } else {
        [destination, source]
    };
    world.entity_mut(parent).insert_children(index, &[split]);
    world.entity_mut(split).add_children(&children);
    Ok(())
}

fn swap(world: &mut World, source: Entity, destination: Entity) -> Result<(), String> {
    if source == destination {
        return Ok(());
    }
    if world.get::<PaneView>(destination).is_none() {
        return Err("swap destination removed".into());
    }
    let a = world
        .get::<ChildOf>(source)
        .ok_or("source has no parent")?
        .parent();
    let b = world
        .get::<ChildOf>(destination)
        .ok_or("destination has no parent")?
        .parent();
    let ai = world
        .get::<Children>(a)
        .unwrap()
        .iter()
        .position(|e| e == source)
        .unwrap();
    let bi = world
        .get::<Children>(b)
        .unwrap()
        .iter()
        .position(|e| e == destination)
        .unwrap();
    if a == b {
        world.get_mut::<Children>(a).unwrap().swap(ai, bi);
    } else {
        world.entity_mut(a).insert_children(ai, &[destination]);
        world.entity_mut(b).insert_children(bi, &[source]);
    }
    Ok(())
}

/// Consume overlay input before the ordinary terminal/prefix observer.
pub fn input(world: &mut World, id: Entity, input: &Input) -> bool {
    let Some(overlay) = world.get::<Overlay>(id).cloned() else {
        return false;
    };
    if !overlay.target.valid(world) {
        world.entity_mut(id).remove::<Overlay>();
        error(world, id, "target changed; action cancelled");
        return !matches!(input, Input::Resize { .. });
    }
    if matches!(input, Input::Resize { .. }) {
        return false;
    }
    let mut next = overlay.clone();
    let mut execute = None;
    let mut cancel = false;
    match (&mut next.mode, input) {
        (_, Input::Key { key, .. }) if key == "escape" => cancel = true,
        (
            Mode::Confirm { action },
            Input::Key {
                key,
                ctrl: false,
                alt: false,
                ..
            },
        ) => {
            if key == "y" {
                execute = Some((action.clone(), None, String::new(), false));
            } else if key == "n" {
                cancel = true;
            }
        }
        (Mode::Text { action, buffer }, Input::Key { key, ctrl, alt, .. }) => {
            if key == "enter" && !buffer.is_empty() {
                execute = Some((action.clone(), None, buffer.clone(), false));
            } else if key == "backspace" {
                buffer.pop();
            } else if !ctrl && !alt && key.chars().count() == 1 {
                buffer.push_str(key);
            }
        }
        (Mode::Text { buffer, .. }, Input::Paste { text }) => buffer.push_str(text),
        (
            Mode::List {
                entries, selected, ..
            },
            Input::Key { key, .. },
        ) => {
            let page = world
                .get::<Viewer>(id)
                .unwrap()
                .rows
                .saturating_sub(4)
                .max(1) as usize;
            match key.as_str() {
                "up" | "k" => *selected = selected.saturating_sub(1),
                "down" | "j" => *selected = (*selected + 1).min(entries.len().saturating_sub(1)),
                "pageup" => *selected = selected.saturating_sub(page),
                "pagedown" => *selected = (*selected + page).min(entries.len().saturating_sub(1)),
                "home" => *selected = 0,
                "end" => *selected = entries.len().saturating_sub(1),
                "enter" => {
                    if let Some(entry) = entries.get(*selected) {
                        execute =
                            Some((entry.action.clone(), entry.destination, String::new(), true));
                    }
                }
                "q" => cancel = true,
                _ => {}
            }
        }
        (
            Mode::List {
                entries, selected, ..
            },
            Input::Mouse { action, .. },
        ) => {
            if action == "scrollup" {
                *selected = selected.saturating_sub(1);
            }
            if action == "scrolldown" {
                *selected = (*selected + 1).min(entries.len().saturating_sub(1));
            }
        }
        _ => {}
    }
    if cancel || execute.is_some() {
        world.entity_mut(id).remove::<Overlay>();
    } else {
        world.entity_mut(id).insert(next);
    }
    if let Some((action, destination, value, interactive)) = execute {
        dispatch(
            world,
            id,
            overlay.target,
            &action,
            destination,
            &value,
            interactive,
        );
    }
    true
}

pub fn dispatch(
    world: &mut World,
    id: Entity,
    target: Target,
    action: &str,
    destination: Option<Entity>,
    value: &str,
    interactive: bool,
) {
    match invoke(world, id, target, action, destination, value, interactive) {
        Ok(true) => {}
        Ok(false) => world.trigger(Control {
            viewer: id,
            action: action.into(),
            value: value.into(),
            target: if matches!(action, "save_layout" | "load_layout") {
                Some(target.workspace)
            } else {
                destination.or(target.leaf)
            },
            mapping: Vec::new(),
        }),
        Err(message) => error(world, id, message),
    }
}

pub fn lines(world: &World, overlay: &Overlay, rows: u16) -> Vec<(String, &'static str)> {
    let mut lines = Vec::new();
    match &overlay.mode {
        Mode::Confirm { action } => {
            let entity = match action.as_str() {
                "close" => overlay.target.leaf,
                "tab_close" => overlay.target.tab,
                _ => Some(overlay.target.workspace),
            };
            lines.push((
                format!(
                    "Close {} {}?",
                    match action.as_str() {
                        "close" => "pane",
                        "tab_close" => "tab",
                        _ => "workspace",
                    },
                    entity.map_or_else(
                        || "removed target".into(),
                        |e| format!("{} #{}", label(world, e), e.to_bits())
                    )
                ),
                "\x1b[1m",
            ));
            lines.push((
                if action == "close" {
                    "Remove pane; stop if last view"
                } else {
                    "Remove all contained pane views"
                }
                .into(),
                "",
            ));
            lines.push(("y confirm · n/Esc cancel".into(), ""));
        }
        Mode::Text { action, buffer } => {
            lines.push((
                actions::metadata(action)
                    .map_or(action.as_str(), |a| a.label)
                    .into(),
                "\x1b[1m",
            ));
            lines.push((format!("{buffer}▏"), "\x1b[7m"));
            lines.push(("Enter accept · Esc cancel".into(), "\x1b[2m"));
        }
        Mode::List {
            title,
            entries,
            selected,
        } => {
            if rows >= 4 {
                lines.push((title.clone(), "\x1b[1m"));
            }
            let capacity = if rows >= 5 {
                rows - 4
            } else {
                rows.saturating_sub(2).max(1)
            } as usize;
            let start = selected.saturating_sub(capacity - 1);
            if start > 0 && rows >= 5 {
                lines.push((format!("▲ {start} more"), "\x1b[2m"));
            }
            for (index, entry) in entries.iter().enumerate().skip(start).take(capacity) {
                let disabled = actions::unavailable(world, overlay.target, &entry.action).is_some();
                lines.push((
                    format!(
                        "{} {}",
                        if index == *selected { "›" } else { " " },
                        entry.label
                    ),
                    if disabled {
                        "\x1b[2m"
                    } else if index == *selected {
                        "\x1b[7m"
                    } else {
                        ""
                    },
                ));
            }
            if entries.len() > start + capacity && rows >= 5 {
                lines.push((
                    format!("▼ {} more", entries.len() - start - capacity),
                    "\x1b[2m",
                ));
            }
            if entries.is_empty() {
                lines.push(("No destinations".into(), "\x1b[2m"));
            }
        }
    }
    // Tiny viewers prioritize the actionable/selected row rather than a heading.
    let available = rows.saturating_sub(1) as usize;
    if available < lines.len() && available <= 2 {
        match &overlay.mode {
            Mode::Confirm { .. } => {
                let title = lines.remove(0);
                lines = vec![(format!("y/n {}", title.0), "\x1b[1m")];
            }
            Mode::Text { .. } => {
                lines.remove(0);
            }
            Mode::List { .. } => {}
        }
    }
    lines.truncate(available);
    lines
}
