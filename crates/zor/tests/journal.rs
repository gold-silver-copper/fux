//! Journal durability (`docs/model.md` "Journal and archive"): snapshot/restore round trip over
//! a temp state dir preserves ids, relationships and states; over-bound extraction refuses and
//! keeps the previous file; the archive moves a closed old task but never an uncertain one,
//! and `zor/task.inspect` finds it read-only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::collections::BTreeMap;
use std::path::Path;

use bevy_app::App;
use bevy_ecs::relationship::RelationshipTarget;
use common::spawn_graph;
use zor::config::Config;
use zor::journal::{self, Journal};
use zor::model::invariants::check_invariants;
use zor::machines::{
    self, MachinesFile,
    catalog::{Catalog, MachineEntry},
    intents::{ActionIntent, IntentLog},
    supervision::{ActionKind, ActionPhase, ActionRecord, Freshness, RemoteView, Supervision},
};
use zor::model::*;

fn app(state: &Path) -> App {
    zor::app::build_headless(&Config::default(), state)
}

fn journal_file(state: &Path) -> std::path::PathBuf {
    state.join(journal::JOURNAL_FILE)
}

#[test]
fn snapshot_and_restore_round_trip_preserves_the_graph() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    {
        let mut app = app(&state);
        app.update();
        let g = spawn_graph(app.world_mut());
        let world = app.world_mut();
        world.entity_mut(g.prompt).insert((
            Delivery::Reserved,
            Receipt {
                instance: "nonce-a".into(),
                pane: 7,
                operation: 3,
                state: "reserved".into(),
                bytes_written: 0,
                seq: None,
                expires_ms: 9_000,
            },
        ));
        world
            .entity_mut(g.other_attempt)
            .insert((Uncertain, Problem("no final evidence".into())));
        world.entity_mut(g.worktree).insert(WorktreeState::Ready);
        app.update();
        assert_eq!(app.world().resource::<Generation>().0, 1);
        assert!(journal_file(&state).is_file());
        let mode = std::fs::metadata(journal_file(&state))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        // Nothing changed: no second generation.
        app.update();
        assert_eq!(app.world().resource::<Generation>().0, 1);
    }
    let mut app = app(&state);
    app.update();
    let world = app.world_mut();
    assert_eq!(check_invariants(world), Ok(()));
    let ids = world.resource::<Ids>().clone();
    let task = ids.task("t1").unwrap();
    let attempt = ids.attempt(AttemptId(1)).unwrap();
    let prompt = ids.prompt("p1").unwrap();
    let check = ids.check("c1").unwrap();
    let other = ids.task("t2").unwrap();
    assert_eq!(world.get::<AttemptOf>(attempt).unwrap().0, task);
    assert_eq!(world.get::<PromptOf>(prompt).unwrap().0, attempt);
    assert_eq!(
        world.get::<Attempts>(task).unwrap().iter().next(),
        Some(attempt)
    );
    assert_eq!(world.get::<CheckOf>(check).unwrap().0, task);
    assert_eq!(
        world.get::<CheckOn>(check).unwrap().0,
        ids.source("s1").unwrap()
    );
    assert_eq!(world.get::<Results>(check).unwrap().len(), 1);
    assert_eq!(world.get::<CheckState>(check), Some(&CheckState::Passed));
    assert_eq!(world.get::<Delivery>(prompt), Some(&Delivery::Reserved));
    assert_eq!(world.get::<Receipt>(prompt).unwrap().operation, 3);
    assert_eq!(world.get::<PaneHandle>(attempt).unwrap().pid, Some(4242));
    assert_eq!(world.get::<Title>(other).unwrap().0, "second");
    assert_eq!(
        world.get::<WorktreeState>(ids.worktree("w1").unwrap()),
        Some(&WorktreeState::Ready)
    );
    let other_attempt = ids.attempt(AttemptId(2)).unwrap();
    assert!(world.get::<Uncertain>(other_attempt).is_some());
    assert_eq!(
        world
            .get::<Members>(ids.group("g1").unwrap())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        world
            .get::<After>(ids.operation("stop-2").unwrap())
            .unwrap()
            .0,
        vec![ids.operation("launch-1").unwrap()]
    );
    // Runtime-only machines in the graph fixture are not a durable machine authority.
    assert!(ids.machine("m1").is_none());
    // Counters continue above the restored ids.
    assert_eq!(world.resource_mut::<Ids>().allocate_attempt(), AttemptId(3));
    // The generation continues from the restored document (never regresses): the restored
    // graph re-committed as generation 2 on this incarnation's first update; nothing changed
    // since, so no third.
    assert_eq!(world.resource::<Generation>().0, 2);
    app.update();
    assert_eq!(app.world().resource::<Generation>().0, 2);
}

