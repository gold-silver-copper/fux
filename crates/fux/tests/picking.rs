//! The cell picking backend and the pointer policy observers against the real `bevy_ui` layout:
//! hits and border regions on flex, grid and absolute trees, zoomed instances, two viewers on
//! one root, border drag-to-resize, Alt-drag-to-move, the wheel and mouse reporting.
//! `check_invariants` runs after every update.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns; flex_grow is compared against exact cell counts"
)]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_math::UVec2;
use bevy_picking::hover::HoverMap;
use bevy_picking::pointer::PointerId;
use bevy_ui::prelude::*;
use bevy_ui::{ScrollPosition, UiTargetCamera};
use fux::app::build_headless;
use fux::config::Config;
use fux::layout::picking::HitRegion;
use fux::layout::{GridTrackPatch, NodePatch, Side, ops};
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::pointer::PointerDrag;

fn app() -> App {
    build_headless(&Config::default())
}

fn shell() -> PaneTemplate {
    PaneTemplate {
        argv: vec!["sh".to_owned()],
        ..Default::default()
    }
}

fn grow() -> Node {
    Node {
        flex_grow: 1.0,
        ..Default::default()
    }
}

/// A flex-growing leaf with a one-cell border on every side.
fn framed() -> Node {
    Node {
        flex_grow: 1.0,
        border: UiRect::all(Val::Px(1.0)),
        ..Default::default()
    }
}

fn check(app: &mut App) {
    check_invariants(app.world_mut()).unwrap();
}

fn viewport(rows: u16, cols: u16) -> Viewport {
    Viewport { rows, cols }
}

fn kids(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.iter().collect())
        .unwrap_or_default()
}

fn pane_of(world: &World, leaf: Entity) -> Entity {
    world.get::<Places>(leaf).unwrap().0
}

fn instance_root(world: &mut World, viewer: Entity) -> Entity {
    let camera = world.get::<ViewerCamera>(viewer).unwrap().0;
    let roots: Vec<Entity> = world
        .query_filtered::<(Entity, &UiTargetCamera), (With<InstanceNode>, Without<ChildOf>)>()
        .iter(world)
        .filter(|(_, c)| c.entity() == camera)
        .map(|(e, _)| e)
        .collect();
    assert_eq!(roots.len(), 1, "exactly one instance root per viewer");
    roots[0]
}

/// The instance node of `template` under the viewer's instance root.
fn instance_of(world: &mut World, viewer: Entity, template: Entity) -> Entity {
    let root = instance_root(world, viewer);
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<InstanceOf>(e).map(|i| i.0) == Some(template) {
            return e;
        }
        stack.extend(kids(world, e));
    }
    panic!("no instance of {template} for {viewer}");
}

fn instance_size(app: &mut App, viewer: Entity, template: Entity) -> UVec2 {
    let instance = instance_of(app.world_mut(), viewer, template);
    size_of(app.world(), instance)
}

fn size_of(world: &World, e: Entity) -> UVec2 {
    world.get::<ComputedNode>(e).unwrap().size.as_uvec2()
}

fn pointer_id(world: &World, viewer: Entity) -> PointerId {
    let pointer = world.get::<ViewerPointer>(viewer).unwrap().0;
    *world.get::<PointerId>(pointer).unwrap()
}

fn pointer_entity(world: &World, viewer: Entity) -> Entity {
    world.get::<ViewerPointer>(viewer).unwrap().0
}

/// Sends one viewer mouse event through the request path and runs an update.
fn mouse(
    app: &mut App,
    viewer: Entity,
    col: u16,
    row: u16,
    kind: PointerKind,
    button: PointerButton,
    modifiers: u8,
) {
    app.world_mut().write_message(Inbound::ViewerRequest {
        viewer,
        request: ViewerRequest::Pointer(PointerEvent {
            col,
            row,
            kind,
            button,
            modifiers,
        }),
    });
    app.update();
    check(app);
}

fn hover(app: &mut App, viewer: Entity, col: u16, row: u16) {
    mouse(
        app,
        viewer,
        col,
        row,
        PointerKind::Move,
        PointerButton::None,
        0,
    );
}

