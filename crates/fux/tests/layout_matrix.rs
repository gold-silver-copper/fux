//! Cell-exact geometry of the layouts prompt 3.4 promises — flex rows/columns, minimums, grid,
//! flex ratios, absolute overlays, scroll containers — at four viewports, laid out by the real
//! `bevy_ui`, plus the old `layout.rs` split arithmetic as an oracle for the default splits.
//!
//! Rects are `(x, y, w, h)` in cells from `ComputedNode.size` and the `UiGlobalTransform` centre.
//! Taffy rounds every edge to a whole cell, so pane rects tile the viewport with no gap; the old
//! layout left one separator cell between siblings (`split_rect` below).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns; cell coordinates are whole numbers by construction"
)]

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ui::prelude::*;
use bevy_ui::{Display, UiGlobalTransform, UiTargetCamera};
use fux::layout::{GridTrackPatch, LayoutPlugin, NodePatch, ops};
use fux::model::invariants::check_invariants;
use fux::model::*;

const VIEWPORTS: [(u16, u16); 4] = [(24, 80), (40, 120), (12, 40), (50, 200)];

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        bevy_state::app::StatesPlugin,
        bevy_time::TimePlugin,
        bevy_asset::AssetPlugin::default(),
        ModelPlugin,
        LayoutPlugin,
    ));
    app
}

fn shell() -> PaneTemplate {
    PaneTemplate {
        argv: vec!["sh".to_owned()],
        ..Default::default()
    }
}

fn grow(grow: f32) -> Node {
    Node {
        flex_grow: grow,
        ..Default::default()
    }
}

fn leaf(world: &mut World, parent: Entity, node: Node) -> Entity {
    let leaf = ops::spawn_node(world, parent, None, node, Some(shell())).unwrap();
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

fn leaf_showing(world: &World, root: Entity, pane: Entity) -> Entity {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<Shows>(e).map(|s| s.0) == Some(pane) {
            return e;
        }
        if let Some(children) = world.get::<Children>(e) {
            stack.extend(children.iter());
        }
    }
    panic!("no instance leaf shows {pane}");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

const fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

/// The laid-out cell rect of an instance node; every edge must land on a whole cell.
fn rect_of(world: &World, node: Entity) -> Rect {
    let size = world.get::<ComputedNode>(node).unwrap().size;
    let centre = world.get::<UiGlobalTransform>(node).unwrap().translation;
    let min = centre - size / 2.0;
    assert_eq!(
        min.fract(),
        bevy_math::Vec2::ZERO,
        "{node} is not cell aligned"
    );
    assert_eq!(
        size.fract(),
        bevy_math::Vec2::ZERO,
        "{node} is not cell sized"
    );
    rect(min.x as i32, min.y as i32, size.x as u32, size.y as u32)
}

fn pane_size(world: &World, pane: Entity) -> PaneSize {
    *world.get::<PaneSize>(pane).unwrap()
}

/// Builds a case in a fresh app for every viewport and hands the laid-out world, the instance
/// root, the viewer and the panes the builder returned to `assert`.
fn matrix(
    build: impl Fn(&mut World, Entity) -> Vec<Entity>,
    assert: impl Fn(&mut App, Entity, Entity, &[Entity], u16, u16),
) {
    for (rows, cols) in VIEWPORTS {
        let mut app = app();
        let world = app.world_mut();
        let ws = ops::new_workspace(world, "matrix").unwrap();
        let root = ops::new_root(world, ws, "root").unwrap();
        let panes = build(world, root);
        let viewer = ops::attach_viewer(world, ws, Viewport { rows, cols }, None).unwrap();
        app.update();
        check_invariants(app.world_mut()).unwrap();
        let instance = instance_root(app.world_mut(), viewer);
        assert_eq!(
            rect_of(app.world(), instance),
            rect(0, 0, u32::from(cols), u32::from(rows))
        );
        assert(&mut app, instance, viewer, &panes, rows, cols);
    }
}

fn assert_rects(world: &World, instance: Entity, panes: &[Entity], expected: &[Rect]) {
    let actual: Vec<Rect> = panes
        .iter()
        .map(|p| rect_of(world, leaf_showing(world, instance, *p)))
        .collect();
    assert_eq!(actual, expected);
}

/// The old `layout.rs` arithmetic (oracle): `split_rect` at the default 5000/10000 ratio keeps one
/// separator cell between the halves, the first half is `floor((extent - 1) / 2)`.
fn old_split(r: Rect, horizontal: bool) -> (Rect, Rect) {
    let extent = if horizontal { r.w } else { r.h };
    let gap = u32::from(extent > 0);
    let usable = extent.saturating_sub(gap);
    let first = usable * 5000 / 10000;
    let second = usable - first;
    let start = (first + gap) as i32;
    if horizontal {
        (
            Rect { w: first, ..r },
            Rect {
                x: r.x + start,
                w: second,
                ..r
            },
        )
    } else {
        (
            Rect { h: first, ..r },
            Rect {
                y: r.y + start,
                h: second,
                ..r
            },
        )
    }
}

/// The documented rounding rule between the two: taffy hands the separator cell to the first
/// half, the second half is identical.
fn absorbs_separator(new: Rect, old: Rect, horizontal: bool) -> bool {
    if horizontal {
        new == Rect {
            w: old.w + 1,
            ..old
        }
    } else {
        new == Rect {
            h: old.h + 1,
            ..old
        }
    }
}

#[test]
fn two_column_flex_halves_the_width_and_matches_the_old_split_plus_separator() {
    matrix(
        |world, root| vec![leaf(world, root, grow(1.0)), leaf(world, root, grow(1.0))],
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let world = app.world();
            let expected = [rect(0, 0, w / 2, h), rect((w / 2) as i32, 0, w / 2, h)];
            assert_rects(world, instance, panes, &expected);
            let (old_a, old_b) = old_split(rect(0, 0, w, h), true);
            assert!(absorbs_separator(expected[0], old_a, true), "{cols}x{rows}");
            assert_eq!(expected[1], old_b, "{cols}x{rows}");
            for pane in panes {
                assert_eq!(
                    pane_size(world, *pane),
                    PaneSize {
                        rows,
                        cols: cols / 2
                    }
                );
            }
        },
    );
}

