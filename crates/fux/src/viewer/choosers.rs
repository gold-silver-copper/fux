//! Choosers (prompt 3.11): the tab chooser (`prefix w`), the workspace chooser (`prefix s`)
//! and the help panel (`prefix ?`) are one viewer-local list popup in the `bevy_ui_widgets`
//! `ListBox`/`ListItem` shape: rows never take focus, the list holds a roving
//! [`ActiveDescendant`] cursor, the cursor row carries `bevy_ui`'s `Selected` marker for the
//! painter, and the rows scroll inside an overflow viewport that keeps the cursor visible
//! (`ScrollIntoView`'s minimal adjustment, computed from the row index). Enter activates the
//! cursor row; Escape, `q` or focus leaving the popup dismisses it.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemId;
use bevy_input::keyboard::Key;
use bevy_input_focus::InputFocus;
use bevy_picking::events::{Pointer, Press};
use bevy_state::prelude::*;
use bevy_ui::interaction_states::Selected;
use bevy_ui::prelude::*;
use bevy_ui::{Overflow, ScrollPosition};

use super::chrome::{
    Align, Chrome, ChromeRoots, PendingNotice, Placement, Popover, Popup, Side, Text, close_popup,
    spawn_popup,
};
use super::focus::Bindings;
use super::keys::KeyChord;
use super::replicate::{Roots, Session, ShowingRoot};
use super::{Brp, BrpReply, BrpTag, Mode, Outbox, Reconnect, Viewport};
use crate::assets::ThemeToken;
use crate::model::{NodeId, ViewerRequest};

/// What a chooser lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChooserKind {
    Tabs,
    Workspaces,
    Help,
}

/// The list popup: `list` is the scrolling viewport whose children are the rows.
#[derive(Component, Debug, Clone, Copy)]
pub struct Chooser {
    pub kind: ChooserKind,
    list: Entity,
    /// Rows the viewport shows at once.
    visible: usize,
}

/// `bevy_ui_widgets::list::ActiveDescendant`: the single roving cursor of a list, held by the
/// list rather than by focusable rows.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[component(immutable)]
pub struct ActiveDescendant(pub Option<Entity>);

/// A list row and what activating it does.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub enum RowAction {
    ShowRoot(NodeId),
    AttachWorkspace(String),
    /// Help rows: activation only closes the panel.
    None,
}

const PLACEMENTS: &[Placement] = &[
    Placement::new(Side::Top, Align::Start),
    Placement::new(Side::Bottom, Align::Start),
];

