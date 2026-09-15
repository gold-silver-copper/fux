//! Viewer-local chrome (prompt 3.11): a column under the local camera holding the pane area
//! (where the replicated root is parented), a one-row status bar with the tab strip (projection
//! of the server's `roots`), the mode indicator and the target pane's title, plus a transient
//! notice overlay whose lifetime is a `bevy_time` `Timer`.

use core::time::Duration;

use bevy_app::prelude::*;
use bevy_color::Color;
use bevy_ecs::prelude::*;
use bevy_input_focus::tab_navigation::TabGroup;
use bevy_state::prelude::*;
use bevy_time::{Time, Timer, TimerMode};
use bevy_ui::interaction_states::Selected;
use bevy_ui::prelude::*;
use bevy_ui::{UiTargetCamera, ZIndex};

use super::replicate::{Grid, Roots, Session, ShowingRoot, TargetPane};
use super::{LocalCamera, Mode, WakeDeadline};
use crate::model::{Ids, NodeId};
use crate::wire::{Color as WireColor, ProcessSummary, Style};

/// Marks viewer-local nodes.
#[derive(Component, Debug, Default)]
pub struct Chrome;

/// Text content painted into a chrome node (`bevy_text` is unused).
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    pub style: Style,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }
}

/// A tab strip entry for one template root.
#[derive(Component, Debug, Clone, Copy)]
pub struct TabEntry(pub NodeId);

#[derive(Component, Debug)]
pub struct ModeIndicator;

#[derive(Component, Debug)]
pub struct StatusTitle;

/// A notice overlay; despawned when the timer finishes.
#[derive(Component, Debug)]
pub struct NoticeOverlay {
    timer: Timer,
}

/// The chrome entities other systems address.
#[derive(Resource, Debug, Clone, Copy)]
pub struct ChromeRoots {
    pub root: Entity,
    pub pane_area: Entity,
    pub status_bar: Entity,
    pub tab_strip: Entity,
    pub mode: Entity,
    pub title: Entity,
}

/// A notice waiting to be shown (from the server or a local binding).
#[derive(Resource, Default, Debug)]
pub struct PendingNotice(pub Option<String>);

const NOTICE_TTL: Duration = Duration::from_secs(3);
const BAR_BG: Color = Color::srgb_u8(40, 40, 48);
const ACCENT: Color = Color::srgb_u8(32, 110, 201);
const NOTICE_BG: Color = Color::srgb_u8(217, 178, 51);

const TEXT_STYLE: Style = Style {
    fg: WireColor::Rgb(220, 220, 220),
    bg: WireColor::Default,
    attrs: 0,
};
const DIM_STYLE: Style = Style {
    fg: WireColor::Rgb(150, 150, 160),
    bg: WireColor::Default,
    attrs: 0,
};
const SELECTED_STYLE: Style = Style {
    fg: WireColor::Rgb(255, 255, 255),
    bg: WireColor::Rgb(32, 110, 201),
    attrs: Style::BOLD,
};
const NOTICE_STYLE: Style = Style {
    fg: WireColor::Rgb(20, 20, 20),
    bg: WireColor::Rgb(217, 178, 51),
    attrs: Style::BOLD,
};

fn bar_row() -> Node {
    Node {
        height: Val::Px(1.0),
        flex_shrink: 0.0,
        ..Default::default()
    }
}

/// Spawns the chrome tree; called by the viewer App builder once the camera exists.
pub fn spawn_chrome(world: &mut World) -> ChromeRoots {
    let camera = world.resource::<LocalCamera>().0;
    let pane_area = world
        .spawn((
            Chrome,
            Name::new("pane-area"),
            Node {
                width: Val::Percent(100.0),
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_height: Val::Px(0.0),
                ..Default::default()
            },
            TabGroup::new(0),
        ))
        .id();
    let tab_strip = world
        .spawn((
            Chrome,
            Name::new("tab-strip"),
            Node {
                flex_direction: FlexDirection::Row,
                flex_shrink: 0.0,
                ..bar_row()
            },
        ))
        .id();
    let spacer = world
        .spawn((
            Chrome,
            Node {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_width: Val::Px(0.0),
                ..bar_row()
            },
        ))
        .id();
    let mode = world
        .spawn((
            Chrome,
            ModeIndicator,
            Node {
                width: Val::Px(0.0),
                flex_shrink: 0.0,
                ..bar_row()
            },
            Text {
                text: String::new(),
                style: SELECTED_STYLE,
            },
        ))
        .id();
    let title = world
        .spawn((
            Chrome,
            StatusTitle,
            Node {
                width: Val::Px(0.0),
                flex_shrink: 1.0,
                overflow: Overflow::clip(),
                ..bar_row()
            },
            Text {
                text: String::new(),
                style: TEXT_STYLE,
            },
        ))
        .id();
    let status_bar = world
        .spawn((
            Chrome,
            Name::new("status-bar"),
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(1.0),
                ..bar_row()
            },
            BackgroundColor(BAR_BG),
        ))
        .add_children(&[tab_strip, spacer, mode, title])
        .id();
    let root = world
        .spawn((
            Chrome,
            Name::new("viewer-root"),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                ..Default::default()
            },
            UiTargetCamera(camera),
        ))
        .add_children(&[pane_area, status_bar])
        .id();
    ChromeRoots {
        root,
        pane_area,
        status_bar,
        tab_strip,
        mode,
        title,
    }
}

