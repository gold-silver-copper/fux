//! Deterministic ECS tests: injected events and time, no sockets, no processes, no sleeps.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use fux::config::Config;
use fux::ecs::{Effect, Inbound, ManagerAction, ManagerOutcome, Session, ViewerRequest};
use fux::ids::{PaneId, TabId, ViewerId};
use fux::layout::Axis;
use fux::proto::attach::{MouseEvent, ServerMessage};
use fux::proto::control::{
    CommandResult, ErrorCode, Event, FocusTarget, Reply, Request, TabAction, WorkspaceAction,
};
use fux::view::{Frame, PaneView};
use std::collections::BTreeMap;

fn input_request(harness: &mut Harness, workspace: &str, request: Request) -> Reply {
    harness.step(vec![Inbound::ControlRequest {
        workspace: workspace.into(),
        request,
        token: 900,
    }]);
    harness.control.last().expect("input reply").1.clone()
}

fn final_reply(h: &mut Harness, pane: PaneId, instance: &str) -> Reply {
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Final {
            instance: instance.into(),
            pane,
        },
        token: 402,
    }]);
    match &h.manager.last().expect("final reply").1 {
        ManagerOutcome::Final(reply) => reply.clone(),
        other => panic!("unexpected final reply: {other:?}"),
    }
}

#[test]
fn explicit_last_pane_kill_retains_final_identity_and_output() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"BEFORE_KILL".to_vec(),
    }]);
    input_request(
        &mut h,
        "default",
        Request::Kill {
            id: 1,
            instance: None,
            pane: PaneId(1),
        },
    );
    let Reply::Completed {
        result: CommandResult::Final { record },
        ..
    } = final_reply(&mut h, PaneId(1), "test-instance")
    else {
        panic!("explicit kill lost final evidence");
    };
    assert_eq!(record.workspace, "default");
    assert!(record.capture.text.contains("BEFORE_KILL"));
    assert_eq!(
        record.exit_status, None,
        "release before exit report must remain unknown"
    );
    assert_eq!(h.session.entity_counts(), Default::default());
}

#[test]
fn closed_tab_keeps_exit_evidence_then_releases_its_internal_identity() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(
        viewer,
        Request::Tab {
            id: 1,
            instance: None,
            action: TabAction::New { name: None },
        },
    );
    h.complete_spawns();
    h.control(
        viewer,
        Request::Kill {
            id: 2,
            instance: None,
            pane: PaneId(2),
        },
    );
    assert_eq!(h.last_frame(viewer).tabs.len(), 1);
    assert_eq!(
        h.session.entity_counts().tabs,
        1,
        "main retires the tab while the pane retains immutable workspace identity"
    );
    let reply = input_request(
        &mut h,
        "default",
        Request::SendKeys {
            id: 3,
            instance: None,
            pane: PaneId(2),
            keys: "must not arrive".into(),
            notation: Default::default(),
        },
    );
    assert!(matches!(reply, Reply::Failed { error, .. } if error.code == ErrorCode::NotFound));
    let reply = input_request(
        &mut h,
        "default",
        Request::Tab {
            id: 4,
            instance: None,
            action: TabAction::Rename {
                tab: TabId(2),
                name: "closed".into(),
            },
        },
    );
    assert!(matches!(reply, Reply::Failed { error, .. } if error.code == ErrorCode::NotFound));
    h.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(2),
            bytes: b"DRAINED_EXIT".to_vec(),
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 19,
        },
    ]);
    let Reply::Completed {
        result: CommandResult::Final { record },
        ..
    } = final_reply(&mut h, PaneId(2), "test-instance")
    else {
        panic!("closed tab lost exit evidence");
    };
    assert_eq!(record.exit_status, Some(19));
    assert!(record.capture.text.contains("DRAINED_EXIT"));
    assert_eq!(h.session.entity_counts().tabs, 1);
    assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
}

#[test]
fn closing_last_tab_explicitly_retains_orphan_pane_before_manager_idle() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"BEFORE_TAB_CLOSE".to_vec(),
    }]);
    input_request(
        &mut h,
        "default",
        Request::Tab {
            id: 8,
            instance: None,
            action: TabAction::Close { tab: TabId(1) },
        },
    );
    assert_eq!(h.session.entity_counts(), Default::default());
    assert!(!h.idle);
    assert!(h.released.contains(&PaneId(1)));
    let before = final_reply(&mut h, PaneId(1), "test-instance");
    assert!(
        matches!(&before, Reply::Completed { result: CommandResult::Final { record }, .. }
        if record.capture.text.contains("BEFORE_TAB_CLOSE") && record.exit_status.is_none())
    );
    h.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"AFTER_RELEASE".to_vec(),
        },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 23,
        },
    ]);
    assert_eq!(final_reply(&mut h, PaneId(1), "test-instance"), before);
    assert!(!h.idle);
    h.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    h.step(Vec::new());
    assert!(h.idle);
}

#[test]
fn final_evidence_survives_workspace_release_and_expires_explicitly() {
    let mut h = Harness::new();
    h.create_workspace("default");
    assert!(
        matches!(final_reply(&mut h, PaneId(1), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Pending)
    );
    h.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"FINAL_MARKER".to_vec(),
        },
        Inbound::PaneEof { pane: PaneId(1) },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 23,
        },
    ]);
    assert_eq!(h.session.entity_counts(), Default::default());
    assert!(!h.idle);
    let record = match final_reply(&mut h, PaneId(1), "test-instance") {
        Reply::Completed {
            result: CommandResult::Final { record },
            ..
        } => record,
        other => panic!("final evidence: {other:?}"),
    };
    assert!(record.capture.text.contains("FINAL_MARKER"));
    assert_eq!(record.exit_status, Some(23));
    assert_eq!(record.workspace, "default");
    assert!(!record.capture.truncated);
    assert!(
        matches!(final_reply(&mut h, PaneId(1), "replacement"), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    h.create_workspace("default");
    let listing = input_request(
        &mut h,
        "default",
        Request::List {
            id: 1,
            instance: None,
        },
    );
    assert!(
        matches!(listing, Reply::Completed { result: CommandResult::Listing { workspaces, .. }, .. }
        if workspaces[0].event_cursor.stream != record.stream)
    );
    assert!(matches!(
        final_reply(&mut h, PaneId(1), "test-instance"),
        Reply::Completed { .. }
    ));
    h.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    assert!(
        matches!(final_reply(&mut h, PaneId(1), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(
        matches!(final_reply(&mut h, PaneId(999), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
}

#[test]
fn final_record_capacity_evicts_oldest_and_idle_waits_only_until_expiry() {
    let mut h = Harness::new();
    for id in 1..=fux::ecs::resources::MAX_FINAL_RECORDS + 1 {
        h.create_workspace("default");
        h.step(vec![Inbound::PaneExited {
            pane: PaneId(id as u32),
            code: 0,
        }]);
    }
    assert!(
        matches!(final_reply(&mut h, PaneId(1), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(matches!(
        final_reply(&mut h, PaneId(2), "test-instance"),
        Reply::Completed { .. }
    ));
    assert!(!h.idle);
    h.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    h.step(vec![]);
    assert!(h.idle);
}

#[test]
fn forced_release_keeps_unknown_exit_and_final_captures_stay_bounded() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"LAST_OBSERVED".to_vec(),
    }]);
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Kill {
            name: "default".into(),
        },
        token: 10,
    }]);
    match final_reply(&mut h, PaneId(1), "test-instance") {
        Reply::Completed {
            result: CommandResult::Final { record },
            ..
        } => {
            assert_eq!(record.exit_status, None);
            assert!(record.capture.text.contains("LAST_OBSERVED"));
        }
        other => panic!("forced evidence: {other:?}"),
    }
    h.create_workspace("default");
    let viewer = h.attach("default", 512, 512);
    h.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(2),
            bytes: vec![b'x'; 512 * 512],
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 0,
        },
    ]);
    h.step(vec![Inbound::ViewerGone { viewer }]);
    match final_reply(&mut h, PaneId(2), "test-instance") {
        Reply::Completed {
            result: CommandResult::Final { record },
            ..
        } => {
            assert!(record.capture.truncated);
            assert!(record.capture.text.len() <= 131_072);
            assert_eq!(record.exit_status, Some(0));
        }
        other => panic!("bounded evidence: {other:?}"),
    }
}

fn input_receipt(reply: Reply) -> fux::proto::control::InputReceipt {
    match reply {
        Reply::Completed {
            result: CommandResult::Input { receipt },
            ..
        } => receipt,
        other => panic!("expected input receipt, got {other:?}"),
    }
}

#[test]
fn manager_create_rejects_reserved_and_existing_names_without_borrowing() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.step(vec![
        Inbound::Manager {
            action: ManagerAction::Create {
                name: "owned".into(),
            },
            token: 600,
        },
        Inbound::Manager {
            action: ManagerAction::Create {
                name: "owned".into(),
            },
            token: 601,
        },
    ]);
    assert_eq!(h.pending_spawns.len(), 1);
    assert!(
        h.manager
            .iter()
            .any(|(token, result)| *token == 601 && matches!(result, ManagerOutcome::Failed(_)))
    );
    h.complete_spawns();
    assert!(h.manager.iter().any(|(token, result)| *token == 600
        && matches!(
            result,
            ManagerOutcome::Attach {
                created: true,
                stream: 2,
                ..
            }
        )));
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Create {
            name: "owned".into(),
        },
        token: 602,
    }]);
    assert!(matches!(
        h.manager.last(),
        Some((602, ManagerOutcome::Failed(_)))
    ));
    assert!(h.pending_spawns.is_empty());
    assert_eq!(h.session.entity_counts().workspaces, 2);
}

