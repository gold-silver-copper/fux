//! `Signal` → `ShuttingDown` → `Terminate` for every live pane → `Exit` once none is live or
//! the deadline passed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use bevy_state::prelude::*;
use fux::app::build_headless;
use fux::config::Config;
use fux::layout::ops;
use fux::lifecycle::{self, Clock, SHUTDOWN_DEADLINE_MS};
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::runner::{self, Params, Sources};

fn step(app: &mut App, messages: Vec<Inbound>) -> Vec<Effect> {
    app.world_mut()
        .resource_mut::<Messages<Inbound>>()
        .write_batch(messages);
    app.update();
    let world = app.world_mut();
    let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
    check_invariants(world).unwrap();
    effects
}

fn panes(app: &mut App) -> Vec<Entity> {
    app.world_mut()
        .query_filtered::<Entity, (With<Pane>, Allow<Disabled>)>()
        .iter(app.world())
        .collect()
}

/// A workspace with two live panes and one viewer.
fn two_live_panes() -> (App, Vec<Entity>) {
    let mut app = build_headless(&Config::default());
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    let world = app.world_mut();
    lifecycle::bootstrap(world, "default", &["/bin/sh".into()]).unwrap();
    let ws = world.resource::<Ids>().workspace("default").unwrap();
    let viewer = ops::attach_viewer(world, ws, Viewport { rows: 24, cols: 80 }, None).unwrap();
    step(&mut app, vec![]);
    let first = panes(&mut app)[0];
    step(
        &mut app,
        vec![Inbound::PaneSpawned {
            pane: first,
            pid: 1,
        }],
    );
    step(
        &mut app,
        vec![Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Split {
                direction: SplitDirection::Right,
                template: None,
            },
        }],
    );
    let second = panes(&mut app).into_iter().find(|&p| p != first).unwrap();
    step(
        &mut app,
        vec![Inbound::PaneSpawned {
            pane: second,
            pid: 2,
        }],
    );
    assert_eq!(
        *app.world().resource::<State<ServerMode>>().get(),
        ServerMode::Serving
    );
    (app, vec![first, second])
}

fn terminated(effects: &[Effect]) -> Vec<Entity> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Terminate { pane } => Some(*pane),
            _ => None,
        })
        .collect()
}

fn exits(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Exit { code: 0 }))
}

#[test]
fn signal_terminates_every_live_pane_then_exits_when_all_exited() {
    let (mut app, panes) = two_live_panes();
    let effects = step(&mut app, vec![Inbound::Signal(Signal::Terminate)]);
    let mut got = terminated(&effects);
    got.sort();
    let mut want = panes.clone();
    want.sort();
    assert_eq!(got, want);
    assert!(!exits(&effects), "panes are still terminating");
    assert_eq!(
        *app.world().resource::<State<ServerMode>>().get(),
        ServerMode::ShuttingDown
    );

    let effects = step(
        &mut app,
        vec![Inbound::PaneExited {
            pane: panes[0],
            code: 0,
        }],
    );
    assert!(!exits(&effects), "one pane still live");
    let effects = step(
        &mut app,
        vec![Inbound::PaneExited {
            pane: panes[1],
            code: 0,
        }],
    );
    assert!(exits(&effects));
}

/// A hot pane cannot delay a signal: with far more pane output queued than one step drains,
/// the runner's next batch still carries the signal, and one `update` enters `ShuttingDown`.
#[test]
fn signal_is_applied_by_the_next_step_ahead_of_queued_pane_output() {
    let (mut app, panes) = two_live_panes();
    let params = Params::default();
    let (inbound_tx, inbound) = async_channel::bounded(8192);
    let (control_tx, control) = async_channel::bounded(runner::CONTROL_QUEUE);
    for _ in 0..5000 {
        inbound_tx
            .try_send(Inbound::PaneOutput {
                pane: panes[0],
                bytes: b"x".to_vec(),
            })
            .unwrap();
    }
    control_tx
        .try_send(Inbound::Signal(Signal::Terminate))
        .unwrap();
    let sources = Sources { control, inbound };
    let mut batch = Vec::with_capacity(params.batch);
    runner::collect(&sources, &mut batch, &params, None).unwrap();
    assert!(matches!(
        batch.first(),
        Some(Inbound::Signal(Signal::Terminate))
    ));
    assert!(batch.len() <= params.batch + 1);
    assert!(sources.inbound.len() >= 5000 - params.batch);

    let effects = step(&mut app, batch);
    assert_eq!(terminated(&effects).len(), 2);
    assert_eq!(
        *app.world().resource::<State<ServerMode>>().get(),
        ServerMode::ShuttingDown
    );
}

#[test]
fn shutdown_deadline_exits_with_panes_still_terminating() {
    let (mut app, _panes) = two_live_panes();
    let effects = step(&mut app, vec![Inbound::Signal(Signal::Interrupt)]);
    assert_eq!(terminated(&effects).len(), 2);
    assert!(!exits(&effects));
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000 + SHUTDOWN_DEADLINE_MS - 1;
    assert!(!exits(&step(&mut app, vec![])));
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000 + SHUTDOWN_DEADLINE_MS;
    assert!(exits(&step(&mut app, vec![])));
}

#[test]
fn panes_going_live_during_shutdown_are_terminated() {
    let mut app = build_headless(&Config::default());
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    lifecycle::bootstrap(app.world_mut(), "default", &["/bin/sh".into()]).unwrap();
    let effects = step(&mut app, vec![]);
    let pane = panes(&mut app)[0];
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnPane { .. }))
    );
    let effects = step(&mut app, vec![Inbound::Signal(Signal::Terminate)]);
    assert!(
        terminated(&effects).is_empty(),
        "a starting pane has no process yet"
    );
    assert!(exits(&effects), "nothing live: exit immediately");
    let effects = step(&mut app, vec![Inbound::PaneSpawned { pane, pid: 9 }]);
    assert_eq!(terminated(&effects), vec![pane], "late spawn is terminated");
}