/// Text nodes are as wide as their content.
fn size_text_nodes(mut nodes: Query<(&Text, &mut Node), Changed<Text>>) {
    for (text, mut node) in &mut nodes {
        let width = Val::Px(text.text.chars().count() as f32);
        if node.width != width {
            node.width = width;
        }
    }
}

/// Rebuilds the tab strip when the roots or the shown root change.
fn sync_tab_strip(
    roots: Res<Roots>,
    showing: Res<ShowingRoot>,
    chrome: Res<ChromeRoots>,
    entries: Query<Entity, With<TabEntry>>,
    mut commands: Commands,
) {
    if !roots.is_changed() && !showing.is_changed() {
        return;
    }
    for entry in &entries {
        commands.entity(entry).despawn();
    }
    for (index, root) in roots.0.iter().enumerate() {
        let active = showing.0 == Some(root.node);
        let mut entry = commands.spawn((
            Chrome,
            TabEntry(root.node),
            ChildOf(chrome.tab_strip),
            Node {
                flex_shrink: 0.0,
                margin: UiRect::right(Val::Px(1.0)),
                ..bar_row()
            },
            Text {
                text: format!("{}:{}", index + 1, root.name),
                style: if active { SELECTED_STYLE } else { DIM_STYLE },
            },
        ));
        if active {
            entry.insert((Selected, BackgroundColor(ACCENT)));
        }
    }
}

/// Mode indicator in the status bar.
fn sync_mode(mode: Res<State<Mode>>, chrome: Res<ChromeRoots>, mut texts: Query<&mut Text>) {
    if !mode.is_changed() {
        return;
    }
    let Ok(mut text) = texts.get_mut(chrome.mode) else {
        return;
    };
    let label = match mode.get() {
        Mode::Normal => "",
        Mode::Prefix => " PREFIX ",
        Mode::Confirm => " close pane? y/n ",
        Mode::CopyMode => " COPY j/k/q ",
    };
    if text.text != label {
        text.text.clear();
        text.text.push_str(label);
    }
}

/// Title: workspace, target pane and its title and process state.
fn sync_title(
    target: Res<TargetPane>,
    session: Res<Session>,
    ids: Res<Ids>,
    grids: Query<&Grid>,
    changed_grids: Query<Entity, Changed<Grid>>,
    chrome: Res<ChromeRoots>,
    mut texts: Query<&mut Text>,
) {
    let pane = target.0.and_then(|id| ids.pane(id));
    let grid_changed = pane.is_some_and(|p| changed_grids.contains(p));
    if !target.is_changed() && !session.is_changed() && !grid_changed {
        return;
    }
    let Ok(mut text) = texts.get_mut(chrome.title) else {
        return;
    };
    let grid = pane.and_then(|p| grids.get(p).ok());
    let mut title = String::new();
    if let Some(welcome) = &session.0 {
        title.push_str(&welcome.workspace);
    }
    if let Some(id) = target.0 {
        use core::fmt::Write as _;
        // `String::write_fmt` cannot fail.
        let _ = write!(title, " · pane {id}");
        match grid.map(|g| g.process) {
            Some(ProcessSummary::Live) => {}
            Some(ProcessSummary::Starting) | None => title.push_str(" (starting)"),
            Some(ProcessSummary::Exited { code }) => {
                let _ = write!(title, " (exited {code})");
            }
        }
        if let Some(t) = grid.and_then(|g| g.title.as_deref())
            && !t.is_empty()
        {
            title.push_str(" · ");
            title.push_str(t);
        }
    }
    if text.text != title {
        text.text = title;
    }
}

/// Shows pending notices as an overlay above the status bar and expires them.
fn notices(
    time: Res<Time>,
    mut pending: ResMut<PendingNotice>,
    chrome: Res<ChromeRoots>,
    mut overlays: Query<(Entity, &mut NoticeOverlay)>,
    mut deadline: ResMut<WakeDeadline>,
    mut commands: Commands,
) {
    if let Some(text) = pending.0.take() {
        for (entity, _) in &overlays {
            commands.entity(entity).despawn();
        }
        commands.spawn((
            Chrome,
            NoticeOverlay {
                timer: Timer::new(NOTICE_TTL, TimerMode::Once),
            },
            ChildOf(chrome.root),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(0.0),
                bottom: Val::Px(1.0),
                height: Val::Px(1.0),
                ..Default::default()
            },
            ZIndex(10),
            BackgroundColor(NOTICE_BG),
            Text {
                text: format!(" {text} "),
                style: NOTICE_STYLE,
            },
        ));
        deadline.0 = Some(NOTICE_TTL);
        return;
    }
    let mut next: Option<Duration> = None;
    for (entity, mut overlay) in &mut overlays {
        overlay.timer.tick(time.delta());
        if overlay.timer.is_finished() {
            commands.entity(entity).despawn();
        } else {
            let remaining = overlay.timer.remaining();
            next = Some(next.map_or(remaining, |d| d.min(remaining)));
        }
    }
    deadline.0 = next;
}

pub struct ChromePlugin;

impl Plugin for ChromePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingNotice>().add_systems(
            Update,
            (
                sync_tab_strip,
                sync_mode,
                sync_title,
                notices,
                size_text_nodes,
            )
                .chain()
                .in_set(super::ViewerSystems::Chrome),
        );
    }
}
