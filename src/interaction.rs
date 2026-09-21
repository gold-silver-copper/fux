//! Viewer-local corner overlays. Captured entities are validated again on execution.
#[cfg(test)]
mod tests;
use crate::{
    actions::{self, Action, Target},
    assets::BindingAction,
    chrome,
    control::{Chooser, Command, Order, Scope, Subject},
    model::*,
    navigation,
    protocol::{Direction, Input, Key, Modifiers, MouseAction},
};
use bevy_ecs::prelude::*;
use bevy_reflect::{Reflect, ReflectDeserialize, ReflectSerialize};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub run: Run,
}
/// A list row either names a bound action (menus, which may prompt again) or
/// carries the exact command a chooser resolved (never prompts again).
#[derive(Clone)]
pub enum Run {
    Action(Action),
    Command(Command),
}
#[derive(Clone)]
pub enum Mode {
    List {
        title: String,
        entries: Vec<Entry>,
        selected: usize,
    },
    Confirm {
        command: Command,
    },
    Text {
        action: Action,
        buffer: String,
    },
}
/// The open prefix command column and its selected action row. Present only
/// while the column is open, like `Overlay` and `Selection`.
#[derive(Component, Default)]
pub struct Prefix {
    pub scroll: usize,
}

/// A modal viewer owns its input: the prefix column, an overlay or copy mode.
pub(crate) fn modal(world: &World, id: Entity) -> bool {
    world.get_entity(id).is_ok_and(|entity| {
        entity.contains::<Prefix>()
            || entity.contains::<Overlay>()
            || entity.contains::<crate::selection::Selection>()
    })
}

pub(crate) fn close_prefix(world: &mut World, id: Entity) {
    if let Ok(mut entity) = world.get_entity_mut(id) {
        entity.remove::<Prefix>();
    }
}

#[derive(Component, Clone)]
#[component(on_insert = crate::paste::overlay_opened)]
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
    if world.get::<Viewer>(id).is_none() {
        return;
    }
    // `paste::overlay_opened` assigns the serial on insertion.
    world.entity_mut(id).remove::<Prefix>().insert(Overlay {
        serial: 0,
        target,
        mode,
    });
}

fn notify_error(world: &mut World, id: Entity, message: impl Into<String>) {
    notify(world, id, Notice::error(message));
}

/// Runs a bound action for a viewer: prompts and confirmations open an
/// overlay whose completion produces the command; everything else is a
/// command right away.
pub fn bound(world: &mut World, id: Entity, target: Target, action: Action) -> Result<(), String> {
    use Action::*;
    if let Some(reason) = actions::unavailable(world, target, action) {
        return Err(reason.into());
    }
    match action {
        Close | TabClose | WorkspaceClose => {
            let command = action.command(target).ok_or("no pane")?;
            settle(world, id);
            open(world, id, target, Mode::Confirm { command });
            Ok(())
        }
        RenamePane | RenameTab | RenameWorkspace | SaveLayout | LoadLayout => {
            settle(world, id);
            open(
                world,
                id,
                target,
                Mode::Text {
                    action,
                    buffer: String::new(),
                },
            );
            Ok(())
        }
        _ => crate::server::execute(world, id, action.command(target).ok_or("no pane")?),
    }
}

/// Every command starts from a quiet viewer: no selection, no column, no notice.
pub(crate) fn settle(world: &mut World, id: Entity) {
    if let Ok(mut entity) = world.get_entity_mut(id) {
        entity.remove::<(crate::selection::Selection, Prefix)>();
    }
    notify(world, id, None);
}

