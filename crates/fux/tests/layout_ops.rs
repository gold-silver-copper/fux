//! `layout::ops` behaviour against the real `bevy_ui` layout: template edits, instances per
//! viewer, the `PaneSize` fold, zoom and navigation. `check_invariants` runs after every step.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns; flex_grow is compared against the exact literal written"
)]

use bevy_app::prelude::*;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_math::UVec2;
use bevy_ui::UiTargetCamera;
use bevy_ui::prelude::*;
use fux::layout::{
    GridTrackPatch, InstanceGeneration, LayoutError, LayoutPlugin, NavDirection, NodePatch, ops,
};
use fux::model::invariants::check_invariants;
use fux::model::*;

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

fn grow() -> Node {
    Node {
        flex_grow: 1.0,
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

fn assert_same_shape(world: &World, template: Entity, instance: Entity) {
    assert_eq!(
        world.get::<InstanceOf>(instance).map(|i| i.0),
        Some(template)
    );
    assert_eq!(world.get::<NodeId>(instance), world.get::<NodeId>(template));
    assert_eq!(world.get::<Name>(instance), world.get::<Name>(template));
    assert_eq!(
        world.get::<Places>(template).map(|p| p.0),
        world.get::<Shows>(instance).map(|s| s.0)
    );
    assert!(world.get::<InstanceNode>(instance).is_some());
    assert!(world.get::<TemplateNode>(instance).is_none());
    let t = kids(world, template);
    let i = kids(world, instance);
    assert_eq!(t.len(), i.len(), "child count differs under {template}");
    for (t, i) in t.iter().zip(i.iter()) {
        assert_same_shape(world, *t, *i);
    }
}

/// The instance leaf showing `pane` under `root`.
fn leaf_showing(world: &World, root: Entity, pane: Entity) -> Entity {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if world.get::<Shows>(e).map(|s| s.0) == Some(pane) {
            return e;
        }
        stack.extend(kids(world, e));
    }
    panic!("no instance leaf shows {pane}");
}

fn size_of(world: &World, e: Entity) -> UVec2 {
    world.get::<ComputedNode>(e).unwrap().size.as_uvec2()
}

fn one_pane(app: &mut App) -> (Entity, Entity, Entity, Entity) {
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    let leaf = ops::spawn_node(world, root, None, grow(), Some(shell())).unwrap();
    let pane = pane_of(world, leaf);
    check(app);
    (ws, root, leaf, pane)
}

#[test]
fn workspace_root_and_placed_pane() {
    let mut app = app();
    let (ws, root, leaf, pane) = one_pane(&mut app);
    let world = app.world();
    assert!(world.get::<Workspace>(ws).is_some());
    assert!(world.get::<Open>(ws).is_some());
    assert_eq!(world.get::<RootOrder>(ws).unwrap().0, vec![root]);
    assert_eq!(world.get::<RootOf>(root).unwrap().0, ws);
    assert!(world.get::<TemplateRoot>(root).is_some());
    assert!(world.get::<LayoutGeneration>(root).unwrap().0 >= 1);
    assert_eq!(kids(world, root), vec![leaf]);
    assert!(world.get::<Pane>(pane).is_some());
    assert!(
        world.get::<Disabled>(pane).is_some(),
        "starting panes are Disabled"
    );
    assert_eq!(world.get::<Process>(pane), Some(&Process::Starting));
    assert_eq!(world.get::<PaneIn>(pane).unwrap().0, ws);
    assert_eq!(world.get::<PaneTemplate>(pane), Some(&shell()));
    assert_eq!(
        world.get::<LaunchAttribution>(pane).unwrap().workspace_name,
        "default"
    );
    assert!(world.get::<PaneId>(pane).is_some());
    assert_eq!(world.resource::<Ids>().workspace("default"), Some(ws));
    assert_eq!(
        ops::new_workspace(app.world_mut(), "default"),
        Err(LayoutError::DuplicateWorkspace("default".to_owned()))
    );
}

#[test]
fn split_right_then_below() {
    let mut app = app();
    let (_, root, leaf1, pane1) = one_pane(&mut app);
    let gen0 = app.world().get::<LayoutGeneration>(root).unwrap().0;
    let (leaf2, pane2) =
        ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    check(&mut app);
    {
        let world = app.world();
        // The root already flexes as a row: the new leaf is a sibling.
        assert_eq!(kids(world, root), vec![leaf1, leaf2]);
        assert_eq!(
            world.get::<Node>(root).unwrap().flex_direction,
            FlexDirection::Row
        );
        assert_eq!(world.get::<Node>(leaf1).unwrap().flex_grow, 1.0);
        assert_eq!(world.get::<Node>(leaf2).unwrap().flex_grow, 1.0);
        assert_eq!(pane_of(world, leaf2), pane2);
        assert!(world.get::<LayoutGeneration>(root).unwrap().0 > gen0);
    }
    let (leaf3, _) = ops::split(app.world_mut(), pane1, SplitDirection::Below, shell()).unwrap();
    check(&mut app);
    let world = app.world();
    // Below: leaf1 is wrapped in a column container in its old slot.
    let top = kids(world, root);
    assert_eq!(top.len(), 2);
    let container = top[0];
    assert_eq!(top[1], leaf2);
    assert_eq!(
        world.get::<Node>(container).unwrap().flex_direction,
        FlexDirection::Column
    );
    assert_eq!(world.get::<Node>(container).unwrap().flex_grow, 1.0);
    assert!(world.get::<Places>(container).is_none());
    assert_eq!(kids(world, container), vec![leaf1, leaf3]);
    assert_eq!(world.get::<Node>(leaf1).unwrap().flex_grow, 1.0);
    assert_eq!(world.get::<Node>(leaf3).unwrap().flex_grow, 1.0);
}

#[test]
fn remove_leaf_collapses_containers_and_closes_empty_root() {
    let mut app = app();
    let (ws, root, leaf1, pane1) = one_pane(&mut app);
    let (leaf2, pane2) =
        ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    let (leaf3, pane3) =
        ops::split(app.world_mut(), pane1, SplitDirection::Below, shell()).unwrap();
    let container = kids(app.world(), root)[0];
    // Closing pane3 leaves the container with one child: it collapses into the container's slot.
    ops::remove_leaf(app.world_mut(), pane3).unwrap();
    app.world_mut().despawn(pane3);
    check(&mut app);
    assert_eq!(kids(app.world(), root), vec![leaf1, leaf2]);
    assert!(app.world().get_entity(container).is_err());
    assert!(app.world().get_entity(leaf3).is_err());
    ops::remove_leaf(app.world_mut(), pane2).unwrap();
    app.world_mut().despawn(pane2);
    check(&mut app);
    assert_eq!(kids(app.world(), root), vec![leaf1]);
    // The last pane closes the root and removes it from the order.
    ops::remove_leaf(app.world_mut(), pane1).unwrap();
    app.world_mut().despawn(pane1);
    check(&mut app);
    assert!(app.world().get_entity(root).is_err());
    assert!(app.world().get::<RootOrder>(ws).unwrap().0.is_empty());
    assert!(app.world().get::<Roots>(ws).is_none_or(|r| r.is_empty()));
}

#[test]
fn despawn_node_refuses_live_pane_subtree() {
    let mut app = app();
    let (_, root, _, pane1) = one_pane(&mut app);
    let (_, _) = ops::split(app.world_mut(), pane1, SplitDirection::Below, shell()).unwrap();
    let container = kids(app.world(), root)[0];
    assert_eq!(
        ops::despawn_node(app.world_mut(), container),
        Err(LayoutError::PaneLive(pane1))
    );
    check(&mut app);
    assert!(app.world().get_entity(container).is_ok());
    // Roots are closed, never despawned as nodes.
    assert_eq!(
        ops::despawn_node(app.world_mut(), root),
        Err(LayoutError::IsATemplateRoot(root))
    );
    assert_eq!(
        ops::close_root(app.world_mut(), root),
        Err(LayoutError::PaneLive(pane1))
    );
    // Once every pane exited the subtree goes, panes included.
    let panes: Vec<Entity> = {
        let world = app.world_mut();
        let panes: Vec<Entity> = world
            .get::<WorkspacePanes>(world.get::<PaneIn>(pane1).unwrap().0)
            .unwrap()
            .iter()
            .collect();
        for &p in &panes {
            world
                .entity_mut(p)
                .insert(Process::Exited { code: 0 })
                .remove::<Disabled>();
        }
        panes
    };
    check(&mut app);
    ops::despawn_node(app.world_mut(), container).unwrap();
    check(&mut app);
    assert!(app.world().get_entity(container).is_err());
    for p in panes {
        assert!(app.world().get_entity(p).is_err());
    }
    assert!(kids(app.world(), root).is_empty());
}

#[test]
fn reparent_into_own_subtree_is_refused() {
    let mut app = app();
    let (_, root, _, _) = one_pane(&mut app);
    let world = app.world_mut();
    let a = ops::spawn_node(world, root, None, grow(), None).unwrap();
    let b = ops::spawn_node(world, a, None, grow(), None).unwrap();
    let c = ops::spawn_node(world, b, None, grow(), None).unwrap();
    assert_eq!(
        ops::reparent_node(world, a, c, None),
        Err(LayoutError::Cycle(a))
    );
    assert_eq!(
        ops::reparent_node(world, a, a, None),
        Err(LayoutError::Cycle(a))
    );
    assert_eq!(kids(world, root).len(), 2);
    assert_eq!(kids(world, a), vec![b]);
    // A placing leaf takes no children.
    let leaf = kids(world, root)[0];
    assert_eq!(
        ops::reparent_node(world, a, leaf, None),
        Err(LayoutError::LeafHasChildren(leaf))
    );
    assert_eq!(
        ops::spawn_node(world, leaf, None, grow(), None),
        Err(LayoutError::LeafHasChildren(leaf))
    );
    // A legal move reorders.
    ops::reparent_node(world, c, root, Some(0)).unwrap();
    assert_eq!(kids(world, root)[0], c);
    ops::reorder_node(world, c, 2).unwrap();
    assert_eq!(kids(world, root)[2], c);
    assert_eq!(
        ops::reorder_node(world, c, 3),
        Err(LayoutError::IndexOutOfRange { index: 3, len: 3 })
    );
    check(&mut app);
}

#[test]
fn depth_and_count_limits_are_enforced() {
    let mut app = app();
    let (_, root, _, _) = one_pane(&mut app);
    let world = app.world_mut();
    let mut parent = root;
    for _ in 0..MAX_DEPTH {
        parent = ops::spawn_node(world, parent, None, grow(), None).unwrap();
    }
    let err = ops::spawn_node(world, parent, None, grow(), None).unwrap_err();
    assert!(matches!(err, LayoutError::DepthExceeded { .. }), "{err}");
    // Reparenting a tall subtree under a deep node is refused too.
    let tall = kids(world, root)[1];
    let err = ops::reparent_node(world, tall, parent, None).unwrap_err();
    assert!(
        matches!(
            err,
            LayoutError::DepthExceeded { .. } | LayoutError::Cycle(_)
        ),
        "{err}"
    );
    check(&mut app);

    let mut app = self::app();
    app.insert_resource(Limits {
        nodes_per_workspace: 4,
        panes_per_workspace: 2,
        ..Default::default()
    });
    let (_, root, _, pane1) = one_pane(&mut app);
    let world = app.world_mut();
    ops::split(world, pane1, SplitDirection::Right, shell()).unwrap(); // 3 nodes, 2 panes
    let err = ops::split(world, pane1, SplitDirection::Right, shell()).unwrap_err();
    assert!(matches!(err, LayoutError::TooManyPanes { .. }), "{err}");
    ops::spawn_node(world, root, None, grow(), None).unwrap(); // 4 nodes
    let err = ops::spawn_node(world, root, None, grow(), None).unwrap_err();
    assert!(matches!(err, LayoutError::TooManyNodes { .. }), "{err}");
    check(&mut app);
}

#[test]
fn attach_viewer_instances_the_template_and_folds_pane_size() {
    let mut app = app();
    let (ws, root, leaf1, pane1) = one_pane(&mut app);
    let (leaf2, pane2) =
        ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    ops::rename(app.world_mut(), leaf2, "right").unwrap();
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    check(&mut app);
    {
        let world = app.world();
        assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(root));
        assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane1));
        assert_eq!(world.get::<Viewing>(viewer).map(|v| v.0), Some(ws));
        assert!(world.get::<ExactTarget>(viewer).is_none());
    }
    app.update();
    check(&mut app);
    let camera = app.world().get::<ViewerCamera>(viewer).unwrap().0;
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_same_shape(world, root, instance);
    assert_eq!(
        world.get::<UiTargetCamera>(instance).map(|c| c.entity()),
        Some(camera)
    );
    assert_eq!(
        world.get::<InstanceGeneration>(instance).map(|g| g.0),
        world.get::<LayoutGeneration>(root).map(|g| g.0)
    );
    assert_eq!(size_of(world, instance), UVec2::new(80, 24));
    let i1 = leaf_showing(world, instance, pane1);
    let i2 = leaf_showing(world, instance, pane2);
    assert_eq!(world.get::<InstanceOf>(i1).unwrap().0, leaf1);
    assert_eq!(size_of(world, i1), UVec2::new(40, 24));
    assert_eq!(size_of(world, i2), UVec2::new(40, 24));
    assert_eq!(world.get::<Name>(i2).map(|n| n.as_str()), Some("right"));
    assert_eq!(
        world.get::<PaneSize>(pane1),
        Some(&PaneSize { rows: 24, cols: 40 })
    );
    assert_eq!(
        world.get::<PaneSize>(pane2),
        Some(&PaneSize { rows: 24, cols: 40 })
    );
    // Templates are never laid out.
    assert_eq!(size_of(world, root), UVec2::ZERO);
    // Ids still map to the templates, not the clones.
    let id = *world.get::<NodeId>(leaf1).unwrap();
    assert_eq!(world.resource::<Ids>().node(id), Some(leaf1));
}

