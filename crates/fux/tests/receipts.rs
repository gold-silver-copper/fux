//! Input receipts and final records (oracle `docs/local-control-protocol.md`, "tracked input"
//! and `final`): through the World with `Inbound`/`Effect`, then over BRP.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use bevy_app::App;
use bevy_ecs::entity_disabling::Disabled;
use bevy_ecs::prelude::*;
use bevy_ecs::query::Allow;
use fux::app::build_headless;
use fux::config::Config;
use fux::finals::{
    self, FinalError, MAX_FINAL_CAPTURE_BYTES, MAX_FINAL_RECORDS, MAX_FINAL_RETENTION_MS,
    MAX_FORGOTTEN_FINAL_IDS,
};
use fux::input_ops::{self, InputError, MAX_INPUT_OPERATIONS, MAX_INPUT_RETENTION_MS, Receipt};
use fux::lifecycle::{self, Clock};
use fux::model::invariants::check_invariants;
use fux::model::*;
use fux::remote::client;
use fux::remote::input_methods::final_codes;
use fux::remote::methods::{self, codes};
use fux::terminal::Terminal;
use serde_json::{Value, json};

const NONCE: &str = "test-instance";

// ---------------------------------------------------------------------------------------------
// World harness
// ---------------------------------------------------------------------------------------------

struct Harness {
    app: App,
}

impl Harness {
    /// A `default` workspace whose one pane is live.
    fn new() -> (Self, Entity) {
        let mut app = build_headless(&Config::default());
        let world = app.world_mut();
        world.resource_mut::<Clock>().now_ms = 1_000;
        world.resource_mut::<ServerInstance>().nonce = NONCE.into();
        lifecycle::bootstrap(world, "default", &["/bin/sh".into()]).unwrap();
        let mut h = Self { app };
        h.step(vec![]);
        let pane = h.panes()[0];
        h.step(vec![Inbound::PaneSpawned { pane, pid: 7 }]);
        assert_eq!(
            h.world().get::<Process>(pane),
            Some(&Process::Live { pid: 7 })
        );
        (h, pane)
    }

    fn step(&mut self, messages: Vec<Inbound>) -> Vec<Effect> {
        let world = self.app.world_mut();
        world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(messages);
        self.app.update();
        let world = self.app.world_mut();
        let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
        check_invariants(world).unwrap();
        effects
    }

    fn panes(&mut self) -> Vec<Entity> {
        self.app
            .world_mut()
            .query_filtered::<Entity, (With<Pane>, Allow<Disabled>)>()
            .iter(self.app.world())
            .collect()
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    fn now(&mut self) -> u64 {
        self.world().resource::<Clock>().now_ms
    }

    fn advance(&mut self, ms: u64) {
        self.world().resource_mut::<Clock>().now_ms += ms;
    }

    fn operation(&mut self, id: u64) -> Option<(InputOperation, Receipt, Option<Entity>)> {
        let entity = input_ops::find(self.world(), id)?;
        let world = self.world();
        Some((
            world.get::<InputOperation>(entity).cloned()?,
            world.get::<Receipt>(entity).cloned()?,
            world.get::<OperationOn>(entity).map(|on| on.0),
        ))
    }

    fn state(&mut self, id: u64) -> InputState {
        self.operation(id).unwrap().0.state
    }

    fn reserve(&mut self, pane: Entity, retain_ms: u64) -> u64 {
        let entity = input_ops::reserve(self.world(), pane, retain_ms).unwrap();
        self.world().get::<InputOperation>(entity).unwrap().id
    }

    fn effects(&mut self) -> Vec<Effect> {
        self.world()
            .resource_mut::<Messages<Effect>>()
            .drain()
            .collect()
    }

    fn records(&mut self) -> Vec<FinalRecord> {
        self.world()
            .query::<&FinalRecord>()
            .iter(self.app.world())
            .cloned()
            .collect()
    }

    fn read_final(&mut self, pane: u64) -> Result<FinalRecord, FinalError> {
        finals::read(self.world(), NONCE, PaneId(pane)).cloned()
    }
}

fn writes(effects: &[Effect]) -> Vec<(Entity, Vec<u8>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::WritePty { pane, bytes } => Some((*pane, bytes.clone())),
            _ => None,
        })
        .collect()
}

fn record(pane: u64, exited_ms: u64, retain_ms: u64) -> FinalRecord {
    FinalRecord {
        pane: PaneId(pane),
        workspace: "default".into(),
        stream: String::new(),
        exit_code: 0,
        title: String::new(),
        last_seq: 0,
        exited_ms,
        expires_ms: exited_ms + retain_ms,
        screen: vec!["$ ".into()],
    }
}