#[test]
fn catalog_is_the_only_machine_authority_across_workflow_journal_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let path = dir.path().join("private/machines.json");
    let mut catalog = Catalog::empty();
    catalog.add(MachineEntry {
        id: "m-one".into(),
        name: "laptop".into(),
        control: None,
        attachments: BTreeMap::new(),
    }).unwrap();
    catalog.save(&path).unwrap();
    let record = ActionRecord {
        id: 1,
        kind: Some(ActionKind::Stop),
        task: "remote-task".into(),
        instance: "zor-original".into(),
        attempt: Some(7),
        phase: ActionPhase::Uncertain,
        problem: Some("reply lost".into()),
        ..Default::default()
    };
    let intent = ActionIntent {
        operation: "stop-once".into(),
        machine: "m-one".into(),
        control: "127.0.0.1:1234".into(),
        record: record.clone(),
    };
    {
        let mut app = app(&state);
        app.insert_resource(MachinesFile { path: path.clone(), asset_root: None });
        app.update();
        let machine = app.world().resource::<Ids>().machine("m-one").unwrap();
        assert_eq!(machines::snapshot(app.world(), "laptop").unwrap().id, "m-one");
        assert!(!journal_file(&state).exists(), "catalog activation is not a workflow commit");
        app.world_mut().resource_mut::<IntentLog>().commit(intent.clone()).unwrap();
        spawn_task(app.world_mut(), TaskSpec {
            id: "local-task",
            title: "unrelated workflow",
            location: Location::Cwd("/tmp".into()),
            created_ms: 0,
        }).unwrap();
        app.update();
        let committed = std::fs::read(journal_file(&state)).unwrap();

        // Machine supervision and passive agent observations must neither dirty the
        // workflow journal nor become authority when it is restored.
        app.world_mut().get_mut::<Supervision>(machine).unwrap().apply_poll(
            Ok(RemoteView { instance: "zor-observed".into(), ..Default::default() }),
            0,
        );
        spawn_observed_agent(
            app.world_mut(),
            common::handle("fux-observed", 9, Some(77)),
            Some(machine),
        ).unwrap();
        app.update();
        assert!(machines::snapshot(app.world(), "m-one").unwrap().view.is_some());
        assert_eq!(std::fs::read(journal_file(&state)).unwrap(), committed);
        assert!(!app.world().resource::<Journal>().is_dirty());

        // Commit while runtime observations exist, not just before they were acquired.
        let task = app.world().resource::<Ids>().task("local-task").unwrap();
        zor::lifecycle::cancel_task(app.world_mut(), task).unwrap();
        app.update();
        assert!(!app.world().resource::<Journal>().is_dirty());
    }
    // Current catalog data, not an older workflow snapshot, owns the semantic name.
    catalog.find_mut("m-one").unwrap().name = "renamed".into();
    catalog.save(&path).unwrap();
    let mut app = app(&state);
    app.insert_resource(MachinesFile { path: path.clone(), asset_root: None });
    app.update();
    let world = app.world_mut();
    assert_eq!(check_invariants(world), Ok(()));
    assert!(!world.resource::<Journal>().is_frozen());
    let machines = world.query_filtered::<(bevy_ecs::entity::Entity, &MachineId), bevy_ecs::query::With<Machine>>()
        .iter(world)
        .map(|(entity, id)| (entity, id.0.clone()))
        .collect::<Vec<_>>();
    let machine = world.resource::<Ids>().machine("m-one").unwrap();
    assert_eq!(machines, vec![(machine, "m-one".into())]);
    let snapshot = machines::snapshot(world, "renamed").unwrap();
    assert_eq!(snapshot.id, "m-one");
    assert!(machines::snapshot(world, "laptop").is_err());
    assert!(snapshot.view.is_none(), "runtime observations cannot survive restart");
    assert_eq!(snapshot.freshness, Freshness::Offline);
    assert_eq!(snapshot.actions, vec![record]);
    assert_eq!(world.resource::<IntentLog>().find("stop-once"), Some(&intent));
    assert_eq!(Catalog::load(&path).unwrap(), catalog);
    let task = world.resource::<Ids>().task("local-task").unwrap();
    assert_eq!(world.get::<TaskState>(task), Some(&TaskState::Closed { outcome: TaskOutcome::Cancelled }));
    assert_eq!(world.get::<Title>(task).unwrap().0, "unrelated workflow");
}

#[test]
fn over_bound_extraction_refuses_and_keeps_the_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let mut app = app(&state);
    app.update();
    let g = spawn_graph(app.world_mut());
    app.update();
    let before = std::fs::read_to_string(journal_file(&state)).unwrap();
    // Bytes.
    app.world_mut().resource_mut::<Limits>().journal_bytes = 64;
    app.world_mut()
        .entity_mut(g.task)
        .insert(TaskState::Running);
    app.update();
    assert_eq!(
        std::fs::read_to_string(journal_file(&state)).unwrap(),
        before
    );
    assert_eq!(app.world().resource::<Generation>().0, 1);
    assert!(app.world().resource::<Journal>().is_dirty());
    // Counts.
    app.world_mut().resource_mut::<Limits>().journal_bytes = 4 * 1024 * 1024;
    app.world_mut().resource_mut::<Limits>().tasks = 1;
    app.update();
    assert_eq!(
        std::fs::read_to_string(journal_file(&state)).unwrap(),
        before
    );
    assert_eq!(app.world().resource::<Generation>().0, 1);
    // Back within bounds, the pending change commits.
    app.world_mut().resource_mut::<Limits>().tasks = 128;
    app.update();
    assert_ne!(
        std::fs::read_to_string(journal_file(&state)).unwrap(),
        before
    );
    assert_eq!(app.world().resource::<Generation>().0, 2);
}

