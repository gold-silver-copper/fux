//! Presentation focus and commands (prompt 3.11): `InputFocus` names the replicated leaf that
//! shows the server's target pane; `FocusedInput<KeyboardInput>` bubbling from it is consumed by
//! one observer that either forwards bytes (`Normal`) or resolves a prefix chord through the
//! bindings table of one-shot systems; `FocusGained` on a leaf becomes `ViewerRequest::Target`
//! (the server's `Targets` follows the viewer's focus, never the reverse); `h j k l` navigate a
//! `DirectionalNavigationMap` rebuilt from the leaves' geometry after every layout; a cell
//! picking backend turns the local mouse pointer into `Pointer<Press>` events for click-to-focus
//! and tab-strip clicks.

use core::time::Duration;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemId;
use bevy_input::ButtonState;
use bevy_input::keyboard::{Key, KeyboardInput};
use bevy_input_focus::directional_navigation::{
    DirectionalNavigation, DirectionalNavigationMap, FocusableArea,
};
use bevy_input_focus::tab_navigation::{NavAction, TabNavigation};
use bevy_input_focus::{
    AcquireFocus, FocusCause, FocusGained, FocusedInput, InputFocus, InputFocusSystems,
    dispatch_focused_input,
};
use bevy_math::CompassOctant;
use bevy_picking::PickingSystems;
use bevy_picking::backend::{HitData, PointerHits};
use bevy_picking::events::{Pointer, Press};
use bevy_picking::pointer::{PointerId, PointerLocation};
use bevy_platform::collections::HashMap;
use bevy_state::prelude::*;
use bevy_time::{Time, Timer, TimerMode};
use bevy_ui::{ComputedNode, UiGlobalTransform, UiStack};
use bevy_window::{Ime, PrimaryWindow};

use super::chrome::{PendingNotice, TabEntry};
use super::keys::{self, KeyChord};
use super::paint::CellRect;
use super::replicate::{Grid, Replicated, Roots, ShowingRoot, TargetPane};
use super::{LocalCamera, Mode, Outbox, ViewerSystems, Viewport};
use crate::model::{NodeId, PaneId, Shows, SplitDirection, ViewerRequest, Zoomed};
use crate::wire::Modes;

/// Prefix-mode chords bound to one-shot systems (`world.register_system`).
#[derive(Resource)]
pub struct Bindings {
    pub prefix: KeyChord,
    map: HashMap<KeyChord, SystemId>,
}

impl Bindings {
    pub fn get(&self, chord: &KeyChord) -> Option<SystemId> {
        self.map.get(chord).copied()
    }
}

/// An exact attachment never retargets and ignores focus-changing bindings.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct ExactAttachment(pub bool);

/// A `Target` request in flight: the server's `target` is ignored until it confirms the pane or
/// the timer runs out (refused, e.g. by an exact attachment).
#[derive(Resource, Debug)]
pub struct PendingTarget {
    pane: Option<PaneId>,
    timer: Timer,
}

impl Default for PendingTarget {
    fn default() -> Self {
        Self {
            pane: None,
            timer: Timer::new(Duration::from_millis(500), TimerMode::Once),
        }
    }
}

/// The action `Confirm` mode commits on `y`.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct PendingConfirm(pub Option<ViewerRequestKind>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerRequestKind {
    ClosePane,
}

const HELP: &str = "C-b: % split right  \" split below  x close  hjkl/o move  n/p/c roots  z zoom  [ copy  d detach";

fn effective_mode(state: &State<Mode>, next: &NextState<Mode>) -> Mode {
    match next {
        NextState::Pending(m) => *m,
        _ => *state.get(),
    }
}

fn modes_of(entity: Entity, leaves: &Query<&Shows>, grids: &Query<&Grid>) -> Modes {
    leaves
        .get(entity)
        .ok()
        .and_then(|s| grids.get(s.0).ok())
        .map(|g| g.modes)
        .unwrap_or_default()
}

