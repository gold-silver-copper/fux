//! `docs/model.md` structural invariants: the typed graph passes `check_invariants`; each rule
//! marked (S) is caught when violated behind the helpers' backs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use bevy_app::App;
use bevy_ecs::prelude::*;
use common::{handle, spawn_graph};
use zor::config::Config;
use zor::model::invariants::check_invariants;
use zor::model::*;

fn app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = zor::app::build_headless(&Config::default(), dir.path());
    (app, dir)
}

fn violation(f: impl FnOnce(&mut World)) -> u8 {
    let (mut app, _dir) = app();
    let world = app.world_mut();
    spawn_graph(world);
    assert_eq!(check_invariants(world), Ok(()));
    f(world);
    match check_invariants(world) {
        Ok(()) => panic!("no violation detected"),
        Err((n, _)) => n,
    }
}

#[test]
fn typed_graph_holds_every_invariant_and_ids_index_it() {
    let (mut app, _dir) = app();
    let world = app.world_mut();
    let g = spawn_graph(world);
    assert_eq!(check_invariants(world), Ok(()));
    let ids = world.resource::<Ids>();
    assert_eq!(ids.task("t1"), Some(g.task));
    assert_eq!(ids.prompt("p1"), Some(g.prompt));
    assert_eq!(ids.operation("launch-1"), Some(g.launch));
    assert_eq!(ids.check("c1"), Some(g.check));
    assert_eq!(ids.group("g1"), Some(g.group));
    assert!(ids.operation_taken("p1") && ids.operation_taken("stop-2"));
    // Helpers refuse what the invariants would catch.
    assert!(matches!(
        spawn_task(
            world,
            TaskSpec {
                id: "t1",
                title: "dup",
                location: Location::Cwd("/".into()),
                created_ms: 0
            }
        ),
        Err(ModelError::DuplicateId(_))
    ));
    assert!(matches!(
        spawn_task(
            world,
            TaskSpec {
                id: "bad id",
                title: "x",
                location: Location::Cwd("/".into()),
                created_ms: 0
            }
        ),
        Err(ModelError::InvalidId(_))
    ));
    assert!(matches!(
        spawn_operation(world, "p1", g.attempt, OperationKind::Stop),
        Err(ModelError::DuplicateId(_))
    ));
    assert!(matches!(
        spawn_prompt(
            world,
            PromptSpec {
                id: "q",
                attempt: g.task,
                text: "x",
                deadline_ms: 1,
                report_token: "t"
            }
        ),
        Err(ModelError::WrongKind { .. })
    ));
    assert!(matches!(
        spawn_group(world, "g2", &[g.launch], 1, &[]),
        Err(ModelError::Invalid(_))
    ));
    assert_eq!(check_invariants(world), Ok(()));
    // Retiring the group frees its members' edges; despawning a task then takes its linked
    // subgraph and its index entries with it.
    retire_group(world, g.group).unwrap();
    assert_eq!(check_invariants(world), Ok(()));
    world.despawn(g.task);
    world.flush();
    assert_eq!(check_invariants(world), Ok(()));
    let ids = world.resource::<Ids>();
    assert!(ids.task("t1").is_none() && ids.prompt("p1").is_none() && ids.check("c1").is_none());
}

#[test]
fn invariant_1_ids_are_valid_and_indexed() {
    assert_eq!(
        violation(|world| {
            world.spawn((Task, TaskId("not valid!".into()), TaskState::Open));
        }),
        1
    );
    assert_eq!(
        violation(|world| {
            // A duplicate id keeps the first mapping; the second entity is unindexed.
            world.spawn((Task, TaskId("t1".into()), TaskState::Open));
        }),
        1
    );
}

#[test]
fn invariant_2_relationships_point_at_their_kinds() {
    assert_eq!(
        violation(|world| {
            world.spawn((Attempt, AttemptId(99), AttemptState::Pending));
        }),
        2
    );
    assert_eq!(
        violation(|world| {
            let not_a_task = world.spawn_empty().id();
            world.spawn((
                Attempt,
                AttemptId(98),
                AttemptOf(not_a_task),
                handle("x", 1, None),
                AttemptState::Pending,
            ));
        }),
        2
    );
}

#[test]
fn invariant_3_one_current_attempt_per_task() {
    assert_eq!(
        violation(|world| {
            let task = world.resource::<Ids>().task("t1").unwrap();
            spawn_attempt(
                world,
                AttemptSpec {
                    task,
                    ownership: Ownership::Managed,
                    handle: handle("nonce-a", 70, None),
                },
            )
            .unwrap();
        }),
        3
    );
}

#[test]
fn invariant_4_verified_needs_a_coherent_seal() {
    assert_eq!(
        violation(|world| {
            let task = world.resource::<Ids>().task("t1").unwrap();
            world.entity_mut(task).insert(TaskState::Closed {
                outcome: TaskOutcome::Verified,
            });
        }),
        4
    );
    assert_eq!(
        violation(|world| {
            let ids = world.resource::<Ids>();
            let task = ids.task("t1").unwrap();
            let source = ids.source("s1").unwrap();
            let check = ids.check("c1").unwrap();
            world.entity_mut(check).insert(CheckState::Failed);
            world.entity_mut(task).insert((
                TaskState::Closed {
                    outcome: TaskOutcome::Verified,
                },
                Seal {
                    source,
                    checks: vec![check],
                    artifacts: Vec::new(),
                    sealed_ms: 1,
                    generation: 1,
                },
            ));
        }),
        4
    );
    // The typed close refuses `Verified` without a seal and accepts it with one.
    let (mut app, _dir) = app();
    let world = app.world_mut();
    let g = spawn_graph(world);
    assert!(close_task(world, g.task, TaskOutcome::Verified, 9).is_err());
    world.entity_mut(g.task).insert(Seal {
        source: g.source,
        checks: vec![g.check],
        artifacts: vec![g.artifact],
        sealed_ms: 1,
        generation: 1,
    });
    close_task(world, g.task, TaskOutcome::Verified, 9).unwrap();
    assert_eq!(check_invariants(world), Ok(()));
    assert!(close_task(world, g.task, TaskOutcome::Cancelled, 10).is_err());
}