/// Opens an interactive list whose rows are resolved commands.
pub(crate) fn choose(
    world: &mut World,
    id: Entity,
    target: Target,
    chooser: Chooser,
) -> Result<(), String> {
    let title = match chooser {
        Chooser::Tab => Action::TabChoose,
        Chooser::Workspace => Action::WorkspaceChoose,
        Chooser::SwapTarget => Action::SwapChoose,
        Chooser::MoveToTab => Action::MoveTab,
        Chooser::MoveToWorkspace => Action::MoveWorkspace,
    };
    let entities = match chooser {
        Chooser::Tab | Chooser::MoveToTab => navigation::tabs(world, target.workspace),
        Chooser::Workspace | Chooser::MoveToWorkspace => roots(world),
        Chooser::SwapTarget => navigation::leaves(world, target.tab.ok_or("no tab")?)
            .into_iter()
            .filter(|e| Some(*e) != target.leaf)
            .collect(),
    };
    let entries = entities
        .into_iter()
        .map(|entity| {
            let command = match chooser {
                Chooser::Tab => Command::Select {
                    scope: Scope::Tab,
                    entity,
                },
                Chooser::Workspace => Command::Select {
                    scope: Scope::Workspace,
                    entity,
                },
                Chooser::SwapTarget => Command::Swap { with: entity },
                Chooser::MoveToTab => Command::Move {
                    to: MoveTo::Tab { tab: entity },
                },
                Chooser::MoveToWorkspace => Command::Move {
                    to: MoveTo::Workspace { workspace: entity },
                },
            };
            Entry {
                label: label(world, entity),
                run: Run::Command(command),
            }
        })
        .collect();
    if let Some(reason) = actions::unavailable(world, target, title) {
        return Err(reason.into());
    }
    settle(world, id);
    open(
        world,
        id,
        target,
        Mode::List {
            title: title.label().into(),
            entries,
            selected: 0,
        },
    );
    Ok(())
}
/// Opens the action menu for a pane, tab or workspace.
pub(crate) fn menu(
    world: &mut World,
    id: Entity,
    mut target: Target,
    subject: Subject,
) -> Result<(), String> {
    use Action::*;
    let (action, group, entity) = match subject {
        Subject::Pane(pane) => {
            target.leaf = Some(pane);
            (PaneMenu, "Panes", pane)
        }
        Subject::Tab(tab) => {
            target.tab = Some(tab);
            target.leaf = None;
            (TabMenu, "Tabs", tab)
        }
        Subject::Workspace(workspace) => {
            target = Target {
                workspace,
                tab: None,
                leaf: None,
            };
            (WorkspaceMenu, "Workspaces", workspace)
        }
    };
    if let Some(reason) = actions::unavailable(world, target, action) {
        return Err(reason.into());
    }
    let entries = actions::ALL
        .iter()
        .copied()
        .filter(|a| {
            (a.group() == group || group == "Workspaces" && matches!(a, SaveLayout | LoadLayout))
                && !matches!(a, PaneMenu | TabMenu | WorkspaceMenu)
                && !matches!(
                    a,
                    TabNext | TabPrevious | TabChoose | WorkspaceNext | WorkspacePrevious
                )
        })
        .map(|a| Entry {
            label: a.label().into(),
            run: Run::Action(a),
        })
        .collect();
    settle(world, id);
    open(
        world,
        id,
        target,
        Mode::List {
            title: format!("{group}: {}", label(world, entity)),
            entries,
            selected: 0,
        },
    );
    Ok(())
}

/// Reorders a tab among its workspace's tabs or a workspace among all workspaces.
pub(crate) fn reorder(world: &mut World, subject: Subject, order: Order) -> Result<(), String> {
    let (mut entities, entity, workspace) = match subject {
        Subject::Tab(tab) => {
            let workspace = world
                .get::<ChildOf>(tab)
                .ok_or("tab has no workspace")?
                .parent();
            (navigation::tabs(world, workspace), tab, Some(workspace))
        }
        Subject::Workspace(workspace) => (roots(world), workspace, None),
        Subject::Pane(_) => return Err("target has the wrong kind for this action".into()),
    };
    let index = entities
        .iter()
        .position(|e| *e == entity)
        .ok_or("target removed")?;
    let other = match order {
        Order::Previous => index.saturating_sub(1),
        Order::Next => (index + 1).min(entities.len() - 1),
    };
    entities.swap(index, other);
    match workspace {
        Some(workspace) => {
            world.entity_mut(workspace).replace_children(&entities);
        }
        None => {
            for (index, entity) in entities.into_iter().enumerate() {
                world
                    .entity_mut(entity)
                    .insert(WorkspaceOrder(index as i64));
            }
        }
    }
    Ok(())
}