/// The single keyboard consumer: every `FocusedInput<KeyboardInput>` stops here.
#[allow(clippy::too_many_arguments)]
fn on_key(
    mut ev: On<FocusedInput<KeyboardInput>>,
    state: Res<State<Mode>>,
    mut next: ResMut<NextState<Mode>>,
    bindings: Res<Bindings>,
    leaves: Query<&Shows>,
    grids: Query<&Grid>,
    nodes: Query<&NodeId>,
    viewport: Res<Viewport>,
    mut outbox: ResMut<Outbox>,
    mut confirm: ResMut<PendingConfirm>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
    mut buf: Local<Vec<u8>>,
) {
    ev.propagate(false);
    if ev.input.state != ButtonState::Pressed {
        return;
    }
    let chord = KeyChord::of(&ev.input);
    let focused = ev.focused_entity;
    match effective_mode(&state, &next) {
        Mode::Normal => {
            if chord == bindings.prefix {
                next.set(Mode::Prefix);
                return;
            }
            if !leaves.contains(focused) {
                return;
            }
            buf.clear();
            if keys::encode(&ev.input, modes_of(focused, &leaves, &grids), &mut buf) {
                outbox.push(ViewerRequest::Input(buf.clone()));
            }
        }
        Mode::Prefix => {
            next.set(Mode::Normal);
            if chord == bindings.prefix {
                buf.clear();
                if keys::encode(&ev.input, Modes::default(), &mut buf) && leaves.contains(focused) {
                    outbox.push(ViewerRequest::Input(buf.clone()));
                }
                return;
            }
            match bindings.get(&chord) {
                Some(system) => commands.run_system(system),
                None => notice.0 = Some(format!("unbound: {:?}", chord.key)),
            }
        }
        Mode::Confirm => {
            next.set(Mode::Normal);
            let yes =
                matches!(&chord.key, Key::Character(c) if c.as_str() == "y" || c.as_str() == "Y");
            if let (true, Some(ViewerRequestKind::ClosePane)) = (yes, confirm.0.take()) {
                outbox.push(ViewerRequest::ClosePane);
            }
            confirm.0 = None;
        }
        Mode::CopyMode => {
            let page = i32::from(viewport.rows.saturating_sub(2).max(1));
            let rows = match &chord.key {
                Key::ArrowDown => 1,
                Key::ArrowUp => -1,
                Key::PageDown => page,
                Key::PageUp => -page,
                Key::Escape => {
                    next.set(Mode::Normal);
                    return;
                }
                Key::Character(c) => match c.as_str() {
                    "j" => 1,
                    "k" => -1,
                    "q" => {
                        next.set(Mode::Normal);
                        return;
                    }
                    _ => return,
                },
                _ => return,
            };
            if let Ok(node) = nodes.get(focused) {
                outbox.push(ViewerRequest::Scroll { node: *node, rows });
            }
        }
    }
}

/// Bracketed paste reaches the focused pane as one `Input`, wrapped if the program asked.
fn on_paste(
    mut ev: On<FocusedInput<Ime>>,
    state: Res<State<Mode>>,
    next: Res<NextState<Mode>>,
    leaves: Query<&Shows>,
    grids: Query<&Grid>,
    mut outbox: ResMut<Outbox>,
    mut buf: Local<Vec<u8>>,
) {
    ev.propagate(false);
    let Ime::Commit { value, .. } = &ev.input else {
        return;
    };
    if effective_mode(&state, &next) != Mode::Normal || !leaves.contains(ev.focused_entity) {
        return;
    }
    buf.clear();
    keys::encode_paste(
        value,
        modes_of(ev.focused_entity, &leaves, &grids),
        &mut buf,
    );
    outbox.push(ViewerRequest::Input(buf.clone()));
}

/// Focus on a leaf retargets the server (unless exact).
fn on_focus_gained(
    ev: On<FocusGained>,
    leaves: Query<&Shows, With<Replicated>>,
    panes: Query<&PaneId>,
    target: Res<TargetPane>,
    exact: Res<ExactAttachment>,
    mut pending: ResMut<PendingTarget>,
    mut outbox: ResMut<Outbox>,
) {
    let Ok(shows) = leaves.get(ev.entity) else {
        return;
    };
    let Ok(pane) = panes.get(shows.0) else {
        return;
    };
    if exact.0 || target.0 == Some(*pane) {
        return;
    }
    outbox.push(ViewerRequest::Target(*pane));
    pending.pane = Some(*pane);
    pending.timer.reset();
}

