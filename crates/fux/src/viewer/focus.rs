//! Presentation focus and commands (prompt 3.11): `InputFocus` names the replicated leaf that
//! shows the server's target pane; `FocusedInput<KeyboardInput>` bubbling from it is consumed by
//! one observer that either forwards bytes (`Normal`) or resolves a prefix chord through the
//! bindings table of one-shot systems; `FocusGained` on a leaf becomes `ViewerRequest::Target`
//! (the server's `Targets` follows the viewer's focus, never the reverse); `h j k l` navigate a
//! `DirectionalNavigationMap` rebuilt from the leaves' geometry after every layout with the
//! server's `AutoNavigationConfig`, so a local move and a server-side retarget agree; a cell
//! picking backend turns the local mouse pointer into `Pointer<Press>` events for click-to-focus
//! and tab-strip clicks.
//!
//! A surface leaf (`Surface` replicated, no `Shows`) is a focus target like a pane leaf: it
//! sits in the navigation map, the ring and the tab order, but focusing it retargets nothing
//! (it shows no pane) and keys typed on it leave as `ViewerRequest::SurfaceKey` for the
//! surface's provider, never as pane `Input`.
//!
//! `TabNavigationPlugin` stays registered for its `AcquireFocus` observer (click-to-focus), but
//! its window-level Tab handler is shadowed on purpose: `on_key` stops every keyboard event
//! because the terminal never reports Shift as a key (`Shift-Tab` is one `CSI Z` event, so the
//! handler's `ButtonInput<KeyCode>` test could never see it) and because Tab is a prefix chord
//! here, not a bare key. Next/Previous run through `next_pane`/`prev_pane` instead, with the
//! plugin's `NoTabGroupForCurrentFocus` recovery.

use core::time::Duration;
use std::collections::VecDeque;

use bevy_app::prelude::*;
use bevy_asset::{AssetEvent, AssetEventSystems, AssetServer, Assets, Handle};
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemId;
use bevy_input::ButtonState;
use bevy_input::keyboard::KeyboardInput;
use bevy_input_focus::directional_navigation::{
    AutoNavigationConfig, DirectionalNavigation, DirectionalNavigationMap, FocusableArea,
    auto_generate_navigation_edges,
};
use bevy_input_focus::tab_navigation::{NavAction, TabNavigation, TabNavigationError};
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
use bevy_ui::{ComputedNode, Node, OverrideClip, UiGlobalTransform, UiStack, clip_check_recursive};
use bevy_window::{Ime, PrimaryWindow};

use super::chrome::{ChromeRoots, PendingNotice, TabEntry};
use super::keys::{self, KeyChord};
use super::paint::CellRect;
use super::replicate::{Grid, Replicated, Roots, ShowingRoot, TargetPane};
use super::{LocalCamera, Modal, Mode, Outbox, ViewerSystems, choosers, copy_mode, prompts};
use crate::assets::Keybindings;
use crate::model::{NodeId, PaneId, Shows, SplitDirection, Surface, ViewerRequest, Zoomed};
use crate::wire::Modes;

/// A focusable replicated leaf: a pane leaf or a surface leaf.
type Leaf = (With<Replicated>, Or<(With<Shows>, With<Surface>)>);

/// Prefix-mode chords bound to one-shot systems (`world.register_system`): the loaded
/// [`Keybindings`] resolved through the [`Actions`] registry. Rebuilt on every reload.
#[derive(Resource)]
pub struct Bindings {
    pub table: Keybindings,
    map: HashMap<KeyChord, SystemId>,
}

impl Bindings {
    pub fn prefix(&self) -> &KeyChord {
        &self.table.prefix
    }

    pub fn get(&self, chord: &KeyChord) -> Option<SystemId> {
        self.map.get(chord).copied()
    }

    fn build(table: &Keybindings, actions: &Actions) -> Self {
        let map = table
            .bindings
            .iter()
            .filter_map(|(chord, name)| Some((chord.clone(), actions.get(name)?)))
            .collect();
        Self {
            table: table.clone(),
            map,
        }
    }
}

/// Built-in action name → registered one-shot system.
#[derive(Resource)]
pub struct Actions(HashMap<&'static str, SystemId>);

impl Actions {
    pub fn get(&self, name: &str) -> Option<SystemId> {
        self.0.get(name).copied()
    }
}

/// Present on an exact attachment: it never retargets and ignores focus-changing bindings.
#[derive(Resource, Debug, Clone, Copy)]
pub struct ExactAttachment;

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

/// Leaves that held focus, most recent last, bounded (`prefix ;` returns to the previous one).
#[derive(Resource, Debug, Default)]
pub struct FocusRing(VecDeque<NodeId>);

impl FocusRing {
    pub const CAPACITY: usize = 8;

    pub fn entries(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.0.iter().copied()
    }

    /// Forgets every leaf (another workspace's ids mean nothing here).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    fn push(&mut self, node: NodeId) {
        if self.0.back() == Some(&node) {
            return;
        }
        self.0.retain(|n| *n != node);
        if self.0.len() == Self::CAPACITY {
            self.0.pop_front();
        }
        self.0.push_back(node);
    }
}

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

