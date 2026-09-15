//! Provider adapters and passive observation (`docs/model.md` invariants 20, 26): a fake
//! sidecar drives bindings and reports through the real `ProviderAdapter`; claims are refused
//! without a matching report token; rule bundles classify screens and hot-reload; the
//! `AgentView` projection reflects the merged evidence.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use bevy_app::{App, TaskPoolPlugin};
use bevy_asset::{AssetPlugin, Assets};
use bevy_ecs::prelude::*;
use common::handle;
use serde_json::json;
use zor::config::Config;
use zor::model::invariants::check_invariants;
use zor::model::*;
use zor::providers::rules::Rules;
use zor::providers::{
    self, AgentRecord, Evidence, Provider, ProviderAdapter, ProviderError, ProviderKind,
    ProviderSession, ProvidersPlugin, RulesBundle, Screen, SessionState, arm, observation_of,
};
use zor::remote::projection::AgentView;
use zor::runner::Adapter;

/// A headless app stepped like the runner does: `Clock` stamped, the adapter's `Inbound`
/// fed in, `Effect`s drained to the adapter (or kept for the test) after every update, the
/// structural invariants checked after every update.
struct Harness {
    app: App,
    inbound: async_channel::Receiver<Inbound>,
    adapter: ProviderAdapter,
    /// Whether `SpawnProvider` reaches the adapter; off when a test injects frames itself.
    spawn: bool,
    /// Frames the World wrote to the sidecar while `spawn` is off.
    writes: Vec<(Entity, Vec<u8>)>,
    fux_calls: Vec<(u64, String, serde_json::Value)>,
    now_ms: u64,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = zor::app::build_headless(&Config::default(), dir.path());
        let (tx, rx) = async_channel::unbounded();
        Self {
            app,
            inbound: rx,
            adapter: ProviderAdapter::new(tx),
            spawn: true,
            writes: Vec::new(),
            fux_calls: Vec::new(),
            now_ms: 1_000_000,
            _dir: dir,
        }
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn step(&mut self) {
        self.now_ms += 20;
        let now = self.now_ms;
        let world = self.app.world_mut();
        world.resource_mut::<Clock>().now_ms = now;
        while let Ok(message) = self.inbound.try_recv() {
            world.write_message(message);
        }
        self.app.update();
        let effects: Vec<Effect> = self
            .app
            .world_mut()
            .resource_mut::<Messages<Effect>>()
            .drain()
            .collect();
        for effect in effects {
            match effect {
                Effect::FuxCall {
                    call,
                    method,
                    params,
                } => self.fux_calls.push((call, method, params)),
                Effect::SpawnProvider { .. } if !self.spawn => {}
                Effect::WriteProvider { attempt, bytes } if !self.spawn => {
                    self.writes.push((attempt, bytes));
                }
                other if self.adapter.handles(&other) => self.adapter.apply(other).unwrap(),
                other => panic!("unexpected effect {other:?}"),
            }
        }
        assert_eq!(check_invariants(self.app.world_mut()), Ok(()));
    }