pub(crate) fn rename(world: &mut World, subject: Subject, name: String) -> Result<(), String> {
    let entity = match subject {
        Subject::Pane(leaf) => world.get::<PaneView>(leaf).ok_or("pane removed")?.pane,
        Subject::Tab(tab) => tab,
        Subject::Workspace(workspace) => workspace,
    };
    world
        .get_entity_mut(entity)
        .map_err(|_| "target no longer exists")?
        .insert(Name::new(name));
    Ok(())
}

/// The kind a subject names must match the entity, whichever caller built it.
pub(crate) fn check(world: &World, subject: Subject) -> Result<Entity, String> {
    let (entity, ok) = match subject {
        Subject::Pane(e) => (e, world.get::<PaneView>(e).is_some()),
        Subject::Tab(e) => (e, world.get::<Tab>(e).is_some()),
        Subject::Workspace(e) => (e, world.get::<Workspace>(e).is_some()),
    };
    if world.get_entity(entity).is_err() {
        return Err("target no longer exists".into());
    }
    if !ok {
        return Err("target has the wrong kind for this action".into());
    }
    Ok(entity)
}

/// Where a pane moves to. New containers are created on demand.
#[derive(Reflect, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MoveTo {
    Tab { tab: Entity },
    NewTab { name: Option<String> },
    Workspace { workspace: Entity },
    NewWorkspace { name: Option<String> },
}

/// Moves the viewer's focused pane and follows it, without overwriting other
/// viewers' memory.
pub(crate) fn move_pane(
    world: &mut World,
    id: Entity,
    target: Target,
    to: MoveTo,
) -> Result<(), String> {
    let leaf = target.leaf.ok_or("no pane")?;
    let (workspace, tab) = match to {
        MoveTo::NewWorkspace { name } => {
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
                    Name::new(name.unwrap_or_else(|| "workspace".into())),
                    tab_node(),
                ))
                .id();
            let tab = world.spawn((Tab, Name::new("main"), ChildOf(root))).id();
            (root, tab)
        }
        MoveTo::NewTab { name } => {
            let tab = world
                .spawn((
                    Tab,
                    Name::new(name.unwrap_or_else(|| "tab".into())),
                    ChildOf(target.workspace),
                ))
                .id();
            (target.workspace, tab)
        }
        MoveTo::Workspace { workspace: root } => {
            if world.get::<Workspace>(root).is_none() {
                return Err("destination workspace removed".into());
            }
            navigation::normalize_workspace(world, root);
            // normalize_workspace spawns a tab when a workspace has none.
            #[expect(clippy::indexing_slicing, reason = "normalized workspace has a tab")]
            let tab = navigation::tabs(world, root)[0];
            (root, tab)
        }
        MoveTo::Tab { tab } => {
            if world.get::<Tab>(tab).is_none() {
                return Err("destination tab removed".into());
            }
            let root = world
                .get::<ChildOf>(tab)
                .ok_or("tab has no workspace")?
                .parent();
            (root, tab)
        }
    };
    if Some(tab) == target.tab {
        return Ok(());
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
    // Following the pane records this viewer's memory through the observers,
    // without touching other viewers'.
    world
        .get_entity_mut(id)
        .map_err(|_| "viewer removed")?
        .insert((Viewing(workspace), OnTab(tab), Focused(leaf)));
    let mut v = world.get_mut::<Viewer>(id).ok_or("viewer removed")?;
    v.zoom = false;
    v.scrollback = 0;
    Ok(())
}