// ---------------------------------------------------------------------------------------------
// Receipts through the World
// ---------------------------------------------------------------------------------------------

#[test]
fn reserve_submit_writes_once_and_records_the_sequence() {
    let (mut h, pane) = Harness::new();
    h.step(vec![Inbound::PaneOutput {
        pane,
        bytes: b"hello\r\n".to_vec(),
    }]);
    let seq = h.world().get::<Terminal>(pane).unwrap().seq();
    assert!(seq > 0, "output advanced the terminal sequence");

    let id = h.reserve(pane, 5_000);
    let (op, receipt, on) = h.operation(id).unwrap();
    assert_eq!(op.state, InputState::Reserved);
    assert_eq!(op.reserved_ms, 1_000);
    assert_eq!(receipt.expires_ms, 6_000);
    assert_eq!(receipt.workspace, "default");
    assert_eq!(on, Some(pane));

    let bytes = input_ops::parse_keys("ls -l\\n").unwrap();
    input_ops::submit(h.world(), id, bytes.clone()).unwrap();
    assert_eq!(writes(&h.effects()), vec![(pane, b"ls -l\n".to_vec())]);
    assert_eq!(
        h.state(id),
        InputState::Submitted {
            seq,
            bytes: bytes.len()
        }
    );

    // Same bytes again: the receipt, no second write. Different bytes: conflict.
    input_ops::submit(h.world(), id, bytes).unwrap();
    assert_eq!(
        input_ops::submit(h.world(), id, b"rm -rf\n".to_vec()),
        Err(InputError::Conflict)
    );
    assert_eq!(
        input_ops::submit(h.world(), id, Vec::new()),
        Err(InputError::Empty)
    );
    assert!(writes(&h.step(vec![])).is_empty());
    assert_eq!(
        h.state(id),
        InputState::Submitted {
            seq,
            bytes: b"ls -l\n".len()
        }
    );
    assert_eq!(
        input_ops::submit(h.world(), 99, b"x".to_vec()),
        Err(InputError::NoSuchOperation(99))
    );
}

#[test]
fn reservation_needs_a_live_pane_and_nonzero_retention() {
    let (mut h, pane) = Harness::new();
    assert_eq!(
        input_ops::reserve(h.world(), pane, 0),
        Err(InputError::ZeroRetention)
    );
    h.step(vec![Inbound::PaneEof { pane }]);
    assert_eq!(
        input_ops::reserve(h.world(), pane, 1_000),
        Err(InputError::PaneNotLive(Process::Eof { pid: 7 }))
    );
}

#[test]
fn retention_clamps_to_the_ceiling_and_expiry_is_two_phased() {
    let (mut h, pane) = Harness::new();
    let id = h.reserve(pane, MAX_INPUT_RETENTION_MS * 3);
    let (_, receipt, _) = h.operation(id).unwrap();
    assert_eq!(receipt.expires_ms, 1_000 + MAX_INPUT_RETENTION_MS);
    assert_eq!(receipt.retain_ms, MAX_INPUT_RETENTION_MS);

    let short = h.reserve(pane, 2_000);
    input_ops::submit(h.world(), short, b"a".to_vec()).unwrap();
    h.effects();
    h.advance(1_999);
    h.step(vec![]);
    assert!(matches!(h.state(short), InputState::Submitted { .. }));

    // At `expires_ms` the receipt reads `expired` and refuses submission...
    h.advance(1);
    h.step(vec![]);
    assert_eq!(h.state(short), InputState::Expired);
    assert_eq!(
        input_ops::submit(h.world(), short, b"a".to_vec()),
        Err(InputError::Expired)
    );
    assert_eq!(h.state(id), InputState::Reserved);

    // ...and one retention window later it is gone.
    h.advance(1_999);
    h.step(vec![]);
    assert!(h.operation(short).is_some());
    h.advance(1);
    h.step(vec![]);
    assert!(h.operation(short).is_none());
    assert_eq!(h.state(id), InputState::Reserved);
}