    /// Steps (real time) until `f` holds or `timeout` passes.
    fn step_until(&mut self, timeout: Duration, mut f: impl FnMut(&mut World) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.step();
            if f(self.app.world_mut()) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Steps with the clock advanced by `ms` in one jump (no real waiting).
    fn advance(&mut self, ms: u64) {
        self.now_ms += ms;
        self.step();
    }
}

fn live_attempt(world: &mut World, provider: Option<Provider>) -> (Entity, Entity) {
    let task = spawn_task(
        world,
        TaskSpec {
            id: "t1",
            title: "task",
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
            handle: handle("fux-1", 7, Some(4242)),
        },
    )
    .unwrap();
    world.entity_mut(attempt).insert(AttemptState::Launching);
    world.entity_mut(attempt).insert(AttemptState::Live);
    world.entity_mut(task).insert(TaskState::Running);
    if let Some(provider) = provider {
        world.entity_mut(attempt).insert(provider);
    }
    (task, attempt)
}

/// A prompt whose bytes fux already accepted (`Delivery::Delivered` with its receipt), or,
/// with `reserved`, one still pending on the pane (at most one per pane, invariant 6).
fn receipted_prompt(
    world: &mut World,
    attempt: Entity,
    id: &str,
    token: &str,
    operation: u64,
    reserved: bool,
) -> Entity {
    let prompt = spawn_prompt(
        world,
        PromptSpec {
            id,
            attempt,
            text: "do the thing",
            deadline_ms: u64::MAX,
            report_token: token,
        },
    )
    .unwrap();
    let (delivery, state, written) = if reserved {
        (Delivery::Reserved, "reserved", 0)
    } else {
        (Delivery::Delivered, "submitted", 13)
    };
    world.entity_mut(prompt).insert((
        delivery,
        Receipt {
            instance: "fux-1".into(),
            pane: 7,
            operation,
            state: state.into(),
            bytes_written: written,
            seq: None,
            expires_ms: u64::MAX,
        },
    ));
    prompt
}

const GOOD: &str = "tok-0123456789abcdef0123456789abcdef";
const OTHER: &str = "tok-fedcba9876543210fedcba9876543210";
const FORGED: &str = "tok-ffffffffffffffffffffffffffffffff";

/// A Codex app-server stand-in: answers `thread/start`, waits for the arm, then emits the
/// turn that consumed `$GOOD`, its completion, and a turn claiming a token zor never armed.
fn fake_codex(dir: &Path) -> String {
    let script = dir.join("fake-codex.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
read -r initialize
read -r thread_start
printf '%s\n' '{"id":"zor:thread","result":{"thread":{"id":"th-1"}}}'
read -r arm
case "$arm" in
  *"\"clientUserMessageId\":\"$GOOD\""*) ;;
  *) printf '%s\n' '{"id":"zor:turn:x","error":{"message":"arm did not carry the token"}}'; exit 3 ;;
esac
printf '%s\n' "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"th-1\",\"turn\":{\"id\":\"turn-1\",\"status\":\"inProgress\",\"items\":[{\"type\":\"userMessage\",\"id\":\"msg-1\",\"clientId\":\"$GOOD\"}]}}}"
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"th-1","turn":{"id":"turn-1","status":"completed","items":[{"type":"agentMessage","id":"msg-2","text":"done"}]}}}'
printf '%s\n' "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"th-1\",\"turn\":{\"id\":\"turn-2\",\"status\":\"inProgress\",\"items\":[{\"type\":\"userMessage\",\"id\":\"msg-3\",\"clientId\":\"$FORGED\"}]}}}"
read -r close_or_eof
exit 0
"#,
    )
    .unwrap();
    script.display().to_string()
}

