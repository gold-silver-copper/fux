//! `fux::session` (prompt 3.8): the session document is written atomically and throttled, a
//! fresh App over the same state directory rebuilds it through the template path (`auto`
//! materialises with the historical screen, `ask` waits for per-pane decisions), a bad file is
//! set aside and bootstrap runs, `none` ignores it; the four `fux/session.*` methods over BRP.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers outside #[test] fns; clippy.toml only relaxes test fns"
)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_state::prelude::*;
use bevy_ui::prelude::*;
use common::{Server, code};
use fux::app::build_headless;
use fux::config::Config;
use fux::layout::ops;
use fux::lifecycle::Clock;
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::remote::methods::codes;
use fux::session::{
    self, Historical, LastFocus, RestoreMode, RestorePending, SessionFile, SessionPlugin,
    SessionState,
};
use fux::terminal::Terminal;
use serde_json::json;

const SERVER: &str = "test";

fn app(dir: &Path, mode: RestoreMode) -> App {
    let mut app = build_headless(&Config::default());
    app.add_plugins(SessionPlugin {
        dir: dir.to_path_buf(),
        server: SERVER.into(),
    })
    .insert_resource(mode);
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    app
}

fn check(app: &mut App) {
    check_invariants(app.world_mut()).unwrap();
}

fn sh(tag: &str) -> PaneTemplate {
    PaneTemplate {
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), tag.to_owned()],
        cwd: Some(format!("/tmp/{tag}")),
        ..Default::default()
    }
}