#[test]
fn input_operations_deduplicate_and_report_actual_completion() {
    use fux::proto::control::InputState;
    let mut h = Harness::new();
    h.create_workspace("default");
    let reserved = input_receipt(input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    ));
    assert_eq!(reserved.state, InputState::Reserved);
    assert!(h.written.is_empty());
    let submit = |keys: &str| Request::InputSubmit {
        id: 2,
        instance: Some("test-instance".into()),
        operation: reserved.operation,
        keys: keys.into(),
    };
    let queued = input_receipt(input_request(&mut h, "default", submit("abc")));
    assert_eq!(queued.state, InputState::Queued);
    assert_eq!(queued.input_sequence, reserved.input_sequence + 1);
    assert_eq!(queued.bytes_written, 0);
    assert_eq!(h.written, vec![(PaneId(1), b"abc".to_vec())]);
    assert_eq!(
        input_receipt(input_request(&mut h, "default", submit("abc"))),
        queued
    );
    assert_eq!(h.written.len(), 1, "retry must never type twice");
    assert!(
        matches!(input_request(&mut h, "default", submit("xyz")), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    h.step(vec![Inbound::InputCompleted {
        pane: PaneId(1),
        operation: reserved.operation,
        bytes_written: 3,
        error: None,
    }]);
    let delivered = input_receipt(input_request(&mut h, "default", submit("abc")));
    assert_eq!(delivered.state, InputState::Delivered);
    assert_eq!(delivered.bytes_written, 3);
    assert_eq!(h.written.len(), 1);
    // A delayed duplicate/forged completion cannot overwrite the established result.
    h.step(vec![Inbound::InputCompleted {
        pane: PaneId(2),
        operation: reserved.operation,
        bytes_written: 0,
        error: Some("wrong pane".into()),
    }]);
    assert_eq!(
        input_receipt(input_request(&mut h, "default", submit("abc"))),
        delivered
    );
}

#[test]
fn input_reservation_retention_is_the_callers_under_the_ceiling() {
    use fux::proto::control::MAX_INPUT_RETENTION_MS;
    let mut h = Harness::new();
    h.create_workspace("default");
    let reserve = |retain_ms: u64| Request::InputReserve {
        id: 1,
        instance: Some("test-instance".into()),
        pane: PaneId(1),
        retain_ms,
    };
    let status = |operation: u64| Request::InputStatus {
        id: 4,
        instance: Some("test-instance".into()),
        operation,
    };
    // Zero is refused before anything is reserved.
    assert!(
        matches!(input_request(&mut h, "default", reserve(0)), Reply::Failed { error, .. } if error.code == ErrorCode::InvalidRequest)
    );
    // A short caller value expires exactly when the caller asked.
    let short = input_receipt(input_request(&mut h, "default", reserve(2_500)));
    assert_eq!(short.expires_ms, h.now + 2_500);
    // A value above the ceiling is clamped, visibly in the receipt.
    let clamped = input_receipt(input_request(
        &mut h,
        "default",
        reserve(MAX_INPUT_RETENTION_MS + 1),
    ));
    assert_eq!(clamped.expires_ms, h.now + MAX_INPUT_RETENTION_MS);
    assert_eq!(
        input_receipt(input_request(&mut h, "default", reserve(u64::MAX))).expires_ms,
        h.now + MAX_INPUT_RETENTION_MS
    );
    assert_eq!(h.session.next_deadline_ms(), Some(short.expires_ms));
    // Every harness step advances the clock by 10 ms: land one step before expiry, then on it.
    h.now = short.expires_ms - 20;
    assert!(matches!(
        input_request(&mut h, "default", status(short.operation)),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(input_request(&mut h, "default", status(short.operation)), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(matches!(
        input_request(&mut h, "default", status(clamped.operation)),
        Reply::Completed { .. }
    ));
    // The clamped reservation lives until the ceiling, not until the requested value.
    h.now = clamped.expires_ms - 20;
    assert!(matches!(
        input_request(&mut h, "default", status(clamped.operation)),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(input_request(&mut h, "default", status(clamped.operation)), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
}

#[test]
fn final_record_retention_is_the_launchers_under_the_ceiling() {
    use fux::proto::control::MAX_FINAL_RETENTION_MS;
    let mut h = Harness::new();
    h.create_workspace("default");
    let launch = |id: u64, final_retain_ms: u64| {
        let mut request = split(id, Axis::Horizontal);
        if let Request::Split {
            final_retain_ms: retention,
            ..
        } = &mut request
        {
            *retention = final_retain_ms;
        }
        request
    };
    assert!(
        matches!(input_request(&mut h, "default", launch(1, 0)), Reply::Failed { error, .. } if error.code == ErrorCode::InvalidRequest)
    );
    assert!(
        h.pending_spawns.is_empty(),
        "a refused split spawns nothing"
    );
    input_request(&mut h, "default", launch(2, 3_000));
    h.complete_spawns();
    input_request(&mut h, "default", launch(3, MAX_FINAL_RETENTION_MS + 1));
    h.complete_spawns();
    assert_eq!(h.session.entity_counts().panes, 3);
    h.step(vec![
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 4,
        },
        Inbound::PaneExited {
            pane: PaneId(3),
            code: 5,
        },
    ]);
    // Every harness step (each `final_reply`) advances the clock by 10 ms.
    let closed = h.now;
    assert!(matches!(
        final_reply(&mut h, PaneId(2), "test-instance"),
        Reply::Completed { .. }
    ));
    // The short record expires at the launcher's value.
    h.now = closed + 3_000 - 20;
    assert!(matches!(
        final_reply(&mut h, PaneId(2), "test-instance"),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(final_reply(&mut h, PaneId(2), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(matches!(
        final_reply(&mut h, PaneId(3), "test-instance"),
        Reply::Completed { .. }
    ));
    // The over-ceiling record expires at the ceiling, not later.
    h.now = closed + MAX_FINAL_RETENTION_MS - 20;
    assert!(matches!(
        final_reply(&mut h, PaneId(3), "test-instance"),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(final_reply(&mut h, PaneId(3), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    // fux's own initial pane keeps the configured default.
    h.step(vec![Inbound::PaneExited {
        pane: PaneId(1),
        code: 0,
    }]);
    let closed = h.now;
    h.now = closed + fux::config::DEFAULT_FINAL_RETAIN_MS - 20;
    assert!(matches!(
        final_reply(&mut h, PaneId(1), "test-instance"),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(final_reply(&mut h, PaneId(1), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(h.idle);
}

#[test]
fn info_publishes_the_retention_ceilings() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let reply = input_request(
        &mut h,
        "default",
        Request::Info {
            id: 9,
            instance: None,
        },
    );
    let Reply::Completed {
        result: CommandResult::Info { info },
        ..
    } = reply
    else {
        panic!("info failed: {reply:?}");
    };
    assert_eq!(
        info.limits.input_retention_ms,
        fux::proto::control::MAX_INPUT_RETENTION_MS
    );
    assert_eq!(
        info.limits.final_retention_ms,
        fux::proto::control::MAX_FINAL_RETENTION_MS
    );
}

#[test]
fn terminal_query_replies_do_not_invalidate_controller_input_reservations() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let reserved = input_receipt(input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    ));
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b[6n".to_vec(),
    }]);
    assert!(!h.written.is_empty(), "terminal query must produce a reply");
    let queued = input_receipt(input_request(
        &mut h,
        "default",
        Request::InputSubmit {
            id: 2,
            instance: Some("test-instance".into()),
            operation: reserved.operation,
            keys: "abc".into(),
        },
    ));
    assert_eq!(queued.input_sequence, reserved.input_sequence + 1);
    assert_eq!(h.written.last(), Some(&(PaneId(1), b"abc".to_vec())));
}

#[test]
fn reservations_detect_intervening_input_and_receipts_expire_without_reusing_ids() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let reserve = || Request::InputReserve {
        id: 1,
        instance: Some("test-instance".into()),
        pane: PaneId(1),
        retain_ms: 60_000,
    };
    let first = input_receipt(input_request(&mut h, "default", reserve()));
    input_request(
        &mut h,
        "default",
        Request::SendKeys {
            notation: Default::default(),
            id: 2,
            instance: None,
            pane: PaneId(1),
            keys: "human".into(),
        },
    );
    let submit = Request::InputSubmit {
        id: 3,
        instance: Some("test-instance".into()),
        operation: first.operation,
        keys: "stale".into(),
    };
    assert!(
        matches!(input_request(&mut h, "default", submit.clone()), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert_eq!(h.written.len(), 1);
    let status = || Request::InputStatus {
        id: 4,
        instance: Some("test-instance".into()),
        operation: first.operation,
    };
    assert!(
        matches!(input_request(&mut h, "other", status()), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(h.session.next_deadline_ms().is_some());
    h.now = first.expires_ms;
    h.step(vec![]);
    assert!(
        matches!(input_request(&mut h, "default", submit), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    let next = input_receipt(input_request(&mut h, "default", reserve()));
    assert!(next.operation > first.operation);
    assert!(
        matches!(input_request(&mut h, "default", status()), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
}

#[test]
fn input_receipt_capacity_and_partial_failure_are_explicit() {
    use fux::proto::control::InputState;
    let mut h = Harness::new();
    h.create_workspace("default");
    let reserve = || Request::InputReserve {
        id: 1,
        instance: Some("test-instance".into()),
        pane: PaneId(1),
        retain_ms: 60_000,
    };
    let first = input_receipt(input_request(&mut h, "default", reserve()));
    for _ in 1..fux::ecs::resources::MAX_INPUT_OPERATIONS {
        input_receipt(input_request(&mut h, "default", reserve()));
    }
    assert!(
        matches!(input_request(&mut h, "default", reserve()), Reply::Failed { error, .. } if error.code == ErrorCode::Limit)
    );
    let submit = || Request::InputSubmit {
        id: 2,
        instance: Some("test-instance".into()),
        operation: first.operation,
        keys: "abc".into(),
    };
    input_receipt(input_request(&mut h, "default", submit()));
    h.step(vec![Inbound::InputCompleted {
        pane: PaneId(1),
        operation: first.operation,
        bytes_written: 1,
        error: Some("broken pipe".into()),
    }]);
    let failed = input_receipt(input_request(&mut h, "default", submit()));
    assert_eq!(failed.state, InputState::Failed);
    assert_eq!(failed.bytes_written, 1);
    assert_eq!(h.written.len(), 1, "a partial write must not be retried");
}

/// A fake operating system: records effects and completes spawns on demand.
struct Harness {
    session: Session,
    now: u64,
    pending_spawns: Vec<PaneId>,
    next_pid: u32,
    effects: Vec<Effect>,
    frames: BTreeMap<ViewerId, Vec<Frame>>,
    /// The frame each viewer holds after applying every update it received.
    retained: BTreeMap<ViewerId, Frame>,
    messages: BTreeMap<ViewerId, Vec<ServerMessage>>,
    events: Vec<(String, Event)>,
    released: Vec<PaneId>,
    terminated: Vec<PaneId>,
    written: Vec<(PaneId, Vec<u8>)>,
    manager: Vec<(u64, ManagerOutcome)>,
    control: Vec<(u64, Reply)>,
    opened: Vec<String>,
    closed: Vec<String>,
    idle: bool,
    next_viewer: u64,
}

#[test]
fn server_incarnation_guards_reused_pane_ids_before_mutation_or_capture() {
    let mut first = Harness::new();
    first.session.set_identity(fux::ecs::ServerIdentity {
        instance_nonce: "server-old".into(),
        ..Default::default()
    });
    first.create_workspace("default");
    let mut current = Harness::new();
    current.session.set_identity(fux::ecs::ServerIdentity {
        instance_nonce: "server-new".into(),
        ..Default::default()
    });
    current.create_workspace("default");
    let viewer = current.attach("default", 24, 80);
    for mut value in [
        serde_json::json!({"command":"send-keys","pane":1,"keys":"must-not-arrive"}),
        serde_json::json!({"command":"kill","pane":1}),
        serde_json::json!({"command":"capture","pane":1,"max_bytes":4096}),
    ] {
        value["id"] = 950.into();
        value["instance"] = "server-old".into();
        current.control(viewer, serde_json::from_value(value).unwrap());
        assert!(
            matches!(current.replies(viewer).last(), Some(Reply::Failed { error, .. }) if error.code == ErrorCode::Conflict)
        );
    }
    assert!(current.written.is_empty());
    assert!(current.terminated.is_empty());
    current.control(
        viewer,
        Request::List {
            id: 951,
            instance: Some("server-new".into()),
        },
    );
    assert!(
        matches!(current.replies(viewer).last(), Some(Reply::Completed { result: fux::proto::control::CommandResult::Listing {instance, ..}, .. }) if instance == "server-new")
    );
}

#[test]
fn control_capture_returns_coherent_metadata_and_separate_grid_sequence() {
    use fux::proto::control::CommandResult;
    let mut harness = Harness::new();
    harness.session.set_identity(fux::ecs::ServerIdentity {
        instance_nonce: "capture-server".into(),
        ..Default::default()
    });
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"hello".to_vec(),
    }]);
    let request = |revision: Option<u64>| {
        serde_json::from_value(serde_json::json!({
            "command":"capture", "id":960, "instance":"capture-server", "pane":1,
            "max_bytes":4096, "if_revision":revision
        }))
        .unwrap()
    };
    let capture = |harness: &Harness| match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Capture { seq, capture, .. },
            ..
        }) => (*seq, capture.clone()),
        other => panic!("unexpected capture reply: {other:?}"),
    };
    harness.control(viewer, request(None));
    let (seq, initial) = capture(&harness);
    assert_eq!(initial.text, "hello");
    assert!(!initial.unchanged);
    harness.control(viewer, request(Some(initial.revision)));
    let (_, unchanged) = capture(&harness);
    assert!(unchanged.unchanged);
    assert!(unchanged.text.is_empty());
    harness.events.clear();
    harness.now += 300;
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b]9;4;1;50\x07".to_vec(),
    }]);
    harness.control(viewer, request(Some(initial.revision)));
    let (after_seq, changed) = capture(&harness);
    assert_eq!(seq, after_seq);
    assert!(!changed.unchanged);
    assert_eq!(changed.text, initial.text);
    assert_eq!(changed.progress, Some((1, 50)));
    assert!(
        harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::WorkspaceChanged { .. })),
        "metadata invalidation must be observable without a grid change"
    );

    // Input can advance without terminal output. Even a cached screen must carry
    // the current writer sequence, not a value from an earlier listing/capture.
    harness.control(
        viewer,
        Request::SendKeys {
            notation: Default::default(),
            id: 961,
            instance: Some("capture-server".into()),
            pane: PaneId(1),
            keys: "human".into(),
        },
    );
    harness.control(viewer, request(Some(changed.revision)));
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Completed {
            result: CommandResult::Capture { input_sequence: 1, capture, .. }, ..
        }) if capture.unchanged && capture.revision == changed.revision
    ));
}

#[test]
fn control_capture_cells_shares_text_coherence_and_carries_wide_and_styled_cells() {
    use fux::proto::control::{CaptureLine, CommandResult};
    use fux::view::CellKind;
    let mut harness = Harness::new();
    harness.session.set_identity(fux::ecs::ServerIdentity {
        instance_nonce: "cells-server".into(),
        ..Default::default()
    });
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: "\x1b]0;shell\x07\x1b[1;31m\u{65e5}\x1b[0mhi"
            .as_bytes()
            .to_vec(),
    }]);
    let request = |format: &str, revision: Option<u64>, max_bytes: usize| {
        serde_json::from_value::<Request>(serde_json::json!({
            "command":"capture", "id":965, "instance":"cells-server", "pane":1,
            "max_bytes":max_bytes, "format":format, "if_revision":revision
        }))
        .unwrap()
    };
    let text = |harness: &Harness| match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result:
                CommandResult::Capture {
                    seq,
                    input_sequence,
                    capture,
                },
            ..
        }) => (*seq, *input_sequence, capture.clone()),
        other => panic!("unexpected text capture reply: {other:?}"),
    };
    let cells = |harness: &Harness| match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Cells { .. },
            ..
        }) => harness.replies(viewer).last().cloned().unwrap(),
        other => panic!("unexpected cells capture reply: {other:?}"),
    };
    let fields = |reply: &Reply| match reply {
        Reply::Completed {
            result:
                CommandResult::Cells {
                    seq,
                    input_sequence,
                    revision,
                    rows,
                    columns,
                    cursor,
                    title,
                    progress,
                    unchanged,
                    truncated,
                    lines,
                },
            ..
        } => (
            (*seq, *input_sequence, *revision),
            (*rows, *columns, *cursor, title.clone(), *progress),
            (*unchanged, *truncated, lines.clone()),
        ),
        other => panic!("not a cells reply: {other:?}"),
    };

    // Same step, same borrow contract: identical revision, grid sequence and input sequence.
    harness.control(viewer, request("text", None, 4096));
    let (text_seq, text_input, snapshot) = text(&harness);
    harness.control(viewer, request("cells", None, 65536));
    let (ids, meta, (unchanged, truncated, lines)) = fields(&cells(&harness));
    assert_eq!(ids, (text_seq, text_input, snapshot.revision));
    assert_eq!(
        meta,
        (
            snapshot.rows,
            snapshot.columns,
            fux::view::Cursor {
                row: 0,
                column: 4,
                hidden: false
            },
            snapshot.title.clone(),
            snapshot.progress
        )
    );
    assert_eq!(snapshot.title, "shell");
    assert!(!unchanged && !truncated);
    assert_eq!(lines.len(), usize::from(snapshot.rows));
    let first: &CaptureLine = lines.first().unwrap();
    assert_eq!((first.row, first.wrapped), (0, false));
    let wide = first.cells.first().unwrap();
    assert_eq!(wide.text.as_deref(), Some("\u{65e5}"));
    assert_eq!(wide.kind, Some(CellKind::WideLeading));
    assert!(wide.style.bold);
    assert_eq!(wide.style.foreground, fux::view::Color::Indexed(1));
    let continuation = first.cells.get(1).unwrap();
    assert_eq!(continuation.text, None);
    assert_eq!(continuation.kind, Some(CellKind::WideContinuation));
    let plain = first.cells.get(2).unwrap();
    assert_eq!((plain.text.as_deref(), plain.kind), (Some("h"), None));
    assert!(plain.style.is_default());
    assert_eq!(
        first.cells.get(3).and_then(|c| c.text.as_deref()),
        Some("i")
    );
    let rest = first.cells.get(4).unwrap();
    assert_eq!(
        (rest.text.as_deref(), rest.kind, rest.run),
        (None, None, 76)
    );
    assert_eq!(first.cells.len(), 5);
    let expanded: u32 = first
        .cells
        .iter()
        .map(|cell| u32::from(cell.run.max(1)))
        .sum();
    assert_eq!(expanded, 80);
    assert!(lines.iter().skip(1).all(|line| line.cells.len() == 1));

    // A CJK cell in text form and the cells form describe the same screen.
    assert_eq!(snapshot.text, "\u{65e5}hi");

    // `unchanged` follows `if_revision` exactly like the text form and carries no lines.
    harness.control(viewer, request("cells", Some(snapshot.revision), 65536));
    let (ids, _, (unchanged, truncated, lines)) = fields(&cells(&harness));
    assert_eq!(ids, (text_seq, text_input, snapshot.revision));
    assert!(unchanged && !truncated && lines.is_empty());

    // `max_bytes` bounds the encoded lines: whole trailing rows are dropped and flagged.
    harness.control(viewer, request("cells", None, 64));
    let (_, _, (unchanged, truncated, lines)) = fields(&cells(&harness));
    assert!(!unchanged && truncated);
    assert!(lines.len() < 24);
    let encoded = serde_json::to_vec(&lines).unwrap().len();
    assert!(
        encoded <= 64 + 2,
        "kept {encoded} bytes of lines for a 64 byte bound"
    );

    // Scrollback and attrs are text-form options; the cells form rejects them.
    for extra in [
        serde_json::json!({"scrollback": 1}),
        serde_json::json!({"attrs": true}),
    ] {
        let mut value = serde_json::json!({
            "command":"capture", "id":966, "pane":1, "max_bytes":4096, "format":"cells"
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let request: Request = serde_json::from_value(value).unwrap();
        assert!(request.validate().is_err());
    }
}

#[test]
fn event_replay_orders_typed_and_request_events_and_round_trips() {
    use fux::proto::control::EventCursor;
    let mut h = Harness::new();
    h.create_workspace("default");
    let cursor = match input_request(
        &mut h,
        "default",
        Request::List {
            id: 970,
            instance: Some("test-instance".into()),
        },
    ) {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => workspaces[0].event_cursor,
        other => panic!("listing: {other:?}"),
    };
    let effects = h.step(vec![
        Inbound::ControlRequest {
            workspace: "default".into(),
            request: Request::Tab {
                instance: Some("test-instance".into()),
                id: 972,
                action: fux::proto::control::TabAction::New {
                    name: Some("replayed-tab".into()),
                },
            },
            token: 972,
        },
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"\x1b]2;replayed-title\x07".to_vec(),
        },
    ]);
    // The tab's pane is created behind a spawn barrier; its events publish on completion.
    let mut effects = effects;
    effects.extend(h.complete_spawns());
    let published: Vec<_> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Event { cursor, event, .. } => Some((*cursor, event.clone())),
            _ => None,
        })
        .collect();
    let request = |after: EventCursor| Request::Events {
        id: 971,
        instance: Some("test-instance".into()),
        after,
    };
    let reply = input_request(&mut h, "default", request(cursor));
    assert_eq!(
        serde_json::from_slice::<Reply>(&serde_json::to_vec(&reply).unwrap()).unwrap(),
        reply
    );
    let Reply::Completed {
        result: CommandResult::Events {
            cursor: latest,
            events,
        },
        ..
    } = reply
    else {
        panic!("replay failed")
    };
    assert_eq!(
        events
            .iter()
            .map(|entry| (entry.cursor, entry.event.clone()))
            .collect::<Vec<_>>(),
        published
    );
    assert!(!events.is_empty(), "the tab request published events");
    assert!(events.iter().any(
        |entry| matches!(&entry.event, Event::TabOpened { name, .. } if name == "replayed-tab")
    ));
    // Title changes are metadata: they advance the output sequence, not the event log.
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("replayed-title")
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].cursor.sequence == pair[0].cursor.sequence + 1)
    );
    assert!(
        events
            .iter()
            .all(|entry| entry.cursor.stream == cursor.stream)
    );
    assert!(
        matches!(input_request(&mut h, "default", request(latest)), Reply::Completed {
        result: CommandResult::Events { events, .. }, .. } if events.is_empty())
    );
    assert!(
        matches!(input_request(&mut h, "default", request(EventCursor { sequence: latest.sequence + 1, ..latest })),
        Reply::Failed { error, .. } if error.code == ErrorCode::Gap)
    );
}