#[test]
fn fake_codex_binds_the_armed_token_and_refuses_a_forged_one() {
    let mut h = Harness::new();
    let script = fake_codex(h._dir.path());
    let (_, attempt) = live_attempt(
        h.world(),
        Some(Provider {
            kind: ProviderKind::Codex,
            argv: vec!["sh".into(), script],
            cwd: None,
            env: vec![
                ("GOOD".into(), GOOD.into()),
                ("FORGED".into(), FORGED.into()),
            ],
        }),
    );
    let good = receipted_prompt(h.world(), attempt, "p-good", GOOD, 11, true);
    let other = receipted_prompt(h.world(), attempt, "p-other", OTHER, 10, false);

    // Arming precedes the sidecar's session: refused, nothing written.
    assert_eq!(
        arm(h.world(), good),
        Err(ProviderError::NotReady("not spawned"))
    );
    assert!(h.step_until(Duration::from_secs(10), |w| {
        w.get::<ProviderSession>(attempt).is_some_and(|s| {
            s.state == SessionState::Running && s.session.as_deref() == Some("th-1")
        })
    }));
    {
        let s = h.world().get::<ProviderSession>(attempt).unwrap();
        assert!(s.producer.starts_with("codex:"), "{}", s.producer);
        assert!(h.world().get::<ProducerLifetime>(attempt).is_some());
    }
    // The pane is observed; before any claim the native channel yields Unknown, never Idle.
    assert_eq!(
        observation_of(h.world(), attempt).map(|o| o.state),
        Some(AgentState::Unknown)
    );

    arm(h.world(), good).unwrap();
    assert!(h.step_until(Duration::from_secs(10), |w| {
        w.get::<ResponseEvent>(good).is_some()
    }));
    let binding = h.world().get::<Binding>(good).cloned().unwrap();
    assert_eq!(binding.session, "th-1");
    assert_eq!(binding.message, "msg-1");
    assert_eq!(binding.input_operation, 11);
    assert!(binding.producer.starts_with("codex:"));
    let report = h.world().get::<ResponseEvent>(good).cloned().unwrap();
    assert_eq!(report.kind, "response");
    assert_eq!(report.input_operation, 11);
    assert!(report.sequence > binding.sequence);

    // The forged turn never binds the other prompt and poisons the native evidence.
    assert!(h.step_until(Duration::from_secs(10), |w| {
        w.get::<ProviderSession>(attempt)
            .is_some_and(|s| s.problem.is_some())
    }));
    assert!(h.world().get::<Binding>(other).is_none());
    assert!(h.world().get::<ResponseEvent>(other).is_none());
    let problem = h
        .world()
        .get::<ProviderSession>(attempt)
        .unwrap()
        .problem
        .clone()
        .unwrap();
    assert!(problem.contains("mismatched report token"), "{problem}");
    h.step();
    let observation = observation_of(h.world(), attempt).unwrap();
    assert_eq!(observation.state, AgentState::Unknown);
    let agent = providers::agent_for(h.world(), &handle("fux-1", 7, None)).unwrap();
    let evidence = h.world().get::<Evidence>(agent).unwrap().clone();
    assert_eq!(evidence.source, providers::EvidenceSource::Native);
    assert!(evidence.problem.unwrap().contains("mismatched"));
    // Nothing here touched the unreported prompt's wait (invariant 20): only the lifecycle's
    // reading of a `ResponseEvent` may.
    assert_eq!(h.world().get::<WaitState>(other), Some(&WaitState::Pending));

    // Finishing the attempt closes the channel; the sidecar exits and the observer goes.
    h.world()
        .entity_mut(attempt)
        .insert((AttemptState::Finished, FinalEvidence::default()));
    assert!(h.step_until(Duration::from_secs(10), |w| {
        w.get::<ProviderSession>(attempt)
            .is_some_and(|s| matches!(s.state, SessionState::Exited { code: 0 }))
    }));
    assert!(providers::agent_for(h.world(), &handle("fux-1", 7, None)).is_none());
    let lifetime = h.world().get::<ProducerLifetime>(attempt).unwrap();
    assert_eq!(lifetime.retired.len(), 1);
    assert!(lifetime.producer.is_empty());
}

fn frame(h: &mut Harness, attempt: Entity, line: &str) {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    h.world()
        .write_message(Inbound::ProviderOutput { attempt, bytes });
    h.step();
}