#[test]
fn two_row_flex_halves_the_height_and_matches_the_old_split_plus_separator() {
    matrix(
        |world, root| {
            let patch = NodePatch {
                flex_direction: Some("column".to_owned()),
                ..Default::default()
            };
            ops::patch_node(world, root, &patch).unwrap();
            vec![leaf(world, root, grow(1.0)), leaf(world, root, grow(1.0))]
        },
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let world = app.world();
            let expected = [rect(0, 0, w, h / 2), rect(0, (h / 2) as i32, w, h / 2)];
            assert_rects(world, instance, panes, &expected);
            let (old_a, old_b) = old_split(rect(0, 0, w, h), false);
            assert!(
                absorbs_separator(expected[0], old_a, false),
                "{cols}x{rows}"
            );
            assert_eq!(expected[1], old_b, "{cols}x{rows}");
            for pane in panes {
                assert_eq!(
                    pane_size(world, *pane),
                    PaneSize {
                        rows: rows / 2,
                        cols
                    }
                );
            }
        },
    );
}

#[test]
fn three_columns_with_a_minimum_width_round_thirds_and_never_shrink_below_it() {
    matrix(
        |world, root| {
            let node = || Node {
                min_width: Val::Px(20.0),
                ..grow(1.0)
            };
            vec![
                leaf(world, root, node()),
                leaf(world, root, node()),
                leaf(world, root, node()),
            ]
        },
        |app, instance, _, panes, rows, cols| {
            let h = u32::from(rows);
            let widths: [u32; 3] = match cols {
                // 26.67 per column: edges round to 0, 27, 53, 80.
                80 => [27, 26, 27],
                120 => [40, 40, 40],
                // 13.33 per column is below the minimum: the row overflows its viewport.
                40 => [20, 20, 20],
                // 66.67 per column: edges round to 0, 67, 133, 200.
                200 => [67, 66, 67],
                other => panic!("unexpected viewport width {other}"),
            };
            let mut x = 0;
            let expected: Vec<Rect> = widths
                .iter()
                .map(|w| {
                    let r = rect(x, 0, *w, h);
                    x += *w as i32;
                    r
                })
                .collect();
            let world = app.world();
            assert_rects(world, instance, panes, &expected);
            for (pane, w) in panes.iter().zip(widths) {
                assert_eq!(
                    pane_size(world, *pane),
                    PaneSize {
                        rows,
                        cols: w as u16
                    }
                );
            }
        },
    );
}