/// `(entity, region)` of the single hovered node under the viewer's pointer.
fn hit(app: &App, viewer: Entity) -> Option<(Entity, HitRegion)> {
    let world = app.world();
    let id = pointer_id(world, viewer);
    let hovered = world.resource::<HoverMap>().get(&id)?;
    assert!(
        hovered.len() <= 1,
        "the hover map holds the topmost hit only: {hovered:?}"
    );
    let (entity, data) = hovered.iter().next()?;
    Some((*entity, *data.extra_as::<HitRegion>().unwrap()))
}

/// Presses at `(c0, r0)`, drags to `(c1, r1)` and releases there, one update per event.
fn drag(app: &mut App, viewer: Entity, from: (u16, u16), to: (u16, u16), modifiers: u8) {
    hover(app, viewer, from.0, from.1);
    mouse(
        app,
        viewer,
        from.0,
        from.1,
        PointerKind::Press,
        PointerButton::Left,
        modifiers,
    );
    mouse(
        app,
        viewer,
        to.0,
        to.1,
        PointerKind::Move,
        PointerButton::Left,
        modifiers,
    );
    mouse(
        app,
        viewer,
        to.0,
        to.1,
        PointerKind::Release,
        PointerButton::Left,
        modifiers,
    );
}

fn drain_effects(app: &mut App) -> Vec<Effect> {
    app.world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .collect()
}

fn pty_writes(effects: &[Effect], pane: Entity) -> Vec<Vec<u8>> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::WritePty { pane: p, bytes } if *p == pane => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

/// A workspace with a root `main`; returns `(ws, root)`.
fn workspace(world: &mut World) -> (Entity, Entity) {
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    (ws, root)
}

/// Two framed leaves in a row: `(ws, root, left leaf, right leaf)`.
fn two_pane_row(world: &mut World) -> (Entity, Entity, Entity, Entity) {
    let (ws, root) = workspace(world);
    let left = ops::spawn_node(world, root, None, framed(), Some(shell())).unwrap();
    let right = ops::spawn_node(world, root, None, framed(), Some(shell())).unwrap();
    (ws, root, left, right)
}

fn flex_grow(world: &World, e: Entity) -> f32 {
    world.get::<Node>(e).unwrap().flex_grow
}