#[test]
fn native_claims_take_precedence_over_passive_rules_and_expire() {
    let mut h = Harness::new();
    h.spawn = false;
    let (_, attempt) = live_attempt(
        h.world(),
        Some(Provider {
            kind: ProviderKind::Codex,
            argv: vec!["true".into()],
            cwd: None,
            env: Vec::new(),
        }),
    );
    let prompt = receipted_prompt(h.world(), attempt, "p1", GOOD, 5, true);
    h.step();
    // The spawn effect was dropped by the harness: drive the session with injected frames.
    h.world()
        .write_message(Inbound::ProviderStarted { attempt, pid: 99 });
    h.step();
    frame(
        &mut h,
        attempt,
        r#"{"id":"zor:thread","result":{"thread":{"id":"th-9"}}}"#,
    );
    assert_eq!(arm(h.world(), prompt), Ok(()));
    // A passive capture saying idle (the recorded Codex composer) cannot cover a silent producer.
    let call = h
        .fux_calls
        .iter()
        .find(|(_, m, _)| m == "fux/pane.capture")
        .unwrap()
        .0;
    h.world().write_message(Inbound::FuxReply {
        call,
        result: Ok(codex_screen(false)),
    });
    h.step();
    let agent = providers::agent_for(h.world(), &handle("fux-1", 7, None)).unwrap();
    let evidence = h.world().get::<Evidence>(agent).unwrap().clone();
    assert_eq!(evidence.passive, AgentState::Idle);
    // A screen that looks idle satisfies no wait (invariant 26).
    assert_eq!(
        h.world().get::<WaitState>(prompt),
        Some(&WaitState::Pending)
    );
    assert_eq!(evidence.rule.as_deref(), Some("codex-0.153.4-input-ready"));
    let observation = observation_of(h.world(), attempt).unwrap();
    assert_eq!(observation.state, AgentState::Unknown);
    assert_eq!(observation.seq, 77);

    frame(
        &mut h,
        attempt,
        &format!(
            r#"{{"method":"turn/started","params":{{"threadId":"th-9","turn":{{"id":"t1","status":"inProgress","items":[{{"type":"userMessage","id":"m1","clientId":"{GOOD}"}}]}}}}}}"#
        ),
    );
    assert_eq!(
        observation_of(h.world(), attempt).unwrap().state,
        AgentState::Working
    );
    assert!(h.world().get::<Binding>(prompt).is_some());
    frame(
        &mut h,
        attempt,
        r#"{"method":"item/tool/requestUserInput","params":{"threadId":"th-9","turnId":"t1","itemId":"q1","isBlocking":true,"questions":[]}}"#,
    );
    assert_eq!(
        observation_of(h.world(), attempt).unwrap().state,
        AgentState::Blocked
    );
    assert_eq!(
        h.world().get::<ResponseEvent>(prompt).unwrap().kind,
        "needs-input"
    );
    frame(
        &mut h,
        attempt,
        r#"{"method":"turn/completed","params":{"threadId":"th-9","turn":{"id":"t1","status":"completed","items":[]}}}"#,
    );
    // The first report is immutable; the later completion only moves the current state.
    assert_eq!(
        h.world().get::<ResponseEvent>(prompt).unwrap().kind,
        "needs-input"
    );
    assert_eq!(
        observation_of(h.world(), attempt).unwrap().state,
        AgentState::Idle
    );

    // Six seconds of silence: the claim expires to Unknown even though the screen says idle.
    h.advance(providers::NATIVE_TTL_MS + 1);
    let observation = observation_of(h.world(), attempt).unwrap();
    assert_eq!(observation.state, AgentState::Unknown);
    assert!(observation.age_upper_bound_ms > providers::NATIVE_TTL_MS);
    let evidence = h.world().get::<Evidence>(agent).unwrap().clone();
    assert_eq!(evidence.problem.as_deref(), Some("native claim expired"));
    // A malformed frame is a refusal too.
    frame(&mut h, attempt, "not json");
    assert!(
        h.world()
            .get::<ProviderSession>(attempt)
            .unwrap()
            .problem
            .is_some()
    );
    // Exit retires the producer; state stays Unknown with the reason.
    h.world()
        .write_message(Inbound::ProviderExited { attempt, code: 0 });
    h.step();
    let evidence = h.world().get::<Evidence>(agent).unwrap().clone();
    assert_eq!(
        observation_of(h.world(), attempt).unwrap().state,
        AgentState::Unknown
    );
    assert!(evidence.problem.is_some());
}