#[test]
fn nested_pair_of_rows_matches_the_old_binary_split_tree_plus_separators() {
    // The old model was a binary tree: `split(1, 2)` then `split(2, 3)` nests the second split
    // inside the second half. The same composition as explicit containers.
    matrix(
        |world, root| {
            let first = leaf(world, root, grow(1.0));
            let inner = ops::spawn_node(world, root, None, grow(1.0), None).unwrap();
            let second = leaf(world, inner, grow(1.0));
            let third = leaf(world, inner, grow(1.0));
            vec![first, second, third]
        },
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let (old_a, old_b) = old_split(rect(0, 0, w, h), true);
            let (old_b1, old_b2) = old_split(old_b, true);
            let world = app.world();
            let actual: Vec<Rect> = panes
                .iter()
                .map(|p| rect_of(world, leaf_showing(world, instance, *p)))
                .collect();
            assert_eq!(
                actual,
                [
                    rect(0, 0, w / 2, h),
                    rect((w / 2) as i32, 0, w / 4, h),
                    rect((w / 2 + w / 4) as i32, 0, w / 4, h)
                ]
            );
            assert!(absorbs_separator(actual[0], old_a, true), "{cols}x{rows}");
            assert!(absorbs_separator(actual[1], old_b1, true), "{cols}x{rows}");
            assert_eq!(actual[2], old_b2, "{cols}x{rows}");
        },
    );
}

#[test]
fn default_split_twice_flattens_into_equal_siblings_unlike_the_old_nesting() {
    // `ops::split` on a leaf whose parent already flexes along the axis adds a sibling, so two
    // right-splits give three equal columns (27|26|27 at 80 cols) where the old binary tree gave
    // 39|19|20 with two separator cells.
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "flat").unwrap();
    let root = ops::new_root(world, ws, "root").unwrap();
    let pane1 = leaf(world, root, grow(1.0));
    let (_, pane2) = ops::split(world, pane1, SplitDirection::Right, shell()).unwrap();
    let (_, pane3) = ops::split(world, pane2, SplitDirection::Right, shell()).unwrap();
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        world.get::<Children>(root).map(|c| c.len()),
        Some(3),
        "three siblings under the root"
    );
    assert_rects(
        world,
        instance,
        &[pane1, pane2, pane3],
        &[rect(0, 0, 27, 24), rect(27, 0, 26, 24), rect(53, 0, 27, 24)],
    );
    let (old_a, old_b) = old_split(rect(0, 0, 80, 24), true);
    let (old_b1, old_b2) = old_split(old_b, true);
    assert_eq!(
        [old_a, old_b1, old_b2],
        [rect(0, 0, 39, 24), rect(40, 0, 19, 24), rect(60, 0, 20, 24)]
    );
}

#[test]
fn grid_2x2_quarters_the_viewport() {
    matrix(
        |world, root| {
            let track = |repeat| GridTrackPatch {
                repeat,
                track: "1fr".to_owned(),
            };
            let patch = NodePatch {
                display: Some("grid".to_owned()),
                grid_template_columns: Some(vec![track(2)]),
                grid_template_rows: Some(vec![track(2)]),
                ..Default::default()
            };
            ops::patch_node(world, root, &patch).unwrap();
            (0..4).map(|_| leaf(world, root, Node::default())).collect()
        },
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols) / 2, u32::from(rows) / 2);
            let world = app.world();
            assert_rects(
                world,
                instance,
                panes,
                &[
                    rect(0, 0, w, h),
                    rect(w as i32, 0, w, h),
                    rect(0, h as i32, w, h),
                    rect(w as i32, h as i32, w, h),
                ],
            );
            for pane in panes {
                assert_eq!(
                    pane_size(world, *pane),
                    PaneSize {
                        rows: rows / 2,
                        cols: cols / 2
                    }
                );
            }
        },
    );
}

