//! The task/attempt/prompt lifecycle against a scripted fux stand-in: `Effect::FuxCall`s are
//! answered with canned `Inbound::FuxReply`s and `fux/events+watch` items are fed as
//! `Inbound::FuxEvent`s. Journal-first ordering, retry semantics, uncertainty, writer exclusion,
//! receipt-driven delivery, wait precedence, stop, cancellation and startup recovery
//! (`docs/model.md` invariants 3, 5-7, 12-20, 27, 28); `check_invariants` after every update.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy_app::App;
use bevy_ecs::prelude::*;
use serde_json::{Value, json};
use zor::config::Config;
use zor::lifecycle::{
    self, AwaitFinal, CALL_TAG, HEARTBEAT_MS, Heartbeat, LaunchSpec, LaunchTemplate,
    LifecycleError, Link, Observed, PendingCall, PromptSpec, TaskSpec,
};
use zor::model::invariants::check_invariants;
use zor::model::*;

const INSTANCE: &str = "fux-1";
const T0: u64 = 1_700_000_000_000;

struct Harness {
    app: App,
    _dir: tempfile::TempDir,
    state: PathBuf,
}

fn build(state: &Path) -> App {
    let mut app = zor::app::build_headless(&Config::default(), state);
    app.world_mut().resource_mut::<Clock>().now_ms = T0;
    app.finish();
    app.cleanup();
    app.update();
    app
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let app = build(&state);
    let mut h = Harness {
        app,
        _dir: dir,
        state,
    };
    h.link(Some(INSTANCE));
    h.step();
    h
}

#[derive(Debug, Clone)]
struct Call {
    id: u64,
    method: String,
    params: Value,
}

impl Harness {
    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn inbound(&mut self, message: Inbound) {
        self.world()
            .resource_mut::<Messages<Inbound>>()
            .write(message);
    }

    fn link(&mut self, instance: Option<&str>) {
        self.inbound(Inbound::FuxLink {
            instance: instance.map(str::to_owned),
        });
    }

    fn event(&mut self, name: &str, body: Value) {
        self.inbound(Inbound::FuxEvent {
            cursor: 1,
            name: name.into(),
            body,
        });
    }

    fn reply(&mut self, call: u64, result: Result<Value, String>) {
        self.inbound(Inbound::FuxReply { call, result });
    }

    /// One update, invariants checked, this module's fux calls drained.
    fn step(&mut self) -> Vec<Call> {
        self.app.update();
        assert_eq!(check_invariants(self.world()), Ok(()));
        self.world()
            .resource_mut::<Messages<Effect>>()
            .drain()
            .filter_map(|e| match e {
                Effect::FuxCall {
                    call,
                    method,
                    params,
                } if call & (0xff << 56) == CALL_TAG => Some(Call {
                    id: call,
                    method,
                    params,
                }),
                _ => None,
            })
            .collect()
    }

    fn now(&mut self, ms: u64) {
        self.world().resource_mut::<Clock>().now_ms = ms;
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.state.join("journal.scn.ron")).unwrap_or_default()
    }

    fn task(&mut self, id: &str) -> Entity {
        lifecycle::create_task(
            self.world(),
            TaskSpec {
                id: id.into(),
                title: format!("task {id}"),
                location: Location::Cwd("/tmp".into()),
            },
        )
        .unwrap()
    }

    /// Launches and drives the attempt to `Live` on pane `pane`.
    fn live(&mut self, task: Entity, op: &str, pane: u64) -> Entity {
        let attempt = lifecycle::launch(self.world(), task, spec(op)).unwrap();
        let calls = self.step();
        let create = one(&calls, "fux/root.new");
        self.reply(
            create.id,
            Ok(json!({ "root": 3, "pane": pane, "generation": 1 })),
        );
        let calls = self.step();
        let list = one(&calls, "fux/workspace.list");
        self.reply(list.id, Ok(listing(&[(pane, "starting", None, &[])])));
        self.event("PaneSpawned", json!({ "pane": pane, "pid": 4242 }));
        self.step();
        assert_eq!(
            self.world().get::<AttemptState>(attempt),
            Some(&AttemptState::Live)
        );
        attempt
    }

    fn prompt(&mut self, attempt: Entity, id: &str) -> Entity {
        lifecycle::prepare_prompt(
            self.world(),
            attempt,
            PromptSpec {
                id: id.into(),
                text: "review the parser".into(),
                timeout_ms: 30_000,
            },
        )
        .unwrap()
    }

    /// Submits a prepared prompt through reserve and submit replies to `Delivered`.
    fn deliver(&mut self, prompt: Entity, operation: u64) {
        lifecycle::submit_prompt(self.world(), prompt).unwrap();
        let calls = self.step();
        let reserve = one(&calls, "fux/input.reserve");
        self.reply(reserve.id, Ok(receipt(operation, "reserved", None)));
        let calls = self.step();
        let submit = one(&calls, "fux/input.submit");
        self.reply(submit.id, Ok(receipt(operation, "submitted", Some(9))));
        self.step();
        assert_eq!(
            self.world().get::<Delivery>(prompt),
            Some(&Delivery::Delivered)
        );
    }

    fn fire_heartbeat(&mut self, attempt: Entity) {
        self.world()
            .get_mut::<Heartbeat>(attempt)
            .unwrap()
            .0
            .set_elapsed(Duration::from_millis(HEARTBEAT_MS));
    }
}

