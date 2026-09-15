//! Mouse policy (prompt 3.4): observers on the instance nodes the cell backend picks
//! ([`crate::layout::picking`]) are the only place pointer policy lives. Each observer resolves
//! the picked instance to its viewer (through the pointer that fired) and to its template
//! (`InstanceOf`) and acts through [`layout::ops`], so instances stay disposable clones.
//!
//! * A primary press on a leaf targets the pane it shows (`Targets` ← `Shows`).
//! * A drag that started on a `Border(side)` region resizes, live, the two template siblings
//!   that share that edge: `flex_grow`/`flex_basis` under a flex parent, the px tracks of
//!   `grid_template_columns/rows` under a grid parent.
//! * An Alt-drag from a leaf's content moves it: dropped on another leaf's content the two
//!   exchange slots; dropped on a leaf's border it is placed beside that leaf on that side.
//! * The wheel scrolls an `Overflow::scroll` container's `ScrollPosition`, or a pane's history
//!   when the pane does not report the mouse.
//! * Panes that report the mouse (`Terminal::modes`) receive the press, release, motion and
//!   wheel translated to their protocol (SGR 1006 or X10) as `Effect::WritePty`; the right
//!   button follows the pane's [`RightClickPolicy`].
//!
//! Drag state lives on the viewer's pointer entity ([`PointerDrag`]), not on the dragged
//! instance: a live resize re-clones the instances under the pointer, and `bevy_picking` keeps
//! sending `Drag`/`DragEnd` for the entity that was pressed even after it is gone.

use bevy_app::prelude::*;
use bevy_camera::RenderTarget;
use bevy_ecs::prelude::*;
use bevy_ecs::system::SystemParam;
use bevy_input::mouse::MouseScrollUnit;
use bevy_input::touch::TouchPhase;
use bevy_log::{debug, warn};
use bevy_math::{UVec2, Vec2};
use bevy_picking::events::{
    Cancel, Drag, DragDrop, DragEnd, DragStart, Move, Pointer, Press, Release, Scroll,
};
use bevy_picking::pointer::{
    Location, PointerAction, PointerButton as PickButton, PointerId, PointerInput, PointerLocation,
    PointerMap, PointerPress,
};
use bevy_time::{Time, Timer, TimerMode};
use bevy_ui::{
    ComputedNode, Display, FlexDirection, FlexWrap, Node, OverflowAxis, PositionType,
    UiGlobalTransform,
};

use crate::layout::picking::HitRegion;
use crate::layout::{GridTrackPatch, NodePatch, Side, ops};
use crate::model::*;
use crate::terminal::Terminal;
use crate::wire::{Modes, MouseMode};

/// How long a pointer-policy notice stays on the viewer.
const NOTICE_SECS: f32 = 3.0;

/// The modifier bits of the viewer's latest mouse event, on its pointer entity
/// (`PointerInput` carries none).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerModifiers(pub u8);

/// What the viewer's primary-button drag is doing, on its pointer entity for the drag's life.
#[derive(Component, Clone, Debug, PartialEq)]
pub enum PointerDrag {
    /// A border drag: the edge between two template siblings moves with the pointer.
    Resize(Resize),
    /// An Alt-drag of the template leaf `leaf`; resolved on `DragDrop`.
    Move { leaf: Entity },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resize {
    pub kind: ResizeKind,
    pub axis: Axis,
    /// Sizes in cells of the two siblings at drag start, in layout order.
    pub first: f32,
    pub second: f32,
    /// Border + padding extent of each along the axis: a flex item's `flex_basis: 0` still
    /// keeps its insets, so grows are written as `size - insets`, and `MIN_PANE_*` applies to
    /// what is left.
    pub insets_first: f32,
    pub insets_second: f32,
    /// The `first` size last written, so an unchanged clamp does not bump the generation.
    pub applied: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResizeKind {
    /// Two template siblings under a flex parent; both get `flex_grow = size, flex_basis = 0`.
    Flex { first: Entity, second: Entity },
    /// Tracks `boundary` and `boundary + 1` of the template parent's grid template, all written
    /// as px.
    Grid {
        parent: Entity,
        boundary: usize,
        tracks: Vec<f32>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn of(side: Side) -> Self {
        match side {
            Side::Left | Side::Right => Self::Horizontal,
            Side::Top | Side::Bottom => Self::Vertical,
        }
    }

    fn pick(self, v: Vec2) -> f32 {
        match self {
            Self::Horizontal => v.x,
            Self::Vertical => v.y,
        }
    }

    fn min_pane(self) -> f32 {
        match self {
            Self::Horizontal => f32::from(MIN_PANE_COLS),
            Self::Vertical => f32::from(MIN_PANE_ROWS),
        }
    }
}

pub struct PointerPlugin;

impl Plugin for PointerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, expire_notices)
            .add_observer(on_press)
            .add_observer(on_release)
            .add_observer(on_move)
            .add_observer(on_drag_start)
            .add_observer(on_drag)
            .add_observer(on_drag_end)
            .add_observer(on_drag_drop)
            .add_observer(on_cancel)
            .add_observer(on_scroll);
    }
}