#[test]
fn capacity_refuses_the_129th_reservation_until_one_expires() {
    let (mut h, pane) = Harness::new();
    let first = h.reserve(pane, 1_000);
    for _ in 1..MAX_INPUT_OPERATIONS {
        h.reserve(pane, 10_000);
    }
    assert_eq!(
        input_ops::reserve(h.world(), pane, 10_000),
        Err(InputError::Capacity)
    );
    // Once the shortest reservation expires, the next reservation takes its place.
    h.advance(1_000);
    let next = h.reserve(pane, 10_000);
    assert_eq!(next, MAX_INPUT_OPERATIONS as u64 + 1);
    assert!(
        h.operation(first).is_none(),
        "the expired receipt made room"
    );
    assert_eq!(
        input_ops::reserve(h.world(), pane, 10_000),
        Err(InputError::Capacity)
    );
}

#[test]
fn exit_before_delivery_is_uncertain_and_receipts_outlive_the_pane() {
    let (mut h, pane) = Harness::new();
    let reserved = h.reserve(pane, 60_000);
    let delivered = h.reserve(pane, 60_000);
    let undelivered = h.reserve(pane, 60_000);
    input_ops::submit(h.world(), delivered, b"a".to_vec()).unwrap();
    // The write is drained after this update; the pane is still live at the next ingest.
    assert_eq!(writes(&h.step(vec![])).len(), 1);
    input_ops::submit(h.world(), undelivered, b"b".to_vec()).unwrap();
    assert_eq!(writes(&h.effects()).len(), 1);

    // The pane exits before the next update confirms the second write.
    h.step(vec![Inbound::PaneExited { pane, code: 0 }]);
    assert_eq!(h.state(reserved), InputState::Uncertain);
    assert_eq!(h.state(undelivered), InputState::Uncertain);
    assert!(matches!(h.state(delivered), InputState::Submitted { .. }));
    for id in [reserved, delivered, undelivered] {
        assert_eq!(h.operation(id).unwrap().2, None, "detached from the pane");
    }
    assert_eq!(
        input_ops::submit(h.world(), reserved, b"a".to_vec()),
        Err(InputError::Unusable)
    );

    // The pane despawns; the receipts stay readable.
    h.step(vec![]);
    h.step(vec![]);
    assert!(h.world().get_entity(pane).is_err());
    assert_eq!(h.state(reserved), InputState::Uncertain);
    assert!(matches!(h.state(delivered), InputState::Submitted { .. }));
}

// ---------------------------------------------------------------------------------------------
// Final records through the World
// ---------------------------------------------------------------------------------------------

#[test]
fn final_record_is_pending_then_present_then_expired() {
    let (mut h, pane) = Harness::new();
    let id = h.world().get::<PaneId>(pane).unwrap().0;
    let retain = Config::default().limits().final_retain_ms;
    assert_eq!(h.read_final(id).unwrap_err(), FinalError::Pending);
    assert_eq!(h.read_final(id + 100).unwrap_err(), FinalError::Unknown);
    assert_eq!(
        finals::read(h.world(), "other", PaneId(id)).unwrap_err(),
        FinalError::Conflict
    );

    h.step(vec![Inbound::PaneOutput {
        pane,
        bytes: b"evidence".to_vec(),
    }]);
    h.advance(4_000);
    h.step(vec![Inbound::PaneExited { pane, code: 3 }]);
    // The record exists, but the pane entity is released next update.
    assert_eq!(h.records().len(), 1);
    assert_eq!(h.read_final(id).unwrap_err(), FinalError::Pending);
    h.step(vec![]);
    let record = h.read_final(id).unwrap();
    assert_eq!(record.exit_code, 3);
    assert_eq!(record.exited_ms, 5_000);
    assert_eq!(record.expires_ms, 5_000 + retain);
    assert!(
        record.screen[0].starts_with("evidence"),
        "{:?}",
        record.screen
    );

    // Past its deadline the record reads expired before and after the sweep.
    h.advance(retain);
    assert_eq!(h.read_final(id).unwrap_err(), FinalError::Expired);
    h.step(vec![]);
    assert!(h.records().is_empty());
    assert_eq!(h.read_final(id).unwrap_err(), FinalError::Expired);
    assert_eq!(h.read_final(id + 100).unwrap_err(), FinalError::Unknown);
}