fn spec(op: &str) -> LaunchSpec {
    LaunchSpec {
        operation: op.into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
        env: vec![("A".into(), "1".into())],
        workspace: "default".into(),
        ephemeral: false,
        integration: None,
    }
}

fn one<'a>(calls: &'a [Call], method: &str) -> &'a Call {
    let matching: Vec<&Call> = calls.iter().filter(|c| c.method == method).collect();
    assert_eq!(matching.len(), 1, "expected one {method} call in {calls:?}");
    matching[0]
}

fn none(calls: &[Call], method: &str) {
    assert!(
        calls.iter().all(|c| c.method != method),
        "unexpected {method} in {calls:?}"
    );
}

fn receipt(operation: u64, state: &str, seq: Option<u64>) -> Value {
    json!({
        "operation": operation, "pane": 7, "state": state,
        "bytes_written": if state == "submitted" { 18 } else { 0 },
        "seq": seq, "expires_ms": 60_000,
    })
}

/// A `fux/workspace.list` reply with the given panes in `default`.
fn listing(panes: &[(u64, &str, Option<u32>, &[String])]) -> Value {
    let panes: Vec<Value> = panes
        .iter()
        .map(|(id, state, pid, argv)| {
            json!({
                "id": id, "node": 10, "state": state, "pid": pid, "exit_code": null,
                "title": "sh", "rows": 24, "cols": 80, "seq": 1, "argv": argv, "cwd": "/tmp",
            })
        })
        .collect();
    json!({ "workspaces": [{
        "name": "default", "open": true, "viewers": 0,
        "roots": [{ "id": 3, "name": "main", "generation": 1, "panes": panes }],
    }] })
}

fn pane_final(pane: u64, code: i32) -> Value {
    json!({
        "pane": pane, "workspace": "default", "stream": "launch-1", "exit_code": code,
        "title": "sh", "last_seq": 12, "exited_ms": T0 + 5, "expires_ms": T0 + 30_005,
        "screen": ["hi", ""],
    })
}

// ---------------------------------------------------------------------------------------------

#[test]
fn launch_is_journaled_before_its_call_and_retries_return_the_record() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = lifecycle::launch(h.world(), task, spec("launch-1")).unwrap();
    let op = h.world().resource::<Ids>().operation("launch-1").unwrap();
    // The intent is in the World before the update that emits the call.
    assert_eq!(
        h.world().get::<OperationPhase>(op),
        Some(&OperationPhase::Submitting)
    );
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Launching)
    );
    assert!(h.journal().is_empty() || !h.journal().contains("launch-1"));
    let calls = h.step();
    let create = one(&calls, "fux/root.new");
    // The journal committed in this update, before the runner drains the effect.
    let journal = h.journal();
    assert!(journal.contains("launch-1"), "journal holds the intent");
    assert!(journal.contains("Submitting"));
    let template = &create.params["template"];
    assert_eq!(template["argv"][0], "/usr/bin/env");
    assert_eq!(template["argv"][1], "--");
    let marker = h.world().get::<LaunchMarker>(attempt).unwrap().0.clone();
    assert_eq!(template["argv"][2], format!("ZOR_LAUNCH_ID={marker}"));
    assert_eq!(template["argv"][3], "/bin/sh");
    assert_eq!(template["cwd"], "/tmp");
    assert_eq!(create.params["workspace"], "default");
    let handle = lifecycle::pane_handle(h.world(), attempt).unwrap();
    assert_eq!(handle.stream, "launch-1");
    assert!(handle.instance.is_empty() && handle.pane == 0);

    // Identical retry: the record, no second call. Conflicting retry: refused.
    assert_eq!(
        lifecycle::launch(h.world(), task, spec("launch-1")),
        Ok(attempt)
    );
    let mut other = spec("launch-1");
    other.argv.push("--verbose".into());
    assert!(matches!(
        lifecycle::launch(h.world(), task, other),
        Err(LifecycleError::Conflict(_))
    ));
    // A second launch of the same task while one attempt is current is refused (invariant 3).
    assert!(matches!(
        lifecycle::launch(h.world(), task, spec("launch-2")),
        Err(LifecycleError::Refused(_))
    ));
    none(&h.step(), "fux/root.new");

    // Creation reply: pane pinned, still Launching until the process is observed.
    h.reply(
        create.id,
        Ok(json!({ "root": 3, "pane": 7, "generation": 1 })),
    );
    let calls = h.step();
    one(&calls, "fux/workspace.list");
    let handle = lifecycle::pane_handle(h.world(), attempt).unwrap();
    assert_eq!((handle.pane, handle.instance.as_str()), (7, INSTANCE));
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Launching)
    );
    h.event("PaneSpawned", json!({ "pane": 7, "pid": 4242 }));
    h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    assert_eq!(
        lifecycle::pane_handle(h.world(), attempt).unwrap().pid,
        Some(4242)
    );
    assert_eq!(
        h.world().get::<OperationPhase>(op),
        Some(&OperationPhase::Attached)
    );
    assert_eq!(h.world().get::<TaskState>(task), Some(&TaskState::Running));
    assert_eq!(lifecycle::attempt_of(h.world(), task), Some(attempt));

    // Task creation retries the same way.
    assert_eq!(
        lifecycle::create_task(
            h.world(),
            TaskSpec {
                id: "t1".into(),
                title: "task t1".into(),
                location: Location::Cwd("/tmp".into())
            }
        ),
        Ok(task)
    );
    assert!(matches!(
        lifecycle::create_task(
            h.world(),
            TaskSpec {
                id: "t1".into(),
                title: "other".into(),
                location: Location::Cwd("/tmp".into())
            }
        ),
        Err(LifecycleError::Conflict(_))
    ));
}

