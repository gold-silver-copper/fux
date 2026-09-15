//! `fux::scene` against the real `bevy_ui` layout and lifecycle: export/apply round trips,
//! refusals that leave the World untouched, template scenes launching through the `Creation`
//! path, files, and the built-in templates. `check_invariants` runs after every step.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]

use std::path::PathBuf;

use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_math::UVec2;
use bevy_ui::UiTargetCamera;
use bevy_ui::prelude::*;
use fux::app::build_headless;
use fux::config::Config;
use fux::layout::ops;
use fux::lifecycle::Clock;
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::scene::{self, ApplyOptions, SceneError};

fn app() -> App {
    let mut app = build_headless(&Config::default());
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    app
}

fn check(app: &mut App) {
    check_invariants(app.world_mut()).unwrap();
}

fn shell() -> PaneTemplate {
    PaneTemplate {
        argv: vec!["/bin/sh".to_owned()],
        ..Default::default()
    }
}

fn grow() -> Node {
    Node {
        flex_grow: 1.0,
        ..Default::default()
    }
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

fn plain() -> ApplyOptions {
    ApplyOptions::default()
}

/// `default` workspace with root `main` = Row [ leaf(pane1), Column [ leaf(pane2), leaf(pane3) ] ].
fn three_panes(app: &mut App) -> (Entity, Entity, [Entity; 3]) {
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    let leaf1 = ops::spawn_node(world, root, None, grow(), Some(shell())).unwrap();
    let pane1 = placed(world, leaf1).unwrap();
    let (_, pane2) = ops::split(world, pane1, SplitDirection::Right, shell()).unwrap();
    let (_, pane3) = ops::split(world, pane2, SplitDirection::Below, shell()).unwrap();
    check(app);
    (ws, root, [pane1, pane2, pane3])
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

fn shape(world: &World, node: Entity) -> String {
    let n = world.get::<Node>(node).unwrap();
    let mut s = format!(
        "{:?}/{}",
        n.flex_direction,
        placed(world, node).map_or("-".to_owned(), |p| world
            .get::<PaneId>(p)
            .unwrap()
            .to_string())
    );
    let children = kids(world, node);
    if !children.is_empty() {
        s.push('[');
        for c in children {
            s.push_str(&shape(world, c));
            s.push(' ');
        }
        s.push(']');
    }
    s
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fux-scenes-{tag}-{}-{}",
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

/// A workspace document with one root whose children are a chain `depth` deep, ending in a
/// leaf that places `pane`.
fn chain_document(depth: usize, pane: u64) -> String {
    let ws = doc_entity(0);
    let mut out = String::from("(\n  resources: {},\n  entities: {\n");
    out.push_str(&format!(
        "    {ws}: (components: {{ \"fux::model::relations::RootOrder\": ([{}]) }}),\n",
        doc_entity(1)
    ));
    let pane_entity = doc_entity(2 + depth as u64 + 1);
    for i in 0..=depth {
        let id = doc_entity(1 + i as u64);
        let mut components = String::from(
            "\"bevy_ui::ui_node::Node\": (width: Percent(100.0), height: Percent(100.0))",
        );
        if i > 0 {
            components.push_str(&format!(
                ", \"bevy_ecs::hierarchy::ChildOf\": ({})",
                doc_entity(i as u64)
            ));
        }
        if i == depth {
            components.push_str(&format!(
                ", \"fux::model::relations::Places\": ({pane_entity})"
            ));
        }
        out.push_str(&format!("    {id}: (components: {{ {components} }}),\n"));
    }
    out.push_str(&format!(
        "    {pane_entity}: (components: {{ \"fux::model::ids::PaneId\": {pane} }}),\n"
    ));
    out.push_str("  },\n)\n");
    out
}

fn pane_id(world: &World, pane: Entity) -> u64 {
    world.get::<PaneId>(pane).unwrap().0
}

// ---------------------------------------------------------------------------------------------

#[test]
fn export_apply_round_trip_keeps_shape_panes_and_viewers() {
    let mut app = app();
    let (ws, root, [pane1, pane2, pane3]) = three_panes(&mut app);
    let viewer =
        ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    ops::target(app.world_mut(), viewer, pane3).unwrap();
    app.update();
    check(&mut app);
    let before = shape(app.world(), root);
    let export = scene::export(app.world(), ws).unwrap();
    let expected: Vec<(NodeId, u64)> = export.roots.iter().map(|r| (r.1, r.2)).collect();
    assert_eq!(export.roots[0].0, root);
    assert_eq!(export.roots.len(), 1);
    assert!(export.document.contains("fux::model::ids::PaneId"));
    assert!(
        !export.document.contains("PaneTemplate"),
        "a plain export references panes, it never launches"
    );

    let report = scene::apply(
        app.world_mut(),
        ws,
        &export.document,
        &ApplyOptions {
            expected: Some(expected.clone()),
            ..plain()
        },
    )
    .unwrap();
    check(&mut app);
    let world = app.world();
    assert_eq!(report.roots.len(), 1);
    let new_root = report.roots[0].0;
    assert_ne!(new_root, root, "roots are rebuilt");
    assert!(
        world.get_entity(root).is_err(),
        "the old template tree is gone"
    );
    assert_eq!(roots(world, ws), vec![new_root]);
    assert_eq!(shape(world, new_root), before);
    assert_eq!(world.get::<Name>(new_root).unwrap().as_str(), "main");
    assert!(report.launched.is_empty() && report.closed.is_empty());
    assert_eq!(
        world.get::<LayoutGeneration>(new_root).unwrap().0,
        report.roots[0].1
    );
    for pane in [pane1, pane2, pane3] {
        assert_eq!(world.get::<PlacedIn>(pane).unwrap().len(), 1);
        assert!(world.get::<Disabled>(pane).is_some(), "still starting");
    }
    // The viewer follows its target into the new root.
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(new_root));
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane3));

    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    assert_eq!(
        world.get::<InstanceOf>(instance).map(|i| i.0),
        Some(new_root)
    );
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

    // A second apply of the same export is refused: the roots it saw are gone.
    let stale = scene::apply(
        app.world_mut(),
        ws,
        &export.document,
        &ApplyOptions {
            expected: Some(expected),
            ..plain()
        },
    );
    assert!(
        matches!(stale, Err(SceneError::StaleGeneration { .. })),
        "{stale:?}"
    );
    assert_eq!(
        roots(app.world(), ws),
        vec![new_root],
        "a refusal changes nothing"
    );
    check(&mut app);
}

