//! `bsn!` templates and layout assets (prompt 3.7): the default session bootstraps through a
//! template, built-ins lay out through the real `bevy_ui`, nested templates build validated
//! trees, and user layouts are assets that hot-reload. `check_invariants` runs after every step.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::App;
use bevy_asset::{AssetEvent, AssetLoadFailedEvent, Assets};
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_math::UVec2;
use bevy_scene::prelude::*;
use bevy_ui::UiTargetCamera;
use bevy_ui::prelude::*;
use fux::app::{build_headless, build_headless_in};
use fux::config::Config;
use fux::layout::ops;
use fux::lifecycle::{self, Clock};
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::paths::Paths;
use fux::scene::templates::{command, node, pane, root, shell};
use fux::scene::{self, ApplyOptions, LayoutAsset, Layouts, SceneError, builtin, templates};

fn app() -> App {
    let mut app = build_headless(&Config::default());
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    app
}

fn check(app: &mut App) {
    check_invariants(app.world_mut()).unwrap();
}

fn kids(world: &World, e: Entity) -> Vec<Entity> {
    world
        .get::<Children>(e)
        .map(|c| c.iter().collect())
        .unwrap_or_default()
}

fn placed(world: &World, leaf: Entity) -> Option<Entity> {
    world.get::<Places>(leaf).map(|p| p.0)
}

fn roots(world: &World, ws: Entity) -> Vec<Entity> {
    world.get::<RootOrder>(ws).unwrap().0.clone()
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
        stack.extend(kids(world, e));
    }
    panic!("no instance leaf shows {pane}");
}

fn size_of(world: &World, e: Entity) -> UVec2 {
    world.get::<ComputedNode>(e).unwrap().size.as_uvec2()
}

/// A fresh directory under the canonical temp root (the asset file watcher compares the paths
/// it is told against the ones the OS reports, so `/var` must not hide `/private/var`).
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "fux-templates-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Document entity `n`: generation 0, index `n` (stored inverted in the low 32 bits).
fn doc_entity(n: u64) -> u64 {
    u64::from(u32::MAX) - n
}

/// A template document: one root `main` in `direction` with two default-command leaves.
fn two_pane_document(direction: &str) -> String {
    let ws = doc_entity(0);
    let root = doc_entity(1);
    let leaves = [doc_entity(2), doc_entity(3)];
    let mut doc = format!(
        "(resources: {{}}, entities: {{\n  {ws}: (components: {{\n    \
         \"fux::model::components::Workspace\": (),\n    \
         \"fux::model::ids::WorkspaceName\": \"doc\",\n    \
         \"fux::model::relations::RootOrder\": ([{root}]),\n  }}),\n  \
         {root}: (components: {{\n    \
         \"bevy_ecs::name::Name\": \"main\",\n    \
         \"fux::model::components::TemplateRoot\": (),\n    \
         \"fux::model::components::TemplateNode\": (),\n    \
         \"bevy_ui::ui_node::Node\": (width: Percent(100.0), height: Percent(100.0), \
         flex_direction: {direction}),\n    \
         \"bevy_ecs::hierarchy::Children\": ([{}, {}]),\n  }}),\n",
        leaves[0], leaves[1]
    );
    for leaf in leaves {
        doc.push_str(&format!(
            "  {leaf}: (components: {{\n    \
             \"fux::model::components::TemplateNode\": (),\n    \
             \"bevy_ecs::hierarchy::ChildOf\": ({root}),\n    \
             \"bevy_ui::ui_node::Node\": (flex_grow: 1.0),\n    \
             \"fux::model::components::PaneTemplate\": (argv: [], cwd: None, env: [], stream: \"\"),\n  \
             }}),\n"
        ));
    }
    doc.push_str("})\n");
    doc
}

fn node_ids(world: &World, root: Entity) -> Vec<u64> {
    let mut ids = Vec::new();
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        ids.push(world.get::<NodeId>(e).unwrap().0);
        stack.extend(kids(world, e));
    }
    ids
}

// ---------------------------------------------------------------------------------------------

