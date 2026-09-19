//! Resume's safety boundary: retained provider authority, exact native session selection,
//! journal-first stable operations, historical evidence, and no prompt/command replay.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::path::Path;

use bevy_app::App;
use bevy_ecs::prelude::*;
use serde_json::{Value, json};
use zor::config::Config;
use zor::lifecycle::{self, Integration, LaunchSpec, LifecycleError, Link, Observed, PromptSpec};
use zor::lifecycle::resume::{ResumeSpec, eligibility, resume};
use zor::model::invariants::check_invariants;
use zor::model::*;
use zor::providers::{Provider, ProviderKind, ProviderSession, SessionState};

const FUX: &str = "fux-resume-test";
const SESSION: &str = "ses_exact_native";

fn build(state: &Path) -> App {
    let mut app = zor::app::build_headless(&Config::default(), state);
    app.finish();
    app.cleanup();
    app.update();
    app.world_mut().resource_mut::<Clock>().now_ms = 1_000;
    app.world_mut().resource_mut::<Link>().instance = Some(FUX.into());
    app
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec {
        operation: "original".into(),
        argv: vec!["/fixture/opencode".into()],
        // The launch captures effective XDG defaults; callers need not know this machinery.
        env: vec![("HOME".into(), "/fixture/home".into())],
        workspace: "default".into(),
        ephemeral: false,
        integration: Some(Integration {
            kind: ProviderKind::OpenCode,
            argv: vec!["/fixture/native-observer".into()],
        }),
    }
}

fn request(operation: &str) -> ResumeSpec {
    ResumeSpec { operation: operation.into(), fux_instance: FUX.into() }
}

fn live(world: &mut World) -> (Entity, Entity, Entity) {
    live_with_spec(world, launch_spec())
}

fn live_with_spec(world: &mut World, spec: LaunchSpec) -> (Entity, Entity, Entity) {
    world.resource_mut::<Link>().instance = Some(FUX.into());
    let task = lifecycle::create_task(world, lifecycle::TaskSpec {
        id: "task".into(), title: "native history".into(), location: Location::Cwd("/tmp".into()),
    }).unwrap();
    let attempt = lifecycle::launch(world, task, spec).unwrap();
    lifecycle::observe(world, attempt, Observed::Created { pane: 17 });
    lifecycle::observe(world, attempt, Observed::Live { pid: Some(4242) });
    let prompt = lifecycle::prepare_prompt(world, attempt, PromptSpec {
        id: "original-prompt".into(), text: "never replay this prompt".into(), timeout_ms: 30_000,
    }).unwrap();
    world.entity_mut(prompt).insert((
        Delivery::Delivered,
        WaitState::ResponseObserved,
        Receipt {
            instance: FUX.into(), pane: 17, operation: 42, state: "submitted".into(),
            bytes_written: 25, seq: Some(1), expires_ms: 60_000,
        },
        Binding {
            producer: "opencode:observer".into(), sequence: 2, input_operation: 42,
            session: SESSION.into(), message: "msg_original".into(), bound_ms: 200,
        },
        ResponseEvent {
            producer: "opencode:observer".into(), sequence: 3, input_operation: 42, kind: "response".into(),
        },
    ));
    world.entity_mut(attempt).insert(ProducerLifetime {
        producer: "opencode:observer".into(), registered_ms: 100, retired: vec![],
    });
    (task, attempt, prompt)
}

fn finished(world: &mut World) -> (Entity, Entity, Entity) {
    let (task, attempt, prompt) = live(world);
    lifecycle::observe(world, attempt, Observed::Final(FinalEvidence {
        exit_code: Some(0), seq: 1, output: "old final output".into(), truncated: false,
    }));
    world.entity_mut(attempt).insert(ProducerLifetime {
        producer: String::new(), registered_ms: 100,
        retired: vec![("opencode:observer".into(), 100, 300)],
    });
    (task, attempt, prompt)
}

fn effects(world: &mut World) -> Vec<Effect> {
    world.resource_mut::<Messages<Effect>>().drain().collect()
}