/// Swaps or moves the focused pane toward its nearest neighbour.
pub(crate) fn beside(
    world: &mut World,
    id: Entity,
    target: Target,
    direction: Direction,
    swap_panes: bool,
) -> Result<(), String> {
    let source = target.leaf.ok_or("no pane")?;
    let destination =
        crate::frame::neighbor(world, id, source, direction).ok_or("no pane in that direction")?;
    if swap_panes {
        swap(world, source, destination)?;
    } else {
        move_beside(world, source, destination, direction)?;
    }
    world.get_mut::<Viewer>(id).ok_or("viewer removed")?.zoom = false;
    Ok(())
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
    direction: Direction,
) -> Result<(), String> {
    use bevy_ui::FlexDirection;
    let parent = world
        .get::<ChildOf>(destination)
        .ok_or("destination removed")?
        .parent();
    let index = world
        .get::<Children>(parent)
        .and_then(|children| children.iter().position(|e| e == destination))
        .ok_or("destination removed")?;
    let mut split = world.spawn(Split);
    if matches!(direction, Direction::Up | Direction::Down) {
        split.insert(split_node(FlexDirection::Column));
    }
    let split = split.id();
    let children = if matches!(direction, Direction::Left | Direction::Up) {
        [source, destination]
    } else {
        [destination, source]
    };
    world.entity_mut(parent).insert_children(index, &[split]);
    world.entity_mut(split).add_children(&children);
    Ok(())
}

