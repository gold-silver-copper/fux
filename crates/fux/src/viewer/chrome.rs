//! Viewer-local chrome (prompt 3.11): a column under the local camera holding the pane area
//! (where the replicated root is parented), a one-row status bar with the tab strip (projection
//! of the server's `roots`), the mode indicator and the target pane's title, plus a transient
//! notice overlay whose lifetime is a `bevy_time` `Timer`. Colours come from the theme: every
//! chrome node names a [`ThemeToken`] and the resolve pass writes its `CellStyle`.
//!
//! Popups (choosers, prompts, confirmations) share two idioms from `bevy_ui_widgets`: the
//! [`Popup`] shell takes presentation focus and closes when focus leaves its subtree
//! (`menu.rs`'s `MenuPopup` dismissal), and [`Popover`] places it beside an anchor node by
//! scoring candidate placements against the viewport rect (`popover.rs`).

use core::time::Duration;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_input_focus::tab_navigation::TabGroup;
use bevy_input_focus::{FocusCause, FocusLost, InputFocus, IsFocused, IsFocusedHelper};
use bevy_math::IVec2;
use bevy_state::prelude::*;
use bevy_time::{Time, Timer, TimerMode};
use bevy_ui::interaction_states::Selected;
use bevy_ui::prelude::*;
use bevy_ui::{ComputedNode, UiGlobalTransform, UiTargetCamera, ZIndex};

use super::paint::CellRect;
use super::replicate::{Grid, Roots, Session, ShowingRoot, TargetPane};
use super::{LocalCamera, Mode, Viewport, WakeDeadline};
use crate::assets::ThemeToken;
use crate::model::{Ids, NodeId};
use crate::wire::ProcessSummary;

/// Marks viewer-local nodes.
#[derive(Component, Debug, Default)]
pub struct Chrome;

/// Text content painted into a chrome node (`bevy_text` is unused); styled by the node's
/// `CellStyle`.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct Text(pub String);

/// A tab strip entry for one template root.
#[derive(Component, Debug, Clone, Copy)]
pub struct TabEntry(pub NodeId);

/// The status bar's mode label.
#[derive(Component, Debug)]
pub struct ModeIndicator;

/// The status bar's title label.
#[derive(Component, Debug)]
pub struct StatusTitle;

/// A notice overlay; despawned when the timer finishes.
#[derive(Component, Debug)]
pub struct NoticeOverlay {
    timer: Timer,
}

/// A viewer-local popup over the chrome root: it holds presentation focus while open and is
/// dismissed when focus leaves it ([`spawn_popup`]). `returns_to` is what had focus when it
/// opened (the `FocusRoot` half of the menu idiom): a keyboard close hands focus straight back
/// so the rest of the same key batch reaches the pane.
#[derive(Component, Debug)]
pub struct Popup {
    pub returns_to: Option<Entity>,
}

/// Which side of the anchor a popover goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// Alignment along the anchor's edge perpendicular to the side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub side: Side,
    pub align: Align,
}

impl Placement {
    pub const fn new(side: Side, align: Align) -> Self {
        Self { side, align }
    }
}

/// Places its node beside `anchor`: the first candidate that fits the viewport wins, otherwise
/// the least occluded one, then the rect is shifted into the viewport (a cell grid has no room
/// to spare). The node's own `width`/`height` in cells must be set, as text nodes do.
#[derive(Component, Debug)]
pub struct Popover {
    pub anchor: Entity,
    pub placements: &'static [Placement],
}

/// Stacking order of popups, above notices.
pub const POPUP_Z: i32 = 20;

/// The chrome entities other systems address.
#[derive(Resource, Debug, Clone, Copy)]
pub struct ChromeRoots {
    pub root: Entity,
    pub pane_area: Entity,
    pub status_bar: Entity,
    pub tab_strip: Entity,
    /// The status bar's mode label: the anchor of confirmation popovers.
    pub mode_indicator: Entity,
}

/// A notice waiting to be shown (from the server or a local binding).
#[derive(Resource, Default, Debug)]
pub struct PendingNotice(pub Option<String>);

