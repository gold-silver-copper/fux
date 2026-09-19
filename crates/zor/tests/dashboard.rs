//! Consumer-visible scene identity, stale target refusal and view-only ownership.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers"
)]
use bevy_app::App;
use bevy_ecs::prelude::*;
use serde_json::{Value, json};
use zor::dashboard::{self, Input, Open, Row, scene::SceneWorld};
use zor::machines::{
    self, MachinesFile,
    catalog::MachineEntry,
    supervision::{Freshness, RemoteTask, RemoteView, Supervision},
};
use zor::model::{Clock, Effect, Inbound, MachineId};

fn app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut app =
        zor::app::build_headless(&zor::config::Config::default(), &dir.path().join("state"));
    app.world_mut().resource_mut::<Clock>().now_ms = 100;
    app.world_mut().resource_mut::<MachinesFile>().path = dir.path().join("private/machines.json");
    app.finish();
    app.cleanup();
    app.update();
    machines::edit_catalog(app.world_mut(), |catalog| {
        catalog.machines.push(MachineEntry {
            id: "remote".into(),
            name: "Remote".into(),
            control: None,
            attachments: Default::default(),
        });
        Ok(())
    })
    .unwrap();
    app.update();
    let world = app.world_mut();
    world.resource_mut::<zor::lifecycle::Link>().instance = Some("fux-original".into());
    let machine = world
        .query::<(Entity, &MachineId)>()
        .iter(world)
        .find(|(_, id)| id.0 == "remote")
        .unwrap()
        .0;
    world.get_mut::<Supervision>(machine).unwrap().apply_poll(
        Ok(RemoteView {
            instance: "zor-original".into(),
            rows: vec![task("a"), task("b")],
            ..Default::default()
        }),
        100,
    );
    (app, dir)
}
fn task(id: &str) -> RemoteTask {
    RemoteTask {
        id: id.into(),
        title: id.into(),
        state: "running".into(),
        ..Default::default()
    }
}
fn row(key: &str, text: &str) -> Row {
    Row {
        key: key.into(),
        text: text.into(),
        target: None,
        freshness: Freshness::Fresh,
        attention: false,
    }
}
fn calls(app: &mut App) -> Vec<(u64, String, Value)> {
    app.world_mut()
        .resource_mut::<Messages<Effect>>()
        .drain()
        .filter_map(|effect| match effect {
            Effect::FuxCall {
                call,
                method,
                params,
            } if call & (0xff << 56) == dashboard::CALL_TAG => Some((call, method, params)),
            _ => None,
        })
        .collect()
}
fn ack(app: &mut App, call: u64) {
    app.world_mut()
        .resource_mut::<Messages<Inbound>>()
        .write(Inbound::FuxReply {
            call,
            result: Ok(json!({})),
        });
    app.update();
}
fn mount(app: &mut App) -> dashboard::State {
    let state = dashboard::open(
        app.world_mut(),
        Open {
            workspace: "dashboard".into(),
            node: 44,
            generation: 1,
            machine: Some("remote".into()),
            viewer: Some(7),
        },
    )
    .unwrap();
    let open = calls(app).pop().unwrap();
    ack(app, open.0);
    let update = calls(app).pop().unwrap();
    ack(app, update.0);
    dashboard::state(app.world_mut(), state.id).unwrap()
}

#[test]
fn equal_snapshots_emit_nothing_and_changed_rows_keep_provider_identity() {
    let (app, _dir) = app();
    let rows = vec![row("a", "A"), row("b", "B")];
    let mut scene = SceneWorld::new(app.world(), &rows).unwrap();
    assert!(scene.sync(app.world(), &rows).unwrap().unwrap().full);
    let identities = scene.row_nodes();
    assert!(scene.sync(app.world(), &rows).unwrap().is_none());
    let changed = vec![row("a", "A changed"), row("b", "B")];
    let delta = scene.sync(app.world(), &changed).unwrap().unwrap();
    assert!(!delta.full);
    assert!(delta.ron.contains("A changed"));
    assert!(!delta.ron.contains("\"B\""));
    assert_eq!(scene.row_nodes(), identities);
    scene
        .sync(app.world(), &[changed[1].clone(), changed[0].clone()])
        .unwrap();
    assert_eq!(
        scene.row_nodes(),
        vec![identities[1].clone(), identities[0].clone()]
    );
    scene.sync(app.world(), &[changed[1].clone()]).unwrap();
    assert_eq!(scene.key_for_node(identities[0].1), None);
}