#[test]
fn two_viewers_get_different_geometry_and_pane_size_is_the_minimum() {
    let mut app = app();
    let (ws, _, _, pane1) = one_pane(&mut app);
    let (_, pane2) = ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    let small = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    let large = ops::attach_viewer(app.world_mut(), ws, viewport(40, 120), None).unwrap();
    app.update();
    check(&mut app);
    let small_root = instance_root(app.world_mut(), small);
    let large_root = instance_root(app.world_mut(), large);
    {
        let world = app.world();
        assert_eq!(
            size_of(world, leaf_showing(world, small_root, pane1)),
            UVec2::new(40, 24)
        );
        assert_eq!(
            size_of(world, leaf_showing(world, large_root, pane1)),
            UVec2::new(60, 40)
        );
        assert_eq!(world.get::<ShownBy>(pane1).unwrap().len(), 2);
        assert_eq!(
            world.get::<PaneSize>(pane1),
            Some(&PaneSize { rows: 24, cols: 40 })
        );
        assert_eq!(
            world.get::<PaneSize>(pane2),
            Some(&PaneSize { rows: 24, cols: 40 })
        );
    }
    // Resizing the small viewer re-folds within one update.
    ops::resize_viewer(app.world_mut(), small, viewport(50, 200)).unwrap();
    app.update();
    check(&mut app);
    {
        let world = app.world();
        assert_eq!(size_of(world, small_root), UVec2::new(200, 50));
        assert_eq!(
            world.get::<PaneSize>(pane1),
            Some(&PaneSize { rows: 40, cols: 60 })
        );
    }
    // Detaching one viewer leaves the other's instance and re-folds to it alone.
    ops::detach_viewer(app.world_mut(), small).unwrap();
    app.update();
    check(&mut app);
    let world = app.world();
    assert!(world.get_entity(small_root).is_err());
    assert!(world.get_entity(large_root).is_ok());
    assert_eq!(world.get::<ShownBy>(pane1).unwrap().len(), 1);
    assert_eq!(
        world.get::<PaneSize>(pane1),
        Some(&PaneSize { rows: 40, cols: 60 })
    );
}