const NOTICE_TTL: Duration = Duration::from_secs(3);

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
            ThemeToken::TAB_ACTIVE,
            Node {
                width: Val::Px(0.0),
                flex_shrink: 0.0,
                ..bar_row()
            },
            Text::default(),
        ))
        .id();
    let title = world
        .spawn((
            Chrome,
            StatusTitle,
            ThemeToken::BAR,
            Node {
                width: Val::Px(0.0),
                flex_shrink: 1.0,
                overflow: Overflow::clip(),
                ..bar_row()
            },
            Text::default(),
        ))
        .id();
    let status_bar = world
        .spawn((
            Chrome,
            Name::new("status-bar"),
            ThemeToken::BAR_BACKGROUND,
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(1.0),
                ..bar_row()
            },
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
        mode_indicator: mode,
    }
}

/// Text nodes are as wide as their content.
pub(super) fn size_text_nodes(mut nodes: Query<(&Text, &mut Node), Changed<Text>>) {
    for (text, mut node) in &mut nodes {
        let width = Val::Px(text.0.chars().count() as f32);
        if node.width != width {
            node.width = width;
        }
    }
}

/// Spawns a popup shell as a child of the chrome root, gives it focus and installs the
/// focus-loss dismissal observer; `bundle` is the popup's own content.
pub fn spawn_popup(
    commands: &mut Commands,
    chrome: &ChromeRoots,
    focus: &mut InputFocus,
    bundle: impl Bundle,
) -> Entity {
    let returns_to = focus.get();
    let popup = commands
        .spawn((
            Chrome,
            Popup { returns_to },
            ChildOf(chrome.root),
            ZIndex(POPUP_Z),
            bundle,
        ))
        .observe(on_popup_focus_lost)
        .id();
    focus.set(popup, FocusCause::Navigated);
    popup
}

/// Closes a popup from a key: despawns it, returns focus to the opener (or releases it, and
/// the target pane takes it back through `sync_focus`) and enters `mode`.
pub fn close_popup(
    commands: &mut Commands,
    focus: &mut InputFocus,
    next: &mut NextState<Mode>,
    (popup, shell): (Entity, &Popup),
    mode: Mode,
) {
    commands.entity(popup).despawn();
    if focus.get() == Some(popup) {
        match shell.returns_to {
            Some(previous) => focus.set(previous, FocusCause::Navigated),
            None => focus.clear(),
        }
    }
    next.set(mode);
}

/// The `MenuPopup` idiom: a popup that no longer contains the focus closes itself.
fn on_popup_focus_lost(
    ev: On<FocusLost>,
    popups: Query<(), With<Popup>>,
    focused: IsFocusedHelper,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let popup = ev.entity;
    if !popups.contains(popup) || focused.is_focus_within(popup) {
        return;
    }
    commands.entity(popup).despawn();
    next.set(Mode::Normal);
}

fn px(val: Val) -> i32 {
    match val {
        Val::Px(v) => v.round() as i32,
        _ => 0,
    }
}