#[test]
fn opencode_sidecar_needs_a_binding_before_a_report_and_the_operation_must_match() {
    let mut h = Harness::new();
    h.spawn = false;
    let (_, attempt) = live_attempt(
        h.world(),
        Some(Provider {
            kind: ProviderKind::OpenCode,
            argv: vec!["true".into()],
            cwd: None,
            env: Vec::new(),
        }),
    );
    let prompt = receipted_prompt(h.world(), attempt, "p1", GOOD, 5, false);
    let unreceipted = spawn_prompt(
        h.world(),
        PromptSpec {
            id: "p2",
            attempt,
            text: "later",
            deadline_ms: u64::MAX,
            report_token: OTHER,
        },
    )
    .unwrap();
    h.step();
    h.world()
        .write_message(Inbound::ProviderStarted { attempt, pid: 7 });
    h.step();
    frame(
        &mut h,
        attempt,
        r#"{"t":"hello","producer":"plugin-abc","session":"s1"}"#,
    );
    assert_eq!(
        h.world().get::<ProviderSession>(attempt).unwrap().producer,
        "plugin-abc"
    );
    assert_eq!(arm(h.world(), unreceipted), Err(ProviderError::NoReceipt));
    assert_eq!(arm(h.world(), prompt), Ok(()));
    h.step();
    // The arm carries the token to the sidecar, never to the terminal.
    let (_, written) = h.writes.pop().unwrap();
    let armed: serde_json::Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(
        armed,
        json!({ "t": "arm", "operation": "p1", "token": GOOD, "text": "do the thing" })
    );
    // Report before binding: refused.
    frame(
        &mut h,
        attempt,
        &format!(r#"{{"t":"report","operation":"p1","token":"{GOOD}","kind":"response"}}"#),
    );
    assert!(h.world().get::<ResponseEvent>(prompt).is_none());
    // Binding under the wrong operation id: refused.
    frame(
        &mut h,
        attempt,
        &format!(
            r#"{{"t":"bound","operation":"p2","token":"{GOOD}","session":"s1","message":"u1"}}"#
        ),
    );
    assert!(h.world().get::<Binding>(prompt).is_none());
    // A claim for a prompt without a receipt: refused (invariant 7).
    frame(
        &mut h,
        attempt,
        &format!(
            r#"{{"t":"bound","operation":"p2","token":"{OTHER}","session":"s1","message":"u2"}}"#
        ),
    );
    assert!(h.world().get::<Binding>(unreceipted).is_none());
    frame(
        &mut h,
        attempt,
        &format!(
            r#"{{"t":"bound","operation":"p1","token":"{GOOD}","session":"s1","message":"u1"}}"#
        ),
    );
    let binding = h.world().get::<Binding>(prompt).cloned().unwrap();
    assert_eq!(
        (binding.producer.as_str(), binding.input_operation),
        ("plugin-abc", 5)
    );
    frame(
        &mut h,
        attempt,
        &format!(r#"{{"t":"report","operation":"p1","token":"{GOOD}","kind":"needs-input"}}"#),
    );
    assert_eq!(
        h.world().get::<ResponseEvent>(prompt).unwrap().kind,
        "needs-input"
    );
    frame(&mut h, attempt, r#"{"t":"state","state":"blocked"}"#);
    assert_eq!(
        observation_of(h.world(), attempt).unwrap().state,
        AgentState::Blocked
    );
}

fn screen(lines: &[&str]) -> Screen {
    Screen::from_lines(24, 80, lines.iter().map(|l| (*l).to_owned()).collect(), "")
}

/// The recorded 0.153.4 composer: working when the interrupt footer shows, input-ready otherwise.
fn codex_screen(working: bool) -> serde_json::Value {
    let mut lines = vec!["│ >_ OpenAI Codex (v0.153.4) │".to_owned(), String::new()];
    if working {
        lines.push("• Working (3s • esc to interrupt)".into());
    } else {
        lines.push(String::new());
    }
    lines.extend(
        [
            "",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt-6-astra default · /tmp",
        ]
        .map(str::to_owned),
    );
    let rows = lines.len() as u16;
    json!({
        "pane": 7, "seq": 77, "rows": rows, "cols": 80, "title": "", "state": "running",
        "cursor": { "row": 0, "col": 0, "visible": true }, "lines": lines, "truncated": false
    })
}

const CUSTOM: &str = r#"
id = 'custom'

[[rules]]
id = 'working'
state = 'working'
visible_working = true
contains = ['esc to interrupt']

[[rules]]
id = 'idle'
state = 'idle'
visible_idle = true
region = 'bottom(2)'
contains = ['❯']

[[rules]]
id = 'permission'
state = 'blocked'
priority = 900
visible_blocker = true
contains = ['permission required']
"#;

#[test]
fn rules_classify_screens_by_priority_and_never_idle_by_default() {
    let bundle = RulesBundle::parse("custom.toml", CUSTOM).unwrap();
    let idle = bundle.evaluate(&screen(&["banner", "", "❯ "]));
    assert_eq!(
        (idle.state, idle.rule.as_deref()),
        (AgentState::Idle, Some("idle"))
    );
    // The blocker outranks the composer, and matching is case-insensitive for `contains`.
    let blocked = bundle.evaluate(&screen(&["Permission Required", "❯"]));
    assert_eq!(
        (blocked.state, blocked.rule.as_deref()),
        (AgentState::Blocked, Some("permission"))
    );
    // Equal priority: the earlier rule wins.
    let working = bundle.evaluate(&screen(&["esc to interrupt", "❯"]));
    assert_eq!(working.rule.as_deref(), Some("working"));
    // The idle rule reads only the bottom two rows.
    let far = bundle.evaluate(&screen(&["❯", "", "", "output"]));
    assert_eq!((far.state, far.rule), (AgentState::Unknown, None));
    // Nothing matches: Unknown, never Idle; an empty screen too.
    assert_eq!(
        bundle.evaluate(&screen(&["compiling..."])).state,
        AgentState::Unknown
    );
    assert_eq!(bundle.evaluate(&screen(&[])).state, AgentState::Unknown);

    // Built-in Codex rules over the recorded composer shapes, through the collection.
    let rules = Rules::new(None);
    let assets = Assets::<RulesBundle>::default();
    let capture =
        |working| Screen::from_capture(serde_json::from_value(codex_screen(working)).unwrap());
    let v = rules.evaluate(&assets, Some("codex"), &capture(true));
    assert_eq!(
        (v.state, v.rule.as_deref()),
        (AgentState::Working, Some("codex-0.153.4-working"))
    );
    let v = rules.evaluate(&assets, Some("codex"), &capture(false));
    assert_eq!(
        (v.state, v.rule.as_deref()),
        (AgentState::Idle, Some("codex-0.153.4-input-ready"))
    );
    // Without a known agent every bundle is consulted; a screen no bundle knows is Unknown.
    assert_eq!(
        rules
            .evaluate(&assets, None, &capture(true))
            .bundle
            .as_deref(),
        Some("codex")
    );
    assert_eq!(
        rules.evaluate(&assets, None, &screen(&["$ "])).state,
        AgentState::Unknown
    );
    assert_eq!(
        rules.evaluate(&assets, Some("nope"), &capture(false)).state,
        AgentState::Unknown
    );
    let ids: Vec<String> = rules.list(&assets).into_iter().map(|b| b.id).collect();
    assert_eq!(ids, ["codex", "claude", "opencode"]);

    // Invalid bundles are refused as a whole.
    for (what, source) in [
        (
            "flags",
            "id='x'\n[[rules]]\nid='a'\nstate='idle'\nvisible_blocker=true\ncontains=['x']\n",
        ),
        ("no matcher", "id='x'\n[[rules]]\nid='a'\nstate='idle'\n"),
        (
            "regex",
            "id='x'\n[[rules]]\nid='a'\nstate='idle'\nregex=['(']\n",
        ),
        (
            "dup",
            "id='x'\n[[rules]]\nid='a'\nstate='idle'\ncontains=['x']\n[[rules]]\nid='a'\nstate='idle'\ncontains=['y']\n",
        ),
        ("agent id", "id='bad id'\nrules=[]\n"),
        ("unknown key", "id='x'\nbogus=1\nrules=[]\n"),
    ] {
        assert!(RulesBundle::parse("t", source).is_err(), "{what} accepted");
    }
}

fn rules_app(root: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        TaskPoolPlugin::default(),
        bevy_state::app::StatesPlugin,
        bevy_time::TimePlugin,
        AssetPlugin {
            file_path: root.display().to_string(),
            watch_for_changes_override: Some(true),
            ..Default::default()
        },
        ModelPlugin,
        ProvidersPlugin {
            asset_root: Some(root.to_path_buf()),
        },
    ));
    app.finish();
    app.cleanup();
    app
}

fn write_atomic(path: &Path, content: &str) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn state_of(app: &mut App, bundle: &str, text: &str) -> AgentState {
    let world = app.world_mut();
    let rules = world.resource::<Rules>();
    let assets = world.resource::<Assets<RulesBundle>>();
    rules.evaluate(assets, Some(bundle), &screen(&[text])).state
}

fn until(app: &mut App, timeout: Duration, mut f: impl FnMut(&mut App) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        app.update();
        if f(app) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn rule_bundle_edits_reload_within_two_seconds_and_invalid_ones_keep_the_previous() {
    let dir = tempfile::tempdir().unwrap();
    // The watcher compares canonical paths: `/var` is `/private/var` on macOS.
    let root = dir.path().canonicalize().unwrap();
    let rules_dir = root.join("rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    let file = rules_dir.join("custom.toml");
    write_atomic(
        &file,
        "id='custom'\n[[rules]]\nid='r'\nstate='idle'\ncontains=['ready']\n",
    );
    let mut app = rules_app(&root);
    assert!(until(&mut app, Duration::from_secs(10), |app| state_of(
        app, "custom", "READY"
    )
        == AgentState::Idle));
    // The asset event that bumps the generation is read on the next update.
    app.update();
    assert_eq!(app.world().resource::<Rules>().generation, 1);
    let listed = app.world_mut().resource_scope(|world, rules: Mut<Rules>| {
        rules.list(world.resource::<Assets<RulesBundle>>())
    });
    let custom = listed.iter().find(|b| b.id == "custom").unwrap();
    assert_eq!(
        (custom.source.as_str(), custom.rules, custom.effective),
        ("rules/custom.toml", 1, true)
    );

    // An edit on disk is live within two seconds.
    write_atomic(
        &file,
        "id='custom'\n[[rules]]\nid='r'\nstate='working'\ncontains=['ready']\n",
    );
    let edited = Instant::now();
    assert!(until(&mut app, Duration::from_secs(5), |app| state_of(
        app, "custom", "READY"
    )
        == AgentState::Working));
    assert!(
        edited.elapsed() < Duration::from_secs(2),
        "reload took {:?}",
        edited.elapsed()
    );
    app.update();
    assert_eq!(app.world().resource::<Rules>().generation, 2);

    // An invalid edit is reported and the previous bundle stays in force.
    write_atomic(
        &file,
        "id='custom'\n[[rules]]\nid='r'\nstate='idle'\nvisible_working=true\ncontains=['ready']\n",
    );
    assert!(until(&mut app, Duration::from_secs(5), |app| app
        .world()
        .resource::<Rules>()
        .problem
        .is_some()));
    let problem = app.world().resource::<Rules>().problem.clone().unwrap();
    assert!(problem.contains("custom.toml"), "{problem}");
    assert_eq!(state_of(&mut app, "custom", "READY"), AgentState::Working);
    assert_eq!(app.world().resource::<Rules>().generation, 2);

    // A new file appears only on an explicit reload; a fix clears the problem.
    write_atomic(
        &file,
        "id='custom'\n[[rules]]\nid='r'\nstate='blocked'\ncontains=['ready']\n",
    );
    write_atomic(
        &rules_dir.join("extra.toml"),
        "id='extra'\n[[rules]]\nid='e'\nstate='idle'\ncontains=['extra']\n",
    );
    assert!(until(&mut app, Duration::from_secs(5), |app| state_of(
        app, "custom", "READY"
    )
        == AgentState::Blocked));
    app.update();
    assert!(app.world().resource::<Rules>().problem.is_none());
    assert_eq!(app.world().resource::<Rules>().generation, 3);
    assert_eq!(state_of(&mut app, "extra", "extra"), AgentState::Unknown);
    let server = app.world().resource::<bevy_asset::AssetServer>().clone();
    assert_eq!(app.world_mut().resource_mut::<Rules>().reload(&server), 2);
    assert!(until(&mut app, Duration::from_secs(5), |app| state_of(
        app, "extra", "extra"
    )
        == AgentState::Idle));
}

#[test]
fn agent_view_and_agent_methods_reflect_the_merged_evidence() {
    let server = common::Server::start();
    let attempt = server.with_world(|world| {
        world.resource_mut::<Clock>().now_ms = 50_000;
        let (_, attempt) = live_attempt(world, None);
        attempt
    });
    // The heartbeat issued a capture; answer it with the recorded Codex working composer.
    let call = server.with_world(|world| {
        world.resource_mut::<Clock>().now_ms = 51_000;
        world
            .resource_mut::<Messages<Effect>>()
            .drain()
            .find_map(|e| match e {
                Effect::FuxCall { call, method, .. } if method == "fux/pane.capture" => Some(call),
                _ => None,
            })
            .unwrap()
    });
    server.with_world(move |world| {
        world.write_message(Inbound::FuxReply {
            call,
            result: Ok(codex_screen(true)),
        });
    });
    let view = server.with_world(|world| {
        assert_eq!(check_invariants(world), Ok(()));
        world
            .query::<&AgentView>()
            .iter(world)
            .next()
            .cloned()
            .unwrap()
    });
    assert_eq!(view.state, "Working");
    assert_eq!(view.source, "passive");
    assert_eq!(view.rule.as_deref(), Some("codex-0.153.4-working"));
    assert_eq!(
        (view.pane, view.pid, view.attempt.is_some()),
        (7, Some(4242), true)
    );
    assert_eq!(view.task.as_deref(), Some("t1"));
    assert_eq!(view.provider, None);
    assert_eq!(view.since_ms, 51_000);

    let listed = server.call("zor/agent.list", json!({})).unwrap();
    let agents = listed["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["state"], "Working");
    assert_eq!(agents[0]["passive"], "Working");
    let id = agents[0]["id"].as_u64().unwrap();
    let one = server
        .call("zor/agent.inspect", json!({ "agent": id }))
        .unwrap();
    assert_eq!(one["rule"], "codex-0.153.4-working");
    assert_eq!(
        common::code(server.call("zor/agent.inspect", json!({ "agent": id + 1 }))),
        -32003
    );

    let rules = server.call("zor/rules.list", json!({})).unwrap();
    assert_eq!(rules["bundles"].as_array().unwrap().len(), 3);
    assert_eq!(rules["generation"], 0);
    let reloaded = server.call("zor/rules.reload", json!({})).unwrap();
    assert_eq!(reloaded["requested"], 0);
    // Mutations need the instance nonce.
    assert_eq!(
        common::code(server.raw(
            "zor/rules.reload",
            json!({ "token": server.descriptor.token.clone() })
        )),
        -32004
    );

    // Live records agree with the projection.
    let record: AgentRecord =
        server.with_world(|world| providers::agent_records(world).remove(0).1);
    assert_eq!(record.state, "Working");
    assert_eq!(record.attempt, view.attempt);
    let _ = attempt;
}