#[test]
fn grid_placement_patch_spans_columns() {
    // A leaf placed explicitly on the second row spanning both columns; the two auto-placed
    // leaves fill the first row.
    matrix(
        |world, root| {
            let track = |repeat| GridTrackPatch {
                repeat,
                track: "1fr".to_owned(),
            };
            let patch = NodePatch {
                display: Some("grid".to_owned()),
                grid_template_columns: Some(vec![track(2)]),
                grid_template_rows: Some(vec![track(2)]),
                ..Default::default()
            };
            ops::patch_node(world, root, &patch).unwrap();
            let wide = ops::spawn_node(world, root, None, Node::default(), Some(shell())).unwrap();
            let placement: NodePatch = serde_json::from_str(
                r#"{"grid_row": {"start": 2}, "grid_column": {"start": 1, "span": 2}}"#,
            )
            .unwrap();
            ops::patch_node(world, wide, &placement).unwrap();
            let a = leaf(world, root, Node::default());
            let b = leaf(world, root, Node::default());
            vec![world.get::<Places>(wide).unwrap().0, a, b]
        },
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            assert_rects(
                app.world(),
                instance,
                panes,
                &[
                    rect(0, (h / 2) as i32, w, h / 2),
                    rect(0, 0, w / 2, h / 2),
                    rect((w / 2) as i32, 0, w / 2, h / 2),
                ],
            );
        },
    );
}

#[test]
fn main_and_sidebar_share_three_to_one() {
    matrix(
        |world, root| vec![leaf(world, root, grow(3.0)), leaf(world, root, grow(1.0))],
        |app, instance, _, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let main = w * 3 / 4;
            let world = app.world();
            assert_rects(
                world,
                instance,
                panes,
                &[rect(0, 0, main, h), rect(main as i32, 0, w - main, h)],
            );
            assert_eq!(
                pane_size(world, panes[0]),
                PaneSize {
                    rows,
                    cols: main as u16
                }
            );
            assert_eq!(
                pane_size(world, panes[1]),
                PaneSize {
                    rows,
                    cols: (w - main) as u16
                }
            );
        },
    );
}

#[test]
fn a_framed_and_padded_leaf_gives_its_pane_the_content_box() {
    // Border 1 and padding 2 on every side: the leaf tiles the viewport, the pane gets six
    // cells less each way.
    matrix(
        |world, root| {
            vec![leaf(
                world,
                root,
                Node {
                    flex_grow: 1.0,
                    border: UiRect::all(Val::Px(1.0)),
                    padding: UiRect::all(Val::Px(2.0)),
                    ..Default::default()
                },
            )]
        },
        |app, instance, _, panes, rows, cols| {
            let world = app.world();
            assert_rects(
                world,
                instance,
                panes,
                &[rect(0, 0, u32::from(cols), u32::from(rows))],
            );
            assert_eq!(
                pane_size(world, panes[0]),
                PaneSize {
                    rows: rows - 6,
                    cols: cols - 6
                }
            );
        },
    );
}

#[test]
fn absolute_overlay_floats_at_its_inset_above_the_base_pane() {
    matrix(
        |world, root| {
            let base = leaf(world, root, grow(1.0));
            let overlay =
                ops::spawn_node(world, root, None, Node::default(), Some(shell())).unwrap();
            let patch: NodePatch = serde_json::from_str(
                r#"{"position_type": "absolute", "inset": {"left": "2px", "top": "2px"},
                    "width": "50%", "height": "50%", "z_index": 1}"#,
            )
            .unwrap();
            ops::patch_node(world, overlay, &patch).unwrap();
            vec![base, world.get::<Places>(overlay).unwrap().0]
        },
        |app, instance, viewer, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let world = app.world();
            assert_rects(
                world,
                instance,
                panes,
                &[rect(0, 0, w, h), rect(2, 2, w / 2, h / 2)],
            );
            assert_eq!(pane_size(world, panes[0]), PaneSize { rows, cols });
            assert_eq!(
                pane_size(world, panes[1]),
                PaneSize {
                    rows: rows / 2,
                    cols: cols / 2
                }
            );
            // The overlay is on top where it floats; the base shows through elsewhere.
            assert_eq!(ops::pane_at(world, viewer, 3, 3), Some(panes[1]));
            assert_eq!(
                ops::pane_at(world, viewer, cols - 1, rows - 1),
                Some(panes[0])
            );
            assert_eq!(ops::pane_at(world, viewer, 1, 1), Some(panes[0]));
        },
    );
}