/// The single keyboard consumer: every `FocusedInput<KeyboardInput>` stops here and is routed by
/// mode; the modal modes run their handler as a one-shot system for the chord, which applies
/// before the next key is dispatched. The pane is the live `InputFocus`, not the event's
/// `focused_entity`: dispatch reads focus once per update, and a popup closed earlier in the
/// same batch has already handed focus back.
#[allow(clippy::too_many_arguments)]
fn on_key(
    mut ev: On<FocusedInput<KeyboardInput>>,
    state: Res<State<Mode>>,
    mut next: ResMut<NextState<Mode>>,
    bindings: Res<Bindings>,
    focus: Res<InputFocus>,
    leaves: Query<&Shows>,
    surfaces: Query<&NodeId, (With<Surface>, With<Replicated>)>,
    grids: Query<&Grid>,
    painted: Res<super::replicate::InputRevision>,
    mut outbox: ResMut<Outbox>,
    mut notice: ResMut<PendingNotice>,
    mut commands: Commands,
    mut buf: Local<Vec<u8>>,
) {
    ev.propagate(false);
    if ev.input.state != ButtonState::Pressed {
        return;
    }
    let chord = KeyChord::of(&ev.input);
    let focused = focus.get().unwrap_or(ev.focused_entity);
    match effective_mode(&state, &next) {
        Mode::Normal => {
            if chord == *bindings.prefix() {
                next.set(Mode::Prefix);
                return;
            }
            if let Ok(node) = surfaces.get(focused) {
                buf.clear();
                if keys::encode(&ev.input, Modes::default(), &mut buf) {
                    outbox.push(ViewerRequest::SurfaceKey {
                        revision: painted.0,
                        node: *node,
                        bytes: buf.clone(),
                    });
                }
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
            match bindings.get(&chord) {
                Some(system) => commands.run_system(system),
                None => {
                    if let Some(action) = bindings.table.bindings.get(&chord)
                        && crate::assets::plugin_action(action).is_some()
                    {
                        commands.run_system_cached_with(run_plugin_action, action.to_string());
                    } else {
                        notice.0 = Some(format!("unbound: {chord}"));
                    }
                }
            }
        }
        Mode::Confirm => commands.run_system_cached_with(prompts::handle_confirm_key, chord),
        Mode::CopyMode => commands.run_system_cached_with(copy_mode::handle_key, chord),
        Mode::Chooser => commands.run_system_cached_with(choosers::handle_key, chord),
        Mode::Prompt => commands.run_system_cached_with(prompts::handle_key, chord),
    }
}

fn run_plugin_action(
    In(action): In<String>,
    brp: Res<super::Brp>,
    session: Res<super::replicate::Session>,
    mut notice: ResMut<PendingNotice>,
) {
    let Some((name, id)) = crate::assets::plugin_action(&action) else { return; };
    let Some(session) = session.0.as_ref() else { return; };
    if brp.path().as_os_str().is_empty() {
        notice.0 = Some("plugin action requires a connected viewer".into());
        return;
    }
    let path = std::env::var_os("ZOR_BRP").filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| crate::paths::Paths::discover().map(|paths| {
            let runtime = paths.runtime_dir;
            let name = runtime.file_name().unwrap().to_string_lossy().replacen("fux", "zor", 1);
            runtime.with_file_name(name).join("default.brp.json")
        }));
    let result = path.map_err(|e| e.to_string()).and_then(|path| {
        brp.call_at(path, super::BrpTag::PluginAction(action.clone()), "zor/plugin.run",
            serde_json::json!({
                "name": name, "action": id, "workspace": session.workspace,
                "expected_fux_instance": session.instance
            }))
    });
    if let Err(error) = result { notice.0 = Some(format!("{action}: {error}")); }
}

fn plugin_replies(mut replies: MessageReader<super::BrpReply>, mut notice: ResMut<PendingNotice>) {
    for reply in replies.read() {
        if let super::BrpTag::PluginAction(action) = &reply.tag {
            notice.0 = Some(match &reply.result {
                Ok(_) => format!("{action}: accepted"),
                Err(error) => format!("{action}: {error}"),
            });
        }
    }
}