#[test]
fn stale_or_foreign_viewer_cannot_change_selection() {
    let (mut app, _dir) = app();
    let state = mount(&mut app);
    let (key, node) = state.row_nodes[1].clone();
    assert_ne!(state.selected.as_ref(), Some(&key));
    assert!(
        dashboard::input(
            app.world_mut(),
            state.id,
            8,
            state.revision,
            Some(node),
            Input::Select
        )
        .is_err()
    );
    assert!(
        dashboard::input(
            app.world_mut(),
            state.id,
            7,
            state.revision + 1,
            Some(node),
            Input::Select
        )
        .is_err()
    );
    assert_eq!(
        dashboard::state(app.world_mut(), state.id)
            .unwrap()
            .selected,
        state.selected
    );
    let selected = dashboard::input(
        app.world_mut(),
        state.id,
        7,
        state.revision,
        Some(node),
        Input::Select,
    )
    .unwrap();
    assert_eq!(selected.selected, Some(key));
}

#[test]
fn replacement_never_inherits_selection_or_old_node_authority() {
    let (mut app, _dir) = app();
    let old = mount(&mut app);
    let old_node = old.row_nodes[0].1;
    let machine = {
        let world = app.world_mut();
        world
            .query::<(Entity, &MachineId)>()
            .iter(world)
            .find(|(_, id)| id.0 == "remote")
            .unwrap()
            .0
    };
    app.world_mut()
        .get_mut::<Supervision>(machine)
        .unwrap()
        .view
        .as_mut()
        .unwrap()
        .instance = "zor-replacement".into();
    // Even before scene refresh, an old row cannot select the replacement target.
    assert!(
        dashboard::input(
            app.world_mut(),
            old.id,
            7,
            old.revision,
            None,
            Input::Cancel
        )
        .is_err()
    );
    app.update();
    let update = calls(&mut app).pop().unwrap();
    ack(&mut app, update.0);
    let current = dashboard::state(app.world_mut(), old.id).unwrap();
    assert_eq!(current.selected, old.selected);
    assert!(
        dashboard::input(
            app.world_mut(),
            old.id,
            7,
            current.revision,
            Some(old_node),
            Input::Select
        )
        .is_err()
    );
    assert!(
        dashboard::input(
            app.world_mut(),
            old.id,
            7,
            current.revision,
            None,
            Input::Cancel
        )
        .is_err()
    );
    assert!(
        machines::snapshot(app.world(), "remote")
            .unwrap()
            .actions
            .is_empty()
    );
}

#[test]
fn closing_destroys_only_surface_and_late_open_is_closed_without_activation() {
    let (mut app, _dir) = app();
    let state = mount(&mut app);
    let before = machines::snapshot(app.world(), "remote").unwrap().view;
    dashboard::close(app.world_mut(), state.id).unwrap();
    let emitted = calls(&mut app);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].1, "fux/surface.close");
    ack(&mut app, emitted[0].0);
    assert_eq!(
        dashboard::state(app.world_mut(), state.id).unwrap().status,
        "closed"
    );
    assert_eq!(
        machines::snapshot(app.world(), "remote").unwrap().view,
        before
    );
    let opening = dashboard::open(
        app.world_mut(),
        Open {
            workspace: "dashboard".into(),
            node: 45,
            generation: 2,
            machine: Some("remote".into()),
            viewer: Some(7),
        },
    )
    .unwrap();
    let pending = calls(&mut app).pop().unwrap();
    dashboard::close(app.world_mut(), opening.id).unwrap();
    ack(&mut app, pending.0);
    let emitted = calls(&mut app);
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].1, "fux/surface.close");
    assert!(dashboard::input(app.world_mut(), opening.id, 7, 0, None, Input::Attach).is_err());
}