#[test]
fn nested_flex_hits_resolve_cells_and_border_regions() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root) = workspace(world);
    // Row [ Column [ a, b ], c ] with framed leaves.
    let column = ops::spawn_node(
        world,
        root,
        None,
        Node {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let a = ops::spawn_node(world, column, None, framed(), Some(shell())).unwrap();
    let b = ops::spawn_node(world, column, None, framed(), Some(shell())).unwrap();
    let c = ops::spawn_node(world, root, None, framed(), Some(shell())).unwrap();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    let ia = instance_of(app.world_mut(), viewer, a);
    let ib = instance_of(app.world_mut(), viewer, b);
    let ic = instance_of(app.world_mut(), viewer, c);
    assert_eq!(size_of(app.world(), ia), UVec2::new(40, 12));

    hover(&mut app, viewer, 10, 5);
    assert_eq!(hit(&app, viewer), Some((ia, HitRegion::Content)));
    hover(&mut app, viewer, 0, 5);
    assert_eq!(hit(&app, viewer), Some((ia, HitRegion::Border(Side::Left))));
    hover(&mut app, viewer, 39, 5);
    assert_eq!(
        hit(&app, viewer),
        Some((ia, HitRegion::Border(Side::Right)))
    );
    hover(&mut app, viewer, 10, 11);
    assert_eq!(
        hit(&app, viewer),
        Some((ia, HitRegion::Border(Side::Bottom)))
    );
    hover(&mut app, viewer, 10, 12);
    assert_eq!(hit(&app, viewer), Some((ib, HitRegion::Border(Side::Top))));
    hover(&mut app, viewer, 10, 23);
    assert_eq!(
        hit(&app, viewer),
        Some((ib, HitRegion::Border(Side::Bottom)))
    );
    // Corners resolve to the nearer edge; a tie goes to Left/Top.
    hover(&mut app, viewer, 0, 0);
    assert_eq!(hit(&app, viewer), Some((ia, HitRegion::Border(Side::Left))));
    hover(&mut app, viewer, 79, 23);
    assert_eq!(
        hit(&app, viewer),
        Some((ic, HitRegion::Border(Side::Right)))
    );
    hover(&mut app, viewer, 60, 12);
    assert_eq!(hit(&app, viewer), Some((ic, HitRegion::Content)));
    hover(&mut app, viewer, 80, 12);
    assert_eq!(hit(&app, viewer), None, "outside the viewport hits nothing");
}

#[test]
fn grid_and_absolute_hits_follow_the_stack_order() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root) = workspace(world);
    let patch = NodePatch {
        display: Some("grid".to_owned()),
        grid_template_columns: Some(vec![GridTrackPatch {
            repeat: 2,
            track: "1fr".to_owned(),
        }]),
        grid_template_rows: Some(vec![GridTrackPatch {
            repeat: 2,
            track: "1fr".to_owned(),
        }]),
        ..Default::default()
    };
    ops::patch_node(world, root, &patch).unwrap();
    let cells: Vec<Entity> = (0..4)
        .map(|_| {
            ops::spawn_node(
                world,
                root,
                None,
                Node {
                    border: UiRect::all(Val::Px(1.0)),
                    ..Default::default()
                },
                Some(shell()),
            )
            .unwrap()
        })
        .collect();
    // A floating pane over the bottom-right cell.
    let overlay = ops::spawn_node(
        world,
        root,
        None,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(50.0),
            top: Val::Px(14.0),
            width: Val::Px(20.0),
            height: Val::Px(6.0),
            ..Default::default()
        },
        Some(shell()),
    )
    .unwrap();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    let br = instance_of(app.world_mut(), viewer, cells[3]);
    let tr = instance_of(app.world_mut(), viewer, cells[1]);
    let io = instance_of(app.world_mut(), viewer, overlay);
    assert_eq!(size_of(app.world(), br), UVec2::new(40, 12));

    hover(&mut app, viewer, 45, 13);
    assert_eq!(hit(&app, viewer), Some((br, HitRegion::Content)));
    hover(&mut app, viewer, 40, 13);
    assert_eq!(hit(&app, viewer), Some((br, HitRegion::Border(Side::Left))));
    hover(&mut app, viewer, 45, 11);
    assert_eq!(
        hit(&app, viewer),
        Some((tr, HitRegion::Border(Side::Bottom)))
    );
    // The absolute pane is above the grid cell it covers.
    hover(&mut app, viewer, 55, 16);
    assert_eq!(hit(&app, viewer), Some((io, HitRegion::Content)));
    hover(&mut app, viewer, 75, 22);
    assert_eq!(hit(&app, viewer), Some((br, HitRegion::Content)));
}

#[test]
fn zoomed_instances_hit_the_zoomed_pane_everywhere() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, _, left, right) = two_pane_row(world);
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    let ir = instance_of(app.world_mut(), viewer, right);
    hover(&mut app, viewer, 10, 5);
    assert_eq!(
        hit(&app, viewer).map(|h| h.0),
        Some(instance_of(app.world_mut(), viewer, left))
    );
    ops::zoom(app.world_mut(), viewer, right).unwrap();
    app.update();
    check(&mut app);
    assert_eq!(size_of(app.world(), ir), UVec2::new(80, 24));
    hover(&mut app, viewer, 10, 5);
    assert_eq!(hit(&app, viewer), Some((ir, HitRegion::Content)));
    ops::unzoom(app.world_mut(), viewer).unwrap();
    app.update();
    check(&mut app);
    hover(&mut app, viewer, 10, 6);
    assert_eq!(
        hit(&app, viewer).map(|h| h.0),
        Some(instance_of(app.world_mut(), viewer, left))
    );
}

