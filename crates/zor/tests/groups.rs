//! Durable prompt groups (GROUPS.md, invariants 10, 18, 24): plan validation, admission that
//! commits the member and the cursor before the prompt's `FuxCall`, capacity released only by
//! verification plus the original prompt's `Delivered` receipt, `--after` needing a retained
//! seal, cancellation releasing coordination only, and `check_invariants` after every update.

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
use zor::config::Config;
use zor::groups::{self, AfterSpec, Cursor, GroupError, GroupSpec, MemberSpec, MemberStatus};
use zor::model::invariants::check_invariants;
use zor::model::*;

struct Harness {
    app: App,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut app = zor::app::build_headless(&Config::default(), &dir.path().join("state"));
        app.update();
        // The fux link the prompts' pane identities belong to.
        app.world_mut().write_message(Inbound::FuxLink {
            instance: Some("fux-1".into()),
        });
        app.update();
        Self { app, _dir: dir }
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn step(&mut self) -> Vec<Effect> {
        self.app.update();
        let world = self.app.world_mut();
        let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
        check_invariants(world).unwrap();
        effects
    }

    fn journal(&self) -> String {
        let path = self
            .app
            .world()
            .resource::<zor::journal::Journal>()
            .path
            .clone();
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// A managed live attempt of a fresh task with one prepared prompt.
    fn worker(&mut self, n: u64) -> Worker {
        let world = self.world();
        let id = format!("t{n}");
        let task = spawn_task(
            world,
            TaskSpec {
                id: &id,
                title: &id,
                location: Location::Cwd("/tmp".into()),
                created_ms: 1,
            },
        )
        .unwrap();
        let attempt = spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Managed,
                handle: common::handle("fux-1", n, Some(100 + n as u32)),
            },
        )
        .unwrap();
        world.entity_mut(attempt).insert(AttemptState::Live);
        world.entity_mut(task).insert(TaskState::Running);
        let prompt = spawn_prompt(
            world,
            PromptSpec {
                id: &format!("p{n}"),
                attempt,
                text: "work",
                deadline_ms: 60_000,
                report_token: "tok",
            },
        )
        .unwrap();
        Worker {
            task,
            attempt,
            prompt,
            pane: n,
        }
    }

    fn delivered(&mut self, w: &Worker) {
        self.world().entity_mut(w.prompt).insert((
            Delivery::Delivered,
            Receipt {
                instance: "fux-1".into(),
                pane: w.pane,
                operation: 1,
                state: "delivered".into(),
                bytes_written: 4,
                seq: Some(1),
                expires_ms: 90_000,
            },
        ));
    }

    fn verified(&mut self, w: &Worker) {
        let world = self.world();
        let source = spawn_source(
            world,
            &format!("s-{}", w.pane),
            w.task,
            SourceRevision {
                expression: "HEAD".into(),
                commit: "abc".into(),
            },
        )
        .unwrap();
        let check = spawn_check(
            world,
            CheckSpec {
                id: &format!("c-{}", w.pane),
                task: w.task,
                source: Some(source),
                command: CheckCommand {
                    argv: vec!["true".into()],
                    timeout_ms: 1_000,
                },
                requirement: None,
                generation: 1,
            },
        )
        .unwrap();
        world.entity_mut(check).insert(CheckState::Passed);
        spawn_result(world, check, Verdict::Passed, OutputTail::default()).unwrap();
        world.entity_mut(w.task).insert(Seal {
            source,
            checks: vec![check],
            artifacts: vec![],
            sealed_ms: 5,
            generation: 1,
        });
        close_task(world, w.task, TaskOutcome::Verified, 5).unwrap();
    }

    fn phase(&self, member: Entity) -> OperationPhase {
        *self.app.world().get::<OperationPhase>(member).unwrap()
    }

    fn delivery(&self, w: &Worker) -> Delivery {
        *self.app.world().get::<Delivery>(w.prompt).unwrap()
    }

    fn intent(&self, group: Entity) -> GroupIntent {
        *self.app.world().get::<GroupIntent>(group).unwrap()
    }

    fn cursor(&self, group: Entity) -> u32 {
        self.app.world().get::<Cursor>(group).unwrap().0
    }

    fn member(&self, op: &str) -> Entity {
        self.app.world().resource::<Ids>().operation(op).unwrap()
    }
}