fn creations(effects: &[Effect]) -> Vec<(u64, Value)> {
    effects.iter().filter_map(|effect| match effect {
        Effect::FuxCall { call, method, params } if method == "fux/root.new" || method == "fux/workspace.new" => Some((*call, params.clone())),
        _ => None,
    }).collect()
}

#[test]
fn resume_selects_exact_native_session_without_replaying_prompt_and_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let (task, old, prompt) = finished(world);
    let old_id = world.get::<AttemptId>(old).unwrap().0;
    let binding = world.get::<Binding>(prompt).unwrap().clone();
    let final_evidence = world.get::<FinalEvidence>(old).unwrap().clone();
    effects(world);
    assert_eq!(eligibility(world, task).unwrap().native_session, SESSION);
    let new = resume(world, task, request("resume-1")).unwrap();
    let sent = effects(world);
    let creates = creations(&sent);
    assert_eq!(creates.len(), 1);
    let argv = creates[0].1["template"]["argv"].as_array().unwrap();
    assert_eq!(&argv[3..], &[json!("/fixture/opencode"), json!("--session"), json!(SESSION)]);
    assert!(!sent.iter().any(|effect| matches!(effect, Effect::WriteProvider { .. })
        || matches!(effect, Effect::FuxCall { method, .. } if method.starts_with("fux/input."))));
    assert_ne!(world.get::<AttemptId>(new).unwrap().0, old_id);
    assert_eq!(world.get::<Attempts>(task).unwrap().len(), 2);
    assert!(world.get::<Prompts>(new).is_none_or(|prompts| prompts.is_empty()));
    assert_eq!(world.get::<PromptText>(prompt).unwrap().0, "never replay this prompt");
    assert_eq!(world.get::<Binding>(prompt), Some(&binding));
    assert_eq!(world.get::<FinalEvidence>(old), Some(&final_evidence));
    assert_eq!(world.get::<AttemptState>(old), Some(&AttemptState::Finished));
    assert_eq!(check_invariants(world), Ok(()));
}

#[test]
fn stable_resume_retry_is_retained_and_conflicting_identity_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let (task, _, _) = finished(world);
    effects(world);
    let new = resume(world, task, request("resume-1")).unwrap();
    assert_eq!(creations(&effects(world)).len(), 1);
    assert_eq!(resume(world, task, request("resume-1")).unwrap(), new);
    let mut changed = request("resume-1");
    changed.fux_instance = "replacement-fux".into();
    assert!(matches!(resume(world, task, changed), Err(LifecycleError::Conflict(_))));
    assert!(matches!(resume(world, task, request("original")), Err(LifecycleError::Conflict(_))));
    assert!(matches!(resume(world, task, request("original-prompt")), Err(LifecycleError::Conflict(_))));
    assert!(resume(world, task, request("resume-2")).is_err());
    assert!(effects(world).is_empty());
}

#[test]
fn live_lost_and_unproven_sidecar_exit_never_authorize_a_new_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let (task, old, _) = live(world);
    effects(world);
    assert!(resume(world, task, request("resume-1")).is_err());
    lifecycle::observe(world, old, Observed::Replaced("another fux incarnation".into()));
    assert!(resume(world, task, request("resume-1")).is_err());
    lifecycle::observe(world, old, Observed::Final(FinalEvidence { exit_code: Some(0), ..Default::default() }));
    assert!(resume(world, task, request("resume-1")).is_err());
    world.entity_mut(old).insert(ProducerLifetime {
        producer: String::new(), registered_ms: 100,
        retired: vec![("opencode:observer".into(), 100, 300)],
    });
    let mut session = ProviderSession::default();
    session.state = SessionState::Running;
    world.entity_mut(old).insert(session);
    assert!(resume(world, task, request("resume-1")).is_err());
    assert_eq!(world.get::<Attempts>(task).unwrap().len(), 1);
    assert!(creations(&effects(world)).is_empty());
}