#[test]
fn scroll_container_clips_rows_to_the_viewport_and_scroll_position_moves_them() {
    const ROW: u32 = 5;
    const ROWS: u32 = 10;
    matrix(
        |world, root| {
            let column = NodePatch {
                flex_direction: Some("column".to_owned()),
                ..Default::default()
            };
            ops::patch_node(world, root, &column).unwrap();
            let container = ops::spawn_node(
                world,
                root,
                None,
                Node {
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(),
                    ..grow(1.0)
                },
                None,
            )
            .unwrap();
            let mut panes: Vec<Entity> = (0..ROWS)
                .map(|_| {
                    leaf(
                        world,
                        container,
                        Node {
                            height: Val::Px(ROW as f32),
                            flex_shrink: 0.0,
                            ..Node::default()
                        },
                    )
                })
                .collect();
            // The container itself rides along as the last "pane" slot's parent for scrolling.
            panes.push(container);
            panes
        },
        |app, instance, viewer, panes, rows, cols| {
            let (w, h) = (u32::from(cols), u32::from(rows));
            let (row_panes, container) = (&panes[..ROWS as usize], panes[ROWS as usize]);
            let visible = |offset: u32, i: u32| -> Option<u16> {
                let top = (i * ROW) as i32 - offset as i32;
                let bottom = top + ROW as i32;
                let seen = bottom.min(h as i32) - top.max(0);
                (seen >= i32::from(MIN_PANE_ROWS)).then_some(seen as u16)
            };
            // Every row is laid out at its full height; only the rows inside the viewport count
            // for the pane size, a partly visible one at its visible height.
            {
                let world = app.world();
                let expected: Vec<Rect> = (0..ROWS)
                    .map(|i| rect(0, (i * ROW) as i32, w, ROW))
                    .collect();
                assert_rects(world, instance, row_panes, &expected);
                for (i, pane) in row_panes.iter().enumerate() {
                    let expected = match visible(0, i as u32) {
                        Some(seen) => PaneSize { rows: seen, cols },
                        None => PaneSize::default(),
                    };
                    assert_eq!(pane_size(world, *pane), expected, "{cols}x{rows} row {i}");
                }
            }
            // Scroll by 7 rows: clamped to what overflows the viewport.
            let offset = 7.min((ROWS * ROW).saturating_sub(h));
            ops::scroll(app.world_mut(), viewer, container, 7).unwrap();
            app.update();
            check_invariants(app.world_mut()).unwrap();
            let world = app.world();
            let expected: Vec<Rect> = (0..ROWS)
                .map(|i| rect(0, (i * ROW) as i32 - offset as i32, w, ROW))
                .collect();
            assert_rects(world, instance, row_panes, &expected);
            for (i, pane) in row_panes.iter().enumerate() {
                let i = i as u32;
                // A row scrolled out of view keeps the size it last had while visible.
                let expected = match visible(offset, i).or(visible(0, i)) {
                    Some(seen) => PaneSize { rows: seen, cols },
                    None => PaneSize::default(),
                };
                assert_eq!(pane_size(world, *pane), expected, "{cols}x{rows} row {i}");
            }
        },
    );
}