// ---------------------------------------------------------------------------------------------
// Viewer → picking
// ---------------------------------------------------------------------------------------------

/// Turns a viewer's mouse event into `PointerInput` for its pointer: a `Move` to the cell (the
/// pointer's location only changes on `Move`), then the press/release/scroll action.
pub fn forward(world: &mut World, viewer: Entity, event: PointerEvent) {
    let Some((pointer_entity, viewport)) = world
        .get::<ViewerPointer>(viewer)
        .map(|p| p.0)
        .zip(world.get::<Viewport>(viewer).copied())
    else {
        return;
    };
    let Some(id) = world.get::<PointerId>(pointer_entity).copied() else {
        return;
    };
    let Some(target) = (RenderTarget::None {
        size: UVec2::new(u32::from(viewport.cols), u32::from(viewport.rows)),
    })
    .normalize(None) else {
        return;
    };
    let modifiers = PointerModifiers(event.modifiers);
    match world.get_mut::<PointerModifiers>(pointer_entity) {
        Some(mut current) => {
            current.set_if_neq(modifiers);
        }
        None => {
            world.entity_mut(pointer_entity).insert(modifiers);
        }
    }
    let position = Vec2::new(f32::from(event.col) + 0.5, f32::from(event.row) + 0.5);
    let previous = world
        .get::<PointerLocation>(pointer_entity)
        .and_then(|l| l.location().map(|l| l.position))
        .unwrap_or(position);
    let location = Location { target, position };
    let Some(mut messages) = world.get_resource_mut::<Messages<PointerInput>>() else {
        return;
    };
    messages.write(PointerInput::new(
        id,
        location.clone(),
        PointerAction::Move {
            delta: position - previous,
        },
    ));
    let button = match event.button {
        PointerButton::Right => PickButton::Secondary,
        PointerButton::Middle => PickButton::Middle,
        PointerButton::Left | PointerButton::None => PickButton::Primary,
    };
    let action = match event.kind {
        PointerKind::Move => return,
        PointerKind::Press => PointerAction::Press(button),
        PointerKind::Release => PointerAction::Release(button),
        PointerKind::ScrollUp | PointerKind::ScrollDown => PointerAction::Scroll {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: if event.kind == PointerKind::ScrollUp {
                1.0
            } else {
                -1.0
            },
            phase: TouchPhase::Moved,
        },
    };
    messages.write(PointerInput::new(id, location, action));
}

// ---------------------------------------------------------------------------------------------
// Observers
// ---------------------------------------------------------------------------------------------

/// The viewer that owns a pointer.
#[derive(SystemParam)]
struct Owners<'w, 's> {
    map: Res<'w, PointerMap>,
    viewers: Query<'w, 's, (Entity, &'static ViewerPointer), With<Viewer>>,
    modifiers: Query<'w, 's, &'static PointerModifiers>,
    drags: Query<'w, 's, &'static PointerDrag>,
    presses: Query<'w, 's, &'static PointerPress>,
}

impl Owners<'_, '_> {
    /// `(viewer, pointer entity)` for a pointer id.
    fn of(&self, id: PointerId) -> Option<(Entity, Entity)> {
        let pointer = self.map.get_entity(id)?;
        self.viewers
            .iter()
            .find(|(_, p)| p.0 == pointer)
            .map(|(viewer, _)| (viewer, pointer))
    }

    fn modifiers(&self, pointer: Entity) -> u8 {
        self.modifiers.get(pointer).map_or(0, |m| m.0)
    }