#[test]
fn two_viewers_with_different_viewports_hit_their_own_instances() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, _, left, right) = two_pane_row(world);
    let small = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    let wide = ops::attach_viewer(world, ws, viewport(30, 120), None).unwrap();
    app.update();
    check(&mut app);
    let small_left = instance_of(app.world_mut(), small, left);
    let small_right = instance_of(app.world_mut(), small, right);
    let wide_left = instance_of(app.world_mut(), wide, left);
    let wide_right = instance_of(app.world_mut(), wide, right);
    assert_eq!(size_of(app.world(), small_left), UVec2::new(40, 24));
    assert_eq!(size_of(app.world(), wide_left), UVec2::new(60, 30));

    // Column 50 is the right pane for the small viewer and still the left one for the wide.
    hover(&mut app, small, 50, 5);
    hover(&mut app, wide, 50, 5);
    assert_eq!(hit(&app, small), Some((small_right, HitRegion::Content)));
    assert_eq!(hit(&app, wide), Some((wide_left, HitRegion::Content)));
    hover(&mut app, wide, 100, 27);
    assert_eq!(hit(&app, wide), Some((wide_right, HitRegion::Content)));
    hover(&mut app, small, 100, 27);
    assert_eq!(hit(&app, small), None);
    let world = app.world();
    let wide_hit = world
        .resource::<HoverMap>()
        .get(&pointer_id(world, wide))
        .and_then(|h| h.get(&wide_right))
        .unwrap();
    assert_eq!(wide_hit.camera, world.get::<ViewerCamera>(wide).unwrap().0);
    assert_eq!(
        wide_hit.position.map(|p| p.truncate()),
        Some(bevy_math::Vec2::new(100.5, 27.5))
    );
}

#[test]
fn press_on_a_leaf_targets_its_pane() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, _, left, right) = two_pane_row(world);
    let (pl, pr) = (pane_of(world, left), pane_of(world, right));
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    assert_eq!(app.world().get::<Targets>(viewer).map(|t| t.0), Some(pl));
    hover(&mut app, viewer, 60, 5);
    mouse(
        &mut app,
        viewer,
        60,
        5,
        PointerKind::Press,
        PointerButton::Left,
        0,
    );
    assert_eq!(app.world().get::<Targets>(viewer).map(|t| t.0), Some(pr));
}

#[test]
fn border_drag_resizes_the_template_and_every_viewer_relayouts() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root, left, right) = two_pane_row(world);
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    let other = ops::attach_viewer(world, ws, viewport(30, 120), None).unwrap();
    app.update();
    check(&mut app);
    let generation = app.world().get::<LayoutGeneration>(root).unwrap().0;
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(40, 24));

    // The left pane's right border is column 39; dragging it 10 cells right widens the left pane.
    drag(&mut app, viewer, (39, 10), (49, 10), 0);
    let world = app.world();
    assert_eq!(
        flex_grow(world, left),
        48.0,
        "flex_grow is the target width minus the two border cells"
    );
    assert_eq!(flex_grow(world, right), 28.0);
    assert_eq!(world.get::<Node>(left).unwrap().flex_basis, Val::Px(0.0));
    assert!(world.get::<LayoutGeneration>(root).unwrap().0 > generation);
    assert!(
        world
            .get::<PointerDrag>(pointer_entity(world, viewer))
            .is_none(),
        "DragEnd clears the drag"
    );
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(50, 24));
    assert_eq!(instance_size(&mut app, viewer, right), UVec2::new(30, 24));
    // The other viewer shares the template and lays it out at its own width.
    assert_eq!(instance_size(&mut app, other, left), UVec2::new(75, 30));
    assert_eq!(instance_size(&mut app, other, right), UVec2::new(45, 30));

    // The minimum pane size bounds a drag: the right pane keeps `MIN_PANE_COLS` of content.
    drag(&mut app, viewer, (49, 10), (79, 10), 0);
    let world = app.world();
    assert_eq!(flex_grow(world, right), f32::from(MIN_PANE_COLS));
    assert_eq!(
        flex_grow(world, left),
        80.0 - f32::from(MIN_PANE_COLS) - 4.0
    );
    assert_eq!(
        instance_size(&mut app, viewer, right),
        UVec2::new(u32::from(MIN_PANE_COLS) + 2, 24)
    );
}