#[test]
fn lost_creation_reply_is_uncertain_and_never_resent() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = lifecycle::launch(h.world(), task, spec("launch-1")).unwrap();
    let op = h.world().resource::<Ids>().operation("launch-1").unwrap();
    let create = one(&h.step(), "fux/root.new").clone();
    h.reply(create.id, Err("connect: connection refused".into()));
    let calls = h.step();
    none(&calls, "fux/root.new");
    // A lost reply asks the listing whether the pane exists; nothing is created again.
    let list = one(&calls, "fux/workspace.list").clone();
    assert_eq!(
        h.world().get::<OperationPhase>(op),
        Some(&OperationPhase::Uncertain)
    );
    assert!(h.world().get::<Uncertain>(attempt).is_some());
    assert!(h.world().get::<Problem>(op).is_some());
    // Retrying the launch returns the record and still sends nothing.
    assert_eq!(
        lifecycle::launch(h.world(), task, spec("launch-1")),
        Ok(attempt)
    );
    none(&h.step(), "fux/root.new");
    // Reconciliation by marker finds the unique live pane: Live, uncertainty cleared.
    let marker = format!(
        "ZOR_LAUNCH_ID={}",
        h.world().get::<LaunchMarker>(attempt).unwrap().0
    );
    let argv = vec!["/usr/bin/env".to_owned(), "--".to_owned(), marker];
    h.reply(list.id, Ok(listing(&[(7, "live", Some(99), &argv)])));
    h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    assert!(h.world().get::<Uncertain>(attempt).is_none());
    assert_eq!(lifecycle::pane_handle(h.world(), attempt).unwrap().pane, 7);
    assert_eq!(
        h.world().get::<OperationPhase>(op),
        Some(&OperationPhase::Attached)
    );
    for _ in 0..3 {
        none(&h.step(), "fux/root.new");
    }
}

#[test]
fn launch_needs_a_connected_fux_and_a_valid_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness {
        app: build(&dir.path().join("state")),
        state: dir.path().join("state"),
        _dir: dir,
    };
    let task = h.task("t1");
    assert!(matches!(
        lifecycle::launch(h.world(), task, spec("launch-1")),
        Err(LifecycleError::Refused(_))
    ));
    h.link(Some(INSTANCE));
    h.step();
    let mut empty = spec("launch-1");
    empty.argv.clear();
    assert!(matches!(
        lifecycle::launch(h.world(), task, empty),
        Err(LifecycleError::Refused(_))
    ));
    assert!(h.world().resource::<Ids>().operation("launch-1").is_none());
}

#[test]
fn one_pending_prompt_per_pane_across_tasks() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);
    let p1 = h.prompt(attempt, "p1");
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Prepared));
    assert!(h.world().get::<Receipt>(p1).is_none());
    assert!(h.world().get::<ReportToken>(p1).is_some());
    // Identical retry returns it; conflicting text under the id fails.
    assert_eq!(h.prompt(attempt, "p1"), p1);
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p1".into(),
                text: "other".into(),
                timeout_ms: 1
            }
        ),
        Err(LifecycleError::Conflict(_))
    ));
    // A second prepared prompt on the same pane is refused, even from another task's adopted
    // attempt on that pane identity.
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p2".into(),
                text: "x".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
    // Text rules: control characters and multiline are refused.
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p3".into(),
                text: "two\nlines".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
    // Abandoning releases the pane for the next prompt (TASKS.md:389-391).
    lifecycle::abandon_prompt(h.world(), p1).unwrap();
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Released));
    assert_eq!(h.world().get::<WaitState>(p1), Some(&WaitState::Cancelled));
    h.step();
    let p2 = h.prompt(attempt, "p2");
    assert_ne!(p2, p1);
    // A released prompt is never reactivated.
    assert!(matches!(
        lifecycle::submit_prompt(h.world(), p1),
        Err(LifecycleError::Refused(_))
    ));
    h.step();
}