fn row_node() -> Node {
    Node {
        height: Val::Px(1.0),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// Spawns the popup with a header and an empty list, takes focus and enters `Mode::Chooser`.
fn open(
    kind: ChooserKind,
    header: &str,
    commands: &mut Commands,
    chrome: &ChromeRoots,
    viewport: &Viewport,
    focus: &mut InputFocus,
    next: &mut NextState<Mode>,
) -> (Entity, Chooser) {
    let list = commands
        .spawn((
            Chrome,
            Node {
                flex_direction: FlexDirection::Column,
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                min_height: Val::Px(0.0),
                overflow: Overflow::scroll_y(),
                ..Default::default()
            },
            ScrollPosition::default(),
            ActiveDescendant::default(),
        ))
        .id();
    let title = commands
        .spawn((
            Chrome,
            ThemeToken::NOTICE,
            row_node(),
            Text(format!(" {header} ")),
        ))
        .id();
    let chooser = Chooser {
        kind,
        list,
        visible: usize::from(viewport.rows.saturating_sub(3).max(1)),
    };
    let popup = spawn_popup(
        commands,
        chrome,
        focus,
        (
            chooser,
            ThemeToken::BAR_BACKGROUND,
            Popover {
                anchor: chrome.tab_strip,
                placements: PLACEMENTS,
            },
            Node {
                position_type: PositionType::Absolute,
                flex_direction: FlexDirection::Column,
                ..Default::default()
            },
        ),
    );
    commands
        .entity(popup)
        .add_children(&[title, list])
        .observe(on_row_press);
    next.set(Mode::Chooser);
    (popup, chooser)
}

/// Replaces the rows with `(label, action, initially active)` and sizes the popup to fit.
fn fill(
    commands: &mut Commands,
    popup: Entity,
    chooser: &Chooser,
    children: &Query<&Children>,
    rows: impl IntoIterator<Item = (String, RowAction, bool)>,
) {
    if let Ok(old) = children.get(chooser.list) {
        for row in old.iter() {
            commands.entity(row).despawn();
        }
    }
    let mut active = None;
    let mut count = 0usize;
    let mut width = 0usize;
    for (label, action, is_active) in rows {
        width = width.max(label.chars().count());
        let row = commands
            .spawn((
                Chrome,
                ThemeToken::BAR,
                action,
                ChildOf(chooser.list),
                row_node(),
                Text(label),
            ))
            .id();
        if is_active || count == 0 {
            active = Some(row);
        }
        count += 1;
    }
    commands
        .entity(chooser.list)
        .insert(ActiveDescendant(active));
    commands.entity(popup).insert(Node {
        position_type: PositionType::Absolute,
        flex_direction: FlexDirection::Column,
        width: Val::Px((width.max(10) + 2) as f32),
        height: Val::Px((1 + count.clamp(1, chooser.visible)) as f32),
        ..Default::default()
    });
}

/// `prefix w`: the workspace's roots in tab order, the shown one under the cursor.
pub fn open_tab_chooser(
    roots: Res<Roots>,
    showing: Res<ShowingRoot>,
    chrome: Res<ChromeRoots>,
    viewport: Res<Viewport>,
    children: Query<&Children>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    if roots.0.is_empty() {
        return;
    }
    let (popup, chooser) = open(
        ChooserKind::Tabs,
        "tabs",
        &mut commands,
        &chrome,
        &viewport,
        &mut focus,
        &mut next,
    );
    let rows = roots.0.iter().enumerate().map(|(i, root)| {
        (
            format!(" {}:{} ", i + 1, root.name),
            RowAction::ShowRoot(root.node),
            showing.0 == Some(root.node),
        )
    });
    fill(&mut commands, popup, &chooser, &children, rows);
}

/// `prefix s`: the server's workspaces, fetched over BRP; the list fills when the reply lands.
pub fn open_workspace_chooser(
    brp: Res<Brp>,
    chrome: Res<ChromeRoots>,
    viewport: Res<Viewport>,
    children: Query<&Children>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let (popup, chooser) = open(
        ChooserKind::Workspaces,
        "workspaces",
        &mut commands,
        &chrome,
        &viewport,
        &mut focus,
        &mut next,
    );
    fill(
        &mut commands,
        popup,
        &chooser,
        &children,
        [(" loading… ".to_owned(), RowAction::None, true)],
    );
    brp.call(
        BrpTag::WorkspaceList,
        "fux/workspace.list",
        serde_json::json!({}),
    );
}

/// `prefix ?`: the bindings table as a scrollable list.
pub fn open_help(
    bindings: Res<Bindings>,
    chrome: Res<ChromeRoots>,
    viewport: Res<Viewport>,
    children: Query<&Children>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let (popup, chooser) = open(
        ChooserKind::Help,
        "bindings",
        &mut commands,
        &chrome,
        &viewport,
        &mut focus,
        &mut next,
    );
    let prefix = bindings.prefix();
    let rows = bindings.table.sorted().into_iter().map(|(chord, action)| {
        (
            format!(" {prefix} {chord:<8} {action} "),
            RowAction::None,
            false,
        )
    });
    fill(&mut commands, popup, &chooser, &children, rows);
}

/// The newest workspace list reply of the update fills the chooser (or names the failure in
/// it).
fn fill_workspaces(
    mut replies: MessageReader<BrpReply>,
    session: Res<Session>,
    choosers: Query<(Entity, &Chooser)>,
    children: Query<&Children>,
    mut commands: Commands,
) {
    let Some(reply) = replies
        .read()
        .filter(|r| r.tag == BrpTag::WorkspaceList)
        .last()
    else {
        return;
    };
    let Some((popup, chooser)) = choosers
        .iter()
        .find(|(_, c)| c.kind == ChooserKind::Workspaces)
    else {
        return;
    };
    let current = session.0.as_ref().map(|w| w.workspace.as_str());
    let rows: Vec<(String, RowAction, bool)> = match &reply.result {
        Ok(value) => value
            .get("workspaces")
            .and_then(|w| w.as_array())
            .into_iter()
            .flatten()
            .filter_map(|w| w.get("name").and_then(|n| n.as_str()))
            .map(|name| {
                let active = current == Some(name);
                let marker = if active { '*' } else { ' ' };
                (
                    format!(" {marker} {name} "),
                    RowAction::AttachWorkspace(name.to_owned()),
                    active,
                )
            })
            .collect(),
        Err(e) => vec![(format!(" {e} "), RowAction::None, true)],
    };
    fill(&mut commands, popup, chooser, &children, rows);
}

/// `Selected` and the row token follow the list's cursor; the viewport scrolls the minimum
/// needed to show it (`ScrollIntoView`).
fn sync_cursor(
    lists: Query<(Entity, &ActiveDescendant, &mut ScrollPosition), Changed<ActiveDescendant>>,
    choosers: Query<&Chooser>,
    children: Query<&Children>,
    rows: Query<(Has<Selected>, &ThemeToken), With<RowAction>>,
    mut commands: Commands,
) {
    for (list, active, mut scroll) in lists {
        let Ok(rows_of) = children.get(list) else {
            continue;
        };
        let visible = choosers
            .iter()
            .find(|c| c.list == list)
            .map_or(usize::MAX, |c| c.visible);
        for (index, row) in rows_of.iter().enumerate() {
            let Ok((selected, token)) = rows.get(row) else {
                continue;
            };
            if active.0 == Some(row) {
                if !selected {
                    commands.entity(row).insert(Selected);
                }
                if *token != ThemeToken::TAB_ACTIVE {
                    commands.entity(row).insert(ThemeToken::TAB_ACTIVE);
                }
                let top = scroll.0.y.round() as usize;
                let target = if index < top {
                    index
                } else if index >= top + visible {
                    index + 1 - visible
                } else {
                    top
                };
                if target != top {
                    scroll.0.y = target as f32;
                }
            } else {
                if selected {
                    commands.entity(row).remove::<Selected>();
                }
                if *token != ThemeToken::BAR {
                    commands.entity(row).insert(ThemeToken::BAR);
                }
            }
        }
    }
}

/// A row's commitment: `Show` for a tab, a local reconnect for another workspace.
fn activate(
    action: &RowAction,
    session: &Session,
    outbox: &mut Outbox,
    notice: &mut PendingNotice,
    commands: &mut Commands,
) {
    match action {
        RowAction::ShowRoot(root) => outbox.push(ViewerRequest::Show(*root)),
        RowAction::AttachWorkspace(name) => {
            let current = session.0.as_ref().map(|w| w.workspace.as_str());
            if current == Some(name.as_str()) {
                notice.0 = Some(format!("already attached to {name}"));
            } else {
                commands.insert_resource(Reconnect(name.clone()));
            }
        }
        RowAction::None => {}
    }
}

/// Keys in `Mode::Chooser`, run for each chord by `focus::on_key`.
#[allow(clippy::too_many_arguments)]
pub fn handle_key(
    In(chord): In<KeyChord>,
    bindings: Res<Bindings>,
    session: Res<Session>,
    choosers: Query<(Entity, &Popup, &Chooser)>,
    lists: Query<&ActiveDescendant>,
    children: Query<&Children>,
    rows: Query<&RowAction>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut outbox: ResMut<Outbox>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
) {
    let Ok((entity, shell, chooser)) = choosers.single() else {
        next.set(Mode::Normal);
        return;
    };
    let popup = (entity, shell);
    if chord == *bindings.prefix() {
        close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Prefix);
        return;
    }
    let items: Vec<Entity> = children
        .get(chooser.list)
        .map(|c| c.iter().filter(|e| rows.contains(*e)).collect())
        .unwrap_or_default();
    let current = lists
        .get(chooser.list)
        .ok()
        .and_then(|a| a.0)
        .and_then(|a| items.iter().position(|e| *e == a));
    let len = items.len();
    // Wrapping step as `listbox_on_key_input`: no cursor moves to the first or last row.
    let step = |delta: isize| {
        (len > 0).then(|| {
            let from = current.unwrap_or(if delta < 0 { 0 } else { len - 1 }) as isize;
            (from + delta).rem_euclid(len as isize) as usize
        })
    };
    let page = chooser.visible;
    enum Do {
        Move(Option<usize>),
        Activate,
        Close,
    }
    let action = match &chord.key {
        Key::Escape => Do::Close,
        Key::Enter => Do::Activate,
        Key::ArrowUp => Do::Move(step(-1)),
        Key::ArrowDown => Do::Move(step(1)),
        Key::Home => Do::Move((len > 0).then_some(0)),
        Key::End => Do::Move(len.checked_sub(1)),
        Key::PageUp => Do::Move((len > 0).then(|| current.unwrap_or(0).saturating_sub(page))),
        Key::PageDown => Do::Move(
            len.checked_sub(1)
                .map(|last| (current.unwrap_or(0) + page).min(last)),
        ),
        Key::Character(c) => match c.as_str() {
            "q" => Do::Close,
            " " => Do::Activate,
            "k" => Do::Move(step(-1)),
            "j" => Do::Move(step(1)),
            "g" => Do::Move((len > 0).then_some(0)),
            "G" => Do::Move(len.checked_sub(1)),
            _ => return,
        },
        _ => return,
    };
    match action {
        Do::Close => close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Normal),
        Do::Activate => {
            if let Some(row) = current.and_then(|i| items.get(i))
                && let Ok(action) = rows.get(*row)
            {
                activate(action, &session, &mut outbox, &mut notice, &mut commands);
            }
            close_popup(&mut commands, &mut focus, &mut next, popup, Mode::Normal);
        }
        Do::Move(Some(index)) if Some(index) != current => {
            if let Some(row) = items.get(index) {
                commands
                    .entity(chooser.list)
                    .insert(ActiveDescendant(Some(*row)));
            }
        }
        Do::Move(_) => {}
    }
}