#[test]
fn default_session_bootstrap_yields_the_previous_shape() {
    let mut app = app();
    lifecycle::bootstrap(app.world_mut(), "default", &[]).unwrap();
    check(&mut app);
    let world = app.world_mut();

    let workspaces: Vec<Entity> = world
        .query_filtered::<Entity, With<Workspace>>()
        .iter(world)
        .collect();
    assert_eq!(workspaces.len(), 1);
    let ws = workspaces[0];
    assert_eq!(world.resource::<Ids>().workspace("default"), Some(ws));
    assert!(world.get::<Open>(ws).is_some());
    assert_eq!(world.get::<Name>(ws).unwrap().as_str(), "default");

    let roots = roots(world, ws);
    assert_eq!(roots.len(), 1);
    let root = roots[0];
    assert!(world.get::<TemplateRoot>(root).is_some());
    assert!(world.get::<TemplateNode>(root).is_some());
    assert_eq!(world.get::<RootOf>(root).map(|r| r.0), Some(ws));
    assert_eq!(world.get::<ChildOf>(root).map(|c| c.parent()), Some(ws));
    assert_eq!(world.get::<LayoutGeneration>(root).map(|g| g.0), Some(0));
    assert_eq!(world.get::<Name>(root).unwrap().as_str(), "main");
    let id = world.get::<NodeId>(root).copied().unwrap();
    assert_eq!(world.resource::<Ids>().node(id), Some(root));
    let node = world.get::<Node>(root).unwrap();
    assert_eq!(
        (node.width, node.height),
        (Val::Percent(100.0), Val::Percent(100.0))
    );

    let leaves = kids(world, root);
    assert_eq!(leaves.len(), 1);
    let leaf = leaves[0];
    assert!(world.get::<NodeId>(leaf).is_some());
    assert_eq!(world.get::<Node>(leaf).unwrap().flex_grow, 1.0);
    assert!(kids(world, leaf).is_empty());
    assert!(
        world.get::<PaneTemplate>(leaf).is_none(),
        "the intent moved to the pane"
    );

    let panes: Vec<Entity> = world
        .query_filtered::<Entity, (With<Pane>, Allow<Disabled>)>()
        .iter(world)
        .collect();
    assert_eq!(panes.len(), 1);
    let pane = panes[0];
    assert_eq!(placed(world, leaf), Some(pane));
    assert!(world.get::<Disabled>(pane).is_some());
    assert_eq!(world.get::<Process>(pane), Some(&Process::Starting));
    assert_eq!(world.get::<PaneIn>(pane).map(|p| p.0), Some(ws));
    let template = world.get::<PaneTemplate>(pane).unwrap();
    assert_eq!(template.argv, vec![lifecycle::default_shell()]);
    let creation = world.get::<Creation>(pane).unwrap();
    assert_eq!(creation.kind, CreationKind::Spawn);
    assert_eq!(creation.requesters, vec![Requester::Server]);

    // A second bootstrap of the same name is refused and leaves the World as it was.
    let refused = lifecycle::bootstrap(app.world_mut(), "default", &[]);
    assert!(refused.is_err());
    check(&mut app);
    let world = app.world_mut();
    assert_eq!(
        world
            .query_filtered::<Entity, With<Workspace>>()
            .iter(world)
            .count(),
        1
    );

    // The pane materialises through the ordinary lifecycle.
    app.update();
    check(&mut app);
    let spawned: Vec<Entity> = app
        .world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .filter_map(|e| match e {
            Effect::SpawnPane { pane, .. } => Some(pane),
            _ => None,
        })
        .collect();
    assert_eq!(spawned, vec![pane]);
}