#[test]
fn delivery_follows_receipts_and_lost_submit_replies_stay_uncertain() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);
    let p1 = h.prompt(attempt, "p1");

    lifecycle::submit_prompt(h.world(), p1).unwrap();
    // Still Prepared while the reservation is in flight; a second submit sends nothing.
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Prepared));
    lifecycle::submit_prompt(h.world(), p1).unwrap();
    let calls = h.step();
    let reserve = one(&calls, "fux/input.reserve").clone();
    assert_eq!(reserve.params["pane"], 7);
    h.reply(reserve.id, Ok(receipt(31, "reserved", None)));
    let calls = h.step();
    // Reserved is committed with its receipt and the submit goes out as Submitting.
    let submit = one(&calls, "fux/input.submit").clone();
    assert_eq!(submit.params["operation"], 31);
    assert_eq!(submit.params["keys"], "review the parser\\r");
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Submitting));
    assert_eq!(h.world().get::<Receipt>(p1).unwrap().operation, 31);
    assert!(h.journal().contains("Submitting"));
    // A submit reply is lost: Uncertain, retained, never resubmitted.
    h.reply(submit.id, Err("read: connection reset".into()));
    h.step();
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Uncertain));
    assert!(matches!(
        lifecycle::submit_prompt(h.world(), p1),
        Err(LifecycleError::Uncertain(_))
    ));
    none(&h.step(), "fux/input.submit");
    // Exclusion holds while uncertain (invariant 6).
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p2".into(),
                text: "x".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
    // The heartbeat's status refresh proves the bytes were written: Delivered.
    h.fire_heartbeat(attempt);
    let calls = h.step();
    let status = one(&calls, "fux/input.status").clone();
    assert_eq!(status.params["operation"], 31);
    h.reply(status.id, Ok(receipt(31, "submitted", Some(9))));
    h.step();
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Delivered));
    assert_eq!(h.world().get::<Receipt>(p1).unwrap().seq, Some(9));
    assert_eq!(h.world().get::<WaitState>(p1), Some(&WaitState::Pending));
    assert_eq!(h.world().get::<TaskState>(task), Some(&TaskState::Running));

    // A refused submit (fux rejected the reservation after human input) is Failed and the
    // wait is Uncertain; a status saying `reserved` after a lost reply returns to Reserved.
    lifecycle::abandon_prompt(h.world(), p1).unwrap();
    h.step();
    let p2 = h.prompt(attempt, "p2");
    lifecycle::submit_prompt(h.world(), p2).unwrap();
    let reserve = one(&h.step(), "fux/input.reserve").clone();
    h.reply(reserve.id, Ok(receipt(32, "reserved", None)));
    let submit = one(&h.step(), "fux/input.submit").clone();
    h.reply(
        submit.id,
        Err("error -32004: reservation superseded".into()),
    );
    h.step();
    assert_eq!(h.world().get::<Delivery>(p2), Some(&Delivery::Failed));
    assert_eq!(h.world().get::<WaitState>(p2), Some(&WaitState::Uncertain));
    lifecycle::abandon_prompt(h.world(), p2).unwrap();
    h.step();
    let p3 = h.prompt(attempt, "p3");
    lifecycle::submit_prompt(h.world(), p3).unwrap();
    let reserve = one(&h.step(), "fux/input.reserve").clone();
    h.reply(reserve.id, Ok(receipt(33, "reserved", None)));
    let submit = one(&h.step(), "fux/input.submit").clone();
    h.reply(submit.id, Err("timeout".into()));
    h.step();
    h.fire_heartbeat(attempt);
    let status = one(&h.step(), "fux/input.status").clone();
    h.reply(status.id, Ok(receipt(33, "reserved", None)));
    h.step();
    assert_eq!(h.world().get::<Delivery>(p3), Some(&Delivery::Reserved));
    // Resuming a retained reservation submits under the same operation.
    lifecycle::submit_prompt(h.world(), p3).unwrap();
    let submit = one(&h.step(), "fux/input.submit").clone();
    assert_eq!(submit.params["operation"], 33);
}