#[test]
fn template_edits_reclone_instances_with_the_new_generation() {
    let mut app = app();
    let (ws, root, leaf1, pane1) = one_pane(&mut app);
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    let before = instance_root(app.world_mut(), viewer);
    let patch: NodePatch = serde_json::from_str(
        r#"{"width": "50%", "flex_grow": 0, "z_index": 3, "background_color": {"rgb": [1, 2, 3]}}"#,
    )
    .unwrap();
    ops::patch_node(app.world_mut(), leaf1, &patch).unwrap();
    check(&mut app);
    assert_eq!(
        app.world().get::<Node>(leaf1).unwrap().width,
        Val::Percent(50.0)
    );
    assert_eq!(app.world().get::<ZIndex>(leaf1), Some(&ZIndex(3)));
    app.update();
    check(&mut app);
    let after = instance_root(app.world_mut(), viewer);
    assert_ne!(before, after, "a template edit re-clones the instance tree");
    assert!(app.world().get_entity(before).is_err());
    let world = app.world();
    assert_same_shape(world, root, after);
    assert_eq!(
        world.get::<InstanceGeneration>(after).map(|g| g.0),
        world.get::<LayoutGeneration>(root).map(|g| g.0)
    );
    let i1 = leaf_showing(world, after, pane1);
    assert_eq!(size_of(world, i1), UVec2::new(40, 24));
    assert_eq!(world.get::<ZIndex>(i1), Some(&ZIndex(3)));
    // Invalid patches are rejected as a whole and change nothing.
    let bad: Result<NodePatch, _> = serde_json::from_str(r#"{"width": "50%", "bogus": 1}"#);
    assert!(bad.is_err());
    let bad: NodePatch =
        serde_json::from_str(r#"{"height": "10%", "display": "sideways"}"#).unwrap();
    let generation = app.world().get::<LayoutGeneration>(root).unwrap().0;
    assert!(matches!(
        ops::patch_node(app.world_mut(), leaf1, &bad),
        Err(LayoutError::InvalidPatch(_))
    ));
    assert_eq!(app.world().get::<Node>(leaf1).unwrap().height, Val::Auto);
    assert_eq!(
        app.world().get::<LayoutGeneration>(root).unwrap().0,
        generation
    );
}

#[test]
fn zoom_fills_the_viewport_and_unzoom_restores() {
    let mut app = app();
    let (ws, _, leaf1, pane1) = one_pane(&mut app);
    let (_, pane2) = ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    ops::zoom(app.world_mut(), viewer, leaf1).unwrap();
    check(&mut app);
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    {
        let world = app.world();
        let i1 = leaf_showing(world, instance, pane1);
        let i2 = leaf_showing(world, instance, pane2);
        assert_eq!(world.get::<Zoomed>(instance), Some(&Zoomed(i1)));
        assert_eq!(size_of(world, i1), UVec2::new(80, 24));
        assert_eq!(world.get::<Node>(i2).unwrap().display, Display::None);
        assert_eq!(size_of(world, i2), UVec2::ZERO);
        assert_eq!(
            world.get::<PaneSize>(pane1),
            Some(&PaneSize { rows: 24, cols: 80 })
        );
        // A hidden instance does not shrink its pane.
        assert_eq!(
            world.get::<PaneSize>(pane2),
            Some(&PaneSize { rows: 24, cols: 40 })
        );
        // The template is untouched.
        assert_eq!(
            world.get::<Node>(leaf1).unwrap().position_type,
            PositionType::Relative
        );
    }
    // Zoom survives a re-clone after a template edit.
    let (_, pane3) = ops::split(app.world_mut(), pane2, SplitDirection::Below, shell()).unwrap();
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    {
        let world = app.world();
        let i1 = leaf_showing(world, instance, pane1);
        assert_eq!(world.get::<Zoomed>(instance), Some(&Zoomed(i1)));
        assert_eq!(size_of(world, i1), UVec2::new(80, 24));
        assert_eq!(
            size_of(world, leaf_showing(world, instance, pane3)),
            UVec2::ZERO
        );
    }
    ops::unzoom(app.world_mut(), viewer).unwrap();
    app.update();
    check(&mut app);
    let world = app.world();
    assert!(world.get::<Zoomed>(instance).is_none());
    assert_eq!(
        size_of(world, leaf_showing(world, instance, pane1)),
        UVec2::new(40, 24)
    );
    assert_eq!(
        size_of(world, leaf_showing(world, instance, pane2)),
        UVec2::new(40, 12)
    );
    assert_eq!(
        size_of(world, leaf_showing(world, instance, pane3)),
        UVec2::new(40, 12)
    );
    assert_eq!(
        world.get::<PaneSize>(pane1),
        Some(&PaneSize { rows: 24, cols: 40 })
    );
    assert_eq!(
        world.get::<PaneSize>(pane2),
        Some(&PaneSize { rows: 12, cols: 40 })
    );
}

#[test]
fn navigate_reaches_every_pane_of_a_grid_and_pane_at_hits_cells() {
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "grid").unwrap();
    let root = ops::new_root(world, ws, "dash").unwrap();
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
    let leaves: Vec<Entity> = (0..4)
        .map(|_| ops::spawn_node(world, root, None, Node::default(), Some(shell())).unwrap())
        .collect();
    let panes: Vec<Entity> = leaves.iter().map(|l| pane_of(world, *l)).collect();
    let (tl, tr, bl, br) = (panes[0], panes[1], panes[2], panes[3]);
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    check(&mut app);
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        size_of(world, leaf_showing(world, instance, br)),
        UVec2::new(40, 12)
    );
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(tl));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Right), Some(tr));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Down), Some(bl));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Left), None);
    assert_eq!(ops::navigate(world, viewer, NavDirection::Up), None);
    assert_eq!(ops::navigate(world, viewer, NavDirection::Next), Some(tr));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Prev), Some(br));
    assert_eq!(ops::pane_at(world, viewer, 0, 0), Some(tl));
    assert_eq!(ops::pane_at(world, viewer, 79, 0), Some(tr));
    assert_eq!(ops::pane_at(world, viewer, 5, 23), Some(bl));
    assert_eq!(ops::pane_at(world, viewer, 40, 12), Some(br));
    assert_eq!(ops::pane_at(world, viewer, 80, 12), None);
    ops::target(app.world_mut(), viewer, br).unwrap();
    check(&mut app);
    let world = app.world();
    assert_eq!(ops::navigate(world, viewer, NavDirection::Left), Some(bl));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Up), Some(tr));
    assert_eq!(ops::navigate(world, viewer, NavDirection::Right), None);
    assert_eq!(ops::navigate(world, viewer, NavDirection::Next), Some(tl));
}