#[test]
fn border_drag_walks_up_to_the_axis_that_has_the_edge() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root) = workspace(world);
    // Column [ Row [ a, b ], c ]: dragging a's bottom border resizes the row against c.
    let row = ops::spawn_node(
        world,
        root,
        None,
        Node {
            flex_direction: FlexDirection::Row,
            flex_grow: 1.0,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    ops::patch_node(
        world,
        root,
        &NodePatch {
            flex_direction: Some("column".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let a = ops::spawn_node(world, row, None, framed(), Some(shell())).unwrap();
    let _b = ops::spawn_node(world, row, None, framed(), Some(shell())).unwrap();
    let c = ops::spawn_node(world, root, None, framed(), Some(shell())).unwrap();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    assert_eq!(instance_size(&mut app, viewer, a), UVec2::new(40, 12));
    drag(&mut app, viewer, (10, 11), (10, 17), 0);
    let world = app.world();
    assert_eq!(flex_grow(world, row), 18.0, "the row has no insets");
    assert_eq!(flex_grow(world, c), 4.0, "6 cells minus two border cells");
    assert_eq!(flex_grow(world, a), 1.0, "the row's children are untouched");
    assert_eq!(instance_size(&mut app, viewer, a), UVec2::new(40, 18));
}

#[test]
fn border_drag_on_a_grid_writes_px_tracks() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root) = workspace(world);
    ops::patch_node(
        world,
        root,
        &NodePatch {
            display: Some("grid".to_owned()),
            grid_template_columns: Some(vec![GridTrackPatch {
                repeat: 2,
                track: "1fr".to_owned(),
            }]),
            grid_template_rows: Some(vec![GridTrackPatch {
                repeat: 2,
                track: "1fr".to_owned(),
            }]),
            ..Default::default()
        },
    )
    .unwrap();
    let cells: Vec<Entity> = (0..4)
        .map(|_| {
            ops::spawn_node(
                world,
                root,
                None,
                Node {
                    border: UiRect::all(Val::Px(1.0)),
                    ..Default::default()
                },
                Some(shell()),
            )
            .unwrap()
        })
        .collect();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    // Top-left cell's right border (column 39) dragged 10 left: columns become 30px / 50px.
    drag(&mut app, viewer, (39, 5), (29, 5), 0);
    let world = app.world();
    let node = world.get::<Node>(root).unwrap();
    assert_eq!(
        node.grid_template_columns,
        vec![
            RepeatedGridTrack::px(1, 30.0),
            RepeatedGridTrack::px(1, 50.0)
        ]
    );
    assert_eq!(
        node.grid_template_rows,
        vec![RepeatedGridTrack::flex(2, 1.0)],
        "rows are untouched"
    );
    assert_eq!(
        instance_size(&mut app, viewer, cells[0]),
        UVec2::new(30, 12)
    );
    assert_eq!(
        instance_size(&mut app, viewer, cells[3]),
        UVec2::new(50, 12)
    );
}

#[test]
fn alt_drag_drop_exchanges_leaves_and_a_border_drop_places_beside() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root, left, right) = two_pane_row(world);
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    assert_eq!(kids(app.world(), root), vec![left, right]);

    // A plain drag between contents is not a move.
    drag(&mut app, viewer, (10, 10), (60, 10), 0);
    assert_eq!(kids(app.world(), root), vec![left, right]);

    drag(&mut app, viewer, (10, 10), (60, 10), PointerEvent::ALT);
    assert_eq!(
        kids(app.world(), root),
        vec![right, left],
        "contents exchanged"
    );
    assert!(
        app.world()
            .get::<PointerDrag>(pointer_entity(app.world(), viewer))
            .is_none()
    );
    assert_eq!(instance_size(&mut app, viewer, right), UVec2::new(40, 24));

    // Dropped on the bottom border of the (now left) `right` leaf: a column wraps it.
    drag(&mut app, viewer, (60, 10), (10, 23), PointerEvent::ALT);
    let world = app.world();
    let top = kids(world, root);
    assert_eq!(top.len(), 1, "one container remains at the top: {top:?}");
    let column = top[0];
    assert_eq!(
        world.get::<Node>(column).unwrap().flex_direction,
        FlexDirection::Column
    );
    assert_eq!(kids(world, column), vec![right, left]);
    assert_eq!(instance_size(&mut app, viewer, left), UVec2::new(80, 12));
}