#[test]
fn wait_outcomes_follow_precedence_and_are_never_erased() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);

    // Response after delivery, with the capture past the receipt's sequence.
    let p1 = h.prompt(attempt, "p1");
    h.deliver(p1, 31);
    h.world().entity_mut(p1).insert(ResponseEvent {
        producer: "codex-1".into(),
        sequence: 1,
        input_operation: 31,
        kind: "response".into(),
    });
    h.step();
    assert_eq!(
        h.world().get::<WaitState>(p1),
        Some(&WaitState::ResponseObserved)
    );
    // A later receipt refresh or final evidence does not erase it.
    h.fire_heartbeat(attempt);
    h.step();
    lifecycle::abandon_prompt(h.world(), p1).unwrap();
    h.step();
    assert_eq!(
        h.world().get::<WaitState>(p1),
        Some(&WaitState::ResponseObserved)
    );
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Released));

    // needs-input is not terminal; the deadline then yields TimedOut, which is retained.
    let p2 = h.prompt(attempt, "p2");
    h.deliver(p2, 32);
    h.world().entity_mut(p2).insert(ResponseEvent {
        producer: "codex-1".into(),
        sequence: 2,
        input_operation: 32,
        kind: "needs-input".into(),
    });
    h.step();
    assert_eq!(h.world().get::<WaitState>(p2), Some(&WaitState::NeedsInput));
    h.now(T0 + 30_001);
    h.step();
    assert_eq!(h.world().get::<WaitState>(p2), Some(&WaitState::TimedOut));
    // Timeout keeps writer exclusion until abandoned (TASKS.md:336).
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p3".into(),
                text: "x".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
    lifecycle::abandon_prompt(h.world(), p2).unwrap();
    h.step();
    assert_eq!(h.world().get::<WaitState>(p2), Some(&WaitState::TimedOut));

    // Final evidence resolves a pending wait as ProcessExited and never verifies the task.
    let p3 = h.prompt(attempt, "p3");
    h.deliver(p3, 33);
    h.event("PaneExited", json!({ "pane": 7, "code": 0 }));
    let calls = h.step();
    let final_call = one(&calls, "fux/pane.final").clone();
    h.reply(final_call.id, Ok(pane_final(7, 0)));
    h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Finished)
    );
    assert_eq!(
        h.world().get::<WaitState>(p3),
        Some(&WaitState::ProcessExited)
    );
    let evidence = h.world().get::<FinalEvidence>(attempt).unwrap();
    assert_eq!(evidence.exit_code, Some(0));
    assert_eq!(evidence.output, "hi\n");
    assert_eq!(h.world().get::<TaskState>(task), Some(&TaskState::Open));
    assert!(h.world().get::<Seal>(task).is_none());
    assert_eq!(lifecycle::attempt_of(h.world(), task), None);
    // Finished attempts reject preparation (TASKS.md:137).
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p4".into(),
                text: "x".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
}

#[test]
fn stop_succeeds_only_on_final_evidence() {
    let mut h = harness();
    let task = h.task("t1");
    let mut ephemeral = spec("launch-1");
    ephemeral.ephemeral = true;
    ephemeral.workspace = "zor-run-1".into();
    let attempt = lifecycle::launch(h.world(), task, ephemeral).unwrap();
    let create = one(&h.step(), "fux/workspace.new").clone();
    assert_eq!(create.params["name"], "zor-run-1");
    h.reply(
        create.id,
        Ok(json!({ "name": "zor-run-1", "root": 3, "pane": 7 })),
    );
    let list = one(&h.step(), "fux/workspace.list").clone();
    h.reply(list.id, Ok(listing(&[(7, "live", Some(4242), &[])])));
    h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    let p1 = h.prompt(attempt, "p1");
    h.deliver(p1, 31);

    lifecycle::request_stop(h.world(), task).unwrap();
    assert!(h.world().get::<StopRequested>(task).is_some());
    assert_eq!(
        h.world().get::<TaskState>(task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Cancelled
        })
    );
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Finishing)
    );
    assert_eq!(h.world().get::<WaitState>(p1), Some(&WaitState::Cancelled));
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Released));
    // Identity is validated against the listing before the close (invariant 16).
    let calls = h.step();
    none(&calls, "fux/pane.close");
    let list = one(&calls, "fux/workspace.list").clone();
    // A retry while the check is in flight sends nothing new.
    lifecycle::request_stop(h.world(), task).unwrap();
    assert!(h.step().is_empty());
    // Wrong pid: refused, no close, uncertain.
    h.reply(list.id, Ok(listing(&[(7, "live", Some(1), &[])])));
    let calls = h.step();
    none(&calls, "fux/pane.close");
    assert!(h.world().get::<Uncertain>(attempt).is_some());
    // Retry: the listing now matches; close goes out; acceptance is not success.
    lifecycle::request_stop(h.world(), task).unwrap();
    let list = one(&h.step(), "fux/workspace.list").clone();
    h.reply(list.id, Ok(listing(&[(7, "live", Some(4242), &[])])));
    let close = one(&h.step(), "fux/pane.close").clone();
    assert_eq!(close.params["pane"], 7);
    assert!(h.world().get::<Uncertain>(attempt).is_none());
    h.reply(close.id, Ok(json!({})));
    let calls = h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Finishing)
    );
    assert!(h.world().get::<FinalEvidence>(attempt).is_none());
    // The final record is pending until the process exits; the heartbeat asks again.
    let final_call = one(&calls, "fux/pane.final").clone();
    h.reply(
        final_call.id,
        Err("error -32010: final record pending".into()),
    );
    h.step();
    assert!(h.world().get::<AwaitFinal>(attempt).is_some());
    h.fire_heartbeat(attempt);
    let final_call = one(&h.step(), "fux/pane.final").clone();
    h.reply(final_call.id, Ok(pane_final(7, 143)));
    let calls = h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Finished)
    );
    assert_eq!(
        h.world().get::<FinalEvidence>(attempt).unwrap().exit_code,
        Some(143)
    );
    // The ephemeral workspace is killed once the attempt finished.
    let kill = one(&calls, "fux/workspace.kill");
    assert_eq!(kill.params["name"], "zor-run-1");
    // Cancellation stands; the delivered prompt's evidence is retained.
    assert_eq!(h.world().get::<Receipt>(p1).unwrap().operation, 31);
    assert!(h.world().get::<Heartbeat>(attempt).is_none());
}