struct Worker {
    task: Entity,
    attempt: Entity,
    prompt: Entity,
    pane: u64,
}

fn member(operation: &str, n: u64) -> MemberSpec {
    MemberSpec {
        operation: operation.into(),
        prompt: format!("p{n}"),
    }
}

fn after(operation: &str, prerequisite: &str) -> AfterSpec {
    AfterSpec {
        operation: operation.into(),
        prerequisite: prerequisite.into(),
    }
}

fn plan(id: &str, concurrency: u32, members: Vec<MemberSpec>, after: Vec<AfterSpec>) -> GroupSpec {
    GroupSpec {
        id: id.into(),
        concurrency,
        members,
        after,
    }
}

/// Every effect except the lifecycle's periodic `fux/pane.capture` observation of live panes.
fn actions(effects: &[Effect]) -> Vec<&Effect> {
    effects
        .iter()
        .filter(|e| !matches!(e, Effect::FuxCall { method, .. } if method == "fux/pane.capture"))
        .collect()
}

fn fux_calls(effects: &[Effect]) -> Vec<(u64, String)> {
    actions(effects)
        .into_iter()
        .filter_map(|e| match e {
            Effect::FuxCall { call, method, .. } => Some((*call, method.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn plans_are_validated_atomically_and_retries_are_idempotent() {
    let mut h = Harness::new();
    let a = h.worker(1);
    let _b = h.worker(2);
    let _c = h.worker(3);
    let refused = |r: Result<Entity, GroupError>| matches!(r, Err(GroupError::Refused(_)));

    assert!(refused(groups::create(
        h.world(),
        plan("g", 1, vec![], vec![])
    )));
    assert!(refused(groups::create(
        h.world(),
        plan("g", 0, vec![member("a", 1)], vec![])
    )));
    assert!(refused(groups::create(
        h.world(),
        plan("g", 2, vec![member("a", 1)], vec![])
    )));
    // Self-dependency, a cycle and an outside prerequisite are rejected before any spawn.
    assert!(refused(groups::create(
        h.world(),
        plan("g", 1, vec![member("a", 1)], vec![after("a", "a")])
    )));
    assert!(refused(groups::create(
        h.world(),
        plan(
            "g",
            1,
            vec![member("a", 1), member("b", 2)],
            vec![after("a", "b"), after("b", "a")]
        )
    )));
    assert!(refused(groups::create(
        h.world(),
        plan("g", 1, vec![member("a", 1)], vec![after("a", "zzz")])
    )));
    assert!(matches!(
        groups::create(
            h.world(),
            plan("g", 1, vec![member("a", 1), member("a", 2)], vec![])
        ),
        Err(GroupError::Refused(_))
    ));
    assert!(matches!(
        groups::create(h.world(), plan("g", 1, vec![member("a", 9)], vec![])),
        Err(GroupError::NotFound(_))
    ));
    assert!(h.world().resource::<Ids>().group("g").is_none());
    assert!(h.world().resource::<Ids>().operation("a").is_none());
    assert_eq!(check_invariants(h.world()), Ok(()));

    let spec = plan(
        "g",
        2,
        vec![member("a", 1), member("b", 2), member("c", 3)],
        vec![after("c", "a"), after("c", "b")],
    );
    let group = groups::create(h.world(), spec.clone()).unwrap();
    assert!(actions(&h.step()).is_empty());
    assert_eq!(groups::create(h.world(), spec).unwrap(), group);
    assert!(matches!(
        groups::create(
            h.world(),
            plan(
                "g",
                1,
                vec![member("a", 1), member("b", 2), member("c", 3)],
                vec![]
            )
        ),
        Err(GroupError::Conflict(_))
    ));
    // The member operations live on the prompts' attempts as group steps.
    let c = h.member("c");
    assert_eq!(
        h.world().get::<OperationKind>(c),
        Some(&OperationKind::GroupStep)
    );
    assert_eq!(h.world().get::<After>(c).unwrap().0.len(), 2);
    let record = groups::inspect(h.world(), group).unwrap();
    assert_eq!(record.members[2].status, MemberStatus::DependenciesPending);
    assert_eq!(record.members[0].status, MemberStatus::Queued);
    // An active group pins its attempts: another group over the same prompt is refused.
    assert!(refused(groups::create(
        h.world(),
        plan("h", 1, vec![member("x", 1)], vec![])
    )));
    let _ = a.attempt;
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn admission_commits_member_and_cursor_before_the_fux_call() {
    let mut h = Harness::new();
    let a = h.worker(1);
    let b = h.worker(2);
    let group = groups::create(
        h.world(),
        plan("g", 1, vec![member("a", 1), member("b", 2)], vec![]),
    )
    .unwrap();
    assert!(actions(&h.step()).is_empty());
    assert_eq!(h.cursor(group), 0);

    let admitted = groups::admit(h.world(), group).unwrap();
    assert_eq!(admitted, vec![h.member("a")]);
    assert_eq!(h.phase(h.member("a")), OperationPhase::Submitting);
    assert_eq!(h.cursor(group), 1);
    // Committed before the effect: the update that emits the reserve call has already written
    // the admitted member and the rotated cursor to the journal.
    let effects = h.step();
    let calls = fux_calls(&effects);
    assert_eq!(calls.len(), 1, "{effects:?}");
    assert_eq!(calls[0].1, "fux/input.reserve");
    let journal = h.journal();
    assert!(journal.contains("Submitting"), "{journal}");
    assert!(journal.contains("Cursor"), "{journal}");
    assert!(journal.contains("(1)"), "{journal}");
    assert_eq!(h.delivery(&a), Delivery::Prepared);
    assert_eq!(h.delivery(&b), Delivery::Prepared);

    // Capacity is taken: a second step admits nothing and sends nothing.
    assert!(groups::admit(h.world(), group).unwrap().is_empty());
    assert!(fux_calls(&h.step()).is_empty());
    assert_eq!(h.phase(h.member("b")), OperationPhase::Prepared);
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn capacity_is_released_only_by_verification_and_delivery() {
    let mut h = Harness::new();
    let a = h.worker(1);
    let b = h.worker(2);
    let c = h.worker(3);
    let group = groups::create(
        h.world(),
        plan(
            "g",
            1,
            vec![member("a", 1), member("b", 2), member("c", 3)],
            vec![],
        ),
    )
    .unwrap();
    h.step();
    assert_eq!(
        groups::admit(h.world(), group).unwrap(),
        vec![h.member("a")]
    );
    h.step();

    // Delivered alone keeps the slot (GROUPS.md:61-63).
    h.delivered(&a);
    h.step();
    assert_eq!(h.phase(h.member("a")), OperationPhase::Attached);
    assert!(groups::admit(h.world(), group).unwrap().is_empty());
    assert_eq!(
        groups::inspect(h.world(), group).unwrap().members[0].status,
        MemberStatus::VerificationRequired
    );

    // Verification alone (task b never had its prompt delivered) keeps the slot too.
    h.verified(&a);
    h.step();
    assert_eq!(h.phase(h.member("a")), OperationPhase::Closed);
    assert_eq!(
        groups::admit(h.world(), group).unwrap(),
        vec![h.member("b")]
    );
    assert_eq!(h.cursor(group), 2);
    h.step();
    h.verified(&b);
    h.step();
    assert_eq!(h.phase(h.member("b")), OperationPhase::Submitting);
    assert!(groups::admit(h.world(), group).unwrap().is_empty());
    h.delivered(&b);
    h.step();
    assert_eq!(h.phase(h.member("b")), OperationPhase::Closed);
    assert_eq!(
        groups::admit(h.world(), group).unwrap(),
        vec![h.member("c")]
    );
    assert_eq!(h.cursor(group), 0);
    h.step();
    h.delivered(&c);
    h.verified(&c);
    h.step();
    assert_eq!(h.intent(group), GroupIntent::Complete);
    let record = groups::inspect(h.world(), group).unwrap();
    assert_eq!(record.active_count, 0);
    assert!(
        record
            .members
            .iter()
            .all(|m| m.status == MemberStatus::Verified)
    );
    assert!(matches!(
        groups::admit(h.world(), group),
        Err(GroupError::Refused(_))
    ));
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn after_needs_a_retained_seal_not_a_delivery() {
    let mut h = Harness::new();
    let a = h.worker(1);
    let _b = h.worker(2);
    let group = groups::create(
        h.world(),
        plan(
            "g",
            2,
            vec![member("a", 1), member("b", 2)],
            vec![after("b", "a")],
        ),
    )
    .unwrap();
    h.step();
    assert_eq!(
        groups::admit(h.world(), group).unwrap(),
        vec![h.member("a")]
    );
    h.step();
    h.delivered(&a);
    h.step();
    // Delivered, even attached, is not verification: b still waits.
    assert!(groups::admit(h.world(), group).unwrap().is_empty());
    assert_eq!(
        groups::inspect(h.world(), group).unwrap().members[1].status,
        MemberStatus::DependenciesPending
    );
    h.verified(&a);
    h.step();
    assert_eq!(
        groups::admit(h.world(), group).unwrap(),
        vec![h.member("b")]
    );
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn cancellation_releases_coordination_only() {
    let mut h = Harness::new();
    let a = h.worker(1);
    let b = h.worker(2);
    let group = groups::create(
        h.world(),
        plan("g", 1, vec![member("a", 1), member("b", 2)], vec![]),
    )
    .unwrap();
    h.step();
    groups::admit(h.world(), group).unwrap();
    h.step();
    h.delivered(&a);
    h.step();

    groups::cancel(h.world(), group).unwrap();
    let effects = h.step();
    assert!(
        actions(&effects).is_empty(),
        "no signal, no stop, no input: {effects:?}"
    );
    assert_eq!(h.intent(group), GroupIntent::Cancelled);
    // Delivered bytes stay delivered; the never-admitted prompt is released.
    assert_eq!(h.delivery(&a), Delivery::Delivered);
    assert_eq!(h.delivery(&b), Delivery::Released);
    assert_eq!(h.phase(h.member("a")), OperationPhase::Attached);
    assert_eq!(h.phase(h.member("b")), OperationPhase::Prepared);
    // Sessions, attempts and task outcomes are untouched.
    for w in [&a, &b] {
        assert_eq!(
            h.world().get::<AttemptState>(w.attempt),
            Some(&AttemptState::Live)
        );
        assert_eq!(
            h.world().get::<TaskState>(w.task),
            Some(&TaskState::Running)
        );
    }
    assert!(matches!(
        groups::admit(h.world(), group),
        Err(GroupError::Refused(_))
    ));
    assert!(matches!(
        groups::resume(h.world(), group),
        Err(GroupError::Refused(_))
    ));
    let record = groups::inspect(h.world(), group).unwrap();
    assert_eq!(
        record.active_count, 1,
        "admitted-but-unverified is retained"
    );
    assert_eq!(record.status, groups::GroupStatus::Cancelled);
    // Repeated cancellation is read-only.
    let generation = h.world().resource::<Generation>().0;
    groups::cancel(h.world(), group).unwrap();
    h.step();
    assert_eq!(h.world().resource::<Generation>().0, generation);
    // A verified task is never touched by cancellation either.
    h.verified(&a);
    h.step();
    assert_eq!(
        h.world().get::<TaskState>(a.task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Verified
        })
    );
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn pause_stops_selection_and_resume_advances_automatically() {
    let mut h = Harness::new();
    let _a = h.worker(1);
    let _b = h.worker(2);
    let group = groups::create(
        h.world(),
        plan("g", 2, vec![member("a", 1), member("b", 2)], vec![]),
    )
    .unwrap();
    h.step();
    groups::pause(h.world(), group).unwrap();
    assert!(matches!(
        groups::admit(h.world(), group),
        Err(GroupError::Refused(_))
    ));
    assert!(actions(&h.step()).is_empty());
    assert_eq!(h.phase(h.member("a")), OperationPhase::Prepared);

    groups::resume(h.world(), group).unwrap();
    assert_eq!(h.intent(group), GroupIntent::Automatic);
    let effects = h.step();
    assert_eq!(fux_calls(&effects).len(), 2, "{effects:?}");
    assert_eq!(h.phase(h.member("a")), OperationPhase::Submitting);
    assert_eq!(h.phase(h.member("b")), OperationPhase::Submitting);
    assert_eq!(h.cursor(group), 0);
    // Nothing further is sent while the submissions are pending.
    assert!(fux_calls(&h.step()).is_empty());
    assert_eq!(check_invariants(h.world()), Ok(()));
}

#[test]
fn group_and_worktree_methods_answer_over_http() {
    let server = common::Server::start();
    server.with_world(|world| {
        let task = spawn_task(
            world,
            TaskSpec {
                id: "alpha",
                title: "alpha",
                location: Location::Cwd("/tmp".into()),
                created_ms: 1,
            },
        )
        .unwrap();
        let attempt = spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Managed,
                handle: common::handle("fux-1", 1, Some(9)),
            },
        )
        .unwrap();
        world.entity_mut(attempt).insert(AttemptState::Live);
        spawn_prompt(
            world,
            PromptSpec {
                id: "alpha-p",
                attempt,
                text: "work",
                deadline_ms: 60_000,
                report_token: "tok",
            },
        )
        .unwrap();
    });
    let created = server
        .call(
            "zor/group.create",
            serde_json::json!({
                "group": "workers",
                "concurrency": 1,
                "members": [{ "operation": "alpha-work", "prompt": "alpha-p" }],
            }),
        )
        .unwrap();
    assert_eq!(created["intent"], "manual");
    assert_eq!(created["members"][0]["status"], "queued");
    let paused = server
        .call("zor/group.pause", serde_json::json!({ "group": "workers" }))
        .unwrap();
    assert_eq!(paused["intent"], "paused");
    assert_eq!(
        common::code(server.call("zor/group.admit", serde_json::json!({ "group": "workers" }))),
        zor::remote::methods::codes::INVALID
    );
    assert_eq!(
        common::code(server.call("zor/group.inspect", serde_json::json!({ "group": "nope" }))),
        zor::remote::methods::codes::NOT_FOUND
    );
    // Worktree allocation validates the repository before recording anything.
    assert_eq!(
        common::code(server.call(
            "zor/worktree.allocate",
            serde_json::json!({
                "worktree": "w", "task": "alpha", "repo": "/nonexistent/repo",
                "branch": "agent/x", "base": "HEAD",
            })
        )),
        zor::remote::methods::codes::INVALID
    );
    let list = server
        .call("zor/worktree.list", serde_json::json!({}))
        .unwrap();
    assert_eq!(list["worktrees"].as_array().unwrap().len(), 0);
    assert_eq!(
        common::code(server.call(
            "zor/worktree.inspect",
            serde_json::json!({ "worktree": "w" })
        )),
        zor::remote::methods::codes::NOT_FOUND
    );
}

#[test]
fn admission_and_cursor_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    {
        let mut h = Harness::new();
        // Re-point the harness at the shared state dir: build a second app over it instead.
        drop(h.app);
        h.app = zor::app::build_headless(&Config::default(), &state);
        h.app.update();
        h.app.world_mut().write_message(Inbound::FuxLink {
            instance: Some("fux-1".into()),
        });
        h.app.update();
        h.worker(1);
        h.worker(2);
        let group = groups::create(
            h.world(),
            plan("g", 1, vec![member("a", 1), member("b", 2)], vec![]),
        )
        .unwrap();
        h.step();
        groups::admit(h.world(), group).unwrap();
        h.step();
        assert_eq!(h.cursor(group), 1);
    }
    let mut h = Harness::new();
    drop(h.app);
    h.app = zor::app::build_headless(&Config::default(), &state);
    h.app.update();
    assert_eq!(check_invariants(h.world()), Ok(()));
    let group = h.world().resource::<Ids>().group("g").unwrap();
    let a = h.member("a");
    let p1 = h.world().resource::<Ids>().prompt("p1").unwrap();
    assert_eq!(h.cursor(group), 1);
    assert_eq!(
        h.world().get::<groups::MemberPrompt>(a).map(|m| m.0),
        Some(p1)
    );
    // The reserve reply was lost with the restart: the lifecycle's recovery marks the admitted
    // operation uncertain while the prompt, never written, stays prepared.
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Prepared));
    assert_eq!(h.phase(a), OperationPhase::Uncertain);
    let record = groups::inspect(h.world(), group).unwrap();
    assert_eq!(record.active_count, 1, "the slot is retained");
    assert_eq!(record.members[0].status, MemberStatus::NeedsAttention);
    // Once fux is back, a step resubmits the retained prompt and admits nothing new.
    h.app.world_mut().write_message(Inbound::FuxLink {
        instance: Some("fux-1".into()),
    });
    h.step();
    assert!(groups::admit(h.world(), group).unwrap().is_empty());
    let calls = fux_calls(&h.step());
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].1, "fux/input.reserve");
    assert_eq!(h.phase(h.member("b")), OperationPhase::Prepared);
    assert_eq!(check_invariants(h.world()), Ok(()));
}