#[test]
fn local_cancel_targets_selected_task_and_closing_preserves_the_other_task() {
    let (mut app, dir) = app();
    let mut tasks = Vec::new();
    for id in ["target", "decoy"] {
        tasks.push(
            zor::lifecycle::create_task(
                app.world_mut(),
                zor::lifecycle::TaskSpec {
                    id: id.into(),
                    title: id.into(),
                    location: zor::model::Location::Cwd(dir.path().display().to_string()),
                },
            )
            .unwrap(),
        );
    }
    let state = dashboard::open(
        app.world_mut(),
        Open {
            workspace: "dashboard".into(),
            node: 46,
            generation: 1,
            machine: Some("local".into()),
            viewer: Some(7),
        },
    )
    .unwrap();
    let opening = calls(&mut app).pop().unwrap();
    ack(&mut app, opening.0);
    let update = calls(&mut app).pop().unwrap();
    ack(&mut app, update.0);
    let state = dashboard::state(app.world_mut(), state.id).unwrap();
    let key = &state
        .rows
        .iter()
        .find(|r| r.target.as_ref().is_some_and(|t| t.task == "target"))
        .unwrap()
        .key;
    let node = state.row_nodes.iter().find(|(k, _)| k == key).unwrap().1;
    dashboard::input(
        app.world_mut(),
        state.id,
        7,
        state.revision,
        Some(node),
        Input::Cancel,
    )
    .unwrap();
    assert!(matches!(
        app.world().get::<zor::model::TaskState>(tasks[0]),
        Some(zor::model::TaskState::Closed {
            outcome: zor::model::TaskOutcome::Cancelled
        })
    ));
    assert_eq!(
        app.world().get::<zor::model::TaskState>(tasks[1]),
        Some(&zor::model::TaskState::Open)
    );
    dashboard::close(app.world_mut(), state.id).unwrap();
    assert_eq!(
        app.world().get::<zor::model::TaskState>(tasks[1]),
        Some(&zor::model::TaskState::Open)
    );
}

#[test]
fn attention_is_ordered_first_but_expired_evidence_cannot_authorize_an_action() {
    let (mut app, _dir) = app();
    let machine = {
        let world = app.world_mut();
        world
            .query::<(Entity, &MachineId)>()
            .iter(world)
            .find(|(_, id)| id.0 == "remote")
            .unwrap()
            .0
    };
    app.world_mut()
        .get_mut::<Supervision>(machine)
        .unwrap()
        .view
        .as_mut()
        .unwrap()
        .rows[1]
        .needs_input = true;
    let rows = dashboard::rows(app.world(), Some("remote")).unwrap();
    assert_eq!(rows[0].target.as_ref().unwrap().task, "b");
    let state = mount(&mut app);
    app.world_mut().resource_mut::<Clock>().now_ms = 100 + zor::machines::supervision::FRESH_MS + 1;
    app.update();
    let update = calls(&mut app).pop().unwrap();
    ack(&mut app, update.0);
    let state = dashboard::state(app.world_mut(), state.id).unwrap();
    assert!(matches!(state.rows[0].freshness, Freshness::Stale { .. }));
    assert!(
        dashboard::input(
            app.world_mut(),
            state.id,
            7,
            state.revision,
            None,
            Input::Cancel
        )
        .is_err()
    );
    assert!(
        machines::snapshot(app.world(), "remote")
            .unwrap()
            .actions
            .is_empty()
    );
}

#[test]
fn timed_out_open_keeps_only_cleanup_authority_for_its_late_completion() {
    let (mut app, _dir) = app();
    let state = dashboard::open(
        app.world_mut(),
        Open {
            workspace: "dashboard".into(),
            node: 49,
            generation: 1,
            machine: Some("remote".into()),
            viewer: Some(7),
        },
    )
    .unwrap();
    let opening = calls(&mut app).pop().unwrap();
    app.world_mut().resource_mut::<Clock>().now_ms = 100 + dashboard::CALL_TIMEOUT_MS;
    app.update();
    let cleanup = calls(&mut app).pop().unwrap();
    assert_eq!(cleanup.1, "fux/surface.close");
    ack(&mut app, cleanup.0);
    assert_eq!(
        dashboard::state(app.world_mut(), state.id).unwrap().status,
        "closed"
    );
    ack(&mut app, opening.0);
    let late_cleanup = calls(&mut app).pop().unwrap();
    assert_eq!(late_cleanup.1, "fux/surface.close");
    assert_eq!(late_cleanup.2["expected_provider"], state.provider);
    assert!(dashboard::input(app.world_mut(), state.id, 7, 0, None, Input::Cancel).is_err());
}