/// Keeps `InputFocus` on the leaf showing the server's target pane: when the server's target
/// changes, or when the focused leaf disappeared (re-instanced after a template edit). Local
/// navigation is left alone while its `Target` request is in flight.
fn sync_focus(
    time: Res<Time>,
    target: Res<TargetPane>,
    panes: Query<&PaneId>,
    leaves: Query<(Entity, &Shows), With<Replicated>>,
    mut focus: ResMut<InputFocus>,
    mut pending: ResMut<PendingTarget>,
) {
    pending.timer.tick(time.delta());
    if pending.pane.is_some() && (pending.pane == target.0 || pending.timer.is_finished()) {
        pending.pane = None;
    }
    let focus_on_leaf = focus.get().is_some_and(|e| leaves.contains(e));
    if focus_on_leaf && (pending.pane.is_some() || !target.is_changed()) {
        return;
    }
    let Some(target) = target.0 else {
        return;
    };
    let leaf = leaves
        .iter()
        .find(|(_, shows)| panes.get(shows.0).is_ok_and(|id| *id == target))
        .map(|(e, _)| e);
    if let Some(leaf) = leaf
        && focus.get() != Some(leaf)
    {
        focus.set(leaf, FocusCause::Navigated);
    }
}

type Leaf = (With<Shows>, With<Replicated>);
type LeafGeometryChanged = (
    With<Shows>,
    With<Replicated>,
    Or<(
        Changed<ComputedNode>,
        Changed<UiGlobalTransform>,
        Added<Shows>,
    )>,
);

/// Rebuilds the directional navigation map from leaf geometry after layout.
///
/// Edges are generated here rather than by `auto_generate_navigation_edges`: its score is the
/// rect-edge distance, which is zero for every touching pane, so a diagonal neighbour ties with
/// the aligned one. A cardinal neighbour must overlap the origin on the perpendicular axis; among
/// those the nearest edge wins, then the closest perpendicular centre.
fn rebuild_nav_map(
    leaves: Query<(Entity, &ComputedNode, &UiGlobalTransform), Leaf>,
    changed: Query<(), LeafGeometryChanged>,
    mut removed: RemovedComponents<Shows>,
    mut map: ResMut<DirectionalNavigationMap>,
    mut areas: Local<Vec<FocusableArea>>,
) {
    let any_removed = removed.read().next().is_some();
    if changed.is_empty() && !any_removed {
        return;
    }
    areas.clear();
    for (entity, node, transform) in &leaves {
        if node.size.x < 0.5 || node.size.y < 0.5 {
            continue;
        }
        areas.push(FocusableArea {
            entity,
            position: transform.translation,
            size: node.size,
        });
    }
    map.clear();
    for origin in &*areas {
        for octant in [
            CompassOctant::North,
            CompassOctant::East,
            CompassOctant::South,
            CompassOctant::West,
        ] {
            if let Some(best) = cardinal_neighbour(origin, octant, &areas) {
                map.add_edge(origin.entity, best, octant);
            }
        }
    }
}

fn cardinal_neighbour(
    origin: &FocusableArea,
    octant: CompassOctant,
    areas: &[FocusableArea],
) -> Option<Entity> {
    let (o_min, o_max) = (
        origin.position - origin.size / 2.0,
        origin.position + origin.size / 2.0,
    );
    let mut best: Option<(i32, f32, Entity)> = None;
    for candidate in areas {
        if candidate.entity == origin.entity {
            continue;
        }
        let (c_min, c_max) = (
            candidate.position - candidate.size / 2.0,
            candidate.position + candidate.size / 2.0,
        );
        // UI y grows downwards: North is smaller y.
        let (gap, overlap, centre_delta) = match octant {
            CompassOctant::East => (
                c_min.x - o_max.x,
                o_max.y.min(c_max.y) - o_min.y.max(c_min.y),
                candidate.position.y - origin.position.y,
            ),
            CompassOctant::West => (
                o_min.x - c_max.x,
                o_max.y.min(c_max.y) - o_min.y.max(c_min.y),
                candidate.position.y - origin.position.y,
            ),
            CompassOctant::South => (
                c_min.y - o_max.y,
                o_max.x.min(c_max.x) - o_min.x.max(c_min.x),
                candidate.position.x - origin.position.x,
            ),
            _ => (
                o_min.y - c_max.y,
                o_max.x.min(c_max.x) - o_min.x.max(c_min.x),
                candidate.position.x - origin.position.x,
            ),
        };
        if gap < -0.5 || overlap <= 0.5 {
            continue;
        }
        // Layout is whole cells, so gaps compare exactly as integers.
        let key = (gap.max(0.0).round() as i32, centre_delta.abs());
        if best.is_none_or(|(g, d, _)| key.0 < g || (key.0 == g && key.1 < d)) {
            best = Some((key.0, key.1, candidate.entity));
        }
    }
    best.map(|(_, _, e)| e)
}