/// `listbox_on_row_click`: a press on a row moves the cursor there and activates it.
fn on_row_press(
    mut ev: On<Pointer<Press>>,
    choosers: Query<(&Popup, &Chooser)>,
    parents: Query<&ChildOf>,
    rows: Query<&RowAction>,
    session: Res<Session>,
    mut focus: ResMut<InputFocus>,
    mut next: ResMut<NextState<Mode>>,
    mut outbox: ResMut<Outbox>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
) {
    let popup = ev.entity;
    let Ok((shell, chooser)) = choosers.get(popup) else {
        return;
    };
    ev.propagate(false);
    let mut cursor = ev.original_event_target();
    let row = loop {
        if rows.contains(cursor) {
            break Some(cursor);
        }
        if cursor == popup {
            break None;
        }
        match parents.get(cursor) {
            Ok(parent) => cursor = parent.parent(),
            Err(_) => break None,
        }
    };
    let Some(row) = row else {
        return;
    };
    commands
        .entity(chooser.list)
        .insert(ActiveDescendant(Some(row)));
    if let Ok(action) = rows.get(row) {
        activate(action, &session, &mut outbox, &mut notice, &mut commands);
    }
    close_popup(
        &mut commands,
        &mut focus,
        &mut next,
        (popup, shell),
        Mode::Normal,
    );
}

/// The chooser actions by name, for the bindings registry.
pub const ACTIONS: &[(&str, fn(&mut World) -> SystemId)] = &[
    ("choose-tab", |w| w.register_system(open_tab_chooser)),
    ("choose-workspace", |w| {
        w.register_system(open_workspace_chooser)
    }),
    ("help", |w| w.register_system(open_help)),
];

pub struct ChoosersPlugin;

impl Plugin for ChoosersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (fill_workspaces, sync_cursor)
                .chain()
                .in_set(super::ViewerSystems::Chrome)
                .before(super::chrome::size_text_nodes),
        );
    }
}