#[test]
fn adopted_attempts_have_no_stop_authority_and_cancellation_signals_nothing() {
    let mut h = harness();
    let task = h.task("t1");
    let handle = PaneHandle {
        instance: INSTANCE.into(),
        workspace: "default".into(),
        stream: String::new(),
        pane: 5,
        pid: Some(77),
    };
    let attempt = lifecycle::adopt(h.world(), task, handle.clone()).unwrap();
    assert_eq!(lifecycle::adopt(h.world(), task, handle), Ok(attempt));
    let list = one(&h.step(), "fux/workspace.list").clone();
    h.reply(list.id, Ok(listing(&[(5, "live", Some(77), &[])])));
    h.step();
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    assert_eq!(
        h.world().get::<Ownership>(attempt),
        Some(&Ownership::Adopted)
    );
    assert!(matches!(
        lifecycle::request_stop(h.world(), task),
        Err(LifecycleError::Refused(_))
    ));
    assert!(h.world().get::<StopRequested>(task).is_none());

    // A prompt is fine on an adopted pane; cancellation releases it without any fux call.
    let p1 = h.prompt(attempt, "p1");
    h.deliver(p1, 41);
    lifecycle::cancel_task(h.world(), task).unwrap();
    assert!(h.step().is_empty());
    assert_eq!(
        h.world().get::<TaskState>(task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Cancelled
        })
    );
    assert_eq!(h.world().get::<WaitState>(p1), Some(&WaitState::Cancelled));
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Released));
    assert_eq!(h.world().get::<Receipt>(p1).unwrap().operation, 41);
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    // Cancelled tasks refuse new preparation; repeating the cancellation is a stored read.
    assert!(matches!(
        lifecycle::prepare_prompt(
            h.world(),
            attempt,
            PromptSpec {
                id: "p2".into(),
                text: "x".into(),
                timeout_ms: 1000
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
    let generation = h.world().resource::<Generation>().0;
    h.step();
    let generation = generation.max(h.world().resource::<Generation>().0);
    lifecycle::cancel_task(h.world(), task).unwrap();
    h.step();
    assert_eq!(h.world().resource::<Generation>().0, generation);
    // A launch on a cancelled task is refused; the attempt never finishes by itself.
    assert!(matches!(
        lifecycle::launch(h.world(), task, spec("launch-9")),
        Err(LifecycleError::Refused(_))
    ));
}

#[test]
fn verified_closure_needs_a_consistent_seal_and_resists_cancellation() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);
    let world = h.world();
    let source = spawn_source(
        world,
        "s1",
        task,
        SourceRevision {
            expression: "HEAD".into(),
            commit: "abc".into(),
        },
    )
    .unwrap();
    let check = spawn_check(
        world,
        CheckSpec {
            id: "c1",
            task,
            source: Some(source),
            command: CheckCommand {
                argv: vec!["true".into()],
                timeout_ms: 1000,
            },
            requirement: Some("build"),
            generation: 1,
        },
    )
    .unwrap();
    let seal = Seal {
        source,
        checks: vec![check],
        artifacts: Vec::new(),
        sealed_ms: T0,
        generation: 1,
    };
    // A queued check cannot be sealed.
    assert!(matches!(
        lifecycle::close_verified(world, task, seal.clone()),
        Err(LifecycleError::Refused(_))
    ));
    assert!(world.get::<Seal>(task).is_none());
    world.entity_mut(check).insert(CheckState::Passed);
    spawn_result(world, check, Verdict::Passed, OutputTail::default()).unwrap();
    // A prompt whose delivery is unsettled blocks sealing; a delivered one does not.
    let p1 = h.prompt(attempt, "p1");
    lifecycle::submit_prompt(h.world(), p1).unwrap();
    let reserve = one(&h.step(), "fux/input.reserve").clone();
    h.reply(reserve.id, Ok(receipt(31, "reserved", None)));
    let submit = one(&h.step(), "fux/input.submit").clone();
    assert_eq!(h.world().get::<Delivery>(p1), Some(&Delivery::Submitting));
    assert!(matches!(
        lifecycle::close_verified(h.world(), task, seal.clone()),
        Err(LifecycleError::Refused(_))
    ));
    h.reply(submit.id, Ok(receipt(31, "submitted", Some(9))));
    h.step();
    lifecycle::close_verified(h.world(), task, seal).unwrap();
    h.step();
    assert_eq!(
        h.world().get::<TaskState>(task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Verified
        })
    );
    assert!(h.world().get::<ClosedMs>(task).is_some());
    assert!(matches!(
        lifecycle::cancel_task(h.world(), task),
        Err(LifecycleError::Refused(_))
    ));
    // Stop still closes the pane and keeps Verified (invariant 23).
    lifecycle::request_stop(h.world(), task).unwrap();
    assert_eq!(
        h.world().get::<TaskState>(task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Verified
        })
    );
    assert!(h.world().get::<StopRequested>(task).is_some());
    one(&h.step(), "fux/workspace.list");
}