#[test]
fn capacity_evicts_the_oldest_closed_record_into_a_bounded_ring() {
    let (mut h, _) = Harness::new();
    let now = h.now();
    let extra = MAX_FORGOTTEN_FINAL_IDS + 1;
    // Ids 1000.. closed in order; the newest MAX_FINAL_RECORDS survive.
    for i in 0..(MAX_FINAL_RECORDS + extra) as u64 {
        let r = record(1_000 + i, now + i, 3_600_000);
        h.world().spawn(r);
    }
    h.step(vec![]);
    let mut kept: Vec<u64> = h.records().into_iter().map(|r| r.pane.0).collect();
    kept.sort_unstable();
    let first_kept = 1_000 + extra as u64;
    assert_eq!(kept.len(), MAX_FINAL_RECORDS);
    assert_eq!(kept[0], first_kept);
    assert!(h.read_final(first_kept).is_ok());
    // The ring remembers the most recent 1024 evictions; the very first is forgotten.
    assert_eq!(
        h.read_final(first_kept - 1).unwrap_err(),
        FinalError::Evicted
    );
    assert_eq!(h.read_final(1_001).unwrap_err(), FinalError::Evicted);
    assert_eq!(h.read_final(1_000).unwrap_err(), FinalError::Unknown);
}

#[test]
fn records_are_bounded_in_capture_and_retention() {
    let (mut h, _) = Harness::new();
    let now = h.now();
    let mut r = record(500, now, MAX_FINAL_RETENTION_MS * 2);
    let line: String = "x".repeat(1024);
    r.screen = vec![line; MAX_FINAL_CAPTURE_BYTES / 1024 + 5];
    h.world().spawn(r);
    h.step(vec![]);
    let record = h.read_final(500).unwrap();
    assert_eq!(record.screen.len(), MAX_FINAL_CAPTURE_BYTES / 1024);
    assert_eq!(record.expires_ms, now + MAX_FINAL_RETENTION_MS);
}

// ---------------------------------------------------------------------------------------------
// Over BRP
// ---------------------------------------------------------------------------------------------

use common::{Server, code};

/// The pane entity behind a public id.
fn pane_entity(server: &Server, id: u64) -> Entity {
    server.with_world(move |world| world.resource::<Ids>().pane(PaneId(id)).unwrap())
}

/// Drains the effects the runner would have applied, keeping the PTY writes.
fn drain_writes(server: &Server) -> Vec<(Entity, Vec<u8>)> {
    server.with_world(|world| {
        let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
        writes(&effects)
    })
}

/// The handler called in-World, for the `data` the thin client does not surface.
fn handler(server: &Server, method: &str, params: Value) -> Result<Value, bevy_remote::BrpError> {
    let methods::Handler::Instant(handler) = methods::all_specs()
        .find(|s| s.name == method)
        .unwrap()
        .handler
    else {
        panic!("{method} is not an instant method");
    };
    let mut params = params;
    params["token"] = Value::String(server.descriptor.token.clone());
    params["instance"] = Value::String(server.descriptor.instance.clone());
    server.with_world(move |world| handler(In(Some(params)), world))
}