#[test]
fn foreign_pane_and_bad_documents_are_refused_without_touching_the_world() {
    let mut app = app();
    let (ws, root, [pane1, ..]) = three_panes(&mut app);
    let world = app.world_mut();
    let other = ops::new_workspace(world, "other").unwrap();
    let other_root = ops::new_root(world, other, "main").unwrap();
    ops::spawn_node(world, other_root, None, grow(), Some(shell())).unwrap();
    check(&mut app);
    let before = shape(app.world(), root);
    let other_doc = scene::export(app.world(), other).unwrap().document;

    let refusals: Vec<(&str, String, ApplyOptions)> = vec![
        ("foreign pane", other_doc, plain()),
        (
            "unknown type path",
            chain_document(1, pane_id(app.world(), pane1)).replace(
                "\"fux::model::ids::PaneId\"",
                "\"nowhere::Nope\": (), \"fux::model::ids::PaneId\"",
            ),
            plain(),
        ),
        (
            "registered but disallowed component",
            chain_document(1, pane_id(app.world(), pane1)).replace(
                "\"fux::model::ids::PaneId\"",
                "\"fux::model::components::Process\": Starting, \"fux::model::ids::PaneId\"",
            ),
            plain(),
        ),
        (
            "depth over the limit",
            chain_document(MAX_DEPTH + 1, pane_id(app.world(), pane1)),
            plain(),
        ),
        (
            "unknown pane",
            chain_document(1, 999_999),
            ApplyOptions {
                allow_templates: true,
                ..plain()
            },
        ),
        (
            "unplaced panes without close_unplaced",
            chain_document(1, pane_id(app.world(), pane1)),
            plain(),
        ),
        (
            "no roots",
            format!(
                "(resources: {{}}, entities: {{ {}: (components: {{ \"fux::model::relations::RootOrder\": ([]) }}) }})",
                doc_entity(0)
            ),
            plain(),
        ),
        (
            "resources",
            "(resources: { \"fux::model::relations::RootOrder\": ([]) }, entities: {})".to_owned(),
            plain(),
        ),
    ];
    for (what, document, options) in refusals {
        let result = scene::apply(app.world_mut(), ws, &document, &options);
        let error = result.expect_err(what);
        match what {
            "foreign pane" => assert!(
                matches!(error, SceneError::ForeignPane(_)),
                "{what}: {error}"
            ),
            "unknown type path" => {
                assert!(matches!(error, SceneError::Parse(_)), "{what}: {error}");
            }
            "registered but disallowed component" => {
                assert!(
                    matches!(error, SceneError::ComponentNotAllowed(_)),
                    "{what}: {error}"
                );
            }
            "depth over the limit" => {
                assert!(
                    matches!(error, SceneError::DepthExceeded { .. }),
                    "{what}: {error}"
                );
            }
            "unknown pane" => assert!(
                matches!(error, SceneError::PaneNotFound(_)),
                "{what}: {error}"
            ),
            "unplaced panes without close_unplaced" => {
                assert!(
                    matches!(&error, SceneError::UnplacedPanes(p) if p.len() == 2),
                    "{what}: {error}"
                );
            }
            "no roots" => assert!(
                matches!(error, SceneError::EmptyDocument),
                "{what}: {error}"
            ),
            "resources" => assert!(
                matches!(error, SceneError::ResourcesNotAllowed),
                "{what}: {error}"
            ),
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            roots(app.world(), ws),
            vec![root],
            "{what} changed the roots"
        );
        assert_eq!(shape(app.world(), root), before, "{what} changed the shape");
        check(&mut app);
    }
    // The depth limit itself is reachable.
    let ok = chain_document(MAX_DEPTH, pane_id(app.world(), pane1));
    let report = scene::apply(
        app.world_mut(),
        ws,
        &ok,
        &ApplyOptions {
            close_unplaced: true,
            ..plain()
        },
    )
    .unwrap();
    assert_eq!(report.closed.len(), 2);
    check(&mut app);
}