/// Bracketed paste reaches the focused pane as one `Input`, wrapped if the program asked, the
/// focused surface as one `SurfaceKey`, or the open prompt as one edit.
fn on_paste(
    mut ev: On<FocusedInput<Ime>>,
    state: Res<State<Mode>>,
    next: Res<NextState<Mode>>,
    focus: Res<InputFocus>,
    leaves: Query<&Shows>,
    surfaces: Query<&NodeId, (With<Surface>, With<Replicated>)>,
    grids: Query<&Grid>,
    painted: Res<super::replicate::InputRevision>,
    mut outbox: ResMut<Outbox>,
    mut commands: Commands,
    mut buf: Local<Vec<u8>>,
) {
    ev.propagate(false);
    let Ime::Commit { value, .. } = &ev.input else {
        return;
    };
    let focused = focus.get().unwrap_or(ev.focused_entity);
    match effective_mode(&state, &next) {
        Mode::Prompt => commands.run_system_cached_with(prompts::handle_paste, value.clone()),
        Mode::Normal if leaves.contains(focused) => {
            buf.clear();
            keys::encode_paste(value, modes_of(focused, &leaves, &grids), &mut buf);
            outbox.push(ViewerRequest::Input(buf.clone()));
        }
        Mode::Normal => {
            if let Ok(node) = surfaces.get(focused) {
                outbox.push(ViewerRequest::SurfaceKey {
                    revision: painted.0,
                    node: *node,
                    bytes: value.as_bytes().to_vec(),
                });
            }
        }
        _ => {}
    }
}

/// Focus on a leaf records it in the ring and, for a pane leaf, retargets the server (unless
/// exact).
fn on_focus_gained(
    ev: On<FocusGained>,
    leaves: Query<(Option<&Shows>, &NodeId), Leaf>,
    panes: Query<&PaneId>,
    target: Res<TargetPane>,
    exact: Option<Res<ExactAttachment>>,
    mut ring: ResMut<FocusRing>,
    mut pending: ResMut<PendingTarget>,
    mut outbox: ResMut<Outbox>,
) {
    let Ok((shows, node)) = leaves.get(ev.entity) else {
        return;
    };
    ring.push(*node);
    let Ok(pane) = shows.map_or(Err(()), |s| panes.get(s.0).map_err(|_| ())) else {
        return;
    };
    if exact.is_some() || target.0 == Some(*pane) {
        return;
    }
    outbox.push(ViewerRequest::Target(*pane));
    pending.pane = Some(*pane);
    pending.timer.reset();
}