#[test]
fn recreated_workspace_rejects_old_stream_and_ignores_delayed_pane_output() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let listing_cursor = |h: &mut Harness| match input_request(
        h,
        "default",
        Request::List {
            id: 980,
            instance: Some("test-instance".into()),
        },
    ) {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => workspaces[0].event_cursor,
        other => panic!("listing: {other:?}"),
    };
    let old = listing_cursor(&mut h);
    h.step(vec![Inbound::PaneExited {
        pane: PaneId(1),
        code: 0,
    }]);
    h.now += 10_000;
    h.step(Vec::new());
    assert!(
        h.session
            .world()
            .resource::<fux::ecs::Ids>()
            .workspace("default")
            .is_none()
    );
    h.create_workspace("default");
    let new = listing_cursor(&mut h);
    assert_ne!(old.stream, new.stream);
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b]2;old-output\x07".to_vec(),
    }]);
    assert_eq!(listing_cursor(&mut h), new);
    assert!(matches!(input_request(&mut h, "default", Request::Events {
        id: 981, instance: Some("test-instance".into()), after: old,
    }), Reply::Failed { error, .. } if error.code == ErrorCode::Gap));
    assert!(
        matches!(input_request(&mut h, "default", Request::Workspace {
        id: 983, instance: Some("test-instance".into()), stream: Some(old.stream),
        action: WorkspaceAction::Kill { name: "default".into() },
    }), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert_eq!(
        listing_cursor(&mut h),
        new,
        "old run cleanup must not kill replacement"
    );
    let mut launch = split(982, Axis::Horizontal);
    if let Request::Split {
        instance, stream, ..
    } = &mut launch
    {
        *instance = Some("test-instance".into());
        *stream = Some(old.stream);
    }
    assert!(
        matches!(input_request(&mut h, "default", launch), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert!(h.pending_spawns.is_empty());
    assert_eq!(listing_cursor(&mut h), new);
}

impl Harness {
    fn new() -> Self {
        let config = Config::from_toml("default-command = { argv = [\"/bin/sh\"] }").unwrap();
        Self {
            session: {
                let mut session = Session::new(&config).unwrap();
                session.set_identity(fux::ecs::ServerIdentity {
                    instance_nonce: "test-instance".into(),
                    ..Default::default()
                });
                session
            },
            now: 1_000,
            pending_spawns: Vec::new(),
            next_pid: 100,
            effects: Vec::new(),
            frames: BTreeMap::new(),
            retained: BTreeMap::new(),
            messages: BTreeMap::new(),
            events: Vec::new(),
            released: Vec::new(),
            terminated: Vec::new(),
            written: Vec::new(),
            manager: Vec::new(),
            control: Vec::new(),
            opened: Vec::new(),
            closed: Vec::new(),
            idle: false,
            next_viewer: 1,
        }
    }

    fn step(&mut self, inbound: Vec<Inbound>) -> Vec<Effect> {
        self.now += 10;
        let effects = self.session.step(self.now, inbound);
        assert_eq!(self.session.retained_messages(), 0, "messages retained");
        self.session
            .check_invariants()
            .unwrap_or_else(|error| panic!("invariant violated: {error}"));
        for effect in &effects {
            match effect {
                Effect::SpawnPane { pane, .. } => self.pending_spawns.push(*pane),
                Effect::ToViewer { viewer, message } => {
                    if let ServerMessage::State { state } = message {
                        let current = self.retained.entry(*viewer).or_default();
                        current
                            .apply((**state).clone())
                            .unwrap_or_else(|error| panic!("invalid update: {error}"));
                        self.frames
                            .entry(*viewer)
                            .or_default()
                            .push(current.clone());
                    }
                    self.messages
                        .entry(*viewer)
                        .or_default()
                        .push(message.clone());
                }
                Effect::Event {
                    workspace, event, ..
                } => {
                    self.events.push((workspace.clone(), event.clone()));
                }
                Effect::ReleasePane { pane } => self.released.push(*pane),
                Effect::Terminate { pane, .. } => self.terminated.push(*pane),
                Effect::WriteTrackedInput { pane, bytes, .. }
                | Effect::WriteInput { pane, bytes } => self.written.push((*pane, bytes.clone())),
                Effect::Manager { token, outcome } => self.manager.push((*token, outcome.clone())),
                Effect::ControlReply { token, reply } => self.control.push((*token, reply.clone())),
                Effect::WorkspaceOpened { name, .. } => self.opened.push(name.clone()),
                Effect::WorkspaceClosed { name } => self.closed.push(name.clone()),
                Effect::Idle => self.idle = true,
                Effect::ResizePty { .. } | Effect::CloseViewer { .. } => {}
            }
        }
        self.effects.extend(effects.iter().cloned());
        effects
    }

    /// Completes every pending spawn successfully and runs the completion step.
    fn complete_spawns(&mut self) -> Vec<Effect> {
        let pending: Vec<PaneId> = std::mem::take(&mut self.pending_spawns);
        let inbound = pending
            .into_iter()
            .map(|pane| {
                self.next_pid += 1;
                Inbound::SpawnCompleted {
                    pane,
                    result: Ok(self.next_pid),
                }
            })
            .collect();
        self.step(inbound)
    }

    fn fail_spawns(&mut self) -> Vec<Effect> {
        let pending: Vec<PaneId> = std::mem::take(&mut self.pending_spawns);
        let inbound = pending
            .into_iter()
            .map(|pane| Inbound::SpawnCompleted {
                pane,
                result: Err("exec failed".into()),
            })
            .collect();
        self.step(inbound)
    }

    fn create_workspace(&mut self, name: &str) {
        self.step(vec![Inbound::Manager {
            action: ManagerAction::Resolve {
                name: Some(name.into()),
            },
            token: 7,
        }]);
        assert_eq!(self.pending_spawns.len(), 1, "initial pane spawn requested");
        self.complete_spawns();
        assert!(self.opened.contains(&name.to_owned()));
        assert!(matches!(
            self.manager.last(),
            Some((7, ManagerOutcome::Attach { created: true, .. }))
        ));
        self.manager.clear();
    }

    fn attach(&mut self, workspace: &str, rows: u16, cols: u16) -> ViewerId {
        let viewer = ViewerId(self.next_viewer);
        self.next_viewer += 1;
        self.step(vec![Inbound::ViewerAttached {
            viewer,
            workspace: workspace.into(),
            rows,
            cols,
        }]);
        assert!(
            self.frames
                .get(&viewer)
                .is_some_and(|frames| !frames.is_empty())
        );
        viewer
    }

    fn request(&mut self, viewer: ViewerId, request: ViewerRequest) -> Vec<Effect> {
        self.step(vec![Inbound::ViewerRequest { viewer, request }])
    }

    fn control(&mut self, viewer: ViewerId, request: Request) -> Vec<Effect> {
        self.request(viewer, ViewerRequest::Control(request))
    }

    fn last_frame(&self, viewer: ViewerId) -> &Frame {
        self.frames[&viewer].last().expect("a frame")
    }

    fn replies(&self, viewer: ViewerId) -> Vec<Reply> {
        self.messages
            .get(&viewer)
            .into_iter()
            .flatten()
            .filter_map(|message| match message {
                ServerMessage::Reply { reply } => Some(reply.clone()),
                _ => None,
            })
            .collect()
    }

    fn pane_ids(&self, viewer: ViewerId) -> Vec<PaneId> {
        self.last_frame(viewer)
            .layout
            .iter()
            .map(|entry| entry.pane)
            .collect()
    }
}

fn split(id: u64, axis: Axis) -> Request {
    Request::Split {
        stream: None,
        instance: None,
        id,
        axis,
        target: None,
        cwd: None,
        argv: Vec::new(),
        env: Vec::new(),
        rows: None,
        columns: None,
        final_retain_ms: fux::config::DEFAULT_FINAL_RETAIN_MS,
    }
}

#[test]
fn fresh_workspace_has_one_tab_and_pane_below_the_bar() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.workspace, "default");
    assert_eq!(frame.tabs.len(), 1);
    assert_eq!(frame.tabs[0].label, "main");
    assert_eq!(frame.layout.len(), 1);
    assert_eq!(
        frame.layout[0].rect.y, 0,
        "panes start at row 0; the bar is the last row"
    );
    assert_eq!(frame.layout[0].rect.height, 23);
    assert_eq!(frame.focused, Some(PaneId(1)));
    assert_eq!(frame.panes[&PaneId(1)].rows, 23);
    assert_eq!(frame.panes[&PaneId(1)].columns, 80);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneOpened {
            pane: PaneId(1),
            ..
        }
    )));
}