#[test]
fn node_count_limit_is_enforced_against_the_document() {
    let mut app = app();
    let (ws, root, [pane1, ..]) = three_panes(&mut app);
    app.world_mut().resource_mut::<Limits>().nodes_per_workspace = 3;
    let doc = chain_document(3, pane_id(app.world(), pane1));
    let error = scene::apply(app.world_mut(), ws, &doc, &plain()).unwrap_err();
    assert!(
        matches!(error, SceneError::TooManyNodes { count: 4, max: 3 }),
        "{error}"
    );
    assert_eq!(roots(app.world(), ws), vec![root]);
    check(&mut app);
}

#[test]
fn template_scene_launches_through_the_creation_path_and_closes_unplaced() {
    let mut app = app();
    let (ws, _, [pane1, pane2, pane3]) = three_panes(&mut app);
    let document = scene::builtin::document("two_column").unwrap().to_owned();
    let refused = scene::apply(app.world_mut(), ws, &document, &plain());
    assert!(
        matches!(refused, Err(SceneError::TemplatesNotAllowed)),
        "{refused:?}"
    );
    check(&mut app);

    let report = scene::apply(
        app.world_mut(),
        ws,
        &document,
        &ApplyOptions {
            allow_templates: true,
            close_unplaced: true,
            ..plain()
        },
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.launched.len(), 2);
    assert!(report.adopted.is_empty());
    assert_eq!(report.closed, vec![pane1, pane2, pane3]);
    let world = app.world();
    let root = roots(world, ws)[0];
    let leaves = kids(world, root);
    assert_eq!(leaves.len(), 2);
    for (leaf, pane) in leaves.iter().zip(&report.launched) {
        assert_eq!(placed(world, *leaf), Some(*pane));
        assert!(world.get::<Disabled>(*pane).is_some());
        assert_eq!(world.get::<Process>(*pane), Some(&Process::Starting));
        assert!(
            world.get::<Creation>(*pane).is_some(),
            "launched through Creation"
        );
        let template = world.get::<PaneTemplate>(*pane).unwrap();
        assert!(
            !template.argv.is_empty(),
            "empty argv resolves to the default command"
        );
    }
    for pane in [pane1, pane2, pane3] {
        assert!(
            world.get::<Disabled>(pane).is_some(),
            "closed panes leave the layout"
        );
        assert!(world.get::<PlacedIn>(pane).is_none_or(|p| p.is_empty()));
    }

    // The lifecycle materialises the launched panes and retires the closed ones.
    app.update();
    check(&mut app);
    let effects: Vec<Effect> = app
        .world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .collect();
    let spawned: Vec<Entity> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::SpawnPane { pane, .. } => Some(*pane),
            _ => None,
        })
        .collect();
    assert_eq!(spawned, report.launched);
    app.update();
    check(&mut app);
    let world = app.world();
    for pane in [pane1, pane2, pane3] {
        assert!(
            world.get_entity(pane).is_err(),
            "unplaced panes are closed and gone"
        );
    }
}