#[test]
fn wheel_scrolls_a_scroll_container_and_pane_history() {
    let mut app = app();
    let world = app.world_mut();
    let (ws, root) = workspace(world);
    // Row [ pane, list(overflow scroll) [ tall content ] ].
    let leaf = ops::spawn_node(world, root, None, grow(), Some(shell())).unwrap();
    let list = ops::spawn_node(
        world,
        root,
        None,
        Node {
            flex_grow: 1.0,
            flex_direction: FlexDirection::Column,
            overflow: Overflow::scroll_y(),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let tall = ops::spawn_node(
        world,
        list,
        None,
        Node {
            height: Val::Px(100.0),
            flex_shrink: 0.0,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    check(&mut app);
    let ilist = instance_of(app.world_mut(), viewer, list);
    let itall = instance_of(app.world_mut(), viewer, tall);
    assert_eq!(size_of(app.world(), itall), UVec2::new(40, 100));

    hover(&mut app, viewer, 60, 5);
    assert_eq!(
        hit(&app, viewer).map(|h| h.0),
        Some(itall),
        "the content is the hit"
    );
    for _ in 0..3 {
        mouse(
            &mut app,
            viewer,
            60,
            5,
            PointerKind::ScrollDown,
            PointerButton::None,
            0,
        );
    }
    let world = app.world();
    assert_eq!(
        world.get::<ScrollPosition>(ilist).map(|p| p.0.y),
        Some(3.0),
        "three lines down"
    );
    assert_eq!(
        world.get::<ComputedNode>(ilist).unwrap().scroll_position.y,
        3.0
    );
    mouse(
        &mut app,
        viewer,
        60,
        5,
        PointerKind::ScrollUp,
        PointerButton::None,
        0,
    );
    assert_eq!(
        app.world().get::<ScrollPosition>(ilist).map(|p| p.0.y),
        Some(2.0)
    );

    // Over a pane that does not report the mouse the wheel scrolls its history.
    let ileaf = instance_of(app.world_mut(), viewer, leaf);
    hover(&mut app, viewer, 10, 5);
    mouse(
        &mut app,
        viewer,
        10,
        5,
        PointerKind::ScrollDown,
        PointerButton::None,
        0,
    );
    let world = app.world();
    assert_eq!(world.get::<ScrollPosition>(ileaf).map(|p| p.0.y), Some(1.0));
    let id = *world.get::<NodeId>(leaf).unwrap();
    assert_eq!(
        world
            .get::<fux::layout::ViewState>(viewer)
            .unwrap()
            .scroll
            .get(&id),
        Some(&1.0)
    );
}

/// Bootstraps `default` with one live pane in SGR button-tracking mode; `(app, viewer, pane)`.
fn live_mouse_pane(modes: &[u8]) -> (App, Entity, Entity) {
    let mut app = app();
    let world = app.world_mut();
    fux::lifecycle::bootstrap(world, "default", &["/bin/sh".into()]).unwrap();
    let ws = world.resource::<Ids>().workspace("default").unwrap();
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    app.update();
    let pane = app
        .world_mut()
        .query_filtered::<Entity, (
            With<Pane>,
            bevy_ecs::query::Allow<bevy_ecs::entity_disabling::Disabled>,
        )>()
        .single(app.world())
        .unwrap();
    app.world_mut()
        .write_message(Inbound::PaneSpawned { pane, pid: 7 });
    app.update();
    app.world_mut().write_message(Inbound::PaneOutput {
        pane,
        bytes: modes.to_vec(),
    });
    app.update();
    check(&mut app);
    drain_effects(&mut app);
    (app, viewer, pane)
}

#[test]
fn panes_reporting_the_mouse_receive_translated_events() {
    let (mut app, viewer, pane) = live_mouse_pane(b"\x1b[?1002h\x1b[?1006h");
    hover(&mut app, viewer, 10, 5);
    mouse(
        &mut app,
        viewer,
        10,
        5,
        PointerKind::Press,
        PointerButton::Left,
        0,
    );
    mouse(
        &mut app,
        viewer,
        12,
        6,
        PointerKind::Move,
        PointerButton::Left,
        0,
    );
    mouse(
        &mut app,
        viewer,
        12,
        6,
        PointerKind::Release,
        PointerButton::Left,
        PointerEvent::SHIFT,
    );
    mouse(
        &mut app,
        viewer,
        12,
        6,
        PointerKind::ScrollUp,
        PointerButton::None,
        0,
    );
    mouse(
        &mut app,
        viewer,
        14,
        6,
        PointerKind::Move,
        PointerButton::None,
        0,
    );
    let writes = pty_writes(&drain_effects(&mut app), pane);
    assert_eq!(
        writes,
        vec![
            b"\x1b[<0;11;6M".to_vec(),
            b"\x1b[<32;13;7M".to_vec(),
            b"\x1b[<4;13;7m".to_vec(),
            b"\x1b[<64;13;7M".to_vec(),
        ],
        "press, drag, shift-release, wheel; hover motion is not reported in button mode"
    );
    // The wheel did not scroll history: the pane owns it.
    let ileaf = app
        .world_mut()
        .query_filtered::<Entity, (With<InstanceNode>, With<Shows>)>()
        .single(app.world())
        .unwrap();
    assert_eq!(
        app.world().get::<ScrollPosition>(ileaf).map(|p| p.0.y),
        Some(0.0)
    );
}

#[test]
fn any_motion_mode_reports_each_movement_once() {
    let (mut app, viewer, pane) = live_mouse_pane(b"\x1b[?1003h\x1b[?1006h");
    hover(&mut app, viewer, 10, 5);
    drain_effects(&mut app);
    hover(&mut app, viewer, 11, 5);
    mouse(
        &mut app,
        viewer,
        11,
        5,
        PointerKind::Press,
        PointerButton::Left,
        0,
    );
    mouse(
        &mut app,
        viewer,
        12,
        6,
        PointerKind::Move,
        PointerButton::Left,
        0,
    );
    mouse(
        &mut app,
        viewer,
        12,
        6,
        PointerKind::Release,
        PointerButton::Left,
        0,
    );
    mouse(
        &mut app,
        viewer,
        14,
        6,
        PointerKind::Move,
        PointerButton::None,
        0,
    );
    assert_eq!(
        pty_writes(&drain_effects(&mut app), pane),
        vec![
            b"\x1b[<35;12;6M".to_vec(),
            b"\x1b[<0;12;6M".to_vec(),
            b"\x1b[<32;13;7M".to_vec(),
            b"\x1b[<0;13;7m".to_vec(),
            b"\x1b[<35;15;7M".to_vec(),
        ],
        "hover motion, press, one drag motion (not a second hover), release, hover motion"
    );
}

#[test]
fn right_click_policy_gates_the_secondary_button() {
    let (mut app, viewer, pane) = live_mouse_pane(b"\x1b[?1000h");
    hover(&mut app, viewer, 3, 2);
    let click = |app: &mut App| {
        mouse(
            app,
            viewer,
            3,
            2,
            PointerKind::Press,
            PointerButton::Right,
            0,
        );
        mouse(
            app,
            viewer,
            3,
            2,
            PointerKind::Release,
            PointerButton::Right,
            0,
        );
        pty_writes(&drain_effects(app), pane)
    };
    // Auto with mouse reporting on forwards (X10 encoding: the pane never asked for SGR).
    assert_eq!(
        click(&mut app),
        vec![b"\x1b[M\"$#".to_vec(), b"\x1b[M#$#".to_vec()]
    );
    assert!(app.world().get::<Notice>(viewer).is_none());
    app.world_mut()
        .entity_mut(pane)
        .insert(RightClickPolicy::Paste);
    assert!(click(&mut app).is_empty(), "Paste never reaches the pane");
    assert!(
        app.world().get::<Notice>(viewer).is_some(),
        "the viewer is told the paste is not available"
    );
    app.world_mut()
        .entity_mut(pane)
        .insert(RightClickPolicy::Forward);
    assert_eq!(click(&mut app).len(), 2);
    // Left presses are unaffected by the policy.
    mouse(
        &mut app,
        viewer,
        3,
        2,
        PointerKind::Press,
        PointerButton::Left,
        0,
    );
    assert_eq!(
        pty_writes(&drain_effects(&mut app), pane),
        vec![b"\x1b[M $#".to_vec()]
    );
}

#[test]
fn no_system_reads_hovered_or_directly_hovered() {
    fn visit(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, usize, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                for (i, line) in source.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    let mentions = |ident: &str| {
                        code.match_indices(ident).any(|(at, _)| {
                            let before = code.get(..at).and_then(|s| s.chars().next_back());
                            let after = code.get(at + ident.len()..).and_then(|s| s.chars().next());
                            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
                        })
                    };
                    if mentions("Hovered") || mentions("DirectlyHovered") {
                        out.push((path.clone(), i + 1, line.to_owned()));
                    }
                }
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    visit(&src, &mut found);
    assert!(
        found.is_empty(),
        "`Hovered`/`DirectlyHovered` are hard-wired to PointerId::Mouse; hover state comes from HoverMap: {found:?}"
    );
}