#[test]
fn a_corrupt_journal_is_preserved_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(journal_file(&state), "(this is not a world").unwrap();
    let mut app = app(&state);
    app.world_mut()
        .insert_resource(bevy_ecs::error::FallbackErrorHandler(bevy_ecs::error::warn));
    app.update();
    assert!(app.world().resource::<Journal>().is_frozen());
    spawn_graph(app.world_mut());
    app.update();
    assert_eq!(
        std::fs::read_to_string(journal_file(&state)).unwrap(),
        "(this is not a world"
    );
}

#[test]
fn archive_moves_old_closed_tasks_but_never_uncertain_ones() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let mut app = app(&state);
    app.update();
    let g = spawn_graph(app.world_mut());
    let world = app.world_mut();
    let now = 1_757_894_400_000; // 2025-09-15
    world.resource_mut::<Clock>().now_ms = now;
    world.resource_mut::<Limits>().archive_after_ms = 1_000;
    // t1: closed long ago, everything resolved.
    world.entity_mut(g.attempt).insert((
        AttemptState::Finished,
        FinalEvidence {
            exit_code: Some(0),
            ..FinalEvidence::default()
        },
    ));
    world.entity_mut(g.prompt).insert(Delivery::Released);
    world.entity_mut(g.launch).insert(OperationPhase::Closed);
    retire_group(world, g.group).unwrap();
    close_task(world, g.task, TaskOutcome::Cancelled, now - 5_000).unwrap();
    // t2: closed long ago, but its attempt is uncertain and its stop operation too.
    world.entity_mut(g.other_attempt).insert(Uncertain);
    world.entity_mut(g.stop).insert(OperationPhase::Uncertain);
    close_task(world, g.other_task, TaskOutcome::Cancelled, now - 5_000).unwrap();
    assert!(journal::archivable(world, g.task));
    assert!(!journal::archivable(world, g.other_task));
    assert_eq!(check_invariants(world), Ok(()));
    app.update();

    let world = app.world_mut();
    let ids = world.resource::<Ids>().clone();
    assert!(ids.task("t1").is_none(), "t1 was archived");
    assert!(ids.task("t2").is_some(), "uncertain t2 stays live");
    assert!(
        ids.prompt("p1").is_none() && ids.check("c1").is_none() && ids.worktree("w1").is_none()
    );
    assert_eq!(check_invariants(world), Ok(()));
    let archive = state.join(journal::ARCHIVE_DIR).join("2025-09-15.scn.ron");
    assert!(archive.is_file(), "archive {} exists", archive.display());
    let mode = std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o400);
    let live = std::fs::read_to_string(journal_file(&state)).unwrap();
    assert!(!live.contains("\"t1\"") && live.contains("\"t2\""));
    let archived = std::fs::read_to_string(&archive).unwrap();
    assert!(
        archived.contains("\"t1\"") && archived.contains("\"p1\"") && archived.contains("\"c1\"")
    );

    // The archive answers read-only lookups; the live journal knows nothing of t1.
    let found = world
        .resource::<Journal>()
        .find_archived(world, "t1")
        .unwrap()
        .unwrap();
    assert_eq!(found.0, "2025-09-15.scn.ron");
    assert_eq!(found.1.as_deref(), Some("Closed { outcome: Cancelled }"));
    assert!(
        world
            .resource::<Journal>()
            .find_archived(world, "t2")
            .unwrap()
            .is_none()
    );

    // Once t2 is resolved, a later sweep on the same date merges it into the same file.
    world.entity_mut(g.other_attempt).remove::<Uncertain>();
    world
        .entity_mut(g.other_attempt)
        .insert((AttemptState::Finished, FinalEvidence::default()));
    world.entity_mut(g.stop).insert(OperationPhase::Closed);
    world.resource_mut::<Clock>().now_ms = now + 120_000;
    app.update();
    let world = app.world_mut();
    assert!(world.resource::<Ids>().task("t2").is_none());
    let archived = std::fs::read_to_string(&archive).unwrap();
    assert!(archived.contains("\"t1\"") && archived.contains("\"t2\""));
    assert!(
        world
            .resource::<Journal>()
            .find_archived(world, "t2")
            .unwrap()
            .is_some()
    );
    // The machine is not task-owned and stays live.
    assert!(world.resource::<Ids>().machine("m1").is_some());
}

#[test]
fn civil_dates_are_utc_gregorian() {
    assert_eq!(journal::civil_date(0), "1970-01-01");
    assert_eq!(journal::civil_date(1_757_894_400_000), "2025-09-15");
    assert_eq!(journal::civil_date(951_782_400_000), "2000-02-29");
}