#[test]
fn split_focus_and_following_input_reach_the_new_pane_only_after_creation() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    // One read: split, then input meant for the new pane.
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(split(1, Axis::Horizontal)),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"ls\n".to_vec()),
        },
    ]);
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    assert!(harness.written.is_empty(), "input waited for the barrier");
    assert!(
        harness.replies(viewer).is_empty(),
        "no reply before the spawn completes"
    );
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)], "no phantom pane");
    harness.complete_spawns();
    assert_eq!(harness.written, vec![(PaneId(2), b"ls\n".to_vec())]);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.focused, Some(PaneId(2)));
    assert_eq!(frame.layout.len(), 2);
    let widths: Vec<u16> = frame.layout.iter().map(|entry| entry.rect.width).collect();
    assert_eq!(
        widths,
        vec![39, 40],
        "one cell between siblings carries the separator"
    );
    let replies = harness.replies(viewer);
    assert!(matches!(
        replies.as_slice(),
        [Reply::Completed {
            id: 1,
            result: CommandResult::Pane { pane: PaneId(2) }
        }]
    ));
    // The frame that shows the new pane was published before the reply.
    let messages = &harness.messages[&viewer];
    let state_index = messages
        .iter()
        .rposition(
            |message| matches!(message, ServerMessage::State { state } if state.layout.len() == 2),
        )
        .unwrap();
    let reply_index = messages
        .iter()
        .position(|message| matches!(message, ServerMessage::Reply { .. }))
        .unwrap();
    assert!(state_index < reply_index);
}