#[test]
fn closing_unplaced_panes_frees_their_share_of_the_pane_cap() {
    // Three panes at a cap of three: the two launching leaves of `two_column` fit only because
    // the three unplaced panes close. The closed panes still sit in `WorkspacePanes` until the
    // lifecycle despawns them, so the commit must not recount them.
    let mut app = app();
    let (ws, _, [pane1, pane2, pane3]) = three_panes(&mut app);
    app.world_mut().resource_mut::<Limits>().panes_per_workspace = 3;
    let document = scene::builtin::document("two_column").unwrap().to_owned();
    let viewer = ops::attach_viewer(
        app.world_mut(),
        ws,
        Viewport { rows: 24, cols: 80 },
        Some(pane3),
    )
    .unwrap();
    check(&mut app);

    let report = scene::apply(
        app.world_mut(),
        ws,
        &document,
        &ApplyOptions {
            allow_templates: true,
            close_unplaced: true,
            ..plain()
        },
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.closed, vec![pane1, pane2, pane3]);
    assert_eq!(report.launched.len(), 2, "both leaves launched");
    let world = app.world();
    let root = roots(world, ws)[0];
    let leaves = kids(world, root);
    assert_eq!(leaves.len(), 2, "the whole document is built");
    for (leaf, pane) in leaves.iter().zip(&report.launched) {
        assert_eq!(placed(world, *leaf), Some(*pane));
    }
    // The exact viewer's pane closed: it shows and targets nothing rather than a Disabled pane
    // and leaves the way it would had the pane exited.
    assert!(world.get::<Showing>(viewer).is_none());
    assert!(world.get::<Targets>(viewer).is_none());
    assert!(world.get::<Detaching>(viewer).is_some());

    // Once the lifecycle has retired the closed panes, a cap the document itself exceeds is
    // still refused up front, untouched.
    app.update();
    app.update();
    check(&mut app);
    assert!(
        app.world().get_entity(pane1).is_err(),
        "closed panes are gone"
    );
    app.world_mut().resource_mut::<Limits>().panes_per_workspace = 1;
    let generation_before = app.world().get::<LayoutGeneration>(root).unwrap().0;
    let refused = scene::apply(
        app.world_mut(),
        ws,
        &document,
        &ApplyOptions {
            allow_templates: true,
            close_unplaced: true,
            ..plain()
        },
    );
    assert!(
        matches!(refused, Err(SceneError::TooManyPanes { count: 2, max: 1 })),
        "{refused:?}"
    );
    assert_eq!(roots(app.world(), ws), vec![root]);
    assert_eq!(
        app.world().get::<LayoutGeneration>(root).unwrap().0,
        generation_before
    );
    check(&mut app);
}