#[test]
fn show_root_and_target_follow_roots_and_exact_attachments_refuse() {
    let mut app = app();
    let (ws, root1, _, pane1) = one_pane(&mut app);
    let world = app.world_mut();
    let root2 = ops::new_root(world, ws, "second").unwrap();
    let leaf2 = ops::spawn_node(world, root2, None, grow(), Some(shell())).unwrap();
    let pane2 = pane_of(world, leaf2);
    let viewer = ops::attach_viewer(world, ws, viewport(24, 80), None).unwrap();
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(root1));
    // Targeting a pane in another root shows that root.
    ops::target(world, viewer, pane2).unwrap();
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(root2));
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane2));
    // Showing a root the target is not in retargets to its first pane.
    ops::show_root(world, viewer, root1).unwrap();
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane1));
    app.update();
    check(&mut app);
    let world = app.world_mut();
    assert_eq!(ops::order_roots(world, ws, &[root2, root1]), Ok(()));
    assert_eq!(
        ops::order_roots(world, ws, &[root2, root2]),
        Err(LayoutError::RootOrderMismatch)
    );
    assert_eq!(world.get::<RootOrder>(ws).unwrap().0, vec![root2, root1]);
    // Exact attachments: shown root and target are fixed.
    let exact = ops::attach_viewer(world, ws, viewport(24, 80), Some(pane2)).unwrap();
    assert!(world.get::<ExactTarget>(exact).is_some());
    assert_eq!(world.get::<Showing>(exact).map(|s| s.0), Some(root2));
    assert_eq!(
        ops::target(world, exact, pane1),
        Err(LayoutError::ExactTarget(exact))
    );
    assert_eq!(
        ops::show_root(world, exact, root1),
        Err(LayoutError::ExactTarget(exact))
    );
    app.update();
    check(&mut app);
    // Closing the shown root moves a plain viewer to the next root; the exact viewer stays.
    let world = app.world_mut();
    world
        .entity_mut(pane1)
        .insert(Process::Exited { code: 0 })
        .remove::<Disabled>();
    ops::close_root(world, root1).unwrap();
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(root2));
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane2));
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    assert_same_shape(app.world(), root2, instance);
    // Retiring a workspace refuses new work and new viewers.
    let world = app.world_mut();
    ops::retire_workspace(world, ws, 1).unwrap();
    assert_eq!(
        ops::new_root(world, ws, "x"),
        Err(LayoutError::WorkspaceRetiring(ws))
    );
    assert_eq!(
        ops::attach_viewer(world, ws, viewport(24, 80), None),
        Err(LayoutError::WorkspaceRetiring(ws))
    );
    check(&mut app);
}