#[test]
fn failed_creation_rolls_back_and_releases_the_barrier() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(split(5, Axis::Vertical)),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"x".to_vec()),
        },
    ]);
    harness.fail_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert_eq!(harness.released, vec![PaneId(2)]);
    assert!(matches!(
        harness.replies(viewer).as_slice(),
        [Reply::Failed { id: 5, .. }]
    ));
    // Input after the failed split reaches the still-focused original pane.
    assert_eq!(harness.written, vec![(PaneId(1), b"x".to_vec())]);
    assert_eq!(harness.session.entity_counts().panes, 1);
    // Ids are never reused.
    harness.control(viewer, split(6, Axis::Vertical));
    assert_eq!(harness.pending_spawns, vec![PaneId(3)]);
}

#[test]
fn stale_targets_fail_without_hitting_replacements() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    // Confirmed close of pane 2 (captured target), then a replacement pane 3.
    harness.control(
        viewer,
        Request::Kill {
            instance: None,
            id: 2,
            pane: PaneId(2),
        },
    );
    assert_eq!(harness.terminated, vec![PaneId(2)]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(2),
        code: 0,
    }]);
    assert!(harness.released.contains(&PaneId(2)));
    harness.control(viewer, split(3, Axis::Horizontal));
    harness.complete_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1), PaneId(3)]);
    // A late confirmation naming the closed pane fails and pane 3 survives.
    harness.control(
        viewer,
        Request::Kill {
            instance: None,
            id: 4,
            pane: PaneId(2),
        },
    );
    let replies = harness.replies(viewer);
    assert!(matches!(
        replies.last(),
        Some(Reply::Failed { id: 4, error }) if error.code == ErrorCode::NotFound
    ));
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1), PaneId(3)]);
    // Late process reports for a released pane are ignored.
    harness.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(2),
            bytes: b"ghost".to_vec(),
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 9,
        },
    ]);
    assert_eq!(harness.session.entity_counts().panes, 2);
    // Rename by stale tab id fails too.
    harness.control(
        viewer,
        Request::Tab {
            instance: None,
            id: 8,
            action: TabAction::Rename {
                tab: TabId(99),
                name: "x".into(),
            },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 8, .. })
    ));
}

#[test]
fn output_eof_and_exit_keep_final_output_and_retire_the_workspace() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"FINAL".to_vec(),
        },
        Inbound::PaneEof { pane: PaneId(1) },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 29,
        },
    ]);
    let messages = &harness.messages[&viewer];
    let last_frame = harness.last_frame(viewer);
    let text: String = last_frame.panes[&PaneId(1)]
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(
        text.contains("FINAL"),
        "final output painted before retirement"
    );
    assert_eq!(last_frame.exit_code, Some(29));
    assert_eq!(last_frame.panes[&PaneId(1)].exit, Some(29));
    assert!(matches!(
        messages.last(),
        Some(ServerMessage::Exited { code: Some(29) })
    ));
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(1),
            exit_status: Some(29),
            ..
        }
    )));
    // The viewer connection closes; retirement completes once the viewer is gone.
    assert!(!harness.closed.contains(&"default".to_owned()));
    harness.step(vec![Inbound::ViewerGone { viewer }]);
    assert_eq!(harness.closed, vec!["default".to_owned()]);
    assert!(harness.released.contains(&PaneId(1)));
    assert!(!harness.idle, "final evidence keeps the manager alive");
    assert_eq!(harness.session.entity_counts(), Default::default());
    harness.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    harness.step(Vec::new());
    assert!(harness.idle);
}

#[test]
fn retirement_grace_expires_without_viewer_acknowledgement() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let _viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(1),
        code: 3,
    }]);
    assert!(harness.closed.is_empty());
    let deadline = harness
        .session
        .next_deadline_ms()
        .expect("retirement deadline");
    assert!(deadline > harness.now);
    harness.now = deadline;
    harness.step(Vec::new());
    assert_eq!(harness.closed, vec!["default".to_owned()]);
    assert!(!harness.idle);
    harness.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    harness.step(Vec::new());
    assert!(harness.idle);
}

#[test]
fn natural_exit_of_one_pane_closes_it_and_of_a_tab_moves_viewers() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(2),
        code: 0,
    }]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert_eq!(harness.last_frame(viewer).focused, Some(PaneId(1)));
    assert!(harness.released.contains(&PaneId(2)));
    // A second tab whose only pane exits closes the tab; the viewer returns to the first tab.
    harness.control(
        viewer,
        Request::Tab {
            instance: None,
            id: 2,
            action: TabAction::New { name: None },
        },
    );
    harness.complete_spawns();
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.tabs.len(), 2);
    assert_eq!(frame.active_tab, Some(TabId(2)));
    assert_eq!(frame.tabs[1].label, "tab-2");
    assert_eq!(frame.layout[0].rect.y, 0, "the bar reserves the last row");
    assert_eq!(frame.layout[0].rect.height, 23);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(3),
        code: 0,
    }]);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.tabs.len(), 1);
    assert_eq!(frame.active_tab, Some(TabId(1)));
    assert_eq!(frame.focused, Some(PaneId(1)));
    assert!(
        harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::TabClosed { tab: TabId(2), .. }))
    );
    assert!(harness.closed.is_empty(), "workspace survives");
}

#[test]
fn viewers_keep_private_tabs_and_focus_while_sharing_layout_edits() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let alice = harness.attach("default", 24, 80);
    let bob = harness.attach("default", 30, 100);
    harness.control(alice, split(1, Axis::Horizontal));
    harness.complete_spawns();
    // Bob sees the split but keeps his focus on pane 1.
    assert_eq!(harness.pane_ids(bob), vec![PaneId(1), PaneId(2)]);
    assert_eq!(harness.last_frame(bob).focused, Some(PaneId(1)));
    assert_eq!(harness.last_frame(alice).focused, Some(PaneId(2)));
    // Geometry is negotiated over the smallest viewer.
    assert_eq!(harness.last_frame(bob).layout[0].rect.height, 23);
    // Alice opens a tab; Bob stays on the first tab.
    harness.control(
        alice,
        Request::Tab {
            instance: None,
            id: 2,
            action: TabAction::New {
                name: Some("work".into()),
            },
        },
    );
    harness.complete_spawns();
    assert_eq!(harness.last_frame(alice).active_tab, Some(TabId(2)));
    assert_eq!(harness.last_frame(bob).active_tab, Some(TabId(1)));
    assert_eq!(harness.last_frame(bob).tabs.len(), 2);
    // Bob's input goes to his own focus.
    harness.request(bob, ViewerRequest::Input(b"b".to_vec()));
    assert_eq!(harness.written.last(), Some(&(PaneId(1), b"b".to_vec())));
    harness.request(alice, ViewerRequest::Input(b"a".to_vec()));
    assert_eq!(harness.written.last(), Some(&(PaneId(3), b"a".to_vec())));
    // Directional focus for Bob within tab one.
    harness.control(
        bob,
        Request::Focus {
            instance: None,
            id: 9,
            target: FocusTarget::Right,
        },
    );
    assert_eq!(harness.last_frame(bob).focused, Some(PaneId(2)));
    assert_eq!(harness.last_frame(alice).focused, Some(PaneId(3)));
}

#[test]
fn detach_applies_preceding_input_and_drops_the_suffix() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"BEFORE".to_vec()),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Detach,
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(Request::Tab {
                instance: None,
                id: 1,
                action: TabAction::New { name: None },
            }),
        },
    ]);
    assert_eq!(harness.written, vec![(PaneId(1), b"BEFORE".to_vec())]);
    assert!(
        harness.pending_spawns.is_empty(),
        "trailing command ignored"
    );
    assert!(matches!(
        harness.messages[&viewer].last(),
        Some(ServerMessage::Exited { code: None })
    ));
    assert_eq!(harness.session.entity_counts().viewers, 0);
    assert_eq!(
        harness.session.entity_counts().panes,
        1,
        "panes survive viewer loss"
    );
}

#[test]
fn workspace_switch_sends_the_suffix_to_the_destination() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("other");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(Request::Workspace {
                stream: None,
                instance: None,
                id: 1,
                action: WorkspaceAction::Select {
                    name: "other".into(),
                },
            }),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"there".to_vec()),
        },
    ]);
    assert_eq!(harness.last_frame(viewer).workspace, "other");
    assert_eq!(harness.written, vec![(PaneId(2), b"there".to_vec())]);
    // The no-name attach rule now prefers the most recently attached workspace.
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Resolve { name: None },
        token: 11,
    }]);
    assert!(matches!(
        harness.manager.last(),
        Some((11, ManagerOutcome::Attach { name, created: false, .. })) if name == "other"
    ));
}

fn pane_seq(harness: &mut Harness, viewer: ViewerId, pane: PaneId) -> u64 {
    harness.control(
        viewer,
        Request::List {
            instance: None,
            id: 900,
        },
    );
    match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        }) => workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|summary| summary.id == pane)
            .map(|summary| summary.seq)
            .expect("pane listed"),
        other => panic!("unexpected listing reply {other:?}"),
    }
}

/// A cells capture of the whole visible grid: the sequence it reflects and its lines.
fn cells_capture(
    harness: &mut Harness,
    viewer: ViewerId,
    pane: PaneId,
) -> (u64, Vec<fux::proto::control::CaptureLine>) {
    harness.control(
        viewer,
        Request::Capture {
            if_revision: None,
            instance: None,
            id: 901,
            pane,
            attrs: false,
            scrollback: 0,
            max_bytes: 65536,
            format: fux::proto::control::CaptureFormat::Cells,
        },
    );
    match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Cells { seq, lines, .. },
            ..
        }) => (*seq, lines.clone()),
        other => panic!("unexpected capture reply {other:?}"),
    }
}

/// The text a captured line carries, blanks as spaces and trailing blanks trimmed.
fn line_text(line: &fux::proto::control::CaptureLine) -> String {
    let mut text = String::new();
    for cell in &line.cells {
        match (&cell.text, cell.kind) {
            (_, Some(fux::view::CellKind::WideContinuation)) => {}
            (Some(t), _) => text.push_str(t),
            (None, _) => text.extend(std::iter::repeat_n(' ', usize::from(cell.run.max(1)))),
        }
    }
    text.trim_end_matches(' ').to_owned()
}