#[test]
fn invariant_5_finished_attempts_carry_evidence_and_are_never_lost() {
    assert_eq!(
        violation(|world| {
            let attempt = world.resource::<Ids>().attempt(AttemptId(1)).unwrap();
            world.entity_mut(attempt).insert(AttemptState::Finished);
        }),
        5
    );
    assert_eq!(
        violation(|world| {
            let attempt = world.resource::<Ids>().attempt(AttemptId(1)).unwrap();
            world.entity_mut(attempt).insert((
                AttemptState::Finished,
                FinalEvidence::default(),
                Lost,
            ));
        }),
        5
    );
}

#[test]
fn invariant_6_one_pending_prompt_per_pane() {
    assert_eq!(
        violation(|world| {
            let attempt = world.resource::<Ids>().attempt(AttemptId(1)).unwrap();
            spawn_prompt(
                world,
                PromptSpec {
                    id: "p2",
                    attempt,
                    text: "again",
                    deadline_ms: 5,
                    report_token: "t",
                },
            )
            .unwrap();
        }),
        6
    );
}

#[test]
fn invariant_7_receipts_follow_delivery() {
    assert_eq!(
        violation(|world| {
            let prompt = world.resource::<Ids>().prompt("p1").unwrap();
            world.entity_mut(prompt).insert(Receipt::default());
        }),
        7
    );
    assert_eq!(
        violation(|world| {
            let prompt = world.resource::<Ids>().prompt("p1").unwrap();
            world.entity_mut(prompt).insert(Delivery::Delivered);
        }),
        7
    );
}

#[test]
fn invariant_8_results_match_terminal_checks() {
    assert_eq!(
        violation(|world| {
            let check = world.resource::<Ids>().check("c1").unwrap();
            world.entity_mut(check).insert(CheckState::Failed);
        }),
        8
    );
    assert_eq!(
        violation(|world| {
            let task = world.resource::<Ids>().task("t1").unwrap();
            let check = spawn_check(
                world,
                CheckSpec {
                    id: "c2",
                    task,
                    source: None,
                    command: CheckCommand::default(),
                    requirement: None,
                    generation: 2,
                },
            )
            .unwrap();
            world.entity_mut(check).insert(CheckState::Passed);
        }),
        8
    );
}

#[test]
fn invariant_9_removing_worktrees_have_no_unresolved_work() {
    assert_eq!(
        violation(|world| {
            let ids = world.resource::<Ids>();
            let worktree = ids.worktree("w1").unwrap();
            let task = ids.task("t1").unwrap();
            spawn_check(
                world,
                CheckSpec {
                    id: "c-queued",
                    task,
                    source: None,
                    command: CheckCommand::default(),
                    requirement: None,
                    generation: 3,
                },
            )
            .unwrap();
            world.entity_mut(worktree).insert(WorktreeState::Removing);
        }),
        9
    );
}

#[test]
fn invariant_10_groups_are_bounded_and_acyclic() {
    assert_eq!(
        violation(|world| {
            let ids = world.resource::<Ids>();
            let launch = ids.operation("launch-1").unwrap();
            let stop = ids.operation("stop-2").unwrap();
            world.entity_mut(launch).insert(After(vec![stop]));
        }),
        10
    );
    assert_eq!(
        violation(|world| {
            let group = world.resource::<Ids>().group("g1").unwrap();
            world.entity_mut(group).insert(Concurrency(3));
        }),
        10
    );
}

#[test]
fn invariant_11_counts_stay_within_limits() {
    assert_eq!(
        violation(|world| {
            world.resource_mut::<Limits>().tasks = 1;
        }),
        11
    );
}

#[test]
fn invariant_12_stop_requested_needs_a_managed_attempt() {
    assert_eq!(
        violation(|world| {
            let task = world.resource::<Ids>().task("t2").unwrap();
            world.entity_mut(task).insert(StopRequested);
        }),
        12
    );
}

#[test]
fn state_tables_match_the_contract() {
    assert!(TaskState::Open.may_become(TaskState::Running));
    assert!(TaskState::Running.may_become(TaskState::Open));
    assert!(!TaskState::Open.may_become(TaskState::Blocked));
    let cancelled = TaskState::Closed {
        outcome: TaskOutcome::Cancelled,
    };
    assert!(!cancelled.may_become(TaskState::Open));
    assert!(!cancelled.may_become(cancelled));
    assert!(AttemptState::Launching.may_become(AttemptState::Finished));
    assert!(!AttemptState::Finished.may_become(AttemptState::Live));
    assert!(CheckState::Queued.may_become(CheckState::Uncertain));
    assert!(!CheckState::Passed.may_become(CheckState::Running));
    assert!(Delivery::Uncertain.is_pending() && !Delivery::Delivered.is_pending());
    assert!(WaitState::TimedOut.is_terminal() && !WaitState::NeedsInput.is_terminal());
}