/// Places popovers when they appear or their anchor moves (`bevy_ui_widgets::popover`'s
/// occlusion scoring against the viewport rect).
fn position_popovers(
    viewport: Res<Viewport>,
    anchors: Query<(&ComputedNode, &UiGlobalTransform)>,
    moved: Query<(), Or<(Changed<ComputedNode>, Changed<UiGlobalTransform>)>>,
    mut popovers: Query<(Ref<Popover>, &mut Node)>,
) {
    let window = CellRect {
        min: IVec2::ZERO,
        max: IVec2::new(i32::from(viewport.cols), i32::from(viewport.rows)),
    };
    for (popover, mut node) in &mut popovers {
        if !popover.is_added()
            && !node.is_changed()
            && !moved.contains(popover.anchor)
            && !viewport.is_changed()
        {
            continue;
        }
        let Ok((anchor_node, anchor_transform)) = anchors.get(popover.anchor) else {
            continue;
        };
        let anchor = CellRect::from_node(anchor_node, anchor_transform);
        let size = IVec2::new(px(node.width).max(1), px(node.height).max(1));
        let mut best: Option<(i32, IVec2)> = None;
        for placement in popover.placements {
            let mut min = IVec2::ZERO;
            match placement.side {
                Side::Top => min.y = anchor.min.y - size.y,
                Side::Bottom => min.y = anchor.max.y,
                Side::Left => min.x = anchor.min.x - size.x,
                Side::Right => min.x = anchor.max.x,
            }
            let horizontal = matches!(placement.side, Side::Top | Side::Bottom);
            let (anchor_len, len) = if horizontal {
                (anchor.width(), size.x)
            } else {
                (anchor.height(), size.y)
            };
            let offset = match placement.align {
                Align::Start => 0,
                Align::Center => (anchor_len - len) / 2,
                Align::End => anchor_len - len,
            };
            if horizontal {
                min.x = anchor.min.x + offset;
            } else {
                min.y = anchor.min.y + offset;
            }
            let rect = CellRect {
                min,
                max: min + size,
            };
            let clipped = rect.intersect(window);
            let occlusion = size.x * size.y - (clipped.width().max(0) * clipped.height().max(0));
            if best.is_none_or(|(o, _)| occlusion < o) {
                best = Some((occlusion, min));
            }
            if occlusion == 0 {
                break;
            }
        }
        let Some((_, mut min)) = best else {
            continue;
        };
        min.x = min.x.min(window.max.x - size.x).max(0);
        min.y = min.y.min(window.max.y - size.y).max(0);
        let (left, top) = (Val::Px(min.x as f32), Val::Px(min.y as f32));
        if node.position_type != PositionType::Absolute {
            node.position_type = PositionType::Absolute;
        }
        if node.left != left {
            node.left = left;
        }
        if node.top != top {
            node.top = top;
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
            if active {
                ThemeToken::TAB_ACTIVE
            } else {
                ThemeToken::TAB
            },
            Node {
                flex_shrink: 0.0,
                margin: UiRect::right(Val::Px(1.0)),
                ..bar_row()
            },
            Text(format!("{}:{}", index + 1, root.name)),
        ));
        if active {
            entry.insert(Selected);
        }
    }
}

/// Mode indicator in the status bar.
pub(super) fn sync_mode(mode: Res<State<Mode>>, mut texts: Query<&mut Text, With<ModeIndicator>>) {
    if !mode.is_changed() {
        return;
    }
    let Ok(mut text) = texts.single_mut() else {
        return;
    };
    let label = match mode.get() {
        Mode::Normal => "",
        Mode::Prefix => " PREFIX ",
        Mode::Confirm => " CONFIRM y/n ",
        Mode::CopyMode => " COPY ",
        Mode::Chooser => " CHOOSE ",
        Mode::Prompt => " PROMPT ",
    };
    if text.0 != label {
        text.0.clear();
        text.0.push_str(label);
    }
}

/// Title: workspace, target pane and its title and process state.
fn sync_title(
    target: Res<TargetPane>,
    session: Res<Session>,
    ids: Res<Ids>,
    grids: Query<&Grid>,
    changed_grids: Query<Entity, Changed<Grid>>,
    mut texts: Query<&mut Text, With<StatusTitle>>,
) {
    let pane = target.0.and_then(|id| ids.pane(id));
    let grid_changed = pane.is_some_and(|p| changed_grids.contains(p));
    if !target.is_changed() && !session.is_changed() && !grid_changed {
        return;
    }
    let Ok(mut text) = texts.single_mut() else {
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
    if text.0 != title {
        text.0 = title;
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
            ThemeToken::NOTICE,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(0.0),
                bottom: Val::Px(1.0),
                height: Val::Px(1.0),
                ..Default::default()
            },
            ZIndex(10),
            Text(format!(" {text} ")),
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
                position_popovers,
            )
                .chain()
                .in_set(super::ViewerSystems::Chrome),
        );
    }
}