#[test]
fn split_carries_env_and_a_requested_headless_size_to_the_spawn() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    // No viewer: a headless workspace. Split with an explicit size and environment.
    harness.step(vec![Inbound::ControlRequest {
        workspace: "default".into(),
        request: Request::Split {
            stream: None,
            instance: None,
            id: 5,
            axis: Axis::Horizontal,
            target: Some(PaneId(1)),
            cwd: None,
            argv: vec!["/bin/sh".into()],
            env: vec![("AGENT".into(), "1".into())],
            rows: Some(30),
            columns: Some(100),
            final_retain_ms: fux::config::DEFAULT_FINAL_RETAIN_MS,
        },
        token: 5,
    }]);
    let spawn = harness
        .effects
        .iter()
        .rev()
        .find_map(|effect| match effect {
            Effect::SpawnPane {
                pane,
                env,
                rows,
                cols,
                ..
            } if *pane == PaneId(2) => Some((env.clone(), *rows, *cols)),
            _ => None,
        })
        .expect("a spawn for the new pane");
    assert_eq!(spawn.0, vec![("AGENT".to_owned(), "1".to_owned())]);
    assert_eq!((spawn.1, spawn.2), (30, 100), "the requested headless size");
}

#[test]
fn workspace_snapshot_changes_are_replayable_without_terminal_output() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let snapshot = input_request(
        &mut h,
        "default",
        Request::List {
            id: 2,
            instance: None,
        },
    );
    let cursor = match snapshot {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => workspaces[0].event_cursor,
        other => panic!("listing: {other:?}"),
    };
    for request in [
        Request::Resize {
            id: 3,
            instance: None,
            pane: PaneId(1),
            delta: 100,
        },
        Request::Focus {
            id: 4,
            instance: None,
            target: FocusTarget::Pane(PaneId(1)),
        },
        Request::Tab {
            id: 5,
            instance: None,
            action: TabAction::Rename {
                tab: TabId(1),
                name: "renamed".into(),
            },
        },
    ] {
        h.events.clear();
        let reply = input_request(&mut h, "default", request.clone());
        assert!(
            matches!(reply, Reply::Completed { .. }),
            "{request:?}: {reply:?}"
        );
        assert!(
            h.events
                .iter()
                .any(|(_, event)| matches!(event, Event::WorkspaceChanged { .. })),
            "missing invalidation for {request:?}: {:?}",
            h.events
        );
    }
    let replay = input_request(
        &mut h,
        "default",
        Request::Events {
            id: 6,
            instance: Some("test-instance".into()),
            after: cursor,
        },
    );
    assert!(
        matches!(replay, Reply::Completed { result: CommandResult::Events { events, .. }, .. } if events.len() >= 3)
    );
    h.events.clear();
    h.request(
        viewer,
        ViewerRequest::Resize {
            rows: 30,
            cols: 120,
        },
    );
    assert!(
        h.events
            .iter()
            .any(|(_, event)| matches!(event, Event::WorkspaceChanged { .. }))
    );
    h.events.clear();
    h.request(
        viewer,
        ViewerRequest::Resize {
            rows: 30,
            cols: 120,
        },
    );
    assert!(
        h.events.is_empty(),
        "unchanged geometry needs no invalidation"
    );
}

#[test]
fn metadata_only_output_is_coalesced_and_publishes_a_final_invalidation() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.now = 1000;
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"fixed".to_vec(),
    }]);
    h.events.clear();
    h.now = 1300;
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b]9;4;1;10\x07".to_vec(),
    }]);
    assert_eq!(h.events.len(), 1);
    assert!(matches!(h.events[0].1, Event::WorkspaceChanged { .. }));
    h.events.clear();
    for percent in [20, 30] {
        h.step(vec![Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: format!("\x1b]9;4;1;{percent}\x07").into_bytes(),
        }]);
    }
    assert!(h.events.is_empty(), "metadata notifications must be paced");
    assert!(
        h.session
            .next_deadline_ms()
            .is_some_and(|deadline| deadline <= 1560)
    );
    h.now = 1600;
    h.step(vec![]);
    assert_eq!(h.events.len(), 1);
    assert!(matches!(h.events[0].1, Event::WorkspaceChanged { .. }));
    h.events.clear();
    h.now = 2000;
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: vec![],
    }]);
    assert!(
        h.events.is_empty(),
        "idle/empty input owes no periodic invalidation"
    );
    // Scroll content into history, then restore the exact live grid and cursor.
    h.now = 2300;
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: format!("{}\x1b[2J\x1b[Hfixed", "history\r\n".repeat(30)).into_bytes(),
    }]);
    assert_eq!(h.events.len(), 1);
    assert!(
        matches!(h.events[0].1, Event::WorkspaceChanged { .. }),
        "history-only capture changes need invalidation without a new grid sequence"
    );
}

#[test]
fn output_sequences_are_reported_by_list_capture_and_paced_events() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    let before = pane_seq(&mut harness, viewer, PaneId(1));
    harness.events.clear();
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"hello".to_vec(),
    }]);
    let after = pane_seq(&mut harness, viewer, PaneId(1));
    assert_eq!(
        after,
        before + 1,
        "one visible change advances the sequence once"
    );
    assert!(
        harness.events.iter().any(|(_, event)| matches!(
            event,
            Event::PaneOutput { pane: PaneId(1), seq, .. } if *seq == after
        )),
        "the output event carries the new sequence: {:?}",
        harness.events
    );
    // The cells capture reports the same sequence and the whole visible grid.
    let (seq, lines) = cells_capture(&mut harness, viewer, PaneId(1));
    assert_eq!(seq, after);
    assert_eq!(lines.len(), 23, "every visible row");
    assert_eq!(lines[0].row, 0);
    assert_eq!(line_text(&lines[0]), "hello");
    assert!(lines[1..].iter().all(|line| line_text(line).is_empty()));
    // The text capture reports the same sequence.
    harness.control(
        viewer,
        Request::Capture {
            if_revision: None,
            instance: None,
            id: 902,
            pane: PaneId(1),
            attrs: false,
            scrollback: 0,
            max_bytes: 4096,
            format: fux::proto::control::CaptureFormat::Text,
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Completed { result: CommandResult::Capture { seq, .. }, .. }) if *seq == after
    ));
    // A byte that changes nothing observable (a bell) fires no pane.output event.
    harness.events.clear();
    let quiet_seq = pane_seq(&mut harness, viewer, PaneId(1));
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x07".to_vec(),
    }]);
    assert_eq!(
        pane_seq(&mut harness, viewer, PaneId(1)),
        quiet_seq,
        "a bell advances no sequence"
    );
    assert!(
        !harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::PaneOutput { .. })),
        "a bell fires no output event"
    );
    // A second change inside the event interval produces no event yet but proposes a deadline.
    harness.events.clear();
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b" world".to_vec(),
    }]);
    assert!(
        !harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::PaneOutput { .. })),
        "events are paced"
    );
    let deadline = harness.session.next_deadline_ms().expect("event deadline");
    assert!(
        deadline <= harness.now + 250,
        "{deadline} vs {}",
        harness.now
    );
    harness.now = deadline;
    harness.step(Vec::new());
    let latest = pane_seq(&mut harness, viewer, PaneId(1));
    assert!(
        harness.events.iter().any(|(_, event)| matches!(
            event,
            Event::PaneOutput { pane: PaneId(1), seq, .. } if *seq == latest
        )),
        "the paced event fires at the deadline with the current sequence"
    );
    // A pane no viewer shows still advances and still reports its output.
    harness.control(
        viewer,
        Request::Tab {
            instance: None,
            id: 903,
            action: TabAction::New { name: None },
        },
    );
    harness.complete_spawns();
    assert_eq!(harness.last_frame(viewer).layout[0].pane, PaneId(2));
    harness.now += 1_000;
    harness.events.clear();
    let hidden_before = pane_seq(&mut harness, viewer, PaneId(1));
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\r\nhidden".to_vec(),
    }]);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneOutput { pane: PaneId(1), seq, .. } if *seq == hidden_before + 1
    )));
    let (seq, lines) = cells_capture(&mut harness, viewer, PaneId(1));
    assert_eq!(seq, hidden_before + 1);
    assert_eq!(lines[1].row, 1);
    assert_eq!(line_text(&lines[1]), "hidden");
    // `info` names the workspace, the crate version and the limits.
    harness.control(
        viewer,
        Request::Info {
            instance: None,
            id: 904,
        },
    );
    match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Info { info },
            ..
        }) => {
            assert_eq!(info.workspace.as_deref(), Some("default"));
            assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
            assert_eq!(
                info.limits.capture_bytes,
                fux::proto::control::MAX_CAPTURE_BYTES
            );
            assert_eq!(info.limits.key_bytes, fux::proto::control::MAX_KEY_BYTES);
            assert_eq!(
                info.limits.frame_bytes,
                fux::proto::control::MAX_FRAME_BYTES
            );
            assert_eq!(info.limits.scrollback_lines, 10_000);
        }
        other => panic!("unexpected info reply {other:?}"),
    }
}

#[test]
fn history_views_are_private_and_clamped() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let alice = harness.attach("default", 8, 40);
    let bob = harness.attach("default", 8, 40);
    let mut output = Vec::new();
    for line in 0..30 {
        output.extend_from_slice(format!("line{line}\r\n").as_bytes());
    }
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: output,
    }]);
    let before_bob = harness.frames[&bob].len();
    harness.request(
        alice,
        ViewerRequest::View {
            request: 4,
            pane: PaneId(1),
            offset: 1_000,
        },
    );
    let view = harness.messages[&alice]
        .iter()
        .find_map(|message| match message {
            ServerMessage::View { reply } if reply.request == 4 => Some(reply.clone()),
            _ => None,
        })
        .expect("view reply");
    let view_pane = PaneView::from_update(&view.view.expect("pane exists")).expect("valid view");
    assert!(view_pane.offset > 0 && view_pane.offset <= view.history);
    let text: String = view_pane
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(text.contains("line0"), "{text}");
    assert_eq!(
        harness.frames[&bob].len(),
        before_bob,
        "reading history did not repaint Bob"
    );
    // A missing pane yields no view.
    harness.request(
        alice,
        ViewerRequest::View {
            request: 5,
            pane: PaneId(77),
            offset: 0,
        },
    );
    assert!(harness.messages[&alice].iter().any(|message| matches!(
        message,
        ServerMessage::View { reply } if reply.request == 5 && reply.view.is_none()
    )));
}