#[test]
fn display_override_hides_an_instance_leaf_per_viewer_and_survives_recloning() {
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "display").unwrap();
    let root = ops::new_root(world, ws, "root").unwrap();
    let left = leaf(world, root, grow(1.0));
    let right = leaf(world, root, grow(1.0));
    let left_leaf = world.get::<PlacedIn>(left).unwrap().iter().next().unwrap();
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let other = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    ops::set_display(app.world_mut(), viewer, left_leaf, Some(Display::None)).unwrap();
    app.update();
    check_invariants(app.world_mut()).unwrap();
    let instance = instance_root(app.world_mut(), viewer);
    let other_instance = instance_root(app.world_mut(), other);
    {
        let world = app.world();
        assert_eq!(
            rect_of(world, leaf_showing(world, instance, right)),
            rect(0, 0, 80, 24)
        );
        assert_eq!(
            rect_of(world, leaf_showing(world, other_instance, right)),
            rect(40, 0, 40, 24)
        );
        // The template is untouched.
        assert_eq!(world.get::<Node>(left_leaf).unwrap().display, Display::Flex);
        // `left` still has one visible instance (the other viewer), so it keeps that size.
        assert_eq!(pane_size(world, left), PaneSize { rows: 24, cols: 40 });
    }
    // A template edit re-clones; the override is re-applied by `NodeId`.
    ops::rename(app.world_mut(), left_leaf, "left").unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        rect_of(world, leaf_showing(world, instance, right)),
        rect(0, 0, 80, 24)
    );
    ops::set_display(app.world_mut(), viewer, left_leaf, None).unwrap();
    app.update();
    let world = app.world();
    assert_eq!(
        rect_of(world, leaf_showing(world, instance, right)),
        rect(40, 0, 40, 24)
    );
}

#[test]
fn exchange_swaps_two_leaves_across_parents() {
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "exchange").unwrap();
    let root = ops::new_root(world, ws, "root").unwrap();
    let a = leaf(world, root, grow(1.0));
    let inner = ops::spawn_node(world, root, None, grow(1.0), None).unwrap();
    let b = leaf(world, inner, grow(1.0));
    let c = leaf(world, inner, grow(1.0));
    let leaf_of =
        |world: &World, pane: Entity| world.get::<PlacedIn>(pane).unwrap().iter().next().unwrap();
    let (leaf_a, leaf_c) = (leaf_of(world, a), leaf_of(world, c));
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    assert_rects(
        app.world(),
        instance,
        &[a, b, c],
        &[rect(0, 0, 40, 24), rect(40, 0, 20, 24), rect(60, 0, 20, 24)],
    );
    let generation = app.world().get::<LayoutGeneration>(root).unwrap().0;
    ops::exchange(app.world_mut(), leaf_a, leaf_c).unwrap();
    check_invariants(app.world_mut()).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        world.get::<LayoutGeneration>(root).unwrap().0,
        generation + 1
    );
    assert_eq!(world.get::<ChildOf>(leaf_a).unwrap().parent(), inner);
    assert_eq!(world.get::<ChildOf>(leaf_c).unwrap().parent(), root);
    assert_rects(
        world,
        instance,
        &[c, b, a],
        &[rect(0, 0, 40, 24), rect(40, 0, 20, 24), rect(60, 0, 20, 24)],
    );
    // A node and its ancestor cannot trade places; a root has no slot.
    assert!(matches!(
        ops::exchange(app.world_mut(), inner, leaf_a),
        Err(fux::layout::LayoutError::Cycle(_))
    ));
    assert!(matches!(
        ops::exchange(app.world_mut(), root, leaf_a),
        Err(fux::layout::LayoutError::IsATemplateRoot(_))
    ));
}