#[test]
fn move_root_between_workspaces_moves_its_panes() {
    let mut app = app();
    let (ws1, root, _, pane1) = one_pane(&mut app);
    let world = app.world_mut();
    let ws2 = ops::new_workspace(world, "other").unwrap();
    let viewer = ops::attach_viewer(world, ws1, viewport(24, 80), None).unwrap();
    ops::move_root(world, root, ws2).unwrap();
    assert_eq!(world.get::<RootOf>(root).unwrap().0, ws2);
    assert_eq!(world.get::<RootOrder>(ws2).unwrap().0, vec![root]);
    assert!(world.get::<RootOrder>(ws1).unwrap().0.is_empty());
    assert_eq!(world.get::<PaneIn>(pane1).unwrap().0, ws2);
    assert!(world.get::<Showing>(viewer).is_none());
    app.update();
    check(&mut app);
}

#[test]
fn custom_pointer_hovers_the_instance_leaf_under_its_cell() {
    use bevy_camera::RenderTarget;
    use bevy_picking::hover::HoverMap;
    use bevy_picking::pointer::{Location, PointerAction, PointerId, PointerInput};

    let mut app = app();
    let (ws, _, _, pane1) = one_pane(&mut app);
    let (_, pane2) = ops::split(app.world_mut(), pane1, SplitDirection::Right, shell()).unwrap();
    let viewer = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    let other = ops::attach_viewer(app.world_mut(), ws, viewport(24, 80), None).unwrap();
    app.update();
    let instance = instance_root(app.world_mut(), viewer);
    let pointer_entity = app.world().get::<ViewerPointer>(viewer).unwrap().0;
    let pointer = *app.world().get::<PointerId>(pointer_entity).unwrap();
    assert!(matches!(pointer, PointerId::Custom(_)));
    let target = RenderTarget::None {
        size: UVec2::new(80, 24),
    }
    .normalize(None)
    .unwrap();
    let at = |col: u16, row: u16| Location {
        target: target.clone(),
        position: bevy_math::Vec2::new(f32::from(col) + 0.5, f32::from(row) + 0.5),
    };
    app.world_mut().write_message(PointerInput::new(
        pointer,
        at(60, 10),
        PointerAction::Move {
            delta: bevy_math::Vec2::ZERO,
        },
    ));
    app.update();
    check(&mut app);
    {
        let world = app.world();
        let hovered = world.resource::<HoverMap>().get(&pointer).unwrap();
        let leaf2 = leaf_showing(world, instance, pane2);
        let leaf1 = leaf_showing(world, instance, pane1);
        assert!(
            hovered.contains_key(&leaf2),
            "the leaf under the cell is hovered"
        );
        assert!(!hovered.contains_key(&leaf1));
        let hit = &hovered[&leaf2];
        assert_eq!(hit.camera, world.get::<ViewerCamera>(viewer).unwrap().0);
        assert_eq!(
            hit.position.map(|p| p.truncate()),
            Some(bevy_math::Vec2::new(60.5, 10.5))
        );
        // Only this viewer's instances are hit, never the other viewer's or the template.
        let other_root = instance_root_of(world, other);
        assert!(
            !hovered
                .keys()
                .any(|e| *e == other_root || kids(world, other_root).contains(e))
        );
        assert_eq!(ops::pane_at(world, viewer, 60, 10), Some(pane2));
    }
    // Moving out of the viewport clears the hover.
    app.world_mut().write_message(PointerInput::new(
        pointer,
        at(90, 30),
        PointerAction::Move {
            delta: bevy_math::Vec2::ZERO,
        },
    ));
    app.update();
    let world = app.world();
    assert!(
        world
            .resource::<HoverMap>()
            .get(&pointer)
            .is_none_or(|h| h.is_empty())
    );
}

fn instance_root_of(world: &World, viewer: Entity) -> Entity {
    let camera = world.get::<ViewerCamera>(viewer).unwrap().0;
    let showing = world.get::<Showing>(viewer).unwrap().0;
    world
        .get::<Instances>(showing)
        .unwrap()
        .iter()
        .find(|i| world.get::<UiTargetCamera>(*i).map(|c| c.entity()) == Some(camera))
        .unwrap()
}