#[test]
fn two_column_lays_out_forty_forty() {
    let mut app = app();
    let ws = ops::new_workspace(app.world_mut(), "default").unwrap();
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    let dir = temp_dir("two-column");
    let report = scene::restore(
        app.world_mut(),
        ws,
        &dir,
        "two_column",
        &ApplyOptions::default(),
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.launched.len(), 2);
    assert_eq!(report.roots.len(), 1);
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    let root = roots(world, ws)[0];
    assert_eq!(root, report.roots[0].0);
    let leaves = kids(world, root);
    assert_eq!(world.get::<Name>(leaves[0]).unwrap().as_str(), "left");
    assert_eq!(world.get::<Name>(leaves[1]).unwrap().as_str(), "right");
    for (leaf, x) in leaves.iter().zip([20, 60]) {
        let pane = placed(world, *leaf).unwrap();
        assert_eq!(
            world.get::<Creation>(pane).unwrap().kind,
            CreationKind::Restore
        );
        let shown = leaf_showing(world, instance, pane);
        assert_eq!(size_of(world, shown), UVec2::new(40, 24));
        assert_eq!(
            world.get::<UiGlobalTransform>(shown).unwrap().translation.x as u32,
            x
        );
    }
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(root));

    for name in builtin::NAMES {
        assert!(builtin::scene(name).is_some(), "{name}");
    }
    assert!(builtin::scene("nope").is_none());
    assert!(matches!(
        scene::restore(app.world_mut(), ws, &dir, "nope", &ApplyOptions::default()),
        Err(SceneError::NotFound(_))
    ));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nested_bsn_builds_the_expected_tree_and_appends() {
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let first = ops::new_root(world, ws, "first").unwrap();
    let first_leaf = ops::spawn_node(
        world,
        first,
        None,
        Node::default(),
        Some(command(&["/bin/sh"], None)),
    )
    .unwrap();
    let first_pane = placed(world, first_leaf).unwrap();
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    check(&mut app);

    let report = templates::spawn(
        app.world_mut(),
        ws,
        bsn! {
            root("main")
            Node { flex_direction: FlexDirection::Row }
            Children [
                (#shell pane(shell()) Node { flex_grow: 1.0 }),
                (
                    #side node()
                    Node { flex_direction: FlexDirection::Column, flex_grow: 0.0, width: px(20.0) }
                    Children [
                        (#top pane(command(&["/bin/cat"], Some("/tmp")))),
                        (#bottom pane(shell()) Node { flex_grow: 3.0 }),
                    ]
                ),
            ]
        },
    )
    .unwrap();
    check(&mut app);
    let world = app.world();

    // Appended after the existing root; the viewer still shows the first.
    assert_eq!(report.roots.len(), 1);
    let main = report.roots[0].0;
    assert_eq!(roots(world, ws), vec![first, main]);
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(first));
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(first_pane));
    assert_eq!(report.launched.len(), 3);
    assert!(report.adopted.is_empty() && report.closed.is_empty());

    // Shape, names, patched fields.
    assert_eq!(world.get::<Name>(main).unwrap().as_str(), "main");
    assert_eq!(
        world.get::<Node>(main).unwrap().flex_direction,
        FlexDirection::Row
    );
    let top_level = kids(world, main);
    assert_eq!(top_level.len(), 2);
    let (shell_leaf, side) = (top_level[0], top_level[1]);
    assert_eq!(world.get::<Name>(shell_leaf).unwrap().as_str(), "shell");
    assert_eq!(world.get::<Name>(side).unwrap().as_str(), "side");
    let side_node = world.get::<Node>(side).unwrap();
    assert_eq!(side_node.flex_direction, FlexDirection::Column);
    assert_eq!(side_node.width, Val::Px(20.0));
    assert_eq!(side_node.flex_grow, 0.0);
    assert_eq!(
        side_node.flex_shrink, 1.0,
        "node() default kept by the patch"
    );
    assert!(placed(world, side).is_none());
    let inner = kids(world, side);
    assert_eq!(inner.len(), 2);
    let (top, bottom) = (inner[0], inner[1]);
    assert_eq!(world.get::<Node>(bottom).unwrap().flex_grow, 3.0);
    assert_eq!(
        world.get::<Node>(top).unwrap().min_width,
        Val::Px(f32::from(MIN_PANE_COLS)),
        "pane() default kept"
    );
    let top_pane = placed(world, top).unwrap();
    let top_template = world.get::<PaneTemplate>(top_pane).unwrap();
    assert_eq!(top_template.argv, vec!["/bin/cat".to_owned()]);
    assert_eq!(top_template.cwd.as_deref(), Some("/tmp"));
    let shell_pane = placed(world, shell_leaf).unwrap();
    assert_eq!(
        world.get::<PaneTemplate>(shell_pane).unwrap().argv,
        vec![lifecycle::default_shell()],
        "an empty argv is the configured default command"
    );
    assert_eq!(
        report.launched,
        vec![shell_pane, top_pane, placed(world, bottom).unwrap()],
        "launched in pre-order"
    );
    for pane in &report.launched {
        assert!(world.get::<Disabled>(*pane).is_some());
        assert_eq!(world.get::<Process>(*pane), Some(&Process::Starting));
        assert_eq!(
            world.get::<Creation>(*pane).unwrap().kind,
            CreationKind::Spawn
        );
    }

    // Unique ids across both roots, all indexed.
    let mut ids = node_ids(world, first);
    ids.extend(node_ids(world, main));
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    assert_eq!(
        unique.len(),
        7,
        "first + leaf, main + shell + side + top + bottom"
    );
    for id in ids {
        assert!(world.resource::<Ids>().node(NodeId(id)).is_some());
    }

    // The laid-out shape once the viewer shows it: 60 | 20, the side column split 1:3.
    ops::show_root(app.world_mut(), viewer, main).unwrap();
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    let shown_shell = leaf_showing(world, instance, shell_pane);
    let shown_top = leaf_showing(world, instance, top_pane);
    assert_eq!(size_of(world, shown_shell), UVec2::new(60, 24));
    assert_eq!(size_of(world, shown_top), UVec2::new(20, 6));

    // A template that is not a workspace or root, and a leaf with children, are refused whole.
    let refused = templates::spawn(app.world_mut(), ws, bsn! { Node });
    assert!(
        matches!(refused, Err(SceneError::Template(_))),
        "{refused:?}"
    );
    let refused = templates::spawn(
        app.world_mut(),
        ws,
        bsn! { root("bad") Children [ (pane(shell()) Children [ pane(shell()) ]) ] },
    );
    assert!(
        matches!(refused, Err(SceneError::BadLeaf(_))),
        "{refused:?}"
    );
    check(&mut app);
    assert_eq!(roots(app.world(), ws), vec![first, main]);

    // `apply` replaces instead: the old roots go, live panes are adopted in order.
    let bottom_pane = placed(app.world(), bottom).unwrap();
    let report = templates::apply(
        app.world_mut(),
        ws,
        builtin::two_row(),
        &ApplyOptions {
            adopt: true,
            close_unplaced: true,
            ..ApplyOptions::default()
        },
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.adopted, vec![first_pane, shell_pane]);
    assert_eq!(report.closed, vec![top_pane, bottom_pane]);
    assert_eq!(roots(app.world(), ws).len(), 1);
}

#[test]
fn layout_assets_are_listed_restored_hot_reloaded_and_kept_on_a_bad_edit() {
    let base = temp_dir("assets");
    let paths = Paths {
        runtime_dir: base.join("run"),
        config_dir: base.join("config"),
        state_dir: base.join("state"),
    };
    let layouts = paths.config_dir.join("layouts");
    scene::save(&layouts, "mine", &two_pane_document("Column")).unwrap();

    let mut app = build_headless_in(&Config::default(), &paths);
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    let ws = ops::new_workspace(app.world_mut(), "default").unwrap();
    app.update();
    check(&mut app);

    // Listed and, after the startup scan, loaded through the asset server.
    assert_eq!(
        scene::scan(app.world_mut()).unwrap(),
        vec!["mine".to_owned()]
    );
    let handle = app.world().resource::<Layouts>().handles["mine"].clone();
    let loaded = poll(&mut app, |world| {
        world.resource::<Assets<LayoutAsset>>().contains(&handle)
    });
    assert!(loaded, "the asset loads within the deadline");

    let restored = |app: &mut App| -> FlexDirection {
        scene::restore(
            app.world_mut(),
            ws,
            &layouts,
            "mine",
            &ApplyOptions {
                adopt: true,
                ..ApplyOptions::default()
            },
        )
        .unwrap();
        check(app);
        let world = app.world();
        world
            .get::<Node>(roots(world, ws)[0])
            .unwrap()
            .flex_direction
    };
    assert_eq!(restored(&mut app), FlexDirection::Column);

    // Edited on disk: the watcher reloads it and the next restore reflects the edit.
    scene::save(&layouts, "mine", &two_pane_document("Row")).unwrap();
    let reloaded = poll(&mut app, |world| {
        world
            .resource::<Messages<AssetEvent<LayoutAsset>>>()
            .iter_current_update_messages()
            .any(|e| matches!(e, AssetEvent::Modified { id } if *id == handle.id()))
    });
    assert!(reloaded, "AssetEvent::Modified within the deadline");
    assert_eq!(restored(&mut app), FlexDirection::Row);

    // An invalid edit keeps the previous document.
    std::fs::write(layouts.join("mine.scn.ron"), "(resources: {}, entities: {").unwrap();
    let failed = poll(&mut app, |world| {
        world
            .resource::<Messages<AssetLoadFailedEvent<LayoutAsset>>>()
            .iter_current_update_messages()
            .any(|e| e.id == handle.id())
    });
    assert!(failed, "the failed reload is reported within the deadline");
    let on_disk = scene::load(&layouts, "mine").unwrap();
    assert!(matches!(
        scene::apply(app.world_mut(), ws, &on_disk, &ApplyOptions::default()),
        Err(SceneError::Parse(_))
    ));
    assert_eq!(
        restored(&mut app),
        FlexDirection::Row,
        "the asset is served, not the file"
    );

    // A file that vanished is neither listed nor restorable.
    std::fs::remove_file(layouts.join("mine.scn.ron")).unwrap();
    assert_eq!(scene::scan(app.world_mut()).unwrap(), Vec::<String>::new());
    assert!(
        !app.world()
            .resource::<Layouts>()
            .handles
            .contains_key("mine")
    );
    assert!(matches!(
        scene::restore(
            app.world_mut(),
            ws,
            &layouts,
            "mine",
            &ApplyOptions::default()
        ),
        Err(SceneError::NotFound(_))
    ));
    std::fs::remove_dir_all(base).unwrap();
}

/// Updates the app until `done` holds or two seconds pass.
fn poll(app: &mut App, done: impl Fn(&World) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        app.update();
        if done(app.world()) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