#[test]
fn control_requests_and_mouse_hit_tests_respect_stale_generations() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::ControlRequest {
        workspace: "default".into(),
        request: Request::List {
            instance: None,
            id: 3,
        },
        token: 42,
    }]);
    let (token, reply) = harness.control.last().cloned().unwrap();
    assert_eq!(token, 42);
    match reply {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => {
            assert_eq!(workspaces[0].name, "default");
            assert_eq!(workspaces[0].viewers, 1);
            assert_eq!(workspaces[0].tabs[0].panes[0].id, PaneId(1));
            assert_eq!(workspaces[0].tabs[0].panes[0].pid, Some(101));
            assert_eq!(workspaces[0].tabs[0].panes[0].geometry.width, 80);
        }
        other => panic!("unexpected reply {other:?}"),
    }
    // Enable SGR mouse in the pane and click inside it with the current generation.
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b[?1003h\x1b[?1006h".to_vec(),
    }]);
    let generation = harness.last_frame(viewer).generation;
    let click = MouseEvent {
        code: 0,
        column: 5,
        row: 4,
        release: false,
    };
    harness.request(
        viewer,
        ViewerRequest::Mouse {
            event: click,
            generation: generation.wrapping_sub(1),
        },
    );
    assert!(harness.written.is_empty(), "stale generation ignored");
    harness.request(
        viewer,
        ViewerRequest::Mouse {
            event: click,
            generation,
        },
    );
    // Pane-relative coordinates start at the leaf rectangle itself (no frame, panes from row 0):
    // x 4 → column 5, y 3 → row 4.
    assert_eq!(harness.written, vec![(PaneId(1), b"\x1b[<0;5;4M".to_vec())]);
    harness.step(vec![Inbound::ControlRequest {
        workspace: "missing".into(),
        request: Request::List {
            instance: None,
            id: 4,
        },
        token: 43,
    }]);
    assert!(matches!(
        harness.control.last(),
        Some((43, Reply::Failed { .. }))
    ));
}

#[test]
fn shutdown_terminates_everything_and_reports_idle() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("two");
    let _viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::Shutdown]);
    assert_eq!(harness.terminated.len(), 2);
    harness.step(vec![
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 0,
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 0,
        },
    ]);
    // Viewers were told to exit; once gone, both workspaces close.
    let viewer = ViewerId(1);
    harness.step(vec![Inbound::ViewerGone { viewer }]);
    assert_eq!(harness.closed.len(), 2);
    assert!(harness.idle);
    assert_eq!(harness.session.entity_counts(), Default::default());
}

#[test]
fn limits_and_queue_overflow_are_enforced() {
    let config = Config::from_toml(
        "default-command = { argv = [\"/bin/sh\"] }\n[limits]\nmax-panes = 2\nmax-tabs = 1\n",
    )
    .unwrap();
    let mut harness = Harness::new();
    harness.session = Session::new(&config).unwrap();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    harness.control(viewer, split(2, Axis::Horizontal));
    assert!(harness.pending_spawns.is_empty());
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 2, error }) if error.code == ErrorCode::Limit
    ));
    harness.control(
        viewer,
        Request::Tab {
            instance: None,
            id: 3,
            action: TabAction::New { name: None },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 3, error }) if error.code == ErrorCode::Limit
    ));
    // Flooding a blocked viewer disconnects it instead of growing without bound.
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    let flood: Vec<Inbound> = (0..300)
        .map(|_| Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"x".to_vec()),
        })
        .collect();
    let effects = harness.step(flood);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseViewer { .. }))
    );
    assert_eq!(harness.session.entity_counts().viewers, 0);
    harness.complete_spawns();
    assert_eq!(
        harness.session.entity_counts().panes,
        2,
        "the pane still joined its tab"
    );
}

#[test]
fn a_viewer_leaving_in_its_arrival_step_is_released_and_the_limit_counts_the_batch() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    // The connection task sends the arrival and the departure back to back when the peer's
    // first read fails; both may land in one step while the spawn is still deferred.
    let ghost = ViewerId(500);
    let effects = harness.step(vec![
        Inbound::ViewerAttached {
            viewer: ghost,
            workspace: "default".into(),
            rows: 10,
            cols: 40,
        },
        Inbound::ViewerGone { viewer: ghost },
    ]);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseViewer { viewer } if *viewer == ghost))
    );
    assert_eq!(harness.session.entity_counts().viewers, 0);
    let viewer = harness.attach("default", 40, 160);
    let rect = harness.last_frame(viewer).layout[0].rect;
    assert_eq!(
        (rect.height, rect.width),
        (39, 160),
        "no ghost clamps the layout"
    );
    // One step with more arrivals than the limit admits only up to the limit.
    let batch: Vec<Inbound> = (0..66)
        .map(|index| Inbound::ViewerAttached {
            viewer: ViewerId(1_000 + index),
            workspace: "default".into(),
            rows: 24,
            cols: 80,
        })
        .collect();
    let effects = harness.step(batch);
    let refused = effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                Effect::ToViewer {
                    message: ServerMessage::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(refused, 3, "one viewer was already attached");
    assert_eq!(harness.session.entity_counts().viewers, 64);
    // A departure in the same step frees its place at once.
    let effects = harness.step(vec![
        Inbound::ViewerGone { viewer },
        Inbound::ViewerAttached {
            viewer: ViewerId(2_000),
            workspace: "default".into(),
            rows: 24,
            cols: 80,
        },
    ]);
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::ToViewer {
            viewer: ViewerId(2_000),
            message: ServerMessage::Error { .. },
        }
    )));
    assert_eq!(harness.session.entity_counts().viewers, 64);
}

#[test]
fn output_frames_are_paced_but_replies_are_not() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    let frames = harness.frames[&viewer].len();
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"one".to_vec(),
    }]);
    assert_eq!(harness.frames[&viewer].len(), frames + 1);
    // One millisecond after a frame, more output waits for the frame interval.
    harness.now -= 9;
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"two".to_vec(),
    }]);
    assert_eq!(harness.frames[&viewer].len(), frames + 1, "paced");
    assert!(
        harness
            .session
            .next_deadline_ms()
            .is_some_and(|at| at <= harness.now + 8),
        "a wake-up is proposed for the pending rows"
    );
    harness.now += 8;
    harness.step(Vec::new());
    assert_eq!(harness.frames[&viewer].len(), frames + 2);
    let text: String = harness.last_frame(viewer).panes[&PaneId(1)]
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(text.contains("onetwo"), "{text}");
    // A reply never waits: its frame goes out in the same step.
    harness.now -= 9;
    harness.control(
        viewer,
        Request::List {
            instance: None,
            id: 7,
        },
    );
    assert_eq!(harness.frames[&viewer].len(), frames + 3);
    // Neither does the echo of the viewer's own input, even when it arrives in fragments.
    harness.now -= 9;
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"xy".to_vec()),
        },
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"x".to_vec(),
        },
    ]);
    assert_eq!(harness.frames[&viewer].len(), frames + 4, "echo not paced");
    harness.now -= 9;
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"y".to_vec(),
    }]);
    assert_eq!(
        harness.frames[&viewer].len(),
        frames + 5,
        "the rest of the echo not paced"
    );
}

// ---------------------------------------------------------------------------------------------
// Randomized command sequences: every reachable interleaving of creations, stale ids, delayed or
// failed completions, viewer churn, process reports and time must keep the World consistent and
// must drain to an idle, empty World after shutdown.
// ---------------------------------------------------------------------------------------------

mod randomized {
    use super::*;
    use proptest::prelude::*;

    const NAMES: [&str; 3] = ["default", "alpha", "beta"];

    #[derive(Clone, Debug)]
    enum Op {
        Attach { workspace: u8, rows: u16, cols: u16 },
        Gone { viewer: u8 },
        Viewer { viewer: u8, request: ViewerRequest },
        Control { workspace: u8, request: Request },
        Manager(ManagerAction),
        Output { pane: u8, bytes: Vec<u8> },
        Eof { pane: u8 },
        Exited { pane: u8, code: u32 },
        Complete { index: u8, ok: bool },
        Advance(u64),
    }

    fn pane() -> impl Strategy<Value = PaneId> {
        (1..=12u32).prop_map(PaneId)
    }

    fn tab() -> impl Strategy<Value = TabId> {
        (1..=8u32).prop_map(TabId)
    }

    fn name() -> impl Strategy<Value = String> {
        (0..3usize).prop_map(|index| NAMES[index].to_owned())
    }

    fn request() -> impl Strategy<Value = Request> {
        prop_oneof![
            any::<bool>().prop_map(|stacked| split(
                1,
                if stacked {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                }
            )),
            pane().prop_map(|pane| Request::Kill {
                instance: None,
                id: 1,
                pane
            }),
            prop_oneof![
                Just(FocusTarget::Left),
                Just(FocusTarget::Right),
                Just(FocusTarget::Up),
                Just(FocusTarget::Down),
                pane().prop_map(FocusTarget::Pane),
            ]
            .prop_map(|target| Request::Focus {
                instance: None,
                id: 1,
                target
            }),
            (pane(), prop_oneof![Just(-3i16), Just(2), Just(40)]).prop_map(|(pane, delta)| {
                Request::Resize {
                    instance: None,
                    id: 1,
                    pane,
                    delta,
                }
            }),
            pane().prop_map(|pane| Request::SendKeys {
                instance: None,
                id: 1,
                pane,
                keys: "x\\n".into(),
                notation: fux::proto::control::KeyNotation::Escapes,
            }),
            (pane(), any::<bool>()).prop_map(|(pane, cells)| Request::Capture {
                if_revision: None,
                instance: None,
                id: 1,
                pane,
                attrs: false,
                scrollback: if cells { 0 } else { 5 },
                max_bytes: 4096,
                format: if cells {
                    fux::proto::control::CaptureFormat::Cells
                } else {
                    fux::proto::control::CaptureFormat::Text
                },
            }),
            Just(Request::List {
                instance: None,
                id: 1
            }),
            prop_oneof![
                Just(TabAction::New { name: None }),
                Just(TabAction::Next),
                Just(TabAction::Previous),
                (0..4u32).prop_map(|index| TabAction::Select {
                    target: fux::proto::control::TabTarget::Index(index)
                }),
                tab().prop_map(|tab| TabAction::Select {
                    target: fux::proto::control::TabTarget::Id(tab)
                }),
                tab().prop_map(|tab| TabAction::Rename {
                    tab,
                    name: "renamed".into(),
                }),
                tab().prop_map(|tab| TabAction::Close { tab }),
            ]
            .prop_map(|action| Request::Tab {
                instance: None,
                id: 1,
                action
            }),
            prop_oneof![
                Just(WorkspaceAction::List),
                Just(WorkspaceAction::New { name: None }),
                name().prop_map(|name| WorkspaceAction::New { name: Some(name) }),
                name().prop_map(|name| WorkspaceAction::Kill { name }),
                name().prop_map(|name| WorkspaceAction::Select { name }),
            ]
            .prop_map(|action| Request::Workspace {
                stream: None,
                instance: None,
                id: 1,
                action
            }),
        ]
    }