#[test]
fn restore_adopts_existing_panes_and_matches_the_old_two_pane_split() {
    let mut app = app();
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    let leaf = ops::spawn_node(world, root, None, grow(), Some(shell())).unwrap();
    let pane1 = placed(world, leaf).unwrap();
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    app.update();
    check(&mut app);
    let dir = temp_dir("restore");
    let report = scene::restore(
        app.world_mut(),
        ws,
        &dir,
        "two_column",
        &ApplyOptions {
            adopt: true,
            ..plain()
        },
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.adopted, vec![pane1]);
    assert_eq!(report.launched.len(), 1);
    assert!(report.closed.is_empty());
    let pane2 = report.launched[0];
    app.update();
    check(&mut app);
    let instance = instance_root(app.world_mut(), viewer);
    let world = app.world();
    let new_root = roots(world, ws)[0];
    let leaves = kids(world, new_root);
    assert_eq!(leaves.len(), 2);
    assert_eq!(placed(world, leaves[0]), Some(pane1));
    assert_eq!(placed(world, leaves[1]), Some(pane2));
    assert_eq!(world.get::<Node>(leaves[0]).unwrap().flex_grow, 1.0);
    assert_eq!(world.get::<Node>(leaves[1]).unwrap().flex_grow, 1.0);
    assert_eq!(world.get::<Name>(leaves[0]).unwrap().as_str(), "left");
    let i1 = leaf_showing(world, instance, pane1);
    let i2 = leaf_showing(world, instance, pane2);
    assert_eq!(size_of(world, i1), UVec2::new(40, 24));
    assert_eq!(size_of(world, i2), UVec2::new(40, 24));
    assert_eq!(
        world.get::<UiGlobalTransform>(i2).unwrap().translation.x as u32,
        60,
        "the right pane is centred at column 60 of 80"
    );
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(pane1));
    assert_eq!(world.get::<Showing>(viewer).map(|s| s.0), Some(new_root));

    // A user file of the same name shadows the built-in.
    let user = scene::builtin::document("two_row").unwrap();
    scene::save(&dir, "two_column", user).unwrap();
    let report = scene::restore(
        app.world_mut(),
        ws,
        &dir,
        "two_column",
        &ApplyOptions {
            adopt: true,
            ..plain()
        },
    )
    .unwrap();
    check(&mut app);
    assert_eq!(report.adopted, vec![pane1, pane2]);
    assert!(report.launched.is_empty());
    let world = app.world();
    let new_root = roots(world, ws)[0];
    assert_eq!(
        world.get::<Node>(new_root).unwrap().flex_direction,
        FlexDirection::Column
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn every_builtin_applies() {
    for name in scene::builtin::NAMES {
        let mut app = app();
        let ws = ops::new_workspace(app.world_mut(), "default").unwrap();
        let viewer =
            ops::attach_viewer(app.world_mut(), ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
        let dir = temp_dir("builtin");
        let report = scene::restore(app.world_mut(), ws, &dir, name, &plain())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        check(&mut app);
        app.update();
        check(&mut app);
        let instance = instance_root(app.world_mut(), viewer);
        let world = app.world();
        let leaves: Vec<Entity> = report
            .launched
            .iter()
            .map(|p| leaf_showing(world, instance, *p))
            .collect();
        let total: u32 = leaves.iter().map(|l| size_of(world, *l).x).sum();
        let is_row = world
            .get::<Node>(roots(world, ws)[0])
            .unwrap()
            .flex_direction
            == FlexDirection::Row;
        if is_row {
            assert_eq!(total, 80, "{name} fills the width");
        } else {
            let total: u32 = leaves.iter().map(|l| size_of(world, *l).y).sum();
            assert_eq!(total, 24, "{name} fills the height");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn files_are_named_listed_and_written_atomically() {
    let dir = temp_dir("files").join("layouts");
    assert_eq!(scene::list(&dir).unwrap(), Vec::<String>::new());
    let doc = scene::builtin::document("two_row").unwrap();
    let path = scene::save(&dir, "mine", doc).unwrap();
    assert_eq!(path, dir.join("mine.scn.ron"));
    scene::save(&dir, "Also-ok_2", doc).unwrap();
    assert_eq!(scene::load(&dir, "mine").unwrap(), doc);
    assert_eq!(
        scene::list(&dir).unwrap(),
        vec!["Also-ok_2".to_owned(), "mine".to_owned()]
    );
    for bad in ["", "../x", "a b", "x/y", &"n".repeat(65)] {
        assert!(
            matches!(
                scene::save(&dir, bad, doc),
                Err(SceneError::InvalidFileName(_))
            ),
            "{bad:?}"
        );
        assert!(
            matches!(scene::load(&dir, bad), Err(SceneError::InvalidFileName(_))),
            "{bad:?}"
        );
    }
    assert!(matches!(
        scene::load(&dir, "missing"),
        Err(SceneError::NotFound(_))
    ));
    assert!(matches!(
        scene::document_named(&dir, "two_column"),
        Ok(d) if d == scene::builtin::TWO_COLUMN
    ));
    assert!(matches!(
        scene::document_named(&dir, "nope"),
        Err(SceneError::NotFound(_))
    ));

    // A write that cannot complete leaves neither the file nor a temp file behind.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = scene::save(&dir, "blocked", doc);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if failed.is_err() {
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains("blocked")), "{names:?}");
    }
    assert_eq!(scene::list(&dir).unwrap().len(), 2);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}