#[test]
fn recovery_marks_submitted_intent_uncertain_and_sweeps_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let marker;
    let live_marker;
    {
        let mut h = Harness {
            app: build(&state),
            state: state.clone(),
            _dir: tempfile::tempdir().unwrap(),
        };
        h.link(Some(INSTANCE));
        h.step();
        // t1: creation in flight. t2: live with a submit in flight. t3: prepared only.
        let t1 = h.task("t1");
        let a1 = lifecycle::launch(h.world(), t1, spec("launch-1")).unwrap();
        marker = h.world().get::<LaunchMarker>(a1).unwrap().0.clone();
        one(&h.step(), "fux/root.new");
        let t2 = h.task("t2");
        let a2 = h.live(t2, "launch-2", 8);
        live_marker = h.world().get::<LaunchMarker>(a2).unwrap().0.clone();
        let p2 = h.prompt(a2, "p2");
        lifecycle::submit_prompt(h.world(), p2).unwrap();
        let reserve = one(&h.step(), "fux/input.reserve").clone();
        h.reply(reserve.id, Ok(receipt(31, "reserved", None)));
        one(&h.step(), "fux/input.submit");
        let t3 = h.task("t3");
        let a3 = h.live(t3, "launch-3", 9);
        h.prompt(a3, "p3");
        h.step();
        assert!(h.journal().contains("Submitting"));
    }

    let mut h = Harness {
        app: build(&state),
        state: state.clone(),
        _dir: dir,
    };
    let world = h.world();
    let ids = world.resource::<Ids>().clone();
    let op1 = ids.operation("launch-1").unwrap();
    let p2 = ids.prompt("p2").unwrap();
    let p3 = ids.prompt("p3").unwrap();
    let a1 = world.get::<OperationOf>(op1).unwrap().0;
    let a2 = world.get::<PromptOf>(p2).unwrap().0;
    // In one startup transaction: submitted intent is uncertain, nothing was resent.
    assert_eq!(
        world.get::<OperationPhase>(op1),
        Some(&OperationPhase::Uncertain)
    );
    assert_eq!(world.get::<Delivery>(p2), Some(&Delivery::Uncertain));
    assert_eq!(world.get::<Delivery>(p3), Some(&Delivery::Prepared));
    assert!(world.get::<Uncertain>(a1).is_some());
    assert!(world.get::<Uncertain>(a2).is_some());
    assert!(world.get::<PendingCall>(op1).is_none());
    assert!(world.get::<PendingCall>(p2).is_none());
    assert!(world.get::<LaunchTemplate>(op1).is_some());
    assert!(h.world().resource::<Link>().sweep_due);
    assert!(h.step().is_empty());

    // The first link triggers exactly one listing sweep.
    h.link(Some(INSTANCE));
    let calls = h.step();
    let list = one(&calls, "fux/workspace.list").clone();
    none(&calls, "fux/root.new");
    none(&calls, "fux/input.submit");
    let argv1 = vec![
        "/usr/bin/env".to_owned(),
        "--".to_owned(),
        format!("ZOR_LAUNCH_ID={marker}"),
    ];
    let argv2 = vec![
        "/usr/bin/env".to_owned(),
        "--".to_owned(),
        format!("ZOR_LAUNCH_ID={live_marker}"),
    ];
    // t1's pane exists after all (creation happened); t2's pane is live; t3's pane is gone.
    h.reply(
        list.id,
        Ok(listing(&[
            (7, "live", Some(11), &argv1),
            (8, "live", Some(4242), &argv2),
        ])),
    );
    let calls = h.step();
    assert!(!h.world().resource::<Link>().sweep_due);
    assert_eq!(h.world().get::<AttemptState>(a1), Some(&AttemptState::Live));
    assert_eq!(lifecycle::pane_handle(h.world(), a1).unwrap().pane, 7);
    assert!(h.world().get::<Uncertain>(a1).is_none());
    assert_eq!(
        h.world().get::<OperationPhase>(op1),
        Some(&OperationPhase::Attached)
    );
    assert!(h.world().get::<Uncertain>(a2).is_none());
    // t3's missing pane asks for a retained final record; nothing else is created.
    let final_call = one(&calls, "fux/pane.final").clone();
    assert_eq!(final_call.params["pane"], 9);
    none(&calls, "fux/root.new");
    h.reply(final_call.id, Err("error -32014: unknown pane".into()));
    h.step();
    let a3 = h.world().get::<PromptOf>(p3).unwrap().0;
    assert!(h.world().get::<Uncertain>(a3).is_some());
    assert_ne!(
        h.world().get::<AttemptState>(a3),
        Some(&AttemptState::Finished)
    );
    // No further sweep: the following updates list nothing.
    for _ in 0..3 {
        none(&h.step(), "fux/workspace.list");
    }
    // The uncertain prompt is resolved by its status, never resubmitted.
    h.fire_heartbeat(a2);
    let calls = h.step();
    let status = one(&calls, "fux/input.status").clone();
    none(&calls, "fux/input.submit");
    h.reply(status.id, Ok(receipt(31, "submitted", Some(4))));
    h.step();
    assert_eq!(h.world().get::<Delivery>(p2), Some(&Delivery::Delivered));
}