/// Cell picking backend: the mouse pointer hits every node whose rect contains its cell; the
/// stack index is the depth so the topmost node wins.
fn cell_backend(
    pointers: Query<(&PointerId, &PointerLocation)>,
    stack: Res<UiStack>,
    nodes: Query<(&ComputedNode, &UiGlobalTransform)>,
    camera: Res<LocalCamera>,
    mut hits: MessageWriter<PointerHits>,
) {
    for (id, location) in &pointers {
        let Some(location) = location.location() else {
            continue;
        };
        let pos = location.position;
        let (col, row) = (pos.x.floor() as i32, pos.y.floor() as i32);
        let picks: Vec<(Entity, HitData)> = stack
            .uinodes
            .iter()
            .enumerate()
            .filter_map(|(index, &entity)| {
                let (node, transform) = nodes.get(entity).ok()?;
                CellRect::from_node(node, transform)
                    .contains(col, row)
                    .then(|| {
                        (
                            entity,
                            HitData {
                                camera: camera.0,
                                depth: -(index as f32),
                                position: Some(pos.extend(0.0)),
                                normal: None,
                                extra: None,
                            },
                        )
                    })
            })
            .collect();
        if !picks.is_empty() {
            hits.write(PointerHits::new(*id, picks, 0.5));
        }
    }
}