/// Keeps `InputFocus` on the leaf showing the server's target pane: when the server's target
/// changes, or when the focused leaf (pane or surface) disappeared (re-instanced after a
/// template edit). Local navigation is left alone while its `Target` request is in flight.
fn sync_focus(
    time: Res<Time>,
    target: Res<TargetPane>,
    panes: Query<&PaneId>,
    leaves: Query<(Entity, &Shows), With<Replicated>>,
    focusable: Query<(), Leaf>,
    surfaces: Query<(Entity, &NodeId), (With<Replicated>, With<Surface>)>,
    mut focus: ResMut<InputFocus>,
    mut pending: ResMut<PendingTarget>,
) {
    pending.timer.tick(time.delta());
    if pending.pane.is_some() && (pending.pane == target.0 || pending.timer.is_finished()) {
        pending.pane = None;
    }
    let focus_on_leaf = focus.get().is_some_and(|e| focusable.contains(e));
    if focus_on_leaf && (pending.pane.is_some() || !target.is_changed()) {
        return;
    }
    let Some(target) = target.0 else {
        if let Some((surface, _)) = surfaces.iter().min_by_key(|(_, node)| node.0) {
            focus.set(surface, FocusCause::Navigated);
        }
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

type LeafGeometryChanged = (
    Leaf,
    Or<(
        Changed<ComputedNode>,
        Changed<UiGlobalTransform>,
        Added<Shows>,
        Added<Surface>,
    )>,
);

/// The server's rule (`layout::ops::navigate`): rect-edge distance with at least half overlap on
/// the perpendicular axis. Touching panes tie at distance zero and the first in tree order wins,
/// so the leaves are collected in the same pre-order walk the server uses.
const NAV_CONFIG: AutoNavigationConfig = AutoNavigationConfig {
    min_alignment_factor: 0.5,
    max_search_distance: None,
    prefer_aligned: true,
};

/// Rebuilds the directional navigation map from leaf geometry after layout.
fn rebuild_nav_map(
    chrome: Res<ChromeRoots>,
    children: Query<&Children>,
    leaves: Query<(&ComputedNode, &UiGlobalTransform), Leaf>,
    changed: Query<(), LeafGeometryChanged>,
    mut removed: RemovedComponents<Shows>,
    mut removed_surfaces: RemovedComponents<Surface>,
    mut map: ResMut<DirectionalNavigationMap>,
    mut areas: Local<Vec<FocusableArea>>,
) {
    let any_removed = removed.read().next().is_some() | removed_surfaces.read().next().is_some();
    if changed.is_empty() && !any_removed {
        return;
    }
    areas.clear();
    for entity in children.iter_descendants_depth_first(chrome.pane_area) {
        let Ok((node, transform)) = leaves.get(entity) else {
            continue;
        };
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
    auto_generate_navigation_edges(&mut map, &areas, &NAV_CONFIG);
}

/// Cell picking backend: the mouse pointer hits every node whose rect contains its cell and
/// whose ancestors do not clip it away (`bevy_ui`'s own backend rule); the stack index is the
/// depth so the topmost node wins.
fn cell_backend(
    pointers: Query<(&PointerId, &PointerLocation)>,
    stack: Res<UiStack>,
    nodes: Query<(&ComputedNode, &UiGlobalTransform, &Node)>,
    child_of: Query<&ChildOf, Without<OverrideClip>>,
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
                let (node, transform, _) = nodes.get(entity).ok()?;
                let hit = CellRect::from_node(node, transform).contains(col, row)
                    && clip_check_recursive(pos, entity, &nodes, &child_of);
                hit.then(|| {
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

/// `prefix ;`: focus the most recently focused other leaf that still exists.
fn previous_pane(
    ring: Res<FocusRing>,
    leaves: Query<(Entity, &NodeId), Leaf>,
    exact: Option<Res<ExactAttachment>>,
    mut focus: ResMut<InputFocus>,
) {
    if exact.is_some() {
        return;
    }
    let current = focus
        .get()
        .and_then(|e| leaves.get(e).ok())
        .map(|(_, n)| *n);
    let previous = ring
        .0
        .iter()
        .rev()
        .copied()
        .filter(|node| Some(*node) != current)
        .find_map(|node| leaves.iter().find(|(_, n)| **n == node).map(|(e, _)| e));
    if let Some(leaf) = previous {
        focus.set(leaf, FocusCause::Navigated);
    }
}

fn navigate(
    nav: &mut DirectionalNavigation,
    exact: Option<&ExactAttachment>,
    direction: CompassOctant,
) {
    if exact.is_some() {
        return;
    }
    // No neighbour in that direction is not an error worth reporting.
    let _ = nav.navigate(direction);
}

fn nav_west(mut nav: DirectionalNavigation, exact: Option<Res<ExactAttachment>>) {
    navigate(&mut nav, exact.as_deref(), CompassOctant::West);
}

fn nav_south(mut nav: DirectionalNavigation, exact: Option<Res<ExactAttachment>>) {
    navigate(&mut nav, exact.as_deref(), CompassOctant::South);
}

fn nav_north(mut nav: DirectionalNavigation, exact: Option<Res<ExactAttachment>>) {
    navigate(&mut nav, exact.as_deref(), CompassOctant::North);
}

fn nav_east(mut nav: DirectionalNavigation, exact: Option<Res<ExactAttachment>>) {
    navigate(&mut nav, exact.as_deref(), CompassOctant::East);
}

/// Tab order, as `bevy_input_focus::tab_navigation::handle_tab_navigation` does it: a focus that
/// lost its tab group (re-instanced after a template edit) still moves to the group's first or
/// last leaf instead of stranding the user.
fn tab_navigate(
    nav: &TabNavigation,
    focus: &mut InputFocus,
    exact: Option<&ExactAttachment>,
    action: NavAction,
) {
    if exact.is_some() {
        return;
    }
    match nav.navigate(focus, action) {
        Ok(next)
        | Err(TabNavigationError::NoTabGroupForCurrentFocus {
            new_focus: next, ..
        }) => {
            focus.set(next, FocusCause::Navigated);
        }
        Err(_) => {}
    }
}

fn next_pane(
    nav: TabNavigation,
    mut focus: ResMut<InputFocus>,
    exact: Option<Res<ExactAttachment>>,
) {
    tab_navigate(&nav, &mut focus, exact.as_deref(), NavAction::Next);
}

fn prev_pane(
    nav: TabNavigation,
    mut focus: ResMut<InputFocus>,
    exact: Option<Res<ExactAttachment>>,
) {
    tab_navigate(&nav, &mut focus, exact.as_deref(), NavAction::Previous);
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

/// Sends the prefix key itself to the focused pane (`prefix prefix` by default).
fn send_prefix(
    bindings: Res<Bindings>,
    focus: Res<InputFocus>,
    leaves: Query<&Shows>,
    mut outbox: ResMut<Outbox>,
    mut buf: Local<Vec<u8>>,
) {
    if !focus.get().is_some_and(|e| leaves.contains(e)) {
        return;
    }
    buf.clear();
    if bindings.prefix().bytes(&mut buf) {
        outbox.push(ViewerRequest::Input(buf.clone()));
    }
}

/// Every bindable action by name; `[bindings]` values are validated against this list.
pub const ACTIONS: &[(&str, fn(&mut World) -> SystemId)] = &[
    ("split-side", |w| w.register_system(split_right)),
    ("split-below", |w| w.register_system(split_below)),
    ("focus-left", |w| w.register_system(nav_west)),
    ("focus-down", |w| w.register_system(nav_south)),
    ("focus-up", |w| w.register_system(nav_north)),
    ("focus-right", |w| w.register_system(nav_east)),
    ("next-pane", |w| w.register_system(next_pane)),
    ("prev-pane", |w| w.register_system(prev_pane)),
    ("previous-pane", |w| w.register_system(previous_pane)),
    ("next-root", |w| w.register_system(next_root)),
    ("prev-root", |w| w.register_system(prev_root)),
    ("new-root", |w| w.register_system(new_root)),
    ("detach", |w| w.register_system(detach)),
    ("zoom", |w| w.register_system(toggle_zoom)),
    ("copy-mode", |w| w.register_system(copy_mode::enter)),
    ("send-prefix", |w| w.register_system(send_prefix)),
];

/// Every bindable action of every viewer module, by name.
pub fn all_actions() -> impl Iterator<Item = &'static (&'static str, fn(&mut World) -> SystemId)> {
    ACTIONS
        .iter()
        .chain(choosers::ACTIONS)
        .chain(prompts::ACTIONS)
}

/// Registers the one-shot systems of every module and the default chord table; the loaded
/// `fux.toml#bindings` replaces the table through [`reload_bindings`].
pub fn register_bindings(world: &mut World) {
    let actions = Actions(
        all_actions()
            .map(|(name, register)| (*name, register(world)))
            .collect(),
    );
    let table = Keybindings::default();
    world.insert_resource(Bindings::build(&table, &actions));
    world.insert_resource(actions);
}

/// Applies a loaded or reloaded `fux.toml#bindings` without reconnecting.
fn reload_bindings(
    mut events: MessageReader<AssetEvent<Keybindings>>,
    handle: Res<BindingsHandle>,
    tables: Res<Assets<Keybindings>>,
    actions: Res<Actions>,
    mut bindings: ResMut<Bindings>,
) {
    let changed = events.read().any(|event| {
        matches!(event, AssetEvent::Added { id } | AssetEvent::Modified { id } if *id == handle.0.id())
    });
    if !changed {
        return;
    }
    let Some(table) = tables.get(&handle.0) else {
        return;
    };
    if bindings.table == *table {
        return;
    }
    *bindings = Bindings::build(table, &actions);
}

/// The viewer's `fux.toml#bindings` handle.
#[derive(Resource, Debug, Clone)]
pub struct BindingsHandle(pub Handle<Keybindings>);

pub struct FocusPlugin;

impl Plugin for FocusPlugin {
    fn build(&self, app: &mut App) {
        let handle = app
            .world()
            .resource::<AssetServer>()
            .load(crate::assets::BINDINGS_PATH);
        app.init_resource::<PendingTarget>()
            .init_resource::<FocusRing>()
            .insert_resource(BindingsHandle(handle))
            .add_message::<Ime>()
            .add_systems(
                PreUpdate,
                (
                    dispatch_focused_input::<Ime>.in_set(InputFocusSystems::Dispatch),
                    cell_backend.in_set(PickingSystems::Backend),
                ),
            )
            .add_systems(
                Update,
                sync_focus
                    .in_set(ViewerSystems::Focus)
                    .run_if(not(in_state(Modal))),
            )
            .add_systems(Update, plugin_replies)
            .add_systems(
                PostUpdate,
                (
                    reload_bindings.after(AssetEventSystems),
                    rebuild_nav_map
                        .after(bevy_ui::UiSystems::PostLayout)
                        .before(InputFocusSystems::FocusChangeEvents),
                ),
            )
            .add_observer(on_key)
            .add_observer(on_paste)
            .add_observer(on_focus_gained)
            .add_observer(on_press);
        register_bindings(app.world_mut());
    }
}