#[test]
fn place_beside_moves_a_leaf_next_to_a_target_like_a_split_would() {
    use fux::layout::Side;
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "beside").unwrap();
    let root = ops::new_root(world, ws, "root").unwrap();
    let a = leaf(world, root, grow(1.0));
    let b = leaf(world, root, grow(1.0));
    let c = leaf(world, root, grow(1.0));
    let leaf_of =
        |world: &World, pane: Entity| world.get::<PlacedIn>(pane).unwrap().iter().next().unwrap();
    let (leaf_a, leaf_b, leaf_c) = (leaf_of(world, a), leaf_of(world, b), leaf_of(world, c));
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 90 }, None).unwrap();
    // Same axis: `a` (before `c`) goes to the right of `c`; the index is taken after `a` left.
    ops::place_beside(app.world_mut(), leaf_a, leaf_c, Side::Right).unwrap();
    check_invariants(app.world_mut()).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        world
            .get::<Children>(root)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [leaf_b, leaf_c, leaf_a]
    );
    assert_rects(
        world,
        instance,
        &[b, c, a],
        &[rect(0, 0, 30, 24), rect(30, 0, 30, 24), rect(60, 0, 30, 24)],
    );
    // Cross axis: `a` goes below `b`, which is wrapped in a column that keeps `b`'s slot.
    ops::place_beside(app.world_mut(), leaf_a, leaf_b, Side::Bottom).unwrap();
    check_invariants(app.world_mut()).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    let column = world.get::<ChildOf>(leaf_b).unwrap().parent();
    assert_ne!(column, root);
    assert_eq!(world.get::<ChildOf>(leaf_a).unwrap().parent(), column);
    assert_eq!(
        world
            .get::<Children>(root)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [column, leaf_c]
    );
    assert_rects(
        world,
        instance,
        &[b, a, c],
        &[rect(0, 0, 45, 12), rect(0, 12, 45, 12), rect(45, 0, 45, 24)],
    );
    assert!(matches!(
        ops::place_beside(app.world_mut(), column, leaf_a, Side::Left),
        Err(fux::layout::LayoutError::Cycle(_))
    ));
}

#[test]
fn node_patch_rejects_invalid_grid_and_aspect_values_as_a_whole() {
    let cases = [
        r#"{"grid_row": {"start": 0}}"#,
        r#"{"grid_column": {"span": 0}}"#,
        r#"{"grid_column": {"start": 1, "span": 2, "end": 3}}"#,
        r#"{"aspect_ratio": "0"}"#,
        r#"{"aspect_ratio": "wide"}"#,
        r#"{"grid_auto_flow": "diagonal"}"#,
        r#"{"grid_auto_rows": ["1em"]}"#,
        r#"{"overflow_clip_margin": {"visual_box": "margin_box"}}"#,
        r#"{"inset": {"all": "2cells"}}"#,
    ];
    for case in cases {
        let patch: NodePatch = serde_json::from_str(case).unwrap();
        let mut node = Node::default();
        let mut z = ZIndex::default();
        let mut bg = BackgroundColor::default();
        let mut border = BorderColor::default();
        assert!(
            matches!(
                patch.apply(&mut node, &mut z, &mut bg, &mut border),
                Err(fux::layout::LayoutError::InvalidPatch(_))
            ),
            "{case}"
        );
    }
    let patch: NodePatch = serde_json::from_str(
        r#"{"aspect_ratio": "2", "grid_auto_flow": "column_dense",
            "grid_auto_rows": ["12px", "auto"], "grid_row": {"start": -1, "end": -1},
            "overflow_clip_margin": {"visual_box": "content_box", "margin": 1},
            "inset": {"all": "1px", "left": "3px"}}"#,
    )
    .unwrap();
    let mut node = Node::default();
    let mut z = ZIndex::default();
    let mut bg = BackgroundColor::default();
    let mut border = BorderColor::default();
    patch
        .apply(&mut node, &mut z, &mut bg, &mut border)
        .unwrap();
    assert_eq!(node.aspect_ratio, Some(2.0));
    assert_eq!(node.grid_auto_flow, GridAutoFlow::ColumnDense);
    assert_eq!(
        node.grid_auto_rows,
        vec![GridTrack::px(12.0), GridTrack::auto()]
    );
    assert_eq!(node.grid_row, GridPlacement::start_end(-1, -1));
    assert_eq!(
        node.overflow_clip_margin,
        OverflowClipMargin::content_box().with_margin(1.0)
    );
    assert_eq!(
        (node.left, node.right, node.top, node.bottom),
        (Val::Px(3.0), Val::Px(1.0), Val::Px(1.0), Val::Px(1.0))
    );
}