    fn viewer_request() -> impl Strategy<Value = ViewerRequest> {
        prop_oneof![
            4 => request().prop_map(ViewerRequest::Control),
            2 => prop::collection::vec(any::<u8>(), 0..8).prop_map(ViewerRequest::Input),
            1 => (0..70u16, 1..=30u16, 1..=90u16, any::<bool>(), 0..6u64).prop_map(
                |(code, row, column, release, generation)| ViewerRequest::Mouse {
                    event: MouseEvent {
                        code,
                        column,
                        row,
                        release,
                    },
                    generation,
                }
            ),
            1 => (pane(), 0..30u32).prop_map(|(pane, offset)| ViewerRequest::View {
                request: 1,
                pane,
                offset,
            }),
            1 => (1..=40u16, 1..=120u16).prop_map(|(rows, cols)| ViewerRequest::Resize { rows, cols }),
            1 => Just(ViewerRequest::Detach),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            2 => (0..3u8, 1..=30u16, 1..=100u16).prop_map(|(workspace, rows, cols)| Op::Attach {
                workspace,
                rows,
                cols,
            }),
            1 => (0..6u8).prop_map(|viewer| Op::Gone { viewer }),
            8 => (0..6u8, viewer_request()).prop_map(|(viewer, request)| Op::Viewer { viewer, request }),
            3 => (0..3u8, request()).prop_map(|(workspace, request)| Op::Control { workspace, request }),
            2 => prop_oneof![
                Just(ManagerAction::List),
                Just(ManagerAction::Resolve { name: None }),
                name().prop_map(|name| ManagerAction::Resolve { name: Some(name) }),
                name().prop_map(|name| ManagerAction::Kill { name }),
            ]
            .prop_map(Op::Manager),
            3 => (0..13u8, prop::collection::vec(any::<u8>(), 0..64))
                .prop_map(|(pane, bytes)| Op::Output { pane, bytes }),
            1 => (0..13u8).prop_map(|pane| Op::Eof { pane }),
            2 => (0..13u8, 0..3u32).prop_map(|(pane, code)| Op::Exited { pane, code }),
            5 => (0..4u8, prop::bool::weighted(0.8)).prop_map(|(index, ok)| Op::Complete { index, ok }),
            1 => prop_oneof![Just(100u64), Just(4_000), Just(6_000)].prop_map(Op::Advance),
        ]
    }

    fn drain_after_shutdown(harness: &mut Harness) {
        harness.step(vec![Inbound::Shutdown]);
        for _ in 0..8 {
            let mut inbound = Vec::new();
            for pane in std::mem::take(&mut harness.pending_spawns) {
                inbound.push(Inbound::SpawnCompleted {
                    pane,
                    result: Err("shutdown".into()),
                });
            }
            for pane in std::mem::take(&mut harness.terminated) {
                inbound.push(Inbound::PaneEof { pane });
                inbound.push(Inbound::PaneExited { pane, code: 129 });
            }
            harness.now += 6_000;
            let quiet = inbound.is_empty();
            harness.step(inbound);
            if harness.idle && quiet && harness.terminated.is_empty() {
                break;
            }
        }
    }

    proptest! {
        #[test]
        fn invariants_survive_random_sequences_stale_ids_and_delayed_completions(
            ops in prop::collection::vec(op(), 1..80)
        ) {
            let mut harness = Harness::new();
            harness.create_workspace("default");
            let mut viewers: Vec<ViewerId> = Vec::new();
            for op in ops {
                match op {
                    Op::Attach { workspace, rows, cols } => {
                        let viewer = ViewerId(harness.next_viewer);
                        harness.next_viewer += 1;
                        harness.step(vec![Inbound::ViewerAttached {
                            viewer,
                            workspace: NAMES[usize::from(workspace)].to_owned(),
                            rows,
                            cols,
                        }]);
                        viewers.push(viewer);
                    }
                    Op::Gone { viewer } => {
                        let viewer = ViewerId(u64::from(viewer) + 1);
                        harness.step(vec![Inbound::ViewerGone { viewer }]);
                    }
                    Op::Viewer { viewer, request } => {
                        let viewer = ViewerId(u64::from(viewer) + 1);
                        harness.step(vec![Inbound::ViewerRequest { viewer, request }]);
                    }
                    Op::Control { workspace, request } => {
                        harness.step(vec![Inbound::ControlRequest {
                            workspace: NAMES[usize::from(workspace)].to_owned(),
                            request,
                            token: 9,
                        }]);
                    }
                    Op::Manager(action) => {
                        harness.step(vec![Inbound::Manager { action, token: 8 }]);
                    }
                    Op::Output { pane, bytes } => {
                        harness.step(vec![Inbound::PaneOutput {
                            pane: PaneId(u32::from(pane)),
                            bytes,
                        }]);
                    }
                    Op::Eof { pane } => {
                        harness.step(vec![Inbound::PaneEof {
                            pane: PaneId(u32::from(pane)),
                        }]);
                    }
                    Op::Exited { pane, code } => {
                        harness.step(vec![Inbound::PaneExited {
                            pane: PaneId(u32::from(pane)),
                            code,
                        }]);
                    }
                    Op::Complete { index, ok } => {
                        if harness.pending_spawns.is_empty() {
                            continue;
                        }
                        let index = usize::from(index) % harness.pending_spawns.len();
                        let pane = harness.pending_spawns.remove(index);
                        harness.next_pid += 1;
                        let result = if ok {
                            Ok(harness.next_pid)
                        } else {
                            Err("exec failed".into())
                        };
                        harness.step(vec![Inbound::SpawnCompleted { pane, result }]);
                    }
                    Op::Advance(ms) => {
                        harness.now += ms;
                        harness.step(Vec::new());
                    }
                }
                // Frames never show a pane that is still starting or already gone.
                for frames in harness.frames.values() {
                    for frame in frames {
                        prop_assert!(frame.valid());
                    }
                }
            }
            drain_after_shutdown(&mut harness);
            let counts = harness.session.entity_counts();
            prop_assert_eq!(
                (counts.workspaces, counts.tabs, counts.panes, counts.viewers),
                (0, 0, 0, 0),
                "shutdown must drain every entity"
            );
            prop_assert!(harness.idle, "server reported idle after shutdown");
            prop_assert!(harness.pending_spawns.is_empty());
            let _ = viewers;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Regressions from the independent review.
// ---------------------------------------------------------------------------------------------

#[test]
fn exit_arriving_with_or_before_the_spawn_completion_is_not_lost() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    // Completion and exit in one batch: the reader thread starts with the process.
    harness.control(viewer, split(1, Axis::Horizontal));
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    harness.pending_spawns.clear();
    harness.step(vec![
        Inbound::SpawnCompleted {
            pane: PaneId(2),
            result: Ok(200),
        },
        Inbound::PaneEof { pane: PaneId(2) },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 3,
        },
    ]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)], "dead pane left");
    assert_eq!(harness.last_frame(viewer).focused, Some(PaneId(1)));
    assert!(harness.released.contains(&PaneId(2)));
    assert!(
        harness.terminated.is_empty(),
        "no signal for a reaped process"
    );
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(2),
            exit_status: Some(3),
            ..
        }
    )));
    // Exit reported one step before the completion.
    harness.control(viewer, split(2, Axis::Vertical));
    assert_eq!(harness.pending_spawns, vec![PaneId(3)]);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(3),
        code: 5,
    }]);
    assert_eq!(
        harness.pane_ids(viewer),
        vec![PaneId(1)],
        "reservation stays hidden"
    );
    harness.complete_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert!(harness.released.contains(&PaneId(3)));
    assert_eq!(harness.session.entity_counts().panes, 1);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(3),
            exit_status: Some(5),
            ..
        }
    )));
}

#[test]
fn a_workspace_whose_first_pane_exits_at_once_retires_with_its_status() {
    let mut harness = Harness::new();
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Resolve {
            name: Some("default".into()),
        },
        token: 7,
    }]);
    assert_eq!(harness.pending_spawns, vec![PaneId(1)]);
    harness.pending_spawns.clear();
    harness.step(vec![
        Inbound::SpawnCompleted {
            pane: PaneId(1),
            result: Ok(100),
        },
        Inbound::PaneEof { pane: PaneId(1) },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 7,
        },
    ]);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(1),
            exit_status: Some(7),
            ..
        }
    )));
    // Nobody is watching, so the workspace finalizes at once; final evidence retains the manager.
    assert!(harness.closed.contains(&"default".to_owned()));
    assert!(!harness.idle);
    harness.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    harness.step(Vec::new());
    assert!(harness.idle);
    let counts = harness.session.entity_counts();
    assert_eq!((counts.workspaces, counts.panes), (0, 0));
}

#[test]
fn killing_a_workspace_with_a_pending_spawn_stops_the_late_process() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.step(vec![Inbound::ControlRequest {
        workspace: "default".into(),
        request: split(1, Axis::Vertical),
        token: 3,
    }]);
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Kill {
            name: "default".into(),
        },
        token: 8,
    }]);
    assert!(harness.closed.contains(&"default".to_owned()));
    assert!(
        harness
            .control
            .iter()
            .any(|(token, reply)| *token == 3 && matches!(reply, Reply::Failed { .. })),
        "the pending creation failed its requester"
    );
    harness.terminated.clear();
    harness.released.clear();
    // The process reports in after its reservation was released: it must be stopped and reaped.
    harness.complete_spawns();
    assert!(harness.terminated.contains(&PaneId(2)));
    assert!(harness.released.contains(&PaneId(2)));
    assert_eq!(harness.session.entity_counts().panes, 0);
}

#[test]
fn viewer_requests_never_reach_other_workspaces() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("other");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(2),
        bytes: b"SECRET".to_vec(),
    }]);
    harness.request(
        viewer,
        ViewerRequest::View {
            request: 4,
            pane: PaneId(2),
            offset: 0,
        },
    );
    let view = harness.messages[&viewer]
        .iter()
        .rev()
        .find_map(|message| match message {
            ServerMessage::View { reply } => Some(reply.clone()),
            _ => None,
        })
        .expect("view reply");
    assert!(view.view.is_none(), "foreign pane is invisible");
    harness.control(
        viewer,
        Request::Workspace {
            stream: None,
            instance: None,
            id: 5,
            action: WorkspaceAction::Kill {
                name: "other".into(),
            },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { error, .. }) if error.code == ErrorCode::Unauthorized
    ));
    assert!(!harness.closed.contains(&"other".to_owned()));
    // Listing and switching remain available through the same attachment.
    harness.control(
        viewer,
        Request::Workspace {
            stream: None,
            instance: None,
            id: 6,
            action: WorkspaceAction::Select {
                name: "other".into(),
            },
        },
    );
    assert_eq!(harness.last_frame(viewer).workspace, "other");
}