    fn drag(&self, pointer: Entity) -> Option<&PointerDrag> {
        self.drags.get(pointer).ok()
    }

    fn any_pressed(&self, pointer: Entity) -> bool {
        self.presses
            .get(pointer)
            .is_ok_and(PointerPress::is_any_pressed)
    }
}

/// Which pane a picked instance node shows, when it is a leaf.
fn shown_pane(leaves: &Query<&Shows>, entity: Entity) -> Option<Entity> {
    leaves.get(entity).ok().map(|s| s.0)
}

fn on_press(
    press: On<Pointer<Press>>,
    owners: Owners,
    leaves: Query<&Shows>,
    mut commands: Commands,
) {
    let Some(pane) = shown_pane(&leaves, press.entity) else {
        return;
    };
    let Some((viewer, pointer)) = owners.of(press.pointer_id) else {
        return;
    };
    let modifiers = owners.modifiers(pointer);
    let leaf = press.entity;
    let button = press.event.button;
    let position = press.pointer_location.position;
    commands.queue(move |world: &mut World| {
        if world.get::<ExactTarget>(viewer).is_none()
            && let Err(error) = ops::target(world, viewer, pane)
        {
            warn!("click target {pane} for {viewer}: {error}");
        }
        if button == PickButton::Primary && modifiers & PointerEvent::ALT != 0 {
            return; // The start of a pane move is fux's, not the pane's.
        }
        report(
            world,
            viewer,
            leaf,
            pane,
            position,
            button,
            modifiers,
            Report::Press,
        );
    });
}

fn on_release(
    release: On<Pointer<Release>>,
    owners: Owners,
    leaves: Query<&Shows>,
    mut commands: Commands,
) {
    let Some(pane) = shown_pane(&leaves, release.entity) else {
        return;
    };
    let Some((viewer, pointer)) = owners.of(release.pointer_id) else {
        return;
    };
    let modifiers = owners.modifiers(pointer);
    let leaf = release.entity;
    let button = release.event.button;
    let position = release.pointer_location.position;
    commands.queue(move |world: &mut World| {
        if button == PickButton::Primary && modifiers & PointerEvent::ALT != 0 {
            return;
        }
        report(
            world,
            viewer,
            leaf,
            pane,
            position,
            button,
            modifiers,
            Report::Release,
        );
    });
}

/// Hover motion: reported only to panes in any-motion mode. With a button held the same
/// movement is a `Drag` and is reported by [`on_drag`] instead.
fn on_move(
    motion: On<Pointer<Move>>,
    owners: Owners,
    leaves: Query<&Shows>,
    mut commands: Commands,
) {
    let Some(pane) = shown_pane(&leaves, motion.entity) else {
        return;
    };
    let Some((viewer, pointer)) = owners.of(motion.pointer_id) else {
        return;
    };
    if owners.any_pressed(pointer) {
        return;
    }
    let modifiers = owners.modifiers(pointer);
    let leaf = motion.entity;
    let position = motion.pointer_location.position;
    commands.queue(move |world: &mut World| {
        report(
            world,
            viewer,
            leaf,
            pane,
            position,
            PickButton::Primary,
            modifiers,
            Report::Hover,
        );
    });
}

fn on_drag_start(
    start: On<Pointer<DragStart>>,
    owners: Owners,
    leaves: Query<&Shows>,
    mut commands: Commands,
) {
    if start.entity != start.original_event_target() || start.event.button != PickButton::Primary {
        return;
    }
    let Some((_, pointer)) = owners.of(start.pointer_id) else {
        return;
    };
    let Some(region) = start.event.hit.extra_as::<HitRegion>().copied() else {
        return;
    };
    let node = start.entity;
    match region {
        HitRegion::Border(side) => {
            commands.queue(move |world: &mut World| {
                if let Some((resize, siblings)) = plan_resize(world, node, side) {
                    debug!("border drag on {node}: {resize:?}");
                    for slot in siblings {
                        patch_grow(world, slot.template, slot.size, slot.insets);
                    }
                    world
                        .entity_mut(pointer)
                        .insert(PointerDrag::Resize(resize));
                }
            });
        }
        HitRegion::Content => {
            if shown_pane(&leaves, node).is_none()
                || owners.modifiers(pointer) & PointerEvent::ALT == 0
            {
                return;
            }
            commands.queue(move |world: &mut World| {
                if let Some(leaf) = world.get::<InstanceOf>(node).map(|i| i.0) {
                    world.entity_mut(pointer).insert(PointerDrag::Move { leaf });
                }
            });
        }
    }
}

fn on_drag(drag: On<Pointer<Drag>>, owners: Owners, leaves: Query<&Shows>, mut commands: Commands) {
    let Some((viewer, pointer)) = owners.of(drag.pointer_id) else {
        return;
    };
    let button = drag.event.button;
    match (owners.drag(pointer), button) {
        (Some(PointerDrag::Resize(_)), PickButton::Primary) => {
            // The dragged instance may be gone (re-cloned); the plan on the pointer is enough.
            if drag.entity != drag.original_event_target() {
                return;
            }
            let distance = drag.event.distance;
            commands.queue(move |world: &mut World| apply_resize(world, pointer, distance));
        }
        (Some(PointerDrag::Move { .. }), PickButton::Primary) => {}
        _ => {
            let Some(pane) = shown_pane(&leaves, drag.entity) else {
                return;
            };
            let modifiers = owners.modifiers(pointer);
            let leaf = drag.entity;
            let position = drag.pointer_location.position;
            commands.queue(move |world: &mut World| {
                report(
                    world,
                    viewer,
                    leaf,
                    pane,
                    position,
                    button,
                    modifiers,
                    Report::Drag,
                );
            });
        }
    }
}

fn on_drag_end(end: On<Pointer<DragEnd>>, owners: Owners, mut commands: Commands) {
    if let Some((_, pointer)) = owners.of(end.pointer_id) {
        commands.entity(pointer).remove::<PointerDrag>();
    }
}

fn on_cancel(cancel: On<Pointer<Cancel>>, owners: Owners, mut commands: Commands) {
    if let Some((_, pointer)) = owners.of(cancel.pointer_id) {
        commands.entity(pointer).remove::<PointerDrag>();
    }
}

/// An Alt-dragged leaf dropped on another leaf: content exchanges the two, a border places the
/// dragged leaf beside the target on that side.
fn on_drag_drop(
    drop: On<Pointer<DragDrop>>,
    owners: Owners,
    leaves: Query<&Shows>,
    mut commands: Commands,
) {
    if drop.entity != drop.original_event_target() || drop.event.button != PickButton::Primary {
        return;
    }
    let Some((_, pointer)) = owners.of(drop.pointer_id) else {
        return;
    };
    let Some(PointerDrag::Move { leaf }) = owners.drag(pointer).cloned() else {
        return;
    };
    if shown_pane(&leaves, drop.entity).is_none() {
        return;
    }
    let Some(region) = drop.event.hit.extra_as::<HitRegion>().copied() else {
        return;
    };
    let target = drop.entity;
    commands.queue(move |world: &mut World| {
        world.entity_mut(pointer).remove::<PointerDrag>();
        let Some(target) = world.get::<InstanceOf>(target).map(|i| i.0) else {
            return;
        };
        if target == leaf {
            return;
        }
        let result = match region {
            HitRegion::Content => ops::exchange(world, leaf, target),
            HitRegion::Border(side) => ops::place_beside(world, leaf, target, side),
        };
        if let Err(error) = result {
            warn!("drop {leaf} on {target} ({region:?}): {error}");
        }
    });
}

/// Wheel over a pane that reports the mouse goes to the pane; otherwise it scrolls the pane's
/// history, or the nearest `Overflow::scroll` container's `ScrollPosition`.
fn on_scroll(
    mut scroll: On<Pointer<Scroll>>,
    owners: Owners,
    leaves: Query<&Shows>,
    nodes: Query<&Node>,
    mut commands: Commands,
) {
    let Some((viewer, pointer)) = owners.of(scroll.pointer_id) else {
        return;
    };
    let rows = -scroll.event.y.round() as i32;
    let node = scroll.entity;
    if let Some(pane) = shown_pane(&leaves, node) {
        scroll.propagate(false);
        let modifiers = owners.modifiers(pointer);
        let position = scroll.pointer_location.position;
        let button = if scroll.event.y > 0.0 {
            Report::WHEEL_UP
        } else {
            Report::WHEEL_DOWN
        };
        commands.queue(move |world: &mut World| {
            if modes_of(world, pane).is_some_and(|m| m.mouse != MouseMode::None) {
                report_code(
                    world,
                    node,
                    pane,
                    position,
                    button,
                    modifiers,
                    Report::Press,
                );
            } else if rows != 0
                && let Err(error) = ops::scroll(world, viewer, node, rows)
            {
                debug!("wheel history on {node}: {error}");
            }
        });
        return;
    }
    if nodes
        .get(node)
        .is_ok_and(|n| n.overflow.y == OverflowAxis::Scroll)
    {
        scroll.propagate(false);
        if rows == 0 {
            return;
        }
        commands.queue(move |world: &mut World| {
            if let Err(error) = ops::scroll(world, viewer, node, rows) {
                debug!("wheel scroll on {node}: {error}");
            }
        });
    }
}

/// Pointer-policy notices expire on their timer.
fn expire_notices(
    time: Res<Time>,
    mut notices: Query<(Entity, &mut Notice), With<Viewer>>,
    mut commands: Commands,
) {
    for (viewer, mut notice) in &mut notices {
        if notice.timer.tick(time.delta()).is_finished() {
            commands.entity(viewer).remove::<Notice>();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Border resize
// ---------------------------------------------------------------------------------------------

/// One in-flow child of a container as laid out: its template, start and size along an axis,
/// and its border + padding extent along that axis.
#[derive(Clone, Copy, Debug)]
struct Slot {
    template: Entity,
    start: f32,
    size: f32,
    insets: f32,
}

/// The in-flow, laid-out children of instance node `parent` along `axis`, in layout order.
fn slots(world: &World, parent: Entity, axis: Axis) -> Vec<Slot> {
    let mut out: Vec<Slot> = world
        .get::<Children>(parent)
        .map(|children| {
            children
                .iter()
                .filter_map(|child| {
                    let node = world.get::<Node>(child)?;
                    if node.position_type == PositionType::Absolute || node.display == Display::None
                    {
                        return None;
                    }
                    let computed = world.get::<ComputedNode>(child)?;
                    let transform = world.get::<UiGlobalTransform>(child)?;
                    let template = world.get::<InstanceOf>(child)?.0;
                    let size = axis.pick(computed.size);
                    if size <= 0.0 {
                        return None;
                    }
                    let insets = computed.content_inset();
                    Some(Slot {
                        template,
                        start: axis.pick(transform.translation - computed.size * 0.5),
                        size,
                        insets: axis.pick(insets.min_inset + insets.max_inset),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Walks up from the instance node whose `side` border was pressed to the nearest ancestor pair
/// of siblings that share that edge, and describes how to move it. For a flex pair the
/// container's in-flow children come back too: every one is written as `flex_grow = size,
/// flex_basis = 0` first, so the pair's new grows are in the same unit as the rest.
fn plan_resize(world: &World, node: Entity, side: Side) -> Option<(Resize, Vec<Slot>)> {
    let axis = Axis::of(side);
    let mut node = node;
    for _ in 0..=MAX_DEPTH {
        let parent = world.get::<ChildOf>(node)?.parent();
        let node_style = world.get::<Node>(node)?;
        if node_style.position_type == PositionType::Absolute {
            return None;
        }
        let parent_style = world.get::<Node>(parent)?;
        let parent_template = world.get::<InstanceOf>(parent)?.0;
        let plan = match parent_style.display {
            Display::Grid => grid_plan(world, parent, parent_template, node, axis, side),
            Display::Flex
                if flex_axis(parent_style) == Some(axis)
                    && parent_style.flex_wrap == FlexWrap::NoWrap =>
            {
                flex_plan(world, parent, node, axis, side)
            }
            _ => None,
        };
        if plan.is_some() {
            return plan;
        }
        node = parent;
    }
    None
}

fn flex_axis(style: &Node) -> Option<Axis> {
    match style.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => Some(Axis::Horizontal),
        FlexDirection::Column | FlexDirection::ColumnReverse => Some(Axis::Vertical),
    }
}

/// The boundary index left/above (`Left`/`Top`) or right/below (`Right`/`Bottom`) of the slot
/// at `index`, when it exists.
fn boundary(index: usize, side: Side, count: usize) -> Option<usize> {
    match side {
        Side::Left | Side::Top => index.checked_sub(1),
        Side::Right | Side::Bottom => (index + 1 < count).then_some(index),
    }
}

fn pair(slots: &[Slot], boundary: usize, axis: Axis) -> Option<Resize> {
    let first = *slots.get(boundary)?;
    let second = *slots.get(boundary + 1)?;
    if first.size + second.size < 2.0 * axis.min_pane() + first.insets + second.insets {
        return None;
    }
    Some(Resize {
        kind: ResizeKind::Flex {
            first: first.template,
            second: second.template,
        },
        axis,
        first: first.size,
        second: second.size,
        insets_first: first.insets,
        insets_second: second.insets,
        applied: first.size,
    })
}

fn flex_plan(
    world: &World,
    parent: Entity,
    node: Entity,
    axis: Axis,
    side: Side,
) -> Option<(Resize, Vec<Slot>)> {
    let template = world.get::<InstanceOf>(node)?.0;
    let slots = slots(world, parent, axis);
    let index = slots.iter().position(|s| s.template == template)?;
    let boundary = boundary(index, side, slots.len())?;
    let resize = pair(&slots, boundary, axis)?;
    Some((resize, slots))
}

/// Grid tracks along `axis` from the laid-out children: one track per distinct start, sized by
/// the smallest child starting there (so a spanning item does not widen its first track).
fn grid_plan(
    world: &World,
    parent: Entity,
    parent_template: Entity,
    node: Entity,
    axis: Axis,
    side: Side,
) -> Option<(Resize, Vec<Slot>)> {
    let template = world.get::<InstanceOf>(node)?.0;
    let slots = slots(world, parent, axis);
    let mut tracks: Vec<Slot> = Vec::new();
    for slot in &slots {
        match tracks.last_mut() {
            Some(last) if (last.start - slot.start).abs() < 0.5 => {
                if slot.size < last.size {
                    *last = *slot;
                }
            }
            _ => tracks.push(*slot),
        }
    }
    let mine = slots.iter().find(|s| s.template == template)?;
    let index = tracks
        .iter()
        .position(|t| (t.start - mine.start).abs() < 0.5)?;
    let boundary = boundary(index, side, tracks.len())?;
    let mut resize = pair(&tracks, boundary, axis)?;
    resize.kind = ResizeKind::Grid {
        parent: parent_template,
        boundary,
        tracks: tracks.iter().map(|t| t.size).collect(),
    };
    Some((resize, Vec::new()))
}

/// `flex_grow = size - insets, flex_basis = 0`: with every in-flow sibling written this way the
/// free space is exactly the sum of the grows, so each item lays out at `size`.
fn patch_grow(world: &mut World, template: Entity, size: f32, insets: f32) {
    let patch = NodePatch {
        flex_grow: Some((size - insets).max(0.0)),
        flex_basis: Some("0px".to_owned()),
        ..Default::default()
    };
    if let Err(error) = ops::patch_node(world, template, &patch) {
        warn!("resize {template}: {error}");
    }
}

/// Applies the pointer's resize plan for a drag `distance` from its start.
fn apply_resize(world: &mut World, pointer: Entity, distance: Vec2) {
    let Some(PointerDrag::Resize(resize)) = world.get::<PointerDrag>(pointer).cloned() else {
        return;
    };
    let total = resize.first + resize.second;
    let min_first = resize.axis.min_pane() + resize.insets_first;
    let min_second = resize.axis.min_pane() + resize.insets_second;
    let first = (resize.first + resize.axis.pick(distance))
        .round()
        .clamp(min_first, total - min_second);
    if first.to_bits() == resize.applied.to_bits() {
        return;
    }
    let second = total - first;
    match &resize.kind {
        ResizeKind::Flex {
            first: a,
            second: b,
        } => {
            patch_grow(world, *a, first, resize.insets_first);
            patch_grow(world, *b, second, resize.insets_second);
        }
        ResizeKind::Grid {
            parent,
            boundary,
            tracks,
        } => {
            let tracks: Vec<GridTrackPatch> = tracks
                .iter()
                .enumerate()
                .map(|(i, size)| {
                    let size = if i == *boundary {
                        first
                    } else if i == boundary + 1 {
                        second
                    } else {
                        *size
                    };
                    GridTrackPatch {
                        repeat: 1,
                        track: format!("{size}px"),
                    }
                })
                .collect();
            let patch = match resize.axis {
                Axis::Horizontal => NodePatch {
                    grid_template_columns: Some(tracks),
                    ..Default::default()
                },
                Axis::Vertical => NodePatch {
                    grid_template_rows: Some(tracks),
                    ..Default::default()
                },
            };
            if let Err(error) = ops::patch_node(world, *parent, &patch) {
                warn!("resize grid {parent}: {error}");
            }
        }
    }
    if let Some(mut drag) = world.get_mut::<PointerDrag>(pointer)
        && let PointerDrag::Resize(resize) = &mut *drag
    {
        resize.applied = first;
    }
}

// ---------------------------------------------------------------------------------------------
// Mouse reporting to the pane
// ---------------------------------------------------------------------------------------------

/// What a report describes; motion kinds are gated by the pane's `MouseMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    Press,
    Release,
    /// Motion with a button held (`MouseMode::Drag` and up).
    Drag,
    /// Motion with no button (`MouseMode::Motion` only).
    Hover,
}

impl Report {
    /// xterm button codes for the wheel.
    pub const WHEEL_UP: u8 = 64;
    pub const WHEEL_DOWN: u8 = 65;
}

fn modes_of(world: &World, pane: Entity) -> Option<Modes> {
    let process = world.get::<Process>(pane)?;
    if matches!(process, Process::Exited { .. }) {
        return None;
    }
    world.get::<Terminal>(pane).map(Terminal::modes)
}

fn button_code(button: PickButton) -> u8 {
    match button {
        PickButton::Primary => 0,
        PickButton::Middle => 1,
        PickButton::Secondary => 2,
    }
}

/// Whether the right button reaches the pane: `Forward` always, `Paste` never (the paste itself
/// is a viewer action), `Auto` only when the pane reports the mouse.
fn right_click_forwards(policy: RightClickPolicy, modes: Modes) -> bool {
    match policy {
        RightClickPolicy::Forward => true,
        RightClickPolicy::Paste => false,
        RightClickPolicy::Auto => modes.mouse != MouseMode::None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one report site per observer; the tuple would only rename the arguments"
)]
fn report(
    world: &mut World,
    viewer: Entity,
    leaf: Entity,
    pane: Entity,
    position: Vec2,
    button: PickButton,
    modifiers: u8,
    kind: Report,
) {
    let Some(modes) = modes_of(world, pane) else {
        return;
    };
    if button == PickButton::Secondary && kind != Report::Hover {
        let policy = world
            .get::<RightClickPolicy>(pane)
            .copied()
            .unwrap_or_default();
        if !right_click_forwards(policy, modes) {
            if kind == Report::Press {
                notice(
                    world,
                    viewer,
                    "right-click paste is not available; set the pane's RightClickPolicy to Forward to send it",
                );
            }
            return;
        }
    }
    report_code(
        world,
        leaf,
        pane,
        position,
        button_code(button),
        modifiers,
        kind,
    );
}

fn report_code(
    world: &mut World,
    leaf: Entity,
    pane: Entity,
    position: Vec2,
    code: u8,
    modifiers: u8,
    kind: Report,
) {
    let Some(modes) = modes_of(world, pane) else {
        return;
    };
    let Some((col, row)) = pane_cell(world, leaf, position) else {
        return;
    };
    if let Some(bytes) = encode_mouse(modes, kind, code, modifiers, col, row) {
        world
            .resource_mut::<Messages<Effect>>()
            .write(Effect::WritePty { pane, bytes });
    }
}

/// The 1-based pane cell under a viewport position, from the instance leaf's content box.
fn pane_cell(world: &World, leaf: Entity, position: Vec2) -> Option<(u16, u16)> {
    let computed = world.get::<ComputedNode>(leaf)?;
    let transform = world.get::<UiGlobalTransform>(leaf)?;
    let origin = transform.translation - computed.size * 0.5 + computed.border.min_inset;
    let local = (position - origin).floor().max(Vec2::ZERO);
    let cell = |v: f32| u16::try_from(v as u32).ok().and_then(|v| v.checked_add(1));
    cell(local.x).zip(cell(local.y))
}

/// Encodes one mouse report for the pane's protocol: SGR 1006 (`ESC [ < code ; col ; row M|m`)
/// when the pane asked for it, else X10 (`ESC [ M` + three bytes offset by 32, unencodable
/// beyond cell 223). `None` when the pane's mode does not report `kind`.
pub fn encode_mouse(
    modes: Modes,
    kind: Report,
    code: u8,
    modifiers: u8,
    col: u16,
    row: u16,
) -> Option<Vec<u8>> {
    let reports = match modes.mouse {
        MouseMode::None => false,
        MouseMode::Press => matches!(kind, Report::Press | Report::Release),
        MouseMode::Drag => kind != Report::Hover,
        MouseMode::Motion => true,
    };
    if !reports {
        return None;
    }
    let mut code = u16::from(code);
    if modifiers & PointerEvent::SHIFT != 0 {
        code |= 4;
    }
    if modifiers & PointerEvent::ALT != 0 {
        code |= 8;
    }
    if modifiers & PointerEvent::CTRL != 0 {
        code |= 16;
    }
    let motion = matches!(kind, Report::Drag | Report::Hover);
    if motion {
        code |= 32;
    }
    if kind == Report::Hover {
        code |= 3;
    }
    if modes.mouse_sgr {
        let terminator = if kind == Report::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{col};{row}{terminator}").into_bytes());
    }
    if kind == Report::Release {
        code = (code & !0b11) | 3;
    }
    let mut bytes = Vec::with_capacity(6);
    bytes.extend_from_slice(b"\x1b[M");
    for value in [code + 32, col + 32, row + 32] {
        bytes.push(u8::try_from(value).ok()?);
    }
    Some(bytes)
}

fn notice(world: &mut World, viewer: Entity, text: &str) {
    if world.get::<Viewer>(viewer).is_none() {
        return;
    }
    world.entity_mut(viewer).insert(Notice {
        text: text.to_owned(),
        timer: Timer::from_seconds(NOTICE_SECS, TimerMode::Once),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(mouse: MouseMode, sgr: bool) -> Modes {
        Modes {
            mouse,
            mouse_sgr: sgr,
            ..Default::default()
        }
    }

    #[test]
    fn sgr_reports_follow_the_mode() {
        let press = modes(MouseMode::Press, true);
        assert_eq!(
            encode_mouse(press, Report::Press, 0, 0, 3, 4).as_deref(),
            Some(b"\x1b[<0;3;4M".as_slice())
        );
        assert_eq!(
            encode_mouse(press, Report::Release, 2, PointerEvent::SHIFT, 3, 4).as_deref(),
            Some(b"\x1b[<6;3;4m".as_slice())
        );
        assert_eq!(encode_mouse(press, Report::Drag, 0, 0, 3, 4), None);
        assert_eq!(
            encode_mouse(modes(MouseMode::Drag, true), Report::Drag, 0, 0, 3, 4).as_deref(),
            Some(b"\x1b[<32;3;4M".as_slice())
        );
        assert_eq!(
            encode_mouse(modes(MouseMode::Drag, true), Report::Hover, 0, 0, 3, 4),
            None
        );
        assert_eq!(
            encode_mouse(modes(MouseMode::Motion, true), Report::Hover, 0, 0, 3, 4).as_deref(),
            Some(b"\x1b[<35;3;4M".as_slice())
        );
        assert_eq!(
            encode_mouse(
                press,
                Report::Press,
                Report::WHEEL_UP,
                PointerEvent::CTRL,
                1,
                1
            )
            .as_deref(),
            Some(b"\x1b[<80;1;1M".as_slice())
        );
        assert_eq!(
            encode_mouse(modes(MouseMode::None, true), Report::Press, 0, 0, 1, 1),
            None
        );
    }

    #[test]
    fn x10_reports_offset_by_32_and_refuse_far_cells() {
        let press = modes(MouseMode::Press, false);
        assert_eq!(
            encode_mouse(press, Report::Press, 0, 0, 1, 1).as_deref(),
            Some(b"\x1b[M !!".as_slice())
        );
        assert_eq!(
            encode_mouse(press, Report::Release, 0, PointerEvent::ALT, 1, 1).as_deref(),
            Some(b"\x1b[M+!!".as_slice())
        );
        assert_eq!(encode_mouse(press, Report::Press, 0, 0, 224, 1), None);
    }
}