/// `sh` launched with an environment, as `fux/pane.new { env }` or `zor` would.
fn sh_env(tag: &str) -> PaneTemplate {
    PaneTemplate {
        env: vec![("ZOR_LAUNCH_ID".to_owned(), tag.to_owned())],
        ..sh(tag)
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

fn workspace(world: &World, name: &str) -> Entity {
    world.resource::<Ids>().workspace(name).unwrap()
}

fn tag_of(world: &World, pane: Entity) -> String {
    world.get::<LaunchAttribution>(pane).unwrap().argv[2].clone()
}

fn pane_tagged(world: &mut World, tag: &str) -> Entity {
    world
        .query_filtered::<(Entity, &LaunchAttribution), bevy_ecs::query::Allow<Disabled>>()
        .iter(world)
        .find(|(_, a)| a.argv.get(2).map(String::as_str) == Some(tag))
        .map_or_else(|| panic!("no pane tagged {tag}"), |(e, _)| e)
}

/// Shape independent of ids: flex direction, node name, and the tag of the placed pane.
fn shape(world: &World, node: Entity) -> String {
    let n = world.get::<Node>(node).unwrap();
    let mut s = format!(
        "{:?}/{}/{}",
        n.flex_direction,
        world.get::<Name>(node).map_or("", |n| n.as_str()),
        placed(world, node).map_or("-".to_owned(), |p| tag_of(world, p))
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

/// Past the throttle window, so the next update writes what changed since the first save.
fn persist(app: &mut App) {
    app.world_mut().resource_mut::<Clock>().now_ms = 6_000;
    app.update();
}

fn spawns(app: &mut App) -> Vec<Entity> {
    app.world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .filter_map(|e| match e {
            Effect::SpawnPane { pane, .. } => Some(pane),
            _ => None,
        })
        .collect()
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join(format!("{SERVER}.scn.ron"))
}

fn read(dir: &Path) -> String {
    std::fs::read_to_string(session_path(dir)).unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `default`: root `main` = Row [ leaf one, Column "stack" [ leaf two, leaf three ] ], a
/// viewer targeting `three`; `alpha`: one pane `solo`. Materialised once; `one` has screen
/// content and a title; `three` was launched with an environment.
fn populate(app: &mut App) -> Entity {
    let world = app.world_mut();
    let ws = ops::new_workspace(world, "default").unwrap();
    let root = ops::new_root(world, ws, "main").unwrap();
    let leaf1 = ops::spawn_node(world, root, None, grow(), Some(sh("one"))).unwrap();
    let pane1 = placed(world, leaf1).unwrap();
    let (_, pane2) = ops::split(world, pane1, SplitDirection::Right, sh("two")).unwrap();
    let (_, pane3) = ops::split(world, pane2, SplitDirection::Below, sh_env("three")).unwrap();
    let column = world
        .get::<ChildOf>(world.get::<PlacedIn>(pane2).unwrap().iter().next().unwrap())
        .unwrap()
        .parent();
    world.entity_mut(column).insert(Name::new("stack"));
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    ops::target(world, viewer, pane3).unwrap();
    let alpha = ops::new_workspace(world, "alpha").unwrap();
    let alpha_root = ops::new_root(world, alpha, "main").unwrap();
    ops::spawn_node(world, alpha_root, None, grow(), Some(sh("solo"))).unwrap();
    app.update();
    check(app);
    let world = app.world_mut();
    world
        .get_mut::<Terminal>(pane1)
        .unwrap()
        .feed(b"hello\r\nworld");
    world.entity_mut(pane1).insert(Title("make".into()));
    ws
}

// ---------------------------------------------------------------------------------------------

#[test]
fn session_is_written_atomically_throttled_and_restored_in_auto_mode() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session");

    let mut first = app(&dir, RestoreMode::Auto);
    let ws = populate(&mut first);
    let root = roots(first.world(), ws)[0];
    let before = shape(first.world(), root);
    persist(&mut first);
    check(&mut first);
    let path = session_path(&dir);
    assert_eq!(
        entries(&dir),
        vec![format!("{SERVER}.scn.ron")],
        "no temp file left"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let doc = read(&dir);
    for expected in [
        "Historical",
        "hello",
        "world",
        "make",
        "stack",
        "LastFocus",
        "alpha",
    ] {
        assert!(doc.contains(expected), "document lacks {expected}: {doc}");
    }
    let instance = first.world().resource::<ServerInstance>().clone();
    for secret in [
        "PaneId",
        "NodeId",
        "ServerInstance",
        "nonce",
        "token",
        "Viewer",
        "Process",
    ] {
        assert!(!doc.contains(secret), "document leaks {secret}");
    }
    assert!(instance.nonce.is_empty() || !doc.contains(&instance.nonce));

    // Throttled: a change within 5 s schedules the write; it lands once the clock passes.
    let world = first.world_mut();
    let beta = ops::new_workspace(world, "beta").unwrap();
    let beta_root = ops::new_root(world, beta, "main").unwrap();
    ops::spawn_node(world, beta_root, None, grow(), Some(sh("tardy"))).unwrap();
    first.update();
    assert!(!read(&dir).contains("beta"), "write throttled");
    assert_eq!(
        first.world().resource::<SessionState>().next_save_ms,
        Some(11_000)
    );
    first.world_mut().resource_mut::<Clock>().now_ms = 11_000;
    first.update();
    assert!(read(&dir).contains("beta"));

    // A retiring workspace is not persisted; shutdown writes immediately.
    let world = first.world_mut();
    ops::retire_workspace(world, beta, 11_000).unwrap();
    world.entity_mut(root).insert(Name::new("renamed"));
    world
        .resource_mut::<NextState<ServerMode>>()
        .set(ServerMode::ShuttingDown);
    first.update();
    let doc = read(&dir);
    assert!(!doc.contains("beta") && !doc.contains("tardy"), "{doc}");
    assert!(doc.contains("renamed"));
    let before = before.replacen("/main/", "/renamed/", 1);
    drop(first);

    // A fresh App over the same directory rebuilds the session.
    let mut second = app(&dir, RestoreMode::Auto);
    session::restore_or_bootstrap(second.world_mut(), "default").unwrap();
    check(&mut second);
    let world = second.world_mut();
    let ws2 = workspace(world, "default");
    let root2 = roots(world, ws2)[0];
    assert_eq!(shape(world, root2), before);
    assert_eq!(world.get::<Name>(root2).unwrap().as_str(), "renamed");
    assert_eq!(roots(world, workspace(world, "alpha")).len(), 1);
    assert!(world.resource::<Ids>().workspace("beta").is_none());
    let one = pane_tagged(world, "one");
    let three = pane_tagged(world, "three");
    assert_eq!(
        world.get::<LaunchAttribution>(one).unwrap().cwd.as_deref(),
        Some("/tmp/one")
    );
    assert_eq!(
        world.get::<PaneTemplate>(three),
        Some(&sh_env("three")),
        "the launch environment survives the restart"
    );
    assert!(
        world.get::<PaneTemplate>(one).unwrap().env.is_empty(),
        "a pane launched without an environment restores without one"
    );
    assert_eq!(world.get::<Title>(one).unwrap().0, "make");
    assert_eq!(
        world.get::<Historical>(one).unwrap().lines,
        vec!["hello".to_owned(), "world".to_owned()]
    );
    assert!(world.get::<LastFocus>(three).is_some());
    assert!(world.get::<LastFocus>(one).is_none());
    assert!(world.get::<RestorePending>(one).is_none(), "auto decides");
    assert!(world.get::<Disabled>(one).is_some() && world.get::<Terminal>(one).is_none());

    // The first viewer resumes on the saved focus; later ones start at the first pane.
    let viewer = ops::attach_viewer(world, ws2, Viewport { rows: 24, cols: 80 }, None).unwrap();
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(three));
    assert!(world.get::<LastFocus>(three).is_none(), "focus consumed");
    let viewer = ops::attach_viewer(world, ws2, Viewport { rows: 24, cols: 80 }, None).unwrap();
    assert_eq!(world.get::<Targets>(viewer).map(|t| t.0), Some(one));

    second.update();
    check(&mut second);
    let spawned = spawns(&mut second);
    assert_eq!(spawned.len(), 4, "one SpawnPane per restored pane");
    let world = second.world();
    for &pane in &spawned {
        assert!(world.get::<Terminal>(pane).is_some());
        assert!(world.get::<Historical>(pane).is_none(), "history consumed");
        assert!(matches!(
            world.get::<Creation>(pane).map(|c| c.kind),
            Some(CreationKind::Restore)
        ));
    }
    let lines = world.get::<Terminal>(one).unwrap().screen_lines();
    assert_eq!(&lines[..2], ["hello", "world"]);
    assert!(
        lines[2].starts_with("───"),
        "separator below the old screen: {lines:?}"
    );
    assert!(
        lines[3].trim().is_empty(),
        "the new prompt goes below: {lines:?}"
    );
}

#[test]
fn ask_mode_parks_panes_until_restore_or_skip() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session");
    let mut first = app(&dir, RestoreMode::Auto);
    populate(&mut first);
    persist(&mut first);
    drop(first);

    let mut second = app(&dir, RestoreMode::Ask);
    session::restore_or_bootstrap(second.world_mut(), "default").unwrap();
    check(&mut second);
    second.update();
    check(&mut second);
    assert!(
        spawns(&mut second).is_empty(),
        "nothing launches before a decision"
    );
    let world = second.world_mut();
    let pending = session::pending(world);
    assert_eq!(pending.len(), 4);
    let one = pane_tagged(world, "one");
    let three = pane_tagged(world, "three");
    assert!(pending.contains(&one) && pending.contains(&three));
    let ws = workspace(world, "default");
    let root = roots(world, ws)[0];
    assert_eq!(kids(world, root).len(), 2);

    session::decide_restore(world, one).unwrap();
    assert!(session::decide_restore(world, one).is_err(), "decided once");
    second.update();
    check(&mut second);
    assert_eq!(spawns(&mut second), vec![one]);
    let lines = second.world().get::<Terminal>(one).unwrap().screen_lines();
    assert_eq!(&lines[..2], ["hello", "world"]);

    let world = second.world_mut();
    session::decide_skip(world, three).unwrap();
    second.update();
    second.update();
    check(&mut second);
    let world = second.world_mut();
    assert!(world.get_entity(three).is_err(), "skipped pane is gone");
    assert!(spawns(&mut second).is_empty());
    let world = second.world_mut();
    assert_eq!(session::pending(world).len(), 2);
    let leaves = kids(world, root);
    assert_eq!(
        leaves.len(),
        2,
        "the column collapsed into its remaining leaf"
    );
    assert_eq!(tag_of(world, placed(world, leaves[1]).unwrap()), "two");
    assert!(
        world
            .get::<Name>(leaves[1])
            .is_none_or(|n| n.as_str() != "stack"),
        "the container is gone"
    );
}

#[test]
fn bad_files_are_set_aside_and_bootstrap_runs() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session");
    std::fs::create_dir_all(&dir).unwrap();
    let docs = [
        "this is not RON".to_owned(),
        // A root without a `Node`.
        "(resources: {}, entities: {4294967295: (components: {\"fux::model::components::Workspace\": (), \"fux::model::ids::WorkspaceName\": \"default\", \"fux::model::relations::RootOrder\": ([4294967294])}), 4294967294: (components: {\"fux::model::components::TemplateRoot\": ()})})".to_owned(),
        // A component outside the session allowlist.
        "(resources: {}, entities: {4294967295: (components: {\"fux::model::components::Workspace\": (), \"fux::model::ids::WorkspaceName\": \"default\", \"fux::model::relations::RootOrder\": ([4294967294]), \"fux::model::components::Process\": Starting}), 4294967294: (components: {\"bevy_ui::ui_node::Node\": ()})})".to_owned(),
    ];
    for (i, doc) in docs.iter().enumerate() {
        std::fs::write(session_path(&dir), doc).unwrap();
        let mut app = app(&dir, RestoreMode::Auto);
        session::restore_or_bootstrap(app.world_mut(), "default").unwrap();
        app.update();
        check(&mut app);
        let world = app.world_mut();
        let ws = workspace(world, "default");
        assert_eq!(roots(world, ws).len(), 1, "case {i}: bootstrap ran");
        assert_eq!(
            spawns(&mut app).len(),
            1,
            "case {i}: the bootstrap pane launches"
        );
        let names = entries(&dir);
        assert!(
            names
                .iter()
                .any(|n| n.starts_with(&format!("{SERVER}.scn.ron.rejected-"))),
            "case {i}: {names:?}"
        );
        assert!(
            !session_path(&dir).is_file() || app.world().resource::<SessionState>().saves > 0,
            "case {i}: the bad file was moved away before the first save"
        );
        for name in names {
            std::fs::remove_file(dir.join(name)).unwrap();
        }
    }
}

#[test]
fn none_mode_ignores_the_file_and_no_file_means_bootstrap() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session");

    let mut plain = app(&dir, RestoreMode::Auto);
    session::restore_or_bootstrap(plain.world_mut(), "default").unwrap();
    let world = plain.world_mut();
    assert!(world.resource::<Ids>().workspace("default").is_some());
    assert!(world.resource::<Ids>().workspace("alpha").is_none());
    drop(plain);

    let mut first = app(&dir, RestoreMode::Auto);
    populate(&mut first);
    persist(&mut first);
    drop(first);
    let saved = read(&dir);

    let mut second = app(&dir, RestoreMode::None);
    session::restore_or_bootstrap(second.world_mut(), "default").unwrap();
    let world = second.world_mut();
    assert!(world.resource::<Ids>().workspace("alpha").is_none());
    assert_eq!(roots(world, workspace(world, "default")).len(), 1);
    assert_eq!(
        kids(world, roots(world, workspace(world, "default"))[0]).len(),
        1
    );
    assert_eq!(
        read(&dir),
        saved,
        "the file is not touched before the first update"
    );
}

#[test]
fn brp_methods_save_report_and_decide() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("session");
    let setup_dir = dir.clone();
    let server = Server::start_with(move |world| {
        world.insert_resource(SessionFile {
            dir: setup_dir,
            server: SERVER.into(),
        });
        world.init_resource::<SessionState>();
        // The bootstrap pane pretends to be a restored one awaiting a decision.
        let pane = *world.resource::<Ids>().panes.values().next().unwrap();
        world.entity_mut(pane).insert((
            RestorePending,
            Historical {
                lines: vec!["old".into()],
            },
        ));
    });

    let status = server.call("fux/session.status", json!({})).unwrap();
    assert!(!status["exists"].as_bool().unwrap());
    assert_eq!(status["pending"].as_array().unwrap().len(), 1);
    let pending = &status["pending"][0];
    assert_eq!(pending["workspace"], "default");
    assert_eq!(pending["history_lines"], 1);
    let pane = pending["pane"].as_u64().unwrap();

    let saved = server.call("fux/session.save", json!({})).unwrap();
    assert_eq!(saved["path"], session_path(&dir).display().to_string());
    assert!(saved["bytes"].as_u64().unwrap() > 0);
    let doc = read(&dir);
    assert!(doc.contains("Historical") && doc.contains("old"));
    let status = server.call("fux/session.status", json!({})).unwrap();
    assert!(status["exists"].as_bool().unwrap());
    assert_eq!(status["saves"], 1);

    assert_eq!(
        code(server.call("fux/session.skip", json!({ "pane": 999 }))),
        codes::NOT_FOUND
    );
    let decided = server
        .call("fux/session.restore", json!({ "pane": pane }))
        .unwrap();
    assert_eq!(decided["pane"], pane);
    assert_eq!(decided["pending"], 0);
    assert_eq!(
        code(server.call("fux/session.skip", json!({ "pane": pane }))),
        codes::INVALID,
        "already decided"
    );
    let materialised = server.with_world(move |world| {
        let pane = world.resource::<Ids>().pane(PaneId(pane)).unwrap();
        world.get::<Terminal>(pane).map(Terminal::screen_lines)
    });
    assert_eq!(materialised.unwrap()[0], "old");
}