#[test]
fn native_session_authority_requires_matching_receipt_and_retired_producer() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let (task, _, prompt) = finished(world);
    let original = world.get::<Binding>(prompt).unwrap().clone();
    let mut foreign = original.clone();
    foreign.producer = "other-provider".into();
    world.entity_mut(prompt).insert(foreign);
    assert!(eligibility(world, task).is_err());
    let mut wrong_receipt = original.clone();
    wrong_receipt.input_operation = 999;
    world.entity_mut(prompt).insert(wrong_receipt);
    assert!(eligibility(world, task).is_err());
    let mut outside_lifetime = original.clone();
    outside_lifetime.bound_ms = 99;
    world.entity_mut(prompt).insert(outside_lifetime);
    assert!(eligibility(world, task).is_err());
    world.entity_mut(prompt).insert(original);
    assert_eq!(eligibility(world, task).unwrap().native_session, SESSION);
}

#[test]
fn unsupported_provider_and_arbitrary_launch_commands_are_not_resume_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let (task, old, _) = finished(world);
    let original_provider = world.get::<Provider>(old).unwrap().clone();
    for kind in [ProviderKind::Codex, ProviderKind::Claude] {
        let mut provider = original_provider.clone();
        provider.kind = kind;
        world.entity_mut(old).insert(provider);
        assert!(eligibility(world, task).is_err());
    }
    world.entity_mut(old).insert(original_provider);
    let operation = lifecycle::launch_of(world, old).unwrap();
    let original = world.get::<lifecycle::LaunchTemplate>(operation).unwrap().clone();
    let mut prompt_command = original.clone();
    prompt_command.argv.extend(["run".into(), "replay an old prompt".into()]);
    world.entity_mut(operation).insert(prompt_command);
    assert!(eligibility(world, task).is_err());
    let mut wrapper = original;
    wrapper.argv[3] = "/bin/sh".into();
    world.entity_mut(operation).insert(wrapper);
    assert!(eligibility(world, task).is_err());
}

#[test]
fn lost_reply_and_journal_restart_preserve_resume_uncertainty_without_recreation() {
    let dir = tempfile::tempdir().unwrap();
    let new_id;
    {
        let mut app = build(dir.path());
        let world = app.world_mut();
        let (task, _, _) = finished(world);
        effects(world);
        let attempt = resume(world, task, request("resume-1")).unwrap();
        new_id = world.get::<AttemptId>(attempt).unwrap().0;
        let call = creations(&effects(world))[0].0;
        world.write_message(Inbound::FuxReply { call, result: Err("connection lost after dispatch".into()) });
        app.update();
        let world = app.world_mut();
        let operation = world.resource::<Ids>().operation("resume-1").unwrap();
        assert_eq!(world.get::<OperationPhase>(operation), Some(&OperationPhase::Uncertain));
        effects(world);
        assert_eq!(resume(world, task, request("resume-1")).unwrap(), attempt);
        assert!(effects(world).is_empty());
    }
    let mut app = build(dir.path());
    let world = app.world_mut();
    let task = world.resource::<Ids>().task("task").unwrap();
    let attempt = resume(world, task, request("resume-1")).unwrap();
    assert_eq!(world.get::<AttemptId>(attempt).unwrap().0, new_id);
    assert_eq!(world.get::<Attempts>(task).unwrap().len(), 2);
    let operation = world.resource::<Ids>().operation("resume-1").unwrap();
    assert_eq!(world.get::<OperationPhase>(operation), Some(&OperationPhase::Uncertain));
    let sent = effects(world);
    assert!(creations(&sent).is_empty());
    assert!(!sent.iter().any(|effect| matches!(effect, Effect::FuxCall { method, .. } if method.starts_with("fux/input."))));
    assert_eq!(check_invariants(world), Ok(()));
}

