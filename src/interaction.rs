//! Viewer-local corner overlays. Captured entities are validated again on execution.
#[cfg(test)]
mod tests;
use crate::{
    actions::{self, Action, Target},
    chrome,
    control::Control,
    model::*,
    navigation,
    protocol::Input,
};
use bevy_ecs::prelude::*;

#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub action: Action,
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
        action: Action,
    },
    Text {
        action: Action,
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
    let Some(mut ownership) = world.get_mut::<crate::paste::Ownership>(id) else {
        return;
    };
    ownership.serial = ownership.serial.wrapping_add(1);
    let serial = ownership.serial;
    let Some(mut v) = world.get_mut::<Viewer>(id) else {
        return;
    };
    v.prefix = false;
    world.entity_mut(id).insert(Overlay {
        serial,
        target,
        mode,
    });
}

fn notify_error(world: &mut World, id: Entity, message: impl Into<String>) {
    notify(world, id, message, true);
}

pub fn invoke(
    world: &mut World,
    id: Entity,
    target: Target,
    action: Action,
    destination: Option<Entity>,
    value: &str,
    interactive: bool,
) -> Result<bool, String> {
    use Action::*;
    if let Some(reason) = actions::unavailable(world, target, action) {
        return Err(reason.into());
    }
    if !matches!(action, CopyMode | ScrollUp | ScrollDown) {
        world.entity_mut(id).remove::<crate::selection::Selection>();
    }
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.prefix = false;
        v.notify("", false);
    }
    if interactive && matches!(action, Close | TabClose | WorkspaceClose) {
        open(world, id, target, Mode::Confirm { action });
        return Ok(true);
    }
    if value.is_empty()
        && matches!(
            action,
            RenamePane | RenameTab | RenameWorkspace | SaveLayout | LoadLayout
        )
    {
        open(
            world,
            id,
            target,
            Mode::Text {
                action,
                buffer: String::new(),
            },
        );
        return Ok(true);
    }
    match action {
        TabNew => {
            world
                .get_mut::<Viewer>(id)
                .ok_or("viewer removed")?
                .workspace = target.workspace;
            navigation::control(world, id, action, None, value)?;
        }
        CopyMode => crate::selection::start(world, id, target.leaf.ok_or("no pane")?)?,
        TabChoose | WorkspaceChoose | SwapChoose | MoveTab | MoveWorkspace
            if destination.is_none() && value.is_empty() =>
        {
            let candidates = match action {
                WorkspaceChoose | MoveWorkspace => roots(world),
                SwapChoose => navigation::leaves(world, target.tab.ok_or("no tab")?)
                    .into_iter()
                    .filter(|e| Some(*e) != target.leaf)
                    .collect(),
                _ => navigation::tabs(world, target.workspace),
            };
            let execute = match action {
                TabChoose => TabSelect,
                WorkspaceChoose => WorkspaceSelect,
                SwapChoose => Swap,
                _ => action,
            };
            let entries = candidates
                .into_iter()
                .map(|entity| Entry {
                    label: label(world, entity),
                    action: execute,
                    destination: Some(entity),
                })
                .collect();
            open(
                world,
                id,
                target,
                Mode::List {
                    title: action.label().into(),
                    entries,
                    selected: 0,
                },
            );
        }
        PaneMenu | TabMenu | WorkspaceMenu => {
            let group = match action {
                PaneMenu => "Panes",
                TabMenu => "Tabs",
                _ => "Workspaces",
            };
            let entries = actions::ALL
                .iter()
                .copied()
                .filter(|a| {
                    (a.group() == group
                        || group == "Workspaces" && matches!(a, SaveLayout | LoadLayout))
                        && !matches!(a, PaneMenu | TabMenu | WorkspaceMenu)
                        && !matches!(
                            a,
                            TabNext | TabPrevious | TabChoose | WorkspaceNext | WorkspacePrevious
                        )
                })
                .map(|a| Entry {
                    label: a.label().into(),
                    action: a,
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
                                PaneMenu => target.leaf.ok_or("no pane")?,
                                TabMenu => target.tab.ok_or("no tab")?,
                                _ => target.workspace,
                            }
                        )
                    ),
                    entries,
                    selected: 0,
                },
            );
        }
        TabReorderPrevious | TabReorderNext | WorkspaceReorderPrevious | WorkspaceReorderNext => {
            let workspace_action =
                matches!(action, WorkspaceReorderPrevious | WorkspaceReorderNext);
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
            let other = if matches!(action, TabReorderPrevious | WorkspaceReorderPrevious) {
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
        Close | TabClose | WorkspaceClose => {
            let entity = match action {
                Close => target.leaf.ok_or("no pane")?,
                TabClose => target.tab.ok_or("no tab")?,
                _ => target.workspace,
            };
            close(world, entity);
            navigation::repair(world);
        }
        RenamePane | RenameTab | RenameWorkspace if !value.is_empty() => {
            let entity = match action {
                RenamePane => {
                    world
                        .get::<PaneView>(target.leaf.ok_or("no pane")?)
                        .ok_or("pane removed")?
                        .pane
                }
                RenameTab => target.tab.ok_or("no tab")?,
                _ => target.workspace,
            };
            world.entity_mut(entity).insert(Name::new(value.to_owned()));
        }
        SwapLeft | SwapRight | SwapUp | SwapDown | MoveLeft | MoveRight | MoveUp | MoveDown => {
            let source = target.leaf.ok_or("no pane")?;
            let direction = action.direction().ok_or("no direction")?;
            let destination = crate::frame::neighbor(world, id, source, direction)
                .ok_or("no pane in that direction")?;
            if matches!(action, SwapLeft | SwapRight | SwapUp | SwapDown) {
                swap(world, source, destination)?;
            } else {
                move_beside(world, source, destination, direction)?;
            }
            world.get_mut::<Viewer>(id).ok_or("viewer removed")?.zoom = false;
        }
        Swap => swap(
            world,
            target.leaf.ok_or("no pane")?,
            destination.ok_or("swap destination required")?,
        )?,
        MoveTab | MoveWorkspace | MoveNewTab | MoveNewWorkspace => {
            let leaf = target.leaf.ok_or("no pane")?;
            let (workspace, tab) = match action {
                MoveNewWorkspace => {
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
                MoveNewTab => {
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
                MoveWorkspace => {
                    let root = destination
                        .or_else(|| {
                            roots(world).into_iter().find(|e| {
                                world.get::<Name>(*e).is_some_and(|n| n.as_str() == value)
                            })
                        })
                        .filter(|e| world.get::<Workspace>(*e).is_some())
                        .ok_or("destination workspace removed")?;
                    navigation::normalize_workspace(world, root);
                    // normalize_workspace spawns a tab when a workspace has none.
                    #[expect(clippy::indexing_slicing, reason = "normalized workspace has a tab")]
                    let tab = navigation::tabs(world, root)[0];
                    (root, tab)
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
            let mut memory = world.get_mut::<Navigation>(id).ok_or("viewer removed")?;
            memory.tabs.insert(workspace, tab);
            memory.focus.insert(tab, leaf);
            let mut v = world.get_mut::<Viewer>(id).ok_or("viewer removed")?;
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
        .and_then(|children| children.iter().position(|e| e == destination))
        .ok_or("destination removed")?;
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
        .and_then(|children| children.iter().position(|e| e == source))
        .ok_or("source has no parent")?;
    let bi = world
        .get::<Children>(b)
        .and_then(|children| children.iter().position(|e| e == destination))
        .ok_or("destination has no parent")?;
    if a == b {
        world
            .get_mut::<Children>(a)
            .ok_or("source has no parent")?
            .swap(ai, bi);
    } else {
        world.entity_mut(a).insert_children(ai, &[destination]);
        world.entity_mut(b).insert_children(bi, &[source]);
    }
    Ok(())
}

/// Command column navigation owns reserved keys before configured prefix bindings.
pub fn command_input(world: &mut World, id: Entity, input: &Input) -> bool {
    let Some(v) = world.get::<Viewer>(id) else {
        return false;
    };
    if !v.prefix {
        return false;
    }
    let mut v = v.clone();
    let settings = world.resource::<crate::assets::Settings>();
    // The prefix itself retains literal forwarding, even for a navigation-key prefix.
    if input.token().as_deref() == Some(settings.prefix.as_str()) {
        return false;
    }
    let scroll =
        |v: &Viewer, down, page| chrome::scroll(settings, v.rows, v.help_scroll, down, page);
    let mut execute = None;
    match input {
        Input::Resize { .. } => return false,
        Input::Key {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        } => match key.as_str() {
            "up" | "pageup" => v.help_scroll = scroll(&v, false, key == "pageup"),
            "down" | "pagedown" => v.help_scroll = scroll(&v, true, key == "pagedown"),
            "left" | "right" => {} // Vertical menus reserve all unmodified arrows.
            "home" => v.help_scroll = 0,
            "end" => v.help_scroll = chrome::help_limit(settings, v.rows),
            "enter" => {
                execute = chrome::selected_action(settings, v.rows, v.cols, v.help_scroll)
                    .map(str::to_owned)
            }
            "escape" => {
                v.prefix = false;
                v.notice.clear();
            }
            _ => return false,
        },
        Input::Mouse { action, .. } => match action.as_str() {
            "scrollup" => v.help_scroll = scroll(&v, false, false),
            "scrolldown" => v.help_scroll = scroll(&v, true, false),
            _ => {}
        },
        Input::Paste { .. } | Input::PasteBegin => return true,
        _ => return false,
    }
    let target = Target::viewer(&v);
    if execute.is_some() {
        v.prefix = false;
    }
    let Some(mut current) = world.get_mut::<Viewer>(id) else {
        return true;
    };
    *current = v;
    if let Some(action) = execute {
        dispatch_named(world, id, target, &action, None, "", true);
    }
    true
}

/// Consume overlay input before the ordinary terminal/prefix observer.
pub fn input(world: &mut World, id: Entity, input: &Input) -> bool {
    let Some(overlay) = world.get::<Overlay>(id).cloned() else {
        return false;
    };
    if !overlay.target.valid(world) {
        world.entity_mut(id).remove::<Overlay>();
        notify_error(world, id, "target changed; action cancelled");
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
                execute = Some((*action, None, String::new(), false));
            } else if key == "n" {
                cancel = true;
            }
        }
        (Mode::Text { action, buffer }, Input::Key { key, ctrl, alt, .. }) => {
            if key == "enter" && !buffer.is_empty() {
                execute = Some((*action, None, buffer.clone(), false));
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
            let page = list_capacity(world.get::<Viewer>(id).map_or(0, |v| v.rows));
            match key.as_str() {
                "up" | "k" => *selected = selected.saturating_sub(1),
                "down" | "j" => *selected = (*selected + 1).min(entries.len().saturating_sub(1)),
                "pageup" => *selected = selected.saturating_sub(page),
                "pagedown" => *selected = (*selected + page).min(entries.len().saturating_sub(1)),
                "home" => *selected = 0,
                "end" => *selected = entries.len().saturating_sub(1),
                "enter" => {
                    if let Some(entry) = entries.get(*selected) {
                        execute = Some((entry.action, entry.destination, String::new(), true));
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
            action,
            destination,
            &value,
            interactive,
        );
    }
    true
}

/// A configured binding names its action as text; unknown names report as before.
pub fn dispatch_named(
    world: &mut World,
    id: Entity,
    target: Target,
    action: &str,
    destination: Option<Entity>,
    value: &str,
    interactive: bool,
) {
    match action.parse() {
        Ok(action) => dispatch(world, id, target, action, destination, value, interactive),
        Err(message) => unknown(world, id, target, message),
    }
}

/// The same visible outcome an unknown action string has always had: target
/// validity is still checked, the selection is dropped, the prefix closes, and
/// the bar shows the error.
pub fn unknown(world: &mut World, id: Entity, target: Target, message: String) {
    if !target.valid(world) {
        return notify_error(world, id, actions::TARGET_GONE);
    }
    world.entity_mut(id).remove::<crate::selection::Selection>();
    if let Some(mut v) = world.get_mut::<Viewer>(id) {
        v.prefix = false;
    }
    notify_error(world, id, message);
}

pub fn dispatch(
    world: &mut World,
    id: Entity,
    target: Target,
    action: Action,
    destination: Option<Entity>,
    value: &str,
    interactive: bool,
) {
    match invoke(world, id, target, action, destination, value, interactive) {
        Ok(true) => {}
        Ok(false) => world.trigger(Control {
            viewer: id,
            action: action.to_string(),
            value: value.into(),
            target: if matches!(action, Action::SaveLayout | Action::LoadLayout) {
                Some(target.workspace)
            } else {
                destination.or(target.leaf)
            },
            mapping: Vec::new(),
        }),
        Err(message) => notify_error(world, id, message),
    }
}

fn list_capacity(rows: u16) -> usize {
    if rows >= 5 {
        usize::from(rows - 4)
    } else {
        usize::from(rows.saturating_sub(2).max(1))
    }
}

pub fn lines(world: &World, overlay: &Overlay, rows: u16) -> Vec<(String, &'static str)> {
    let mut lines = Vec::new();
    match &overlay.mode {
        Mode::Confirm { action } => {
            let entity = match action {
                Action::Close => overlay.target.leaf,
                Action::TabClose => overlay.target.tab,
                _ => Some(overlay.target.workspace),
            };
            lines.push((
                format!(
                    "Close {} {}?",
                    match action {
                        Action::Close => "pane",
                        Action::TabClose => "tab",
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
                if *action == Action::Close {
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
            lines.push((action.label().into(), "\x1b[1m"));
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
            let capacity = list_capacity(rows);
            let start = selected.saturating_sub(capacity - 1);
            if start > 0 && rows >= 5 {
                lines.push((format!("▲ {start} more"), "\x1b[2m"));
            }
            for (index, entry) in entries.iter().enumerate().skip(start).take(capacity) {
                let disabled = actions::unavailable(world, overlay.target, entry.action).is_some();
                lines.push((
                    format!(
                        "{} {}",
                        if index == *selected { "›" } else { " " },
                        entry.label
                    ),
                    if disabled && index == *selected {
                        "\x1b[2;7m"
                    } else if disabled {
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