#[test]
fn receipts_and_finals_over_http() {
    let server = Server::start();
    let created = server
        .call("fux/workspace.new", json!({ "name": "alpha" }))
        .unwrap();
    let pane_id = created["pane"].as_u64().unwrap();
    let pane = pane_entity(&server, pane_id);

    // Not live yet: no reservation. Final: pending.
    assert_eq!(
        code(server.call(
            "fux/input.reserve",
            json!({ "pane": pane_id, "retain_ms": 1000 })
        )),
        codes::INVALID
    );
    assert_eq!(
        code(server.call("fux/pane.final", json!({ "pane": pane_id }))),
        final_codes::PENDING
    );
    let pending = handler(&server, "fux/pane.final", json!({ "pane": pane_id })).unwrap_err();
    assert_eq!(pending.data, Some(json!({ "reason": "pending" })));

    server.with_world(move |world| {
        world.resource_mut::<Clock>().now_ms = 10_000;
        world
            .resource_mut::<Messages<Inbound>>()
            .write(Inbound::PaneSpawned { pane, pid: 42 });
    });
    drain_writes(&server);

    assert_eq!(
        code(server.call(
            "fux/input.reserve",
            json!({ "pane": pane_id, "retain_ms": 0 })
        )),
        bevy_remote::error_codes::INVALID_PARAMS
    );
    let reserved = server
        .call(
            "fux/input.reserve",
            json!({ "pane": pane_id, "retain_ms": MAX_INPUT_RETENTION_MS * 2 }),
        )
        .unwrap();
    let operation = reserved["operation"].as_u64().unwrap();
    assert_eq!(reserved["state"], "reserved");
    assert_eq!(reserved["pane"], pane_id);
    assert_eq!(reserved["expires_ms"], 10_000 + MAX_INPUT_RETENTION_MS);

    assert_eq!(
        code(server.call(
            "fux/input.submit",
            json!({ "operation": operation, "keys": "bad\\q" })
        )),
        codes::INVALID
    );
    assert_eq!(
        code(server.call(
            "fux/input.submit",
            json!({ "operation": operation + 1, "keys": "x" })
        )),
        codes::NOT_FOUND
    );
    let submitted = server
        .call(
            "fux/input.submit",
            json!({ "operation": operation, "keys": "echo hi\\n" }),
        )
        .unwrap();
    assert_eq!(submitted["state"], "submitted");
    assert_eq!(submitted["bytes_written"], 8);
    let seq = server.with_world(move |world| world.get::<Terminal>(pane).unwrap().seq());
    assert_eq!(submitted["seq"], seq);
    assert_eq!(drain_writes(&server), vec![(pane, b"echo hi\n".to_vec())]);

    // Identical resubmission: the receipt, no write. Different: conflict.
    let again = server
        .call(
            "fux/input.submit",
            json!({ "operation": operation, "keys": "echo hi\\n" }),
        )
        .unwrap();
    assert_eq!(again, submitted);
    assert!(drain_writes(&server).is_empty());
    assert_eq!(
        code(server.call(
            "fux/input.submit",
            json!({ "operation": operation, "keys": "echo bye\\n" })
        )),
        codes::INVALID
    );
    let status = server
        .call("fux/input.status", json!({ "operation": operation }))
        .unwrap();
    assert_eq!(status, submitted);

    // A narrowed token for another workspace cannot read the receipt.
    let minted = server
        .call(
            "fux/token.mint",
            json!({ "workspace": "default", "capabilities": ["read", "mutate"] }),
        )
        .unwrap();
    let mut narrowed = server.descriptor.clone();
    narrowed.token = minted["token"].as_str().unwrap().to_owned();
    assert_eq!(
        code(client::call_with(
            &narrowed,
            "fux/input.status",
            json!({ "operation": operation })
        )),
        codes::UNAUTHORIZED
    );

    // Exit: the receipt becomes readable evidence, the final record appears.
    let second = server
        .call(
            "fux/input.reserve",
            json!({ "pane": pane_id, "retain_ms": 60_000 }),
        )
        .unwrap()["operation"]
        .as_u64()
        .unwrap();
    server.with_world(move |world| {
        world.resource_mut::<Clock>().now_ms = 20_000;
        world
            .resource_mut::<Messages<Inbound>>()
            .write(Inbound::PaneExited { pane, code: 9 });
    });
    // Two updates: exit handled, pane released.
    server.with_world(|_| ());
    server.with_world(|_| ());
    let uncertain = server
        .call("fux/input.status", json!({ "operation": second }))
        .unwrap();
    assert_eq!(uncertain["state"], "uncertain");
    assert_eq!(uncertain["pane"], Value::Null);
    assert_eq!(
        code(server.call(
            "fux/input.submit",
            json!({ "operation": second, "keys": "x" })
        )),
        codes::INVALID
    );
    let record = server
        .call("fux/pane.final", json!({ "pane": pane_id }))
        .unwrap();
    assert_eq!(record["pane"], pane_id);
    assert_eq!(record["workspace"], "alpha");
    assert_eq!(record["exit_code"], 9);
    assert_eq!(record["exited_ms"], 20_000);

    // Conflict: a stale instance nonce.
    let mut stale = server.descriptor.clone();
    stale.instance = "stale".into();
    assert_eq!(
        code(client::call_with(
            &stale,
            "fux/pane.final",
            json!({ "pane": pane_id })
        )),
        final_codes::CONFLICT
    );
    assert_eq!(
        code(server.call("fux/pane.final", json!({ "pane": pane_id + 1000 }))),
        final_codes::UNKNOWN
    );

    // Expired after retention; the receipt too.
    let retain = Config::default().limits().final_retain_ms;
    server.with_world(move |world| {
        world.resource_mut::<Clock>().now_ms = 20_000 + retain;
    });
    server.with_world(|_| ());
    let expired = handler(&server, "fux/pane.final", json!({ "pane": pane_id })).unwrap_err();
    assert_eq!(expired.code, final_codes::EXPIRED);
    assert_eq!(expired.data, Some(json!({ "reason": "expired" })));
    // `second` was reserved at 10 000 for 60 000: expired at 70 000, forgotten at 130 000.
    server.with_world(move |world| {
        world.resource_mut::<Clock>().now_ms = 80_000;
    });
    server.with_world(|_| ());
    assert_eq!(
        server
            .call("fux/input.status", json!({ "operation": second }))
            .unwrap()["state"],
        "expired"
    );
}