pub(crate) fn swap(world: &mut World, source: Entity, destination: Entity) -> Result<(), String> {
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
    let (Some(v), Some(prefix)) = (world.get::<Viewer>(id), world.get::<Prefix>(id)) else {
        return false;
    };
    let (rows, cols, mut scroll) = (v.rows, v.cols, prefix.scroll);
    let settings = world.resource::<crate::assets::Settings>();
    // The prefix itself retains literal forwarding, even for a navigation-key prefix.
    if input.token().as_ref() == Some(&settings.prefix) {
        return false;
    }
    let step = |current, down, page| chrome::scroll(settings, rows, current, down, page);
    let mut execute = None;
    let mut close = false;
    match input {
        Input::Resize { .. } => return false,
        Input::Key {
            key,
            modifiers:
                Modifiers {
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
        } => match key {
            Key::Arrow(Direction::Up) => scroll = step(scroll, false, false),
            Key::PageUp => scroll = step(scroll, false, true),
            Key::Arrow(Direction::Down) => scroll = step(scroll, true, false),
            Key::PageDown => scroll = step(scroll, true, true),
            Key::Arrow(_) => {} // Vertical menus reserve all unmodified arrows.
            Key::Home => scroll = 0,
            Key::End => scroll = chrome::help_limit(settings, rows),
            Key::Enter => {
                execute = chrome::selected_action(settings, rows, cols, scroll).cloned();
                close = true;
            }
            Key::Escape => close = true,
            _ => return false,
        },
        Input::Mouse { action, .. } => match action {
            MouseAction::ScrollUp => scroll = step(scroll, false, false),
            MouseAction::ScrollDown => scroll = step(scroll, true, false),
            _ => {}
        },
        Input::Paste { .. } | Input::PasteBegin => return true,
        _ => return false,
    }
    if close {
        close_prefix(world, id);
    } else if let Some(mut prefix) = world.get_mut::<Prefix>(id) {
        prefix.scroll = scroll;
    }
    match execute {
        Some(binding) => {
            let Some(target) = Target::of(world, id) else {
                return true;
            };
            dispatch_binding(world, id, target, &binding);
        }
        None if close => notify(world, id, None),
        None => {}
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
    let mut execute: Option<Run> = None;
    let mut cancel = false;
    match (&mut next.mode, input) {
        (
            _,
            Input::Key {
                key: Key::Escape, ..
            },
        ) => cancel = true,
        (
            Mode::Confirm { command },
            Input::Key {
                key: Key::Char(c),
                modifiers:
                    Modifiers {
                        ctrl: false,
                        alt: false,
                        ..
                    },
            },
        ) => {
            if *c == 'y' {
                execute = Some(Run::Command(command.clone()));
            } else if *c == 'n' {
                cancel = true;
            }
        }
        (Mode::Text { action, buffer }, Input::Key { key, modifiers }) => match key {
            Key::Enter if !buffer.is_empty() => {
                execute = action
                    .with_text(overlay.target, buffer.clone())
                    .map(Run::Command);
            }
            Key::Backspace => {
                buffer.pop();
            }
            Key::Char(c) if !modifiers.ctrl && !modifiers.alt => buffer.push(*c),
            _ => {}
        },
        (Mode::Text { buffer, .. }, Input::Paste { text }) => buffer.push_str(text),
        (
            Mode::List {
                entries, selected, ..
            },
            Input::Key { key, .. },
        ) => {
            let page = list_capacity(world.get::<Viewer>(id).map_or(0, |v| v.rows));
            let last = entries.len().saturating_sub(1);
            match key {
                Key::Arrow(Direction::Up) | Key::Char('k') => {
                    *selected = selected.saturating_sub(1);
                }
                Key::Arrow(Direction::Down) | Key::Char('j') => {
                    *selected = (*selected + 1).min(last);
                }
                Key::PageUp => *selected = selected.saturating_sub(page),
                Key::PageDown => *selected = (*selected + page).min(last),
                Key::Home => *selected = 0,
                Key::End => *selected = last,
                Key::Enter => {
                    if let Some(entry) = entries.get(*selected) {
                        execute = Some(entry.run.clone());
                    }
                }
                Key::Char('q') => cancel = true,
                _ => {}
            }
        }
        (
            Mode::List {
                entries, selected, ..
            },
            Input::Mouse { action, .. },
        ) => match action {
            MouseAction::ScrollUp => *selected = selected.saturating_sub(1),
            MouseAction::ScrollDown => {
                *selected = (*selected + 1).min(entries.len().saturating_sub(1));
            }
            _ => {}
        },
        _ => {}
    }
    if cancel || execute.is_some() {
        world.entity_mut(id).remove::<Overlay>();
    } else {
        world.entity_mut(id).insert(next);
    }
    if let Some(run) = execute {
        let result = match run {
            Run::Action(action) => bound(world, id, overlay.target, action),
            Run::Command(command) => crate::server::execute(world, id, command),
        };
        if let Err(message) = result {
            notify_error(world, id, message);
        }
    }
    true
}

/// The one place a configured binding is dispatched. A custom name has always
/// closed the prefix, dropped the selection and reported itself as unknown.
pub fn dispatch_binding(world: &mut World, id: Entity, target: Target, binding: &BindingAction) {
    let result = match binding {
        BindingAction::Known(action) => bound(world, id, target, *action),
        BindingAction::Custom(name) => {
            if !target.valid(world) {
                Err(actions::TARGET_GONE.into())
            } else {
                settle(world, id);
                Err(format!("unknown action {name}"))
            }
        }
    };
    if let Err(message) = result {
        notify_error(world, id, message);
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
        Mode::Confirm { command } => {
            let (kind, entity) = match command {
                Command::Close {
                    subject: Subject::Pane(e),
                } => ("pane", *e),
                Command::Close {
                    subject: Subject::Tab(e),
                } => ("tab", *e),
                Command::Close {
                    subject: Subject::Workspace(e),
                } => ("workspace", *e),
                _ => ("target", Entity::PLACEHOLDER),
            };
            let named = if world.get_entity(entity).is_ok() {
                format!("{} #{}", label(world, entity), entity.to_bits())
            } else {
                "removed target".into()
            };
            lines.push((format!("Close {kind} {named}?"), "\x1b[1m"));
            lines.push((
                if kind == "pane" {
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
                let disabled = match &entry.run {
                    Run::Action(action) => {
                        actions::unavailable(world, overlay.target, *action).is_some()
                    }
                    Run::Command(_) => false,
                };
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