#[test]
fn link_loss_is_uncertain_and_a_replaced_instance_is_lost() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);
    h.link(None);
    h.step();
    assert!(h.world().get::<Uncertain>(attempt).is_some());
    let t2 = h.task("t2");
    assert!(matches!(
        lifecycle::launch(h.world(), t2, spec("launch-2")),
        Err(LifecycleError::Refused(_))
    ));
    // Same instance back: reconciled live, uncertainty cleared.
    h.link(Some(INSTANCE));
    let list = one(&h.step(), "fux/workspace.list").clone();
    h.reply(list.id, Ok(listing(&[(7, "live", Some(4242), &[])])));
    h.step();
    assert!(h.world().get::<Uncertain>(attempt).is_none());
    // Another incarnation: Lost, retained through a later outage, never Finished by it.
    h.link(Some("fux-2"));
    let calls = h.step();
    none(&calls, "fux/pane.close");
    assert!(h.world().get::<Lost>(attempt).is_some());
    assert_eq!(
        h.world().get::<AttemptState>(attempt),
        Some(&AttemptState::Live)
    );
    h.link(None);
    h.step();
    assert!(h.world().get::<Lost>(attempt).is_some());
    assert!(matches!(
        lifecycle::request_stop(h.world(), task),
        Err(LifecycleError::Refused(_))
    ));
    // Adoption against the wrong incarnation is refused too.
    let t3 = h.task("t3");
    assert!(matches!(
        lifecycle::adopt(
            h.world(),
            t3,
            PaneHandle {
                instance: INSTANCE.into(),
                workspace: "default".into(),
                stream: String::new(),
                pane: 9,
                pid: None,
            }
        ),
        Err(LifecycleError::Refused(_))
    ));
}

#[test]
fn observation_is_the_only_attempt_state_writer() {
    let mut h = harness();
    let task = h.task("t1");
    let attempt = h.live(task, "launch-1", 7);
    // Final is terminal: later observations change nothing.
    lifecycle::observe(
        h.world(),
        attempt,
        Observed::Final(FinalEvidence {
            exit_code: Some(3),
            seq: 1,
            output: "hi".into(),
            truncated: false,
        }),
    );
    lifecycle::observe(h.world(), attempt, Observed::Live { pid: Some(1) });
    lifecycle::observe(h.world(), attempt, Observed::Missing("x".into()));
    lifecycle::observe(h.world(), attempt, Observed::Replaced("y".into()));
    h.step();
    let entity = h.world().entity(attempt);
    assert_eq!(entity.get::<AttemptState>(), Some(&AttemptState::Finished));
    assert!(!entity.contains::<Uncertain>() && !entity.contains::<Lost>());
    assert_eq!(entity.get::<FinalEvidence>().unwrap().exit_code, Some(3));
    // Pending never skips to Live: an unsubmitted attempt ignores a live observation.
    let t2 = h.task("t2");
    let pending = spawn_attempt(
        h.world(),
        AttemptSpec {
            task: t2,
            ownership: Ownership::Managed,
            handle: PaneHandle::default(),
        },
    )
    .unwrap();
    lifecycle::observe(h.world(), pending, Observed::Live { pid: Some(1) });
    assert_eq!(
        h.world().get::<AttemptState>(pending),
        Some(&AttemptState::Pending)
    );
    h.step();
}