/// Click-to-focus and tab-strip clicks.
fn on_press(
    mut press: On<Pointer<Press>>,
    tabs: Query<&TabEntry>,
    replicated: Query<(), With<Replicated>>,
    windows: Query<Entity, With<PrimaryWindow>>,
    mut outbox: ResMut<Outbox>,
    mut commands: Commands,
) {
    if press.entity != press.original_event_target() {
        return;
    }
    if let Ok(tab) = tabs.get(press.entity) {
        press.propagate(false);
        outbox.push(ViewerRequest::Show(tab.0));
        return;
    }
    if replicated.contains(press.entity)
        && let Ok(window) = windows.single()
    {
        press.propagate(false);
        commands.trigger(AcquireFocus {
            focused_entity: press.entity,
            window,
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Bound one-shot systems
// ---------------------------------------------------------------------------------------------

fn split_right(mut outbox: ResMut<Outbox>) {
    outbox.push(ViewerRequest::Split {
        direction: SplitDirection::Right,
        template: None,
    });
}

fn split_below(mut outbox: ResMut<Outbox>) {
    outbox.push(ViewerRequest::Split {
        direction: SplitDirection::Below,
        template: None,
    });
}

fn confirm_close(mut next: ResMut<NextState<Mode>>, mut confirm: ResMut<PendingConfirm>) {
    confirm.0 = Some(ViewerRequestKind::ClosePane);
    next.set(Mode::Confirm);
}

fn navigate(nav: &mut DirectionalNavigation, exact: &ExactAttachment, direction: CompassOctant) {
    if exact.0 {
        return;
    }
    // No neighbour in that direction is not an error worth reporting.
    let _ = nav.navigate(direction);
}

fn nav_west(mut nav: DirectionalNavigation, exact: Res<ExactAttachment>) {
    navigate(&mut nav, &exact, CompassOctant::West);
}

fn nav_south(mut nav: DirectionalNavigation, exact: Res<ExactAttachment>) {
    navigate(&mut nav, &exact, CompassOctant::South);
}

fn nav_north(mut nav: DirectionalNavigation, exact: Res<ExactAttachment>) {
    navigate(&mut nav, &exact, CompassOctant::North);
}

fn nav_east(mut nav: DirectionalNavigation, exact: Res<ExactAttachment>) {
    navigate(&mut nav, &exact, CompassOctant::East);
}

fn next_pane(nav: TabNavigation, mut focus: ResMut<InputFocus>, exact: Res<ExactAttachment>) {
    if exact.0 {
        return;
    }
    if let Ok(next) = nav.navigate(&focus, NavAction::Next) {
        focus.set(next, FocusCause::Navigated);
    }
}

fn show_root(roots: &Roots, showing: &ShowingRoot, outbox: &mut Outbox, offset: isize) {
    if roots.0.is_empty() {
        return;
    }
    let len = roots.0.len() as isize;
    let current = showing
        .0
        .and_then(|node| roots.0.iter().position(|r| r.node == node))
        .map_or(0, |i| i as isize);
    let index = (current + offset).rem_euclid(len) as usize;
    if let Some(root) = roots.0.get(index) {
        outbox.push(ViewerRequest::Show(root.node));
    }
}

fn next_root(roots: Res<Roots>, showing: Res<ShowingRoot>, mut outbox: ResMut<Outbox>) {
    show_root(&roots, &showing, &mut outbox, 1);
}

fn prev_root(roots: Res<Roots>, showing: Res<ShowingRoot>, mut outbox: ResMut<Outbox>) {
    show_root(&roots, &showing, &mut outbox, -1);
}

fn new_root(mut outbox: ResMut<Outbox>) {
    outbox.push(ViewerRequest::NewRoot { template: None });
}

fn detach(mut outbox: ResMut<Outbox>) {
    outbox.push(ViewerRequest::Detach);
}

fn toggle_zoom(
    focus: Res<InputFocus>,
    zoomed: Query<(), (With<Zoomed>, With<Replicated>)>,
    nodes: Query<&NodeId, With<Replicated>>,
    mut outbox: ResMut<Outbox>,
) {
    if !zoomed.is_empty() {
        outbox.push(ViewerRequest::Unzoom);
        return;
    }
    if let Some(node) = focus.get().and_then(|e| nodes.get(e).ok()) {
        outbox.push(ViewerRequest::Zoom(*node));
    }
}

fn copy_mode(mut next: ResMut<NextState<Mode>>) {
    next.set(Mode::CopyMode);
}

fn help(mut notice: ResMut<PendingNotice>) {
    notice.0 = Some(HELP.into());
}

/// Registers the one-shot systems and the chord table.
pub fn register_bindings(world: &mut World) {
    let mut map = HashMap::default();
    let mut bind = |chord: KeyChord, id: SystemId| {
        map.insert(chord, id);
    };
    bind(KeyChord::character('%'), world.register_system(split_right));
    bind(KeyChord::character('"'), world.register_system(split_below));
    bind(
        KeyChord::character('x'),
        world.register_system(confirm_close),
    );
    bind(KeyChord::character('h'), world.register_system(nav_west));
    bind(KeyChord::character('j'), world.register_system(nav_south));
    bind(KeyChord::character('k'), world.register_system(nav_north));
    bind(KeyChord::character('l'), world.register_system(nav_east));
    let next = world.register_system(next_pane);
    bind(KeyChord::character('o'), next);
    bind(KeyChord::plain(Key::Tab), next);
    bind(KeyChord::character('n'), world.register_system(next_root));
    bind(KeyChord::character('p'), world.register_system(prev_root));
    bind(KeyChord::character('c'), world.register_system(new_root));
    bind(KeyChord::character('d'), world.register_system(detach));
    bind(KeyChord::character('z'), world.register_system(toggle_zoom));
    bind(KeyChord::character('['), world.register_system(copy_mode));
    bind(KeyChord::character('?'), world.register_system(help));
    world.insert_resource(Bindings {
        prefix: KeyChord::ctrl('b'),
        map,
    });
}

pub struct FocusPlugin;

impl Plugin for FocusPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingTarget>()
            .init_resource::<PendingConfirm>()
            .init_resource::<ExactAttachment>()
            .add_message::<Ime>()
            .add_systems(
                PreUpdate,
                (
                    dispatch_focused_input::<Ime>.in_set(InputFocusSystems::Dispatch),
                    cell_backend.in_set(PickingSystems::Backend),
                ),
            )
            .add_systems(Update, sync_focus.in_set(ViewerSystems::Focus))
            .add_systems(
                PostUpdate,
                rebuild_nav_map
                    .after(bevy_ui::UiSystems::PostLayout)
                    .before(InputFocusSystems::FocusChangeEvents),
            )
            .add_observer(on_key)
            .add_observer(on_paste)
            .add_observer(on_focus_gained)
            .add_observer(on_press);
        register_bindings(app.world_mut());
    }
}