#[test]
fn guarded_endpoint_distinguishes_zor_envelope_from_fux_target_and_allows_stable_retry() {
    let server = common::Server::start();
    let old_id = server.with_world(|world| {
        let (_, old, _) = finished(world);
        world.get::<AttemptId>(old).unwrap().0
    });
    let status = server.call("zor/task.resume-status", json!({ "task": "task" })).unwrap();
    assert_eq!(status["eligible"], true);
    assert_eq!(status["native_session"], SESSION);
    assert!(server.call("zor/task.resume", json!({
        "task": "task", "operation": "resume-1", "fux_instance": FUX,
        "guard": { "attempt": old_id, "pane": null }
    })).is_err());
    assert!(server.call("zor/task.resume", json!({
        "task": "task", "operation": "resume-1", "fux_instance": "wrong-fux",
        "guard": { "attempt": null, "pane": null }
    })).is_err());
    let params = json!({ "task": "task", "operation": "resume-1", "fux_instance": FUX,
        "guard": { "attempt": null, "pane": null } });
    let first = server.call("zor/task.resume", params.clone()).unwrap();
    let retry = server.call("zor/task.resume", params).unwrap();
    assert_eq!(first["attempt"], retry["attempt"]);
    assert_ne!(first["attempt"], old_id);
    server.with_world(|world| {
        let task = world.resource::<Ids>().task("task").unwrap();
        assert_eq!(world.get::<Attempts>(task).unwrap().len(), 2);
    });
}

#[test]
fn late_ephemeral_cleanup_cannot_target_the_resumed_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = build(dir.path());
    let world = app.world_mut();
    let mut spec = launch_spec();
    spec.ephemeral = true;
    let (task, old, _) = live_with_spec(world, spec);
    effects(world);
    lifecycle::observe(world, old, Observed::Final(FinalEvidence {
        exit_code: Some(0), ..Default::default()
    }));
    world.entity_mut(old).insert(ProducerLifetime {
        producer: String::new(), registered_ms: 100,
        retired: vec![("opencode:observer".into(), 100, 300)],
    });
    let pending_cleanup = effects(world).into_iter().find_map(|effect| match effect {
        Effect::FuxCall { method, params, .. } if method == "fux/workspace.kill" => Some(params),
        _ => None,
    }).unwrap();
    let attempt = resume(world, task, request("resume-1")).unwrap();
    let create = creations(&effects(world));
    assert_eq!(create.len(), 1);
    let new_workspace = &create[0].1["name"];
    assert_ne!(new_workspace, &pending_cleanup["name"]);
    assert_eq!(new_workspace, &world.get::<PaneHandle>(attempt).unwrap().workspace);
    assert_eq!(resume(world, task, request("resume-1")).unwrap(), attempt);
    assert!(effects(world).is_empty());
}

#[test]
fn sidecar_exit_after_finished_commit_preserves_native_authority_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut app = build(dir.path());
        let world = app.world_mut();
        let (task, attempt, _) = live(world);
        let mut session = ProviderSession::default();
        session.state = SessionState::Running;
        session.producer = "opencode:observer".into();
        world.entity_mut(attempt).insert(session);
        lifecycle::observe(world, attempt, Observed::Final(FinalEvidence {
            exit_code: Some(0), ..Default::default()
        }));
        // First commit knows the pane finished but the sidecar has not exited.
        app.update();
        assert!(eligibility(app.world(), task).is_err());
        // Run a quiescent update so unrelated lifecycle dirtiness cannot accidentally
        // persist the producer retirement in the later, otherwise isolated completion.
        app.update();
        app.world_mut().write_message(Inbound::ProviderExited { attempt, code: 0 });
        app.update();
        assert_eq!(eligibility(app.world(), task).unwrap().native_session, SESSION);
    }
    let mut app = build(dir.path());
    let world = app.world_mut();
    let task = world.resource::<Ids>().task("task").unwrap();
    assert_eq!(eligibility(world, task).unwrap().native_session, SESSION);
    effects(world);
    let attempt = resume(world, task, request("resume-after-restart")).unwrap();
    let create = creations(&effects(world));
    assert_eq!(create.len(), 1);
    assert_eq!(create[0].1["template"]["argv"][5], SESSION);
    assert_eq!(lifecycle::attempt_of(world, task), Some(attempt));
}
