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
        matches!(final_reply(&mut h, PaneId(999), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Unknown)
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
        matches!(final_reply(&mut h, PaneId(1), "test-instance"), Reply::Failed { error, .. } if error.code == ErrorCode::Evicted)
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

/// `final` tells an evicted record (cap pressure) from an expired one (retention elapsed) and
/// from an id that never had a record; the rings remembering the first two are bounded.
#[test]
fn final_outcomes_distinguish_evicted_expired_and_unknown_within_bounded_rings() {
    use fux::ecs::resources::{MAX_FINAL_RECORDS, MAX_FORGOTTEN_FINAL_IDS};
    let code = |h: &mut Harness, pane: u32| match final_reply(h, PaneId(pane), "test-instance") {
        Reply::Failed { error, .. } => Some(error.code),
        Reply::Completed { .. } => None,
        other => panic!("unexpected final reply: {other:?}"),
    };
    let mut h = Harness::new();
    // Pane ids are allocated in sequence; `closed` is the id of the last closed pane.
    let mut closed = 0u32;
    let mut close_one = |h: &mut Harness| {
        h.create_workspace("default");
        closed += 1;
        h.step(vec![Inbound::PaneExited {
            pane: PaneId(closed),
            code: 0,
        }]);
        closed
    };
    for _ in 0..=MAX_FINAL_RECORDS {
        close_one(&mut h);
    }
    // Pane 1 was pushed out by the cap while still within its retention.
    assert_eq!(code(&mut h, 1), Some(ErrorCode::Evicted));
    assert_eq!(code(&mut h, 2), None);
    assert_eq!(code(&mut h, 9_999), Some(ErrorCode::Unknown));
    // Retention elapses for every remaining record: those ids answer `expired`, not `evicted`.
    h.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    assert_eq!(code(&mut h, 2), Some(ErrorCode::Expired));
    assert_eq!(
        code(&mut h, MAX_FINAL_RECORDS as u32 + 1),
        Some(ErrorCode::Expired)
    );
    assert_eq!(code(&mut h, 1), Some(ErrorCode::Evicted));
    assert_eq!(code(&mut h, 9_999), Some(ErrorCode::Unknown));
    // The eviction ring is bounded: once more than MAX_FORGOTTEN_FINAL_IDS records have been
    // evicted, the oldest evicted id is forgotten and answers `unknown`.
    let first_evicted = MAX_FINAL_RECORDS as u32 + 2;
    for _ in 0..MAX_FINAL_RECORDS {
        close_one(&mut h);
    }
    // The cap is full again; every further close evicts one record early.
    let mut last_closed = 0;
    for _ in 0..MAX_FORGOTTEN_FINAL_IDS {
        last_closed = close_one(&mut h);
    }
    let last_evicted = last_closed - MAX_FINAL_RECORDS as u32;
    assert_eq!(code(&mut h, first_evicted), Some(ErrorCode::Evicted));
    assert_eq!(code(&mut h, last_evicted), Some(ErrorCode::Evicted));
    assert_eq!(
        code(&mut h, 1),
        Some(ErrorCode::Unknown),
        "pane 1 fell off the ring"
    );
    close_one(&mut h);
    assert_eq!(
        code(&mut h, first_evicted),
        Some(ErrorCode::Unknown),
        "the oldest evicted id is dropped once the ring is full"
    );
    assert_eq!(code(&mut h, last_evicted + 1), Some(ErrorCode::Evicted));
    // The expired ring is bounded the same way.
    h.now += fux::config::DEFAULT_FINAL_RETAIN_MS;
    assert_eq!(code(&mut h, last_evicted + 2), Some(ErrorCode::Expired));
    assert_eq!(
        code(&mut h, 2),
        Some(ErrorCode::Expired),
        "still within the expired ring"
    );
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
        fixed_workspace: false,
        right_click: Default::default(),
        ratio: 5000,
        focus: true,
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
            fixed_workspace: false,
            right_click: Default::default(),
            ratio: 5000,
            focus: true,
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
fn manual_pane_labels_are_shared_bounded_and_independent_of_application_titles() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let alice = h.attach("default", 24, 80);
    let bob = h.attach("default", 24, 80);
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b]0;application\x07hello".to_vec(),
    }]);
    let rename = |name: &str| Request::RenamePane {
        id: 1,
        instance: Some("test-instance".into()),
        pane: PaneId(1),
        name: name.into(),
    };
    let before = h.last_frame(alice).clone();
    let effects = h.control(alice, rename("build 界"));
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::ResizePty { .. }
            | Effect::SpawnPane { .. }
            | Effect::ReleasePane { .. }
            | Effect::WriteInput { .. }
    )));
    for viewer in [alice, bob] {
        let pane = &h.last_frame(viewer).panes[&PaneId(1)];
        assert_eq!(pane.label.as_deref(), Some("build 界"));
        assert_eq!(pane.title, "application");
        assert_eq!(pane.cells, before.panes[&PaneId(1)].cells);
        let update = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::ToViewer {
                    viewer: target,
                    message: ServerMessage::State { state },
                } if *target == viewer => Some(state),
                _ => None,
            })
            .unwrap();
        assert!(
            update.panes[&PaneId(1)].cells.is_empty(),
            "label-only edits do not resend terminal rows"
        );
    }
    let history = h.request(
        alice,
        ViewerRequest::View {
            request: 7,
            pane: PaneId(1),
            offset: 1,
        },
    );
    assert!(history.iter().any(|effect| matches!(effect,
        Effect::ToViewer { message: ServerMessage::View { reply }, .. }
            if reply.view.as_ref().is_some_and(|view| view.label.as_deref() == Some("build 界") && view.title == "application")
    )));
    let events = h.events.len();
    h.control(alice, rename("build 界"));
    assert_eq!(
        h.events.len(),
        events,
        "unchanged labels do not publish mutations"
    );
    let saved = h.last_frame(alice).panes[&PaneId(1)].clone();
    for name in ["bad\x1b[31m".to_owned(), "界".repeat(43)] {
        let reply = input_request(&mut h, "default", rename(&name));
        assert!(matches!(reply, Reply::Failed { .. }));
        assert_eq!(h.last_frame(alice).panes[&PaneId(1)], saved);
    }
    assert!(matches!(
        input_request(&mut h, "other", rename("foreign")),
        Reply::Failed { .. }
    ));
    let mut stale = rename("stale");
    if let Request::RenamePane { instance, .. } = &mut stale {
        *instance = Some("old-server".into());
    }
    assert!(matches!(
        input_request(&mut h, "default", stale),
        Reply::Failed { .. }
    ));
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b]0;new application\x07".to_vec(),
    }]);
    assert_eq!(
        h.last_frame(alice).panes[&PaneId(1)].label.as_deref(),
        Some("build 界")
    );
    h.control(alice, rename(""));
    for viewer in [alice, bob] {
        let pane = &h.last_frame(viewer).panes[&PaneId(1)];
        assert!(pane.label.is_none());
        assert_eq!(pane.title, "new application");
    }
    h.control(
        alice,
        Request::Kill {
            id: 2,
            instance: None,
            pane: PaneId(1),
        },
    );
    assert!(matches!(
        input_request(&mut h, "default", rename("gone")),
        Reply::Failed { .. }
    ));
}

#[test]
fn sparse_frame_catalog_tracks_mutations_and_resets_on_workspace_switch() {
    fn update(h: &Harness, viewer: ViewerId) -> &fux::view::FrameUpdate {
        h.messages[&viewer]
            .iter()
            .rev()
            .find_map(|message| match message {
                ServerMessage::State { state } => Some(state.as_ref()),
                _ => None,
            })
            .expect("published frame")
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let viewer = h.attach("default", 24, 80);
    let initial = update(&h, viewer);
    assert!(initial.full);
    assert_eq!(initial.viewer, Some(viewer));
    assert_eq!(initial.server_instance.as_deref(), Some("test-instance"));
    let original_stream = initial.workspace_stream.expect("workspace stream");
    assert_ne!(original_stream, 0);
    assert_eq!(initial.tabs.as_ref().unwrap().len(), 1);

    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"ordinary output".to_vec(),
    }]);
    let delta = update(&h, viewer);
    assert!(!delta.full);
    assert!(delta.viewer.is_none() && delta.server_instance.is_none());
    assert!(delta.tabs.is_none());
    assert!(delta.workspace_stream.is_none());
    assert!(delta.workspace_presentation.is_none());
    assert_eq!(h.last_frame(viewer).workspace_stream, original_stream);
    h.control(
        viewer,
        Request::Workspace {
            id: 1,
            instance: Some("test-instance".into()),
            stream: Some(original_stream),
            action: WorkspaceAction::Rename {
                label: "Build team".into(),
            },
        },
    );
    assert_eq!(
        update(&h, viewer)
            .workspace_presentation
            .as_ref()
            .unwrap()
            .label
            .as_deref(),
        Some("Build team")
    );
    h.now += 100;
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"more output".to_vec(),
    }]);
    assert!(update(&h, viewer).workspace_presentation.is_none());
    assert_eq!(
        h.last_frame(viewer).workspace_label.as_deref(),
        Some("Build team")
    );
    h.control(
        viewer,
        Request::Tab {
            id: 1,
            instance: None,
            action: TabAction::Rename {
                tab: TabId(1),
                name: "renamed".into(),
            },
        },
    );
    assert_eq!(
        update(&h, viewer).tabs.as_ref().unwrap()[0].label,
        "renamed"
    );
    h.control(viewer, split(2, Axis::Horizontal));
    h.complete_spawns();
    let frame = h.last_frame(viewer);
    assert_eq!(frame.tabs[0].layout_generation, frame.layout_generation);
    assert_eq!(frame.tabs[0].label, "renamed");
    assert_eq!(frame.server_instance, "test-instance");

    h.control(
        viewer,
        Request::Workspace {
            id: 3,
            instance: None,
            stream: None,
            action: WorkspaceAction::Select {
                name: "other".into(),
            },
        },
    );
    let switched = update(&h, viewer);
    assert!(switched.full);
    assert_eq!(switched.viewer, Some(viewer));
    assert_eq!(switched.server_instance.as_deref(), Some("test-instance"));
    assert_ne!(switched.workspace_stream, Some(original_stream));
    assert_eq!(
        switched.workspace_stream,
        Some(h.last_frame(viewer).workspace_stream)
    );
    assert_eq!(switched.tabs.as_ref().unwrap()[0].id, TabId(2));
    assert_eq!(h.last_frame(viewer).workspace, "other");
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

fn layout_request(generation: Option<u64>, action: fux::proto::control::LayoutAction) -> Request {
    Request::Layout {
        id: 950,
        instance: Some("test-instance".into()),
        tab: TabId(1),
        generation,
        action,
    }
}

fn exported_layout(h: &mut Harness) -> (u64, fux::layout::LayoutDocument<PaneId>) {
    let result = input_request(
        h,
        "default",
        layout_request(None, fux::proto::control::LayoutAction::Export),
    );
    match result {
        Reply::Completed {
            result:
                CommandResult::Layout {
                    generation,
                    document,
                    ..
                },
            ..
        } => (generation, document),
        other => panic!("layout export: {other:?}"),
    }
}

#[test]
fn layout_edits_are_atomic_generation_checked_and_never_relaunch_panes() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, before) = exported_layout(&mut h);
    let start = h.effects.len();
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Swap {
                pane: PaneId(1),
                target: PaneId(2),
            },
        ),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    let (next, after) = exported_layout(&mut h);
    assert!(next > generation);
    assert_ne!(before, after);
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::ReleasePane { .. } | Effect::Terminate { .. }
    )));
    let stale = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Apply {
                labels: None,
                zoom: fux::proto::control::LayoutZoom::Preserve,
                remap: Vec::new(),
                document: before.clone(),
            },
        ),
    );
    assert!(matches!(stale, Reply::Failed { error, .. } if error.code == ErrorCode::Conflict));
    assert_eq!(exported_layout(&mut h), (next, after.clone()));
    let mut invalid = before;
    invalid
        .nodes
        .push(fux::layout::LayoutNode::Pane { pane: PaneId(999) });
    let rejected = input_request(
        &mut h,
        "default",
        layout_request(
            Some(next),
            LayoutAction::Apply {
                labels: None,
                zoom: fux::proto::control::LayoutZoom::Preserve,
                document: invalid,
                remap: Vec::new(),
            },
        ),
    );
    assert!(matches!(rejected, Reply::Failed { .. }));
    assert_eq!(exported_layout(&mut h), (next, after.clone()));
    let start = h.effects.len();
    let unchanged = input_request(
        &mut h,
        "default",
        layout_request(
            Some(next),
            LayoutAction::Apply {
                labels: None,
                zoom: fux::proto::control::LayoutZoom::Preserve,
                remap: Vec::new(),
                document: after.clone(),
            },
        ),
    );
    assert!(matches!(unchanged, Reply::Completed { .. }));
    assert_eq!(exported_layout(&mut h), (next, after));
    assert!(
        !h.effects[start..]
            .iter()
            .any(|effect| matches!(effect, Effect::ResizePty { .. }))
    );
}

#[test]
fn workspace_display_labels_preserve_routes_and_restore_atomically() {
    fn archive(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        h.step(vec![Inbound::Manager {
            action: ManagerAction::ExportLayout,
            token: 994,
        }]);
        let ManagerOutcome::LayoutArchive(archive) = h.manager.last().unwrap().1.clone() else {
            panic!("archive failed")
        };
        archive
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 81);
    let second = h.attach("default", 24, 81);
    let before = h.last_frame(viewer).clone();
    let original = archive(&mut h);
    let rename = |label: &str| Request::Workspace {
        id: 1,
        instance: Some("test-instance".into()),
        stream: Some(before.workspace_stream),
        action: WorkspaceAction::Rename {
            label: label.into(),
        },
    };
    let start = h.effects.len();
    assert!(matches!(
        input_request(&mut h, "default", rename("Build 界")),
        Reply::Completed { .. }
    ));
    for viewer in [viewer, second] {
        let frame = h.last_frame(viewer);
        assert_eq!(frame.workspace, "default");
        assert_eq!(frame.workspace_stream, before.workspace_stream);
        assert_eq!(frame.workspace_label.as_deref(), Some("Build 界"));
        assert_eq!(frame.layout, before.layout);
        assert_eq!(frame.panes, before.panes);
    }
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::ResizePty { .. }
    )));
    let named = archive(&mut h);
    assert_eq!(named.workspaces[0].label.as_deref(), Some("Build 界"));
    let event_count = h.events.len();
    input_request(&mut h, "default", rename("Build 界"));
    assert_eq!(
        h.events.len(),
        event_count,
        "unchanged label emitted an event"
    );
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Catalog,
        token: 994,
    }]);
    let ManagerOutcome::Catalog(catalog) = &h.manager.last().unwrap().1 else {
        panic!("catalog failed")
    };
    assert_eq!(catalog.entries[0].name, "default");
    assert_eq!(catalog.entries[0].label.as_deref(), Some("Build 界"));
    assert_eq!(catalog.entries[0].stream, before.workspace_stream);
    for (instance, stream) in [
        (None, Some(before.workspace_stream)),
        (Some("test-instance".into()), None),
        (Some("test-instance".into()), Some(0)),
    ] {
        let request = Request::Workspace {
            id: 1,
            instance,
            stream,
            action: WorkspaceAction::Rename {
                label: "unguarded".into(),
            },
        };
        assert!(request.validate().is_err());
        assert!(matches!(
            input_request(&mut h, "default", request),
            Reply::Failed { .. }
        ));
        assert_eq!(archive(&mut h), named);
    }
    for label in ["bad\nlabel".to_owned(), "x".repeat(129)] {
        assert!(matches!(
            input_request(&mut h, "default", rename(&label)),
            Reply::Failed { .. }
        ));
        assert_eq!(archive(&mut h), named);
    }
    let mut stale = rename("stale");
    if let Request::Workspace { stream, .. } = &mut stale {
        *stream = Some(before.workspace_stream + 1);
    }
    assert!(matches!(
        input_request(&mut h, "default", stale),
        Reply::Failed { .. }
    ));
    h.step(vec![Inbound::Manager {
        token: 994,
        action: ManagerAction::ApplyLayout {
            expected: original.clone(),
            archive: original.clone(),
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::Failed(_)
    ));
    assert_eq!(archive(&mut h), named);
    let mut invalid = original.clone();
    invalid.workspaces[0].label = Some("bad\nlabel".into());
    h.step(vec![Inbound::Manager {
        token: 994,
        action: ManagerAction::ApplyLayout {
            expected: named.clone(),
            archive: invalid,
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::Failed(_)
    ));
    assert_eq!(archive(&mut h), named);
    h.step(vec![Inbound::Manager {
        token: 994,
        action: ManagerAction::ApplyLayout {
            expected: named,
            archive: original.clone(),
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::LayoutArchive(_)
    ));
    assert_eq!(archive(&mut h), original);
    assert_eq!(h.last_frame(viewer).workspace_label, None);
    input_request(&mut h, "default", rename("again"));
    input_request(&mut h, "default", rename(""));
    assert_eq!(h.last_frame(second).workspace_label, None);
}

#[test]
fn equal_sized_pane_swaps_publish_positions_without_resizing_terminals() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    // 81 columns minus the separator gives two equal 40-column terminals.
    let viewer = h.attach("default", 24, 81);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let before = h.last_frame(viewer).clone();
    let (generation, _) = exported_layout(&mut h);
    let start = h.effects.len();
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Swap {
                pane: PaneId(1),
                target: PaneId(2),
            },
        ),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    let after = h.last_frame(viewer);
    assert_ne!(before.layout, after.layout);
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::ResizePty { .. } | Effect::SpawnPane { .. } | Effect::Terminate { .. }
    )));
}

#[test]
fn layout_generation_changes_after_legacy_resize_and_viewer_area_change() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, _) = exported_layout(&mut h);
    h.control(
        viewer,
        Request::Resize {
            id: 2,
            instance: None,
            pane: PaneId(1),
            delta: 1_000,
        },
    );
    let (resized, _) = exported_layout(&mut h);
    assert!(resized > generation);
    h.request(viewer, ViewerRequest::Resize { rows: 12, cols: 40 });
    let (area_changed, _) = exported_layout(&mut h);
    assert!(area_changed > resized);
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(resized),
            LayoutAction::SetRatio {
                split: fux::layout::NodeId(0),
                ratio: 5_000,
            },
        ),
    );
    assert!(matches!(reply, Reply::Failed { error, .. } if error.code == ErrorCode::Conflict));
}

#[test]
fn zoom_is_shared_preserves_tree_and_routes_every_viewers_input_to_visible_pane() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let a = h.attach("default", 24, 80);
    h.control(a, split(1, Axis::Horizontal));
    h.complete_spawns();
    let b = h.attach("default", 24, 80);
    h.control(
        b,
        Request::Focus {
            id: 3,
            instance: None,
            target: FocusTarget::Pane(PaneId(2)),
        },
    );
    let (generation, original) = exported_layout(&mut h);
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Zoom {
                pane: Some(PaneId(1)),
            },
        ),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    for viewer in [a, b] {
        assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
        assert_eq!(h.last_frame(viewer).zoomed, Some(PaneId(1)));
        assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
        h.request(viewer, ViewerRequest::Input(b"visible".to_vec()));
        assert_eq!(h.written.last(), Some(&(PaneId(1), b"visible".to_vec())));
    }
    let (generation, document) = exported_layout(&mut h);
    assert_eq!(document, original);
    let reply = input_request(
        &mut h,
        "default",
        layout_request(Some(generation), LayoutAction::Zoom { pane: None }),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    assert_eq!(exported_layout(&mut h).1, original);
    assert_eq!(
        h.last_frame(b).focused,
        Some(PaneId(2)),
        "private focus restored"
    );
    let generation = exported_layout(&mut h).0;
    input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Zoom {
                pane: Some(PaneId(1)),
            },
        ),
    );
    h.control(
        a,
        Request::Kill {
            id: 4,
            instance: None,
            pane: PaneId(1),
        },
    );
    assert_eq!(h.last_frame(b).zoomed, None);
    assert_eq!(h.pane_ids(b), vec![PaneId(2)]);
}

#[test]
fn pointer_resize_validates_separator_and_revision_before_changing_geometry() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, before) = exported_layout(&mut h);
    let invalid = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::ResizeBorder {
                column: 5,
                row: 5,
                to_column: 50,
                to_row: 5,
            },
        ),
    );
    assert!(matches!(invalid, Reply::Failed { .. }));
    assert_eq!(exported_layout(&mut h), (generation, before));
    let rect = h.last_frame(viewer).rect(PaneId(1)).unwrap();
    let resize = LayoutAction::ResizeBorder {
        column: rect.x + rect.width,
        row: 5,
        to_column: 50,
        to_row: 5,
    };
    assert!(matches!(
        input_request(
            &mut h,
            "default",
            layout_request(Some(generation), resize.clone())
        ),
        Reply::Completed { .. }
    ));
    assert_eq!(h.last_frame(viewer).rect(PaneId(1)).unwrap().width, 50);
    assert!(
        matches!(input_request(&mut h, "default", layout_request(Some(generation), resize)), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
}

#[test]
fn moving_last_pane_to_new_tab_keeps_receipts_process_and_viewer_valid() {
    use fux::proto::control::{LayoutAction, PaneDestination};
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    let reserved = input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    );
    let Reply::Completed {
        result: CommandResult::Input { receipt },
        ..
    } = reserved
    else {
        panic!("reserve failed")
    };
    let generation = exported_layout(&mut h).0;
    let start = h.effects.len();
    let moved = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Transfer {
                focus: false,
                pane: PaneId(1),
                destination: PaneDestination::NewTab {
                    label: Some("moved".into()),
                },
                side: fux::layout::Direction::Right,
            },
        ),
    );
    let Reply::Completed {
        result: CommandResult::Layout { tab, .. },
        ..
    } = moved
    else {
        panic!("transfer failed: {moved:?}")
    };
    assert_ne!(tab, TabId(1));
    assert_eq!(h.last_frame(viewer).active_tab, Some(tab));
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
    assert_eq!(h.last_frame(viewer).tabs.len(), 1);
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::ReleasePane { .. } | Effect::Terminate { .. }
    )));
    let submitted = input_request(
        &mut h,
        "default",
        Request::InputSubmit {
            id: 3,
            instance: Some("test-instance".into()),
            operation: receipt.operation,
            keys: "same process".into(),
        },
    );
    assert!(matches!(submitted, Reply::Completed { .. }));
    assert_eq!(
        h.written.last(),
        Some(&(PaneId(1), b"same process".to_vec()))
    );
}

#[test]
fn hidden_tab_layout_metadata_refreshes_other_viewers() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let source_viewer = h.attach("default", 24, 80);
    let destination_viewer = h.attach("default", 24, 80);
    h.control(
        destination_viewer,
        Request::Tab {
            id: 1,
            instance: None,
            action: TabAction::New { name: None },
        },
    );
    h.complete_spawns();
    let before = h
        .last_frame(source_viewer)
        .tabs
        .iter()
        .find(|entry| entry.id == TabId(2))
        .unwrap()
        .clone();
    assert_eq!(before.first_pane, Some(PaneId(2)));
    h.control(destination_viewer, split(2, Axis::Horizontal));
    h.complete_spawns();
    let split_generation = h.last_frame(destination_viewer).layout_generation;
    h.control(
        destination_viewer,
        Request::Layout {
            id: 3,
            instance: None,
            tab: TabId(2),
            generation: Some(split_generation),
            action: LayoutAction::Swap {
                pane: PaneId(2),
                target: PaneId(3),
            },
        },
    );
    let source = h.last_frame(source_viewer);
    assert_eq!(source.active_tab, Some(TabId(1)));
    assert_eq!(source.focused, Some(PaneId(1)));
    let destination = source
        .tabs
        .iter()
        .find(|entry| entry.id == TabId(2))
        .unwrap();
    assert_eq!(destination.first_pane, Some(PaneId(3)));
    assert!(destination.layout_generation > before.layout_generation);
    assert_eq!(
        destination.layout_generation,
        h.last_frame(destination_viewer).layout_generation
    );
}

#[test]
fn cross_tab_transfer_checks_both_layouts_and_keeps_other_panes() {
    use fux::proto::control::{LayoutAction, PaneDestination};
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    h.control(
        viewer,
        Request::Tab {
            id: 2,
            instance: None,
            action: TabAction::New {
                name: Some("destination".into()),
            },
        },
    );
    h.complete_spawns();
    let destination = input_request(
        &mut h,
        "default",
        Request::Layout {
            id: 3,
            instance: None,
            tab: TabId(2),
            generation: None,
            action: LayoutAction::Export,
        },
    );
    let Reply::Completed {
        result:
            CommandResult::Layout {
                generation: mut destination_generation,
                ..
            },
        ..
    } = destination
    else {
        panic!("destination export")
    };
    let (generation, original) = exported_layout(&mut h);
    let action = |generation| LayoutAction::Transfer {
        focus: false,
        pane: PaneId(2),
        destination: PaneDestination::Tab {
            ratio: 5000,
            tab: TabId(2),
            generation,
            target: PaneId(3),
        },
        side: fux::layout::Direction::Left,
    };
    assert!(
        matches!(input_request(&mut h, "default", layout_request(Some(generation), action(destination_generation + 1))), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert_eq!(exported_layout(&mut h), (generation, original));
    // Resize and transfer can arrive in one input batch, before the layout phase increments
    // the destination revision. A matching old revision must still reject this edit.
    h.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Resize { rows: 20, cols: 70 },
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(layout_request(
                Some(generation),
                action(destination_generation),
            )),
        },
    ]);
    assert!(matches!(h.replies(viewer).last().unwrap(),
        Reply::Failed { error, .. } if error.code == ErrorCode::Conflict));
    assert_eq!(h.pane_ids(viewer), vec![PaneId(3)]);
    destination_generation = h.last_frame(viewer).layout_generation;

    assert!(matches!(
        input_request(
            &mut h,
            "default",
            layout_request(Some(generation), action(destination_generation))
        ),
        Reply::Completed { .. }
    ));
    assert_eq!(h.pane_ids(viewer), vec![PaneId(2), PaneId(3)]);
    assert_eq!(h.last_frame(viewer).tabs.len(), 2);
    h.control(
        viewer,
        Request::Tab {
            id: 4,
            instance: None,
            action: TabAction::Select {
                target: fux::proto::control::TabTarget::Id(TabId(1)),
            },
        },
    );
    assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
}

#[test]
fn tab_order_edits_preserve_focus_identity_and_reject_foreign_targets() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    for _ in 0..2 {
        h.control(
            viewer,
            Request::Tab {
                id: 1,
                instance: None,
                action: TabAction::New { name: None },
            },
        );
        h.complete_spawns();
    }
    let active = h.last_frame(viewer).active_tab;
    let focus = h.last_frame(viewer).focused;
    h.control(
        viewer,
        Request::Tab {
            id: 2,
            instance: None,
            action: TabAction::Reorder {
                tab: TabId(3),
                before: Some(TabId(1)),
            },
        },
    );
    assert_eq!(
        h.last_frame(viewer)
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![TabId(3), TabId(1), TabId(2)]
    );
    assert_eq!(h.last_frame(viewer).active_tab, active);
    assert_eq!(h.last_frame(viewer).focused, focus);
    h.control(
        viewer,
        Request::Tab {
            id: 3,
            instance: None,
            action: TabAction::Reorder {
                tab: TabId(3),
                before: Some(TabId(999)),
            },
        },
    );
    assert!(matches!(
        h.replies(viewer).last(),
        Some(Reply::Failed { .. })
    ));
    assert_eq!(
        h.last_frame(viewer)
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![TabId(3), TabId(1), TabId(2)]
    );
    h.control(
        viewer,
        Request::Tab {
            id: 4,
            instance: None,
            action: TabAction::Reorder {
                tab: TabId(1),
                before: None,
            },
        },
    );
    assert_eq!(
        h.last_frame(viewer)
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![TabId(3), TabId(2), TabId(1)]
    );
}

#[test]
fn layout_labels_remap_restore_clear_and_reject_invalid_edits_atomically() {
    use fux::proto::control::{LayoutAction, LayoutZoom};
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (initial_generation, document) = exported_layout(&mut h);
    h.control(
        viewer,
        Request::RenamePane {
            id: 2,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            name: "original".into(),
        },
    );
    let (generation, _) = exported_layout(&mut h);
    assert!(generation > initial_generation);
    let apply = |labels| LayoutAction::Apply {
        document: document.clone(),
        remap: vec![(PaneId(1), PaneId(2)), (PaneId(2), PaneId(1))],
        zoom: LayoutZoom::Preserve,
        labels,
    };
    let stale = input_request(
        &mut h,
        "default",
        layout_request(Some(initial_generation), apply(Some(Vec::new()))),
    );
    assert!(matches!(stale, Reply::Failed { .. }));
    let before = h.last_frame(viewer).clone();
    for labels in [
        vec![(PaneId(1), "valid".into()), (PaneId(99), "foreign".into())],
        vec![(PaneId(1), "valid".into()), (PaneId(1), "duplicate".into())],
        vec![(PaneId(1), "valid".into()), (PaneId(2), "\x1b".into())],
        vec![(PaneId(1), "x".repeat(129))],
    ] {
        let reply = input_request(
            &mut h,
            "default",
            layout_request(Some(generation), apply(Some(labels))),
        );
        assert!(matches!(reply, Reply::Failed { .. }));
        assert_eq!(h.last_frame(viewer).layout, before.layout);
        assert_eq!(h.last_frame(viewer).panes, before.panes);
        assert_eq!(h.last_frame(viewer).layout_generation, generation);
    }
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            apply(Some(vec![(PaneId(1), "mapped界".into())])),
        ),
    );
    let Reply::Completed {
        result: CommandResult::Layout {
            labels, generation, ..
        },
        ..
    } = reply
    else {
        panic!("apply failed")
    };
    assert_eq!(labels, vec![(PaneId(2), "mapped界".into())]);
    assert_eq!(
        h.last_frame(viewer).panes[&PaneId(2)].label.as_deref(),
        Some("mapped界")
    );
    assert!(h.last_frame(viewer).panes[&PaneId(1)].label.is_none());
    let effects = h.effects.len();
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            apply(Some(vec![(PaneId(1), "mapped界".into())])),
        ),
    );
    assert!(
        matches!(reply, Reply::Completed { result: CommandResult::Layout { generation: same, .. }, .. } if same == generation)
    );
    assert!(
        !h.effects[effects..]
            .iter()
            .any(|effect| matches!(effect, Effect::ResizePty { .. } | Effect::SpawnPane { .. }))
    );
    let reply = input_request(
        &mut h,
        "default",
        layout_request(Some(generation), apply(None)),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    assert_eq!(
        h.last_frame(viewer).panes[&PaneId(2)].label.as_deref(),
        Some("mapped界")
    );
    let reply = input_request(
        &mut h,
        "default",
        layout_request(Some(generation), apply(Some(Vec::new()))),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    assert!(
        h.last_frame(viewer)
            .panes
            .values()
            .all(|pane| pane.label.is_none())
    );
}

#[test]
fn layout_import_restores_remapped_zoom_atomically_and_noops_when_unchanged() {
    use fux::proto::control::{LayoutAction, LayoutZoom};
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (_, document) = exported_layout(&mut h);
    for zoom in [Some(PaneId(1)), None] {
        let (generation, _) = exported_layout(&mut h);
        let action = LayoutAction::Apply {
            labels: None,
            document: document.clone(),
            remap: vec![(PaneId(1), PaneId(2)), (PaneId(2), PaneId(1))],
            zoom: LayoutZoom::Set { pane: zoom },
        };
        let reply = input_request(
            &mut h,
            "default",
            layout_request(Some(generation), action.clone()),
        );
        assert!(matches!(reply, Reply::Completed { .. }));
        assert_eq!(h.last_frame(viewer).zoomed, zoom.map(|_| PaneId(2)));
        let before = exported_layout(&mut h);
        let effects = h.effects.len();
        let reply = input_request(&mut h, "default", layout_request(Some(before.0), action));
        assert!(matches!(reply, Reply::Completed { .. }));
        assert_eq!(exported_layout(&mut h), before);
        assert!(
            !h.effects[effects..].iter().any(|effect| matches!(
                effect,
                Effect::ResizePty { .. } | Effect::SpawnPane { .. }
            ))
        );
        let rejected = input_request(
            &mut h,
            "default",
            layout_request(
                Some(before.0),
                LayoutAction::Apply {
                    labels: None,
                    document: document.clone(),
                    remap: Vec::new(),
                    zoom: LayoutZoom::Set {
                        pane: Some(PaneId(999)),
                    },
                },
            ),
        );
        assert!(matches!(rejected, Reply::Failed { .. }));
        assert_eq!(exported_layout(&mut h), before);
        assert_eq!(h.last_frame(viewer).zoomed, zoom.map(|_| PaneId(2)));
    }
}

#[test]
fn layout_import_remapping_is_complete_bijective_and_atomic() {
    use fux::layout::{LayoutDocument, LayoutNode};
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, before) = exported_layout(&mut h);
    let document = LayoutDocument {
        root: Some(0),
        nodes: vec![
            LayoutNode::Split {
                axis: Axis::Vertical,
                ratio: 5_000,
                first: 1,
                second: 2,
            },
            LayoutNode::Pane { pane: PaneId(101) },
            LayoutNode::Pane { pane: PaneId(102) },
        ],
    };
    for remap in [
        vec![(PaneId(101), PaneId(1))],
        vec![(PaneId(101), PaneId(1)), (PaneId(101), PaneId(2))],
        vec![(PaneId(101), PaneId(1)), (PaneId(102), PaneId(1))],
        vec![(PaneId(101), PaneId(1)), (PaneId(102), PaneId(999))],
        vec![
            (PaneId(101), PaneId(1)),
            (PaneId(102), PaneId(2)),
            (PaneId(103), PaneId(3)),
        ],
    ] {
        let reply = input_request(
            &mut h,
            "default",
            layout_request(
                Some(generation),
                LayoutAction::Apply {
                    labels: None,
                    zoom: fux::proto::control::LayoutZoom::Preserve,
                    document: document.clone(),
                    remap,
                },
            ),
        );
        assert!(matches!(reply, Reply::Failed { .. }));
        assert_eq!(exported_layout(&mut h), (generation, before.clone()));
    }
    let reply = input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Apply {
                labels: None,
                zoom: fux::proto::control::LayoutZoom::Preserve,
                document,
                remap: vec![(PaneId(101), PaneId(2)), (PaneId(102), PaneId(1))],
            },
        ),
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    assert_eq!(h.pane_ids(viewer), vec![PaneId(2), PaneId(1)]);
    assert_eq!(h.last_frame(viewer).rect(PaneId(2)).unwrap().y, 0);
    assert!(h.last_frame(viewer).rect(PaneId(1)).unwrap().y > 0);
}

#[test]
fn workspace_order_is_manager_scoped_and_missing_targets_leave_order_unchanged() {
    let mut h = Harness::new();
    for name in ["alpha", "beta", "gamma"] {
        h.create_workspace(name);
    }
    let viewer = h.attach("beta", 24, 80);
    let focused = h.last_frame(viewer).focused;
    h.step(vec![Inbound::Manager {
        token: 801,
        action: ManagerAction::Reorder {
            name: "gamma".into(),
            before: Some("alpha".into()),
        },
    }]);
    assert!(
        matches!(h.manager.last(), Some((801, ManagerOutcome::Names(names))) if names == &vec!["gamma".to_owned(), "alpha".to_owned(), "beta".to_owned()])
    );
    h.step(vec![Inbound::Manager {
        token: 802,
        action: ManagerAction::Reorder {
            name: "alpha".into(),
            before: Some("missing".into()),
        },
    }]);
    assert!(matches!(
        h.manager.last(),
        Some((802, ManagerOutcome::Failed(_)))
    ));
    h.step(vec![Inbound::Manager {
        token: 803,
        action: ManagerAction::List,
    }]);
    assert!(
        matches!(h.manager.last(), Some((803, ManagerOutcome::Names(names))) if names == &vec!["gamma".to_owned(), "alpha".to_owned(), "beta".to_owned()])
    );
    assert_eq!(h.last_frame(viewer).workspace, "beta");
    assert_eq!(h.last_frame(viewer).focused, focused);
    h.step(vec![Inbound::Manager {
        token: 804,
        action: ManagerAction::Reorder {
            name: "gamma".into(),
            before: None,
        },
    }]);
    assert!(
        matches!(h.manager.last(), Some((804, ManagerOutcome::Names(names))) if names == &vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()])
    );
}

fn workspace_stream(h: &mut Harness, name: &str) -> u64 {
    match input_request(
        h,
        name,
        Request::List {
            id: 0,
            instance: Some("test-instance".into()),
        },
    ) {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => workspaces[0].event_cursor.stream,
        other => panic!("listing: {other:?}"),
    }
}

fn transfer_request(h: &mut Harness, transfer: fux::proto::control::WorkspaceTransfer) -> Reply {
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Transfer { transfer },
        token: 990,
    }]);
    match &h.manager.last().unwrap().1 {
        ManagerOutcome::Layout(reply) => reply.clone(),
        other => panic!("transfer: {other:?}"),
    }
}

fn pane_location(h: &mut Harness, instance: &str, pane: PaneId) -> Reply {
    h.step(vec![Inbound::Manager {
        token: 991,
        action: ManagerAction::PaneLocation {
            instance: instance.into(),
            pane,
        },
    }]);
    let ManagerOutcome::PaneLocation(reply) = &h.manager.last().unwrap().1 else {
        panic!("location reply missing")
    };
    reply.clone()
}

fn manager_input_status(h: &mut Harness, instance: &str, pane: PaneId, operation: u64) -> Reply {
    h.step(vec![Inbound::Manager {
        token: 992,
        action: ManagerAction::InputStatus {
            instance: instance.into(),
            pane,
            operation,
        },
    }]);
    let ManagerOutcome::InputStatus(reply) = &h.manager.last().unwrap().1 else {
        panic!("receipt reply missing")
    };
    reply.clone()
}

#[test]
fn pane_location_is_read_only_and_rejects_wrong_server_missing_and_closed_panes() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    let before = h.last_frame(viewer).clone();
    let start = h.effects.len();
    let Reply::Completed {
        result: CommandResult::PaneLocation { location },
        ..
    } = pane_location(&mut h, "test-instance", PaneId(1))
    else {
        panic!("live location missing")
    };
    assert_eq!(location.workspace, "default");
    assert_eq!(location.origin_workspace, "default");
    assert_eq!(location.stream, before.workspace_stream);
    assert_eq!(location.origin_stream, before.workspace_stream);
    assert_eq!(location.tab, TabId(1));
    assert_eq!(location.layout_generation, before.layout_generation);
    assert!(location.pid > 0);
    assert_eq!(h.last_frame(viewer), &before);
    assert!(
        h.effects[start..]
            .iter()
            .all(|effect| matches!(effect, Effect::Manager { .. }))
    );
    assert!(
        matches!(pane_location(&mut h, "replacement", PaneId(1)), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert!(
        matches!(pane_location(&mut h, "test-instance", PaneId(999)), Reply::Failed { error, .. } if error.code == ErrorCode::NotFound)
    );
    assert!(location.accepts_input);
    h.step(vec![Inbound::PaneEof { pane: PaneId(1) }]);
    let Reply::Completed {
        result: CommandResult::PaneLocation { location: eof },
        ..
    } = pane_location(&mut h, "test-instance", PaneId(1))
    else {
        panic!("EOF route missing")
    };
    assert!(!eof.accepts_input);
    assert_eq!(eof.pid, location.pid);
    assert_eq!(eof.workspace, location.workspace);
    assert_eq!(eof.origin_stream, location.origin_stream);
    h.control(
        viewer,
        Request::Kill {
            id: 1,
            instance: None,
            pane: PaneId(1),
        },
    );
    assert!(
        matches!(pane_location(&mut h, "test-instance", PaneId(1)), Reply::Failed { error, .. } if error.code == ErrorCode::NotFound)
    );
}

#[test]
fn creation_pin_release_checks_process_identity_and_preserves_explicit_pins() {
    fn release(h: &mut Harness, instance: &str, pane: PaneId, pid: u32) -> Reply {
        h.step(vec![Inbound::Manager {
            token: 993,
            action: ManagerAction::ReleasePanePin {
                instance: instance.into(),
                pane,
                pid,
            },
        }]);
        let ManagerOutcome::ReleasePanePin(reply) = &h.manager.last().unwrap().1 else {
            panic!("release reply missing")
        };
        reply.clone()
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    let mut request = split(1, Axis::Horizontal);
    if let Request::Split {
        fixed_workspace, ..
    } = &mut request
    {
        *fixed_workspace = true;
    }
    h.control(viewer, request);
    h.complete_spawns();
    let Reply::Completed {
        result: CommandResult::PaneLocation { location },
        ..
    } = pane_location(&mut h, "test-instance", PaneId(2))
    else {
        panic!("live pane missing")
    };
    let before = h.last_frame(viewer).clone();
    for (instance, pane, pid) in [
        ("replacement", PaneId(2), location.pid),
        ("test-instance", PaneId(2), location.pid + 1),
        ("test-instance", PaneId(999), location.pid),
    ] {
        assert!(matches!(
            release(&mut h, instance, pane, pid),
            Reply::Failed { .. }
        ));
    }
    let start = h.effects.len();
    assert!(matches!(
        release(&mut h, "test-instance", PaneId(2), location.pid),
        Reply::Completed { .. }
    ));
    assert_eq!(h.last_frame(viewer).layout, before.layout);
    assert_eq!(h.last_frame(viewer).panes, before.panes);
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::ResizePty { .. }
    )));
    let events = h.events.len();
    assert!(matches!(
        release(&mut h, "test-instance", PaneId(2), location.pid),
        Reply::Completed { .. }
    ));
    assert_eq!(h.events.len(), events, "repeated release was not a no-op");
    assert!(matches!(
        input_request(
            &mut h,
            "default",
            Request::FixWorkspace {
                id: 2,
                instance: Some("test-instance".into()),
                stream: location.stream,
                pane: PaneId(2),
            }
        ),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(release(&mut h, "test-instance", PaneId(2), location.pid), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    let Reply::Completed {
        result: CommandResult::Listing { workspaces, .. },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::List {
            id: 3,
            instance: None,
        },
    )
    else {
        panic!("listing missing")
    };
    assert!(
        workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|pane| pane.id == PaneId(2))
            .unwrap()
            .fixed_workspace
    );
}

#[test]
fn workspace_move_preserves_delivered_input_sequence() {
    use fux::proto::control::{
        InputState, PaneDestination, WorkspaceDestination, WorkspaceTransfer,
    };
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let Reply::Completed {
        result: CommandResult::Input { receipt },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    )
    else {
        panic!("reserve failed")
    };
    let Reply::Completed {
        result: CommandResult::Input { receipt: accepted },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::InputSubmit {
            id: 2,
            instance: Some("test-instance".into()),
            operation: receipt.operation,
            keys: "prompt".into(),
        },
    )
    else {
        panic!("submit failed")
    };
    h.step(vec![Inbound::InputCompleted {
        pane: PaneId(1),
        operation: receipt.operation,
        bytes_written: 6,
        error: None,
    }]);
    let stream = workspace_stream(&mut h, "other");
    let transfer = WorkspaceTransfer {
        focus: false,
        follow: None,
        instance: "test-instance".into(),
        source: TabId(1),
        generation: h.last_frame(viewer).layout_generation,
        pane: PaneId(1),
        workspace: WorkspaceDestination::Existing {
            name: "other".into(),
            stream,
        },
        destination: PaneDestination::NewTab { label: None },
        side: fux::layout::Direction::Right,
    };
    assert!(matches!(
        transfer_request(&mut h, transfer),
        Reply::Completed { .. }
    ));
    let Reply::Completed {
        result: CommandResult::Listing { workspaces, .. },
        ..
    } = input_request(
        &mut h,
        "other",
        Request::List {
            id: 3,
            instance: None,
        },
    )
    else {
        panic!("listing failed")
    };
    let pane = workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .find(|pane| pane.id == PaneId(1))
        .unwrap();
    assert_eq!(
        pane.input_sequence, accepted.input_sequence,
        "layout movement is not intervening input"
    );
    let Reply::Completed {
        result: CommandResult::Input { receipt: delivered },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::InputStatus {
            id: 4,
            instance: Some("test-instance".into()),
            operation: receipt.operation,
        },
    )
    else {
        panic!("delivered receipt missing")
    };
    assert_eq!(delivered.state, InputState::Delivered);
    assert_eq!(delivered.input_sequence, accepted.input_sequence);
    assert_eq!(delivered.bytes_written, 6);
    assert_eq!(
        manager_input_status(&mut h, "test-instance", PaneId(1), receipt.operation),
        Reply::Completed {
            id: 0,
            result: CommandResult::Input {
                receipt: delivered.clone()
            }
        }
    );
    input_request(
        &mut h,
        "default",
        Request::Workspace {
            id: 5,
            instance: None,
            stream: None,
            action: WorkspaceAction::Kill {
                name: "default".into(),
            },
        },
    );
    assert_eq!(
        manager_input_status(&mut h, "test-instance", PaneId(1), receipt.operation),
        Reply::Completed {
            id: 0,
            result: CommandResult::Input {
                receipt: delivered.clone()
            }
        }
    );
    assert!(
        matches!(manager_input_status(&mut h, "replacement", PaneId(1), receipt.operation), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert!(
        matches!(manager_input_status(&mut h, "test-instance", PaneId(2), receipt.operation), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    h.now = delivered.expires_ms;
    assert!(
        matches!(manager_input_status(&mut h, "test-instance", PaneId(1), receipt.operation), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
}

#[test]
fn workspace_transfer_preserves_pane_and_rejects_old_route_receipts_even_after_return() {
    use fux::proto::control::{PaneDestination, WorkspaceTransfer};
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let source = h.attach("default", 24, 80);
    let destination = h.attach("other", 24, 80);
    h.control(source, split(1, Axis::Horizontal));
    h.complete_spawns();
    let source_stream = workspace_stream(&mut h, "default");
    let destination_stream = workspace_stream(&mut h, "other");
    let Reply::Completed {
        result: CommandResult::Input { receipt },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    )
    else {
        panic!("reserve failed")
    };
    let request = WorkspaceTransfer {
        focus: false,
        follow: None,
        instance: "test-instance".into(),
        source: TabId(1),
        generation: h.last_frame(source).layout_generation,
        pane: PaneId(1),
        workspace: fux::proto::control::WorkspaceDestination::Existing {
            name: "other".into(),
            stream: destination_stream,
        },
        destination: PaneDestination::Tab {
            ratio: 5000,
            tab: TabId(2),
            generation: h.last_frame(destination).layout_generation,
            target: PaneId(2),
        },
        side: fux::layout::Direction::Right,
    };
    for invalid in [
        WorkspaceTransfer {
            focus: false,
            workspace: fux::proto::control::WorkspaceDestination::Existing {
                name: "other".into(),
                stream: destination_stream + 100,
            },
            ..request.clone()
        },
        WorkspaceTransfer {
            focus: false,
            instance: "old-instance".into(),
            ..request.clone()
        },
        WorkspaceTransfer {
            focus: false,
            generation: request.generation + 1,
            ..request.clone()
        },
    ] {
        assert!(matches!(
            transfer_request(&mut h, invalid),
            Reply::Failed { .. }
        ));
        assert_eq!(h.pane_ids(source), vec![PaneId(1), PaneId(3)]);
        assert_eq!(h.pane_ids(destination), vec![PaneId(2)]);
        let Reply::Completed {
            result: CommandResult::Input {
                receipt: still_reserved,
            },
            ..
        } = manager_input_status(&mut h, "test-instance", PaneId(1), receipt.operation)
        else {
            panic!("reservation missing")
        };
        assert_eq!(
            still_reserved, receipt,
            "rejected transfer changed reservation"
        );
    }
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"SURVIVES_WORKSPACE_MOVE".to_vec(),
    }]);
    let start = h.effects.len();
    assert!(matches!(
        transfer_request(&mut h, request),
        Reply::Completed { .. }
    ));
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::ReleasePane { .. }
    )));
    assert_eq!(h.pane_ids(source), vec![PaneId(3)]);
    assert_eq!(h.pane_ids(destination), vec![PaneId(2), PaneId(1)]);
    let Reply::Completed {
        result: CommandResult::PaneLocation { location },
        ..
    } = pane_location(&mut h, "test-instance", PaneId(1))
    else {
        panic!("moved location missing")
    };
    assert_eq!(location.workspace, "other");
    assert_eq!(location.stream, destination_stream);
    assert_eq!(location.origin_workspace, "default");
    assert_eq!(location.origin_stream, source_stream);
    assert_eq!(location.tab, TabId(2));
    let Reply::Completed {
        result: CommandResult::Input {
            receipt: invalidated,
        },
        ..
    } = manager_input_status(&mut h, "test-instance", PaneId(1), receipt.operation)
    else {
        panic!("invalidated receipt missing")
    };
    assert_eq!(invalidated.state, fux::proto::control::InputState::Failed);
    assert_eq!(invalidated.input_sequence, receipt.input_sequence);
    assert_eq!(invalidated.bytes_written, 0);
    assert!(
        invalidated
            .error
            .as_deref()
            .is_some_and(|error| error.contains("workspace"))
    );
    let submit = || Request::InputSubmit {
        id: 2,
        instance: Some("test-instance".into()),
        operation: receipt.operation,
        keys: "wrong-route".into(),
    };
    assert!(
        matches!(input_request(&mut h, "default", submit()), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert!(
        matches!(input_request(&mut h, "other", submit()), Reply::Failed { error, .. } if error.code == ErrorCode::Expired)
    );
    assert!(
        matches!(input_request(&mut h, "default", Request::SendKeys {
        id: 3, instance: Some("test-instance".into()), pane: PaneId(1), keys: "no".into(), notation: Default::default(),
    }), Reply::Failed { error, .. } if error.code == ErrorCode::NotFound)
    );
    let back = WorkspaceTransfer {
        focus: false,
        follow: None,
        instance: "test-instance".into(),
        source: TabId(2),
        generation: h.last_frame(destination).layout_generation,
        pane: PaneId(1),
        workspace: fux::proto::control::WorkspaceDestination::Existing {
            name: "default".into(),
            stream: source_stream,
        },
        destination: PaneDestination::Tab {
            ratio: 5000,
            tab: TabId(1),
            generation: h.last_frame(source).layout_generation,
            target: PaneId(3),
        },
        side: fux::layout::Direction::Left,
    };
    assert!(matches!(
        transfer_request(&mut h, back),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(input_request(&mut h, "default", submit()), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"_AND_AFTER".to_vec(),
    }]);
    let capture = input_request(
        &mut h,
        "default",
        Request::Capture {
            id: 4,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            attrs: false,
            scrollback: 0,
            max_bytes: 131_072,
            format: Default::default(),
            if_revision: None,
        },
    );
    assert!(
        serde_json::to_string(&capture)
            .unwrap()
            .contains("SURVIVES_WORKSPACE_MOVE_AND_AFTER")
    );
}

#[test]
fn workspace_transfer_respects_fixed_routes_and_queued_input() {
    use fux::proto::control::{PaneDestination, WorkspaceTransfer};
    let mut h = Harness::new();
    h.create_workspace("default");
    h.create_workspace("other");
    let viewer = h.attach("default", 24, 80);
    let stream = workspace_stream(&mut h, "default");
    let destination_stream = workspace_stream(&mut h, "other");
    let request = WorkspaceTransfer {
        focus: false,
        follow: None,
        instance: "test-instance".into(),
        source: TabId(1),
        generation: h.last_frame(viewer).layout_generation,
        pane: PaneId(1),
        workspace: fux::proto::control::WorkspaceDestination::Existing {
            name: "other".into(),
            stream: destination_stream,
        },
        destination: PaneDestination::NewTab {
            label: Some("moved".into()),
        },
        side: fux::layout::Direction::Right,
    };
    let Reply::Completed {
        result: CommandResult::Input { receipt },
        ..
    } = input_request(
        &mut h,
        "default",
        Request::InputReserve {
            id: 1,
            instance: Some("test-instance".into()),
            pane: PaneId(1),
            retain_ms: 60_000,
        },
    )
    else {
        panic!("reserve failed")
    };
    input_request(
        &mut h,
        "default",
        Request::InputSubmit {
            id: 2,
            instance: Some("test-instance".into()),
            operation: receipt.operation,
            keys: "pending".into(),
        },
    );
    assert!(
        matches!(transfer_request(&mut h, request.clone()), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    h.step(vec![Inbound::InputCompleted {
        pane: PaneId(1),
        operation: receipt.operation,
        bytes_written: 7,
        error: None,
    }]);
    assert!(matches!(
        input_request(
            &mut h,
            "default",
            Request::FixWorkspace {
                id: 3,
                instance: Some("test-instance".into()),
                stream: stream + 1,
                pane: PaneId(1),
            }
        ),
        Reply::Failed { .. }
    ));
    assert!(matches!(
        input_request(
            &mut h,
            "default",
            Request::FixWorkspace {
                id: 4,
                instance: Some("test-instance".into()),
                stream,
                pane: PaneId(1),
            }
        ),
        Reply::Completed { .. }
    ));
    assert!(
        matches!(transfer_request(&mut h, request), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict && error.message.contains("fixed"))
    );
    assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
}

#[test]
fn transfer_creates_workspace_without_a_replacement_process_and_rolls_back_rejections() {
    use fux::proto::control::{PaneDestination, WorkspaceDestination, WorkspaceTransfer};
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    let transfer = WorkspaceTransfer {
        focus: false,
        follow: Some(viewer),
        instance: "test-instance".into(),
        source: TabId(1),
        generation: h.last_frame(viewer).layout_generation,
        pane: PaneId(1),
        workspace: WorkspaceDestination::New {
            name: "moved".into(),
        },
        destination: PaneDestination::NewTab {
            label: Some("preserved".into()),
        },
        side: fux::layout::Direction::Right,
    };
    for invalid in [
        WorkspaceTransfer {
            focus: false,
            follow: Some(ViewerId(999)),
            ..transfer.clone()
        },
        WorkspaceTransfer {
            focus: false,
            workspace: WorkspaceDestination::New {
                name: "default".into(),
            },
            ..transfer.clone()
        },
        WorkspaceTransfer {
            focus: false,
            workspace: WorkspaceDestination::New {
                name: "../invalid".into(),
            },
            ..transfer.clone()
        },
        WorkspaceTransfer {
            focus: false,
            pane: PaneId(999),
            ..transfer.clone()
        },
    ] {
        let start = h.effects.len();
        assert!(matches!(
            transfer_request(&mut h, invalid),
            Reply::Failed { .. }
        ));
        assert!(!h.effects[start..].iter().any(|effect| matches!(
            effect,
            Effect::WorkspaceOpened { .. } | Effect::SpawnPane { .. }
        )));
        assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
    }
    let start = h.effects.len();
    let reply = transfer_request(&mut h, transfer);
    assert!(matches!(reply, Reply::Completed { .. }), "{reply:?}");
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::ReleasePane { .. }
    )));
    assert_eq!(
        h.effects[start..]
            .iter()
            .filter(
                |effect| matches!(effect, Effect::WorkspaceOpened { name, .. } if name == "moved")
            )
            .count(),
        1
    );
    assert_eq!(h.last_frame(viewer).workspace, "moved");
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
    assert_eq!(h.last_frame(viewer).exit_code, None);
    let viewer = h.attach("moved", 24, 80);
    assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
    assert_eq!(h.last_frame(viewer).tabs[0].label, "preserved");
    let stream = workspace_stream(&mut h, "moved");
    input_request(
        &mut h,
        "moved",
        Request::FixWorkspace {
            id: 2,
            instance: Some("test-instance".into()),
            stream,
            pane: PaneId(1),
        },
    );
    let start = h.effects.len();
    let blocked = WorkspaceTransfer {
        focus: false,
        follow: None,
        instance: "test-instance".into(),
        source: h.last_frame(viewer).active_tab.unwrap(),
        generation: h.last_frame(viewer).layout_generation,
        pane: PaneId(1),
        workspace: WorkspaceDestination::New {
            name: "blocked".into(),
        },
        destination: PaneDestination::NewTab { label: None },
        side: fux::layout::Direction::Right,
    };
    assert!(matches!(
        transfer_request(&mut h, blocked),
        Reply::Failed { .. }
    ));
    assert!(!h.effects[start..].iter().any(|effect| matches!(
        effect,
        Effect::WorkspaceOpened { .. } | Effect::SpawnPane { .. }
    )));
    assert!(matches!(
        input_request(
            &mut h,
            "blocked",
            Request::List {
                id: 3,
                instance: None
            }
        ),
        Reply::Failed { .. }
    ));
    assert_eq!(h.pane_ids(viewer), vec![PaneId(1)]);
}

#[test]
fn archive_labels_validate_all_tabs_before_commit_and_detect_concurrent_renames() {
    fn request(h: &mut Harness, action: ManagerAction) -> ManagerOutcome {
        h.step(vec![Inbound::Manager { action, token: 994 }]);
        h.manager.last().unwrap().1.clone()
    }
    fn export(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        let ManagerOutcome::LayoutArchive(archive) = request(h, ManagerAction::ExportLayout) else {
            panic!("export failed")
        };
        archive
    }
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
    let original = export(&mut h);
    let mut desired = original.clone();
    desired.workspaces[0].tabs[0].labels = vec![(PaneId(1), "first".into())];
    desired.workspaces[0].tabs[1].labels = vec![(PaneId(2), "second".into())];
    for labels in [
        vec![(PaneId(1), "wrong tab".into())],
        vec![(PaneId(2), "bad\n".into())],
        vec![(PaneId(2), "a".into()), (PaneId(2), "b".into())],
    ] {
        let mut invalid = desired.clone();
        invalid.workspaces[0].tabs[1].labels = labels;
        assert!(!matches!(
            request(
                &mut h,
                ManagerAction::ApplyLayout {
                    expected: original.clone(),
                    archive: invalid
                }
            ),
            ManagerOutcome::LayoutArchive(_)
        ));
        assert_eq!(export(&mut h), original);
    }
    let effects = h.effects.len();
    let ManagerOutcome::LayoutArchive(applied) = request(
        &mut h,
        ManagerAction::ApplyLayout {
            expected: original.clone(),
            archive: desired.clone(),
        },
    ) else {
        panic!("apply failed")
    };
    assert_eq!(
        applied.workspaces[0].tabs[0].labels,
        desired.workspaces[0].tabs[0].labels
    );
    assert_eq!(
        applied.workspaces[0].tabs[1].labels,
        desired.workspaces[0].tabs[1].labels
    );
    assert!(applied.workspaces[0].tabs[0].generation > original.workspaces[0].tabs[0].generation);
    assert!(
        !h.effects[effects..]
            .iter()
            .any(|effect| matches!(effect, Effect::ResizePty { .. } | Effect::SpawnPane { .. }))
    );
    assert_eq!(export(&mut h), applied);
    h.control(
        viewer,
        Request::RenamePane {
            id: 2,
            instance: Some("test-instance".into()),
            pane: PaneId(2),
            name: "concurrent".into(),
        },
    );
    assert!(!matches!(
        request(
            &mut h,
            ManagerAction::ApplyLayout {
                expected: applied,
                archive: original.clone()
            }
        ),
        ManagerOutcome::LayoutArchive(_)
    ));
    assert_eq!(
        h.last_frame(viewer).panes[&PaneId(2)].label.as_deref(),
        Some("concurrent")
    );
    let current = export(&mut h);
    assert!(matches!(
        request(
            &mut h,
            ManagerAction::ApplyLayout {
                expected: current,
                archive: original
            }
        ),
        ManagerOutcome::LayoutArchive(_)
    ));
    assert!(
        export(&mut h).workspaces[0]
            .tabs
            .iter()
            .all(|tab| tab.labels.is_empty())
    );
}

#[test]
fn archive_focus_fallback_uses_tree_order_not_serialized_node_order() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let a = h.attach("default", 24, 80);
    h.control(a, split(1, Axis::Horizontal));
    h.complete_spawns();
    let b = h.attach("default", 24, 80);
    h.control(
        b,
        Request::Focus {
            id: 1,
            instance: None,
            target: FocusTarget::Pane(PaneId(1)),
        },
    );
    h.control(
        a,
        Request::Tab {
            id: 2,
            instance: None,
            action: TabAction::New {
                name: Some("second".into()),
            },
        },
    );
    h.complete_spawns();
    h.create_workspace("other");
    h.step(vec![Inbound::Manager {
        action: ManagerAction::ExportLayout,
        token: 981,
    }]);
    let before = match &h.manager.last().unwrap().1 {
        ManagerOutcome::LayoutArchive(archive) => archive.clone(),
        other => panic!("archive: {other:?}"),
    };
    let mut desired = before.clone();
    for tab in &mut desired.workspaces[0].tabs {
        for node in &mut tab.document.nodes {
            if let fux::layout::LayoutNode::Pane { pane } = node {
                *pane = match *pane {
                    PaneId(1) => PaneId(3),
                    PaneId(3) => PaneId(1),
                    other => other,
                };
            }
        }
        tab.focused = Some(if tab.id == TabId(1) {
            PaneId(3)
        } else {
            PaneId(1)
        });
        if tab.id == TabId(1) {
            tab.document.nodes.swap(1, 2);
            if let fux::layout::LayoutNode::Split { first, second, .. } = &mut tab.document.nodes[0]
            {
                std::mem::swap(first, second);
            }
        }
    }
    desired.workspaces.reverse();
    h.step(vec![Inbound::Manager {
        action: ManagerAction::ApplyLayout {
            expected: before,
            archive: desired,
        },
        token: 982,
    }]);
    assert!(matches!(
        &h.manager.last().unwrap().1,
        ManagerOutcome::LayoutArchive(_)
    ));
    assert_eq!(h.last_frame(b).active_tab, Some(TabId(1)));
    assert_eq!(h.last_frame(b).focused, Some(PaneId(3)));
    h.step(vec![Inbound::Manager {
        action: ManagerAction::ExportLayout,
        token: 983,
    }]);
    let ManagerOutcome::LayoutArchive(archive) = &h.manager.last().unwrap().1 else {
        panic!("archive missing")
    };
    assert_eq!(archive.workspaces[0].name, "other");
}

#[test]
fn archive_apply_restores_order_membership_and_selection_atomically() {
    fn request(h: &mut Harness, action: ManagerAction) -> ManagerOutcome {
        h.step(vec![Inbound::Manager { action, token: 989 }]);
        h.manager.last().unwrap().1.clone()
    }
    fn export(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        match request(h, ManagerAction::ExportLayout) {
            ManagerOutcome::LayoutArchive(archive) => archive,
            other => panic!("archive: {other:?}"),
        }
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    h.control(
        viewer,
        Request::Tab {
            id: 2,
            instance: None,
            action: TabAction::New {
                name: Some("second".into()),
            },
        },
    );
    h.complete_spawns();
    let before = export(&mut h);
    let mut desired = before.clone();
    let workspace = &mut desired.workspaces[0];
    workspace.selected = Some(TabId(1));
    for tab in &mut workspace.tabs {
        for node in &mut tab.document.nodes {
            if let fux::layout::LayoutNode::Pane { pane } = node {
                *pane = match *pane {
                    PaneId(1) => PaneId(3),
                    PaneId(3) => PaneId(1),
                    other => other,
                };
            }
        }
        tab.focused = Some(if tab.id == TabId(1) {
            PaneId(3)
        } else {
            PaneId(1)
        });
        tab.zoomed = (tab.id == TabId(1)).then_some(PaneId(3));
        tab.label = format!("restored-{}", tab.id);
    }
    workspace.tabs.reverse();
    for mutation in 0..5 {
        let mut invalid = desired.clone();
        match mutation {
            0 => invalid.workspaces[0].tabs[1].focused = Some(PaneId(999)),
            1 => invalid.workspaces[0].tabs[1].document.root = Some(999),
            2 => invalid.workspaces[0].stream += 1,
            3 => {
                invalid.workspaces[0].tabs.pop();
            }
            _ => invalid.workspaces[0].tabs[1].zoomed = Some(PaneId(999)),
        }
        assert!(matches!(
            request(
                &mut h,
                ManagerAction::ApplyLayout {
                    expected: before.clone(),
                    archive: invalid
                }
            ),
            ManagerOutcome::Failed(_)
        ));
        assert_eq!(export(&mut h), before);
    }
    let effects = h.effects.len();
    assert!(matches!(
        request(
            &mut h,
            ManagerAction::ApplyLayout {
                expected: before.clone(),
                archive: desired
            }
        ),
        ManagerOutcome::LayoutArchive(_)
    ));
    assert!(
        !h.effects[effects..]
            .iter()
            .any(|effect| matches!(effect, Effect::SpawnPane { .. }))
    );
    let after = export(&mut h);
    assert_eq!(after.workspaces[0].selected, Some(TabId(1)));
    assert_eq!(
        after.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        vec![TabId(2), TabId(1)]
    );
    assert_eq!(after.workspaces[0].tabs[1].zoomed, Some(PaneId(3)));
    assert_eq!(h.last_frame(viewer).active_tab, Some(TabId(2)));
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
    assert!(matches!(
        request(
            &mut h,
            ManagerAction::ApplyLayout {
                expected: before,
                archive: after.clone()
            }
        ),
        ManagerOutcome::Failed(_)
    ));
    let effects = h.effects.len();
    assert!(matches!(
        request(
            &mut h,
            ManagerAction::ApplyLayout {
                expected: after.clone(),
                archive: after.clone()
            }
        ),
        ManagerOutcome::LayoutArchive(_)
    ));
    assert_eq!(export(&mut h), after);
    assert!(
        !h.effects[effects..]
            .iter()
            .any(|effect| matches!(effect, Effect::ResizePty { .. } | Effect::SpawnPane { .. }))
    );
}

#[test]
fn manager_layout_archive_is_ordered_complete_and_read_only() {
    fn export(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        let effects = h.step(vec![Inbound::Manager {
            action: ManagerAction::ExportLayout,
            token: 990,
        }]);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::SpawnPane { .. } | Effect::ResizePty { .. } | Effect::WorkspaceOpened { .. }
        )));
        match &h.manager.last().unwrap().1 {
            ManagerOutcome::LayoutArchive(archive) => archive.clone(),
            other => panic!("archive: {other:?}"),
        }
    }
    let mut h = Harness::new();
    h.create_workspace("alpha");
    let viewer = h.attach("alpha", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    input_request(
        &mut h,
        "alpha",
        Request::Focus {
            id: 10,
            instance: None,
            target: FocusTarget::Pane(PaneId(1)),
        },
    );
    h.control(
        viewer,
        Request::Layout {
            id: 1,
            instance: None,
            tab: TabId(1),
            generation: Some(h.last_frame(viewer).layout_generation),
            action: fux::proto::control::LayoutAction::Zoom {
                pane: Some(PaneId(2)),
            },
        },
    );
    h.create_workspace("beta");
    h.control(
        viewer,
        Request::Tab {
            id: 2,
            instance: None,
            action: TabAction::New {
                name: Some("extra".into()),
            },
        },
    );
    h.complete_spawns();
    let extra = h.last_frame(viewer).active_tab.unwrap();
    h.control(
        viewer,
        Request::Tab {
            id: 3,
            instance: None,
            action: TabAction::Reorder {
                tab: extra,
                before: Some(TabId(1)),
            },
        },
    );
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Reorder {
            name: "beta".into(),
            before: Some("alpha".into()),
        },
        token: 991,
    }]);
    let archive = export(&mut h);
    assert_eq!(archive.version, 1);
    assert_eq!(archive.instance, "test-instance");
    assert_eq!(
        archive
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect::<Vec<_>>(),
        vec!["beta", "alpha"]
    );
    let alpha = &archive.workspaces[1];
    assert_eq!(
        alpha.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
        vec![extra, TabId(1)]
    );
    let main = &alpha.tabs[1];
    assert_eq!(main.zoomed, Some(PaneId(2)));
    assert_eq!(main.focused, Some(PaneId(1)));
    assert_eq!(
        main.document
            .nodes
            .iter()
            .filter(|node| matches!(node, fux::layout::LayoutNode::Pane { .. }))
            .count(),
        2
    );
    assert_eq!(export(&mut h), archive);
    assert_eq!(
        serde_json::from_slice::<fux::proto::control::LayoutArchive>(
            &serde_json::to_vec(&archive).unwrap()
        )
        .unwrap(),
        archive
    );
}

#[test]
fn manager_catalog_is_ordered_read_only_and_distinguishes_recreated_workspaces() {
    fn catalog(h: &mut Harness) -> fux::proto::control::WorkspaceCatalog {
        let effects = h.step(vec![Inbound::Manager {
            action: ManagerAction::Catalog,
            token: 991,
        }]);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::SpawnPane { .. } | Effect::WorkspaceOpened { .. }
        )));
        match &h.manager.last().unwrap().1 {
            ManagerOutcome::Catalog(catalog) => catalog.clone(),
            other => panic!("catalog: {other:?}"),
        }
    }
    let mut h = Harness::new();
    h.create_workspace("alpha");
    h.create_workspace("beta");
    let before = catalog(&mut h);
    assert_eq!(before.instance, "test-instance");
    assert_eq!(
        before
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    let old_stream = before.entries[0].stream;
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Reorder {
            name: "beta".into(),
            before: Some("alpha".into()),
        },
        token: 992,
    }]);
    assert_eq!(catalog(&mut h).entries[0].name, "beta");
    h.step(vec![Inbound::Manager {
        action: ManagerAction::Kill {
            name: "alpha".into(),
        },
        token: 993,
    }]);
    assert!(
        catalog(&mut h)
            .entries
            .iter()
            .all(|entry| entry.name != "alpha")
    );
    h.step(vec![
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 0,
        },
        Inbound::PaneEof { pane: PaneId(1) },
    ]);
    h.create_workspace("alpha");
    assert_ne!(
        catalog(&mut h)
            .entries
            .iter()
            .find(|entry| entry.name == "alpha")
            .unwrap()
            .stream,
        old_stream
    );
}

#[test]
fn pane_geometry_inspection_is_coherent_read_only_and_matches_focus() {
    use fux::proto::control::{LayoutAction, PaneGeometry};
    fn request(tab: u32, pane: u32) -> Request {
        Request::Layout {
            id: 42,
            instance: None,
            tab: TabId(tab),
            generation: None,
            action: LayoutAction::Inspect { pane: PaneId(pane) },
        }
    }
    fn inspect(h: &mut Harness, pane: u32) -> Box<PaneGeometry> {
        let Reply::Completed {
            result: CommandResult::PaneGeometry { geometry },
            ..
        } = input_request(h, "default", request(1, pane))
        else {
            panic!("inspection failed")
        };
        geometry
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.control(viewer, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, document) = exported_layout(&mut h);
    let before = h.last_frame(viewer).clone();
    let effect_start = h.effects.len();
    let left = inspect(&mut h, 1);
    let right = inspect(&mut h, 2);
    assert_eq!(left.generation, generation);
    assert_eq!(left.document, document);
    assert_eq!(left.instance, "test-instance");
    assert_eq!(left.area, left.navigation_area);
    assert_eq!(left.rect, left.visible_rect.unwrap());
    assert_eq!(left.neighbors.right, Some(PaneId(2)));
    assert_eq!(right.neighbors.left, Some(PaneId(1)));
    assert_eq!(left.neighbors.left, None);
    assert_eq!(left.neighbors.up, None);
    assert_eq!(left.neighbors.down, None);
    assert!(left.edges.left && left.edges.up && left.edges.down && !left.edges.right);
    assert!(right.edges.right && right.edges.up && right.edges.down && !right.edges.left);
    assert_eq!(h.last_frame(viewer), &before);
    assert!(!h.effects[effect_start..].iter().any(|effect| matches!(
        effect,
        Effect::ResizePty { .. }
            | Effect::SpawnPane { .. }
            | Effect::Terminate { .. }
            | Effect::ReleasePane { .. }
    )));
    assert_eq!(exported_layout(&mut h), (generation, document.clone()));
    h.control(
        viewer,
        Request::Focus {
            id: 1,
            instance: None,
            target: FocusTarget::Pane(PaneId(1)),
        },
    );
    h.control(
        viewer,
        Request::Focus {
            id: 2,
            instance: None,
            target: FocusTarget::Right,
        },
    );
    assert_eq!(h.last_frame(viewer).focused, left.neighbors.right);
    assert!(matches!(
        input_request(
            &mut h,
            "default",
            layout_request(
                Some(generation),
                LayoutAction::Zoom {
                    pane: Some(PaneId(2))
                }
            )
        ),
        Reply::Completed { .. }
    ));
    let hidden = inspect(&mut h, 1);
    let zoomed = inspect(&mut h, 2);
    assert_eq!(hidden.rect, left.rect);
    assert_eq!(hidden.visible_rect, None);
    assert_eq!(hidden.neighbors, left.neighbors);
    assert_eq!(zoomed.rect, right.rect);
    assert_eq!(zoomed.visible_rect, Some(zoomed.area));
    assert_eq!(zoomed.zoomed, Some(PaneId(2)));
    assert_eq!(zoomed.document, document);
    h.create_workspace("other");
    for (tab, pane) in [(1, 999), (1, 3), (2, 3)] {
        assert!(
            matches!(input_request(&mut h, "default", request(tab, pane)),
            Reply::Failed { error, .. } if error.code == ErrorCode::NotFound)
        );
    }
    let mut stale = request(1, 1);
    if let Request::Layout { instance, .. } = &mut stale {
        *instance = Some("replacement".into());
    }
    assert!(
        matches!(input_request(&mut h, "default", stale), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    h.request(viewer, ViewerRequest::Resize { rows: 0, cols: 0 });
    let empty = inspect(&mut h, 1);
    assert_eq!(empty.visible_rect, None);
    assert!(!empty.edges.left && !empty.edges.right && !empty.edges.up && !empty.edges.down);
    assert_eq!(empty.navigation_area.width, 1000);
    assert_eq!(empty.neighbors.right, Some(PaneId(2)));
}

#[test]
fn last_focus_is_private_toggles_across_workspaces_and_rejects_removed_targets() {
    fn focus(h: &mut Harness, viewer: ViewerId, target: FocusTarget) {
        h.control(
            viewer,
            Request::Focus {
                id: 71,
                instance: None,
                target,
            },
        );
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let alice = h.attach("default", 24, 80);
    let bob = h.attach("default", 24, 80);
    h.control(alice, split(1, Axis::Horizontal));
    h.complete_spawns();
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(1)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(2)));
    focus(&mut h, bob, FocusTarget::Last);
    assert_eq!(
        h.last_frame(bob).focused,
        Some(PaneId(1)),
        "Alice changed Bob's history"
    );
    assert!(
        matches!(h.replies(bob).last(), Some(Reply::Failed { error, .. }) if error.code == ErrorCode::NotFound)
    );

    h.step(
        [
            FocusTarget::Pane(PaneId(1)),
            FocusTarget::Next,
            FocusTarget::Last,
            FocusTarget::Previous,
        ]
        .into_iter()
        .map(|target| Inbound::ViewerRequest {
            viewer: alice,
            request: ViewerRequest::Control(Request::Focus {
                id: 70,
                instance: None,
                target,
            }),
        })
        .collect(),
    );
    assert_eq!(
        h.last_frame(alice).focused,
        Some(PaneId(2)),
        "ordered traversal/last lost history before a frame"
    );
    // Repeating explicit focus on the current pane must not replace the previous target.
    focus(&mut h, alice, FocusTarget::Pane(PaneId(2)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(1)));
    h.control(
        alice,
        Request::Tab {
            id: 1,
            instance: None,
            action: TabAction::New {
                name: Some("other-tab".into()),
            },
        },
    );
    h.complete_spawns();
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(3)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).active_tab, Some(TabId(1)));
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(1)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(3)));
    h.create_workspace("other");
    h.control(
        alice,
        Request::Workspace {
            id: 1,
            instance: None,
            stream: None,
            action: WorkspaceAction::Select {
                name: "other".into(),
            },
        },
    );
    assert_eq!(h.last_frame(alice).workspace, "other");
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(4)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).workspace, "default");
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(3)));
    focus(&mut h, alice, FocusTarget::Last);
    assert_eq!(h.last_frame(alice).workspace, "other");
    assert_eq!(h.last_frame(alice).focused, Some(PaneId(4)));
    assert_eq!(h.last_frame(bob).workspace, "default");
    assert_eq!(h.last_frame(bob).focused, Some(PaneId(1)));
    // A newly attached viewer inherits a selection, not a previous viewer's history.
    let fresh = h.attach("other", 24, 80);
    focus(&mut h, fresh, FocusTarget::Last);
    assert_eq!(h.last_frame(fresh).workspace, "other");
    assert_eq!(h.last_frame(fresh).focused, Some(PaneId(4)));
    // Remove Alice's previous pane; a new tab cannot replace its generational handle.
    input_request(
        &mut h,
        "default",
        Request::Kill {
            id: 1,
            instance: None,
            pane: PaneId(3),
        },
    );
    h.control(
        bob,
        Request::Tab {
            id: 1,
            instance: None,
            action: TabAction::New { name: None },
        },
    );
    h.complete_spawns();
    let before = h.last_frame(alice).clone();
    let effects = h.control(
        alice,
        Request::Focus {
            id: 71,
            instance: None,
            target: FocusTarget::Last,
        },
    );
    assert_eq!(h.last_frame(alice).focused, before.focused);
    assert_eq!(h.last_frame(alice).workspace, before.workspace);
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::WriteInput { .. }
    )));
}

#[test]
fn last_focus_tracks_batched_workspace_switches_and_respects_viewer_limits() {
    let mut h = Harness::new();
    for name in ["default", "second", "third"] {
        h.create_workspace(name);
    }
    let viewer = h.attach("default", 24, 80);
    h.step(
        ["second", "third"]
            .into_iter()
            .map(|name| Inbound::ViewerRequest {
                viewer,
                request: ViewerRequest::Control(Request::Workspace {
                    id: 80,
                    instance: None,
                    stream: None,
                    action: WorkspaceAction::Select { name: name.into() },
                }),
            })
            .collect(),
    );
    let last = || Request::Focus {
        id: 81,
        instance: None,
        target: FocusTarget::Last,
    };
    h.control(viewer, last());
    assert_eq!(h.last_frame(viewer).workspace, "second");
    h.control(viewer, last());
    assert_eq!(h.last_frame(viewer).workspace, "third");
    for _ in 0..fux::proto::attach::MAX_VIEWERS_PER_WORKSPACE {
        h.attach("second", 24, 80);
    }
    h.control(viewer, last());
    assert!(
        matches!(h.replies(viewer).last(), Some(Reply::Failed { error, .. }) if error.code == ErrorCode::Limit)
    );
    assert_eq!(h.last_frame(viewer).workspace, "third");
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(3)));
    // Selecting the current workspace remains a no-op even when it is full.
    for _ in 1..fux::proto::attach::MAX_VIEWERS_PER_WORKSPACE {
        h.attach("third", 24, 80);
    }
    h.control(
        viewer,
        Request::Workspace {
            id: 82,
            instance: None,
            stream: None,
            action: WorkspaceAction::Select {
                name: "third".into(),
            },
        },
    );
    assert!(!matches!(
        h.replies(viewer).last(),
        Some(Reply::Failed { .. })
    ));
}

#[test]
fn last_focus_observes_shared_zoom_and_preserves_workspace_socket_scope() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let a = h.attach("default", 24, 80);
    let b = h.attach("default", 24, 80);
    h.control(a, split(1, Axis::Horizontal));
    h.complete_spawns();
    let (generation, _) = exported_layout(&mut h);
    input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            fux::proto::control::LayoutAction::Zoom {
                pane: Some(PaneId(2)),
            },
        ),
    );
    assert_eq!(h.last_frame(b).focused, Some(PaneId(2)));
    h.control(
        b,
        Request::Focus {
            id: 1,
            instance: None,
            target: FocusTarget::Last,
        },
    );
    assert_eq!(h.last_frame(b).focused, Some(PaneId(1)));
    assert_eq!(h.last_frame(b).zoomed, None);
    // Workspace control has its own default-selection history, never viewer cross-route authority.
    let result = input_request(
        &mut h,
        "default",
        Request::Focus {
            id: 1,
            instance: None,
            target: FocusTarget::Last,
        },
    );
    assert!(matches!(
        result,
        Reply::Completed {
            result: CommandResult::Pane { pane: PaneId(2) },
            ..
        }
    ));
}

#[test]
fn right_click_policy_updates_all_viewers_and_follows_the_live_pane() {
    use fux::proto::control::{PaneDestination, WorkspaceDestination, WorkspaceTransfer};
    use fux::view::RightClickPolicy;
    let mut h = Harness::new();
    h.create_workspace("default");
    let a = h.attach("default", 24, 80);
    let b = h.attach("default", 24, 80);
    let request = |policy| Request::PaneInput {
        id: 83,
        instance: Some("test-instance".into()),
        pane: PaneId(1),
        right_click: policy,
    };
    let generation = h.last_frame(a).layout_generation;
    let effects = h.control(a, request(RightClickPolicy::Fux));
    for viewer in [a, b] {
        assert_eq!(
            h.last_frame(viewer).panes[&PaneId(1)].right_click,
            RightClickPolicy::Fux
        );
        assert_eq!(h.last_frame(viewer).layout_generation, generation + 1);
    }
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::WriteInput { .. }
            | Effect::ResizePty { .. }
            | Effect::SpawnPane { .. }
            | Effect::Terminate { .. }
    )));
    h.request(
        a,
        ViewerRequest::View {
            request: 85,
            pane: PaneId(1),
            offset: 0,
        },
    );
    assert!(h.messages[&a].iter().any(|message| matches!(message,
        ServerMessage::View { reply } if reply.request == 85 && reply.view.as_ref().is_some_and(|view| view.right_click == RightClickPolicy::Fux))));
    h.control(a, request(RightClickPolicy::Fux));
    assert_eq!(
        h.last_frame(a).layout_generation,
        generation + 1,
        "unchanged policy is a no-op"
    );
    h.control(a, request(RightClickPolicy::Auto));
    assert_eq!(
        h.last_frame(b).panes[&PaneId(1)].right_click,
        RightClickPolicy::Auto,
        "default clears retained metadata"
    );
    let mut split = split(1, Axis::Horizontal);
    if let Request::Split { right_click, .. } = &mut split {
        *right_click = RightClickPolicy::Pane;
    }
    h.control(a, split);
    h.complete_spawns();
    assert_eq!(
        h.last_frame(b).panes[&PaneId(2)].right_click,
        RightClickPolicy::Pane
    );
    let generation = h.last_frame(a).layout_generation;
    let reply = transfer_request(
        &mut h,
        WorkspaceTransfer {
            focus: false,
            follow: Some(a),
            instance: "test-instance".into(),
            source: TabId(1),
            generation,
            pane: PaneId(2),
            workspace: WorkspaceDestination::New {
                name: "moved".into(),
            },
            destination: PaneDestination::NewTab { label: None },
            side: fux::layout::Direction::Right,
        },
    );
    assert!(matches!(reply, Reply::Completed { .. }));
    assert_eq!(
        h.last_frame(a).panes[&PaneId(2)].right_click,
        RightClickPolicy::Pane
    );
    let request = Request::PaneInput {
        id: 84,
        instance: Some("test-instance".into()),
        pane: PaneId(2),
        right_click: RightClickPolicy::Auto,
    };
    // A retained source socket cannot mutate the moved pane.
    assert!(
        matches!(input_request(&mut h, "default", request.clone()), Reply::Failed { error, .. } if error.code == ErrorCode::NotFound)
    );
    let mut stale = request.clone();
    if let Request::PaneInput { instance, .. } = &mut stale {
        *instance = Some("replaced".into());
    }
    assert!(
        matches!(input_request(&mut h, "moved", stale), Reply::Failed { error, .. } if error.code == ErrorCode::Conflict)
    );
    assert!(matches!(
        input_request(&mut h, "moved", request),
        Reply::Completed { .. }
    ));
    assert_eq!(
        h.last_frame(a).panes[&PaneId(2)].right_click,
        RightClickPolicy::Auto
    );
}

#[test]
fn split_options_preserve_focus_input_and_zoom_and_apply_initial_ratio() {
    use fux::proto::control::LayoutAction;
    let mut h = Harness::new();
    h.create_workspace("default");
    let a = h.attach("default", 24, 80);
    let b = h.attach("default", 24, 80);
    let options = |axis, ratio, focus| {
        let mut request = split(1, axis);
        if let Request::Split {
            ratio: r, focus: f, ..
        } = &mut request
        {
            *r = ratio;
            *f = focus;
        }
        request
    };
    for ratio in [0, 499, 9501, u16::MAX] {
        let effects = h.control(a, options(Axis::Horizontal, ratio, false));
        assert!(
            matches!(h.replies(a).last(), Some(Reply::Failed { error, .. }) if error.code == ErrorCode::InvalidRequest)
        );
        assert!(h.pending_spawns.is_empty());
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::SpawnPane { .. } | Effect::ResizePty { .. } | Effect::WriteInput { .. }
        )));
    }
    h.step(vec![
        Inbound::ViewerRequest {
            viewer: a,
            request: ViewerRequest::Control(options(Axis::Horizontal, 7000, false)),
        },
        Inbound::ViewerRequest {
            viewer: a,
            request: ViewerRequest::Input(b"stay".to_vec()),
        },
    ]);
    assert!(h.written.is_empty());
    h.complete_spawns();
    assert_eq!(h.written, vec![(PaneId(1), b"stay".to_vec())]);
    for viewer in [a, b] {
        assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
        assert_eq!(
            h.last_frame(viewer)
                .layout
                .iter()
                .map(|entry| entry.rect.width)
                .collect::<Vec<_>>(),
            vec![55, 24]
        );
    }
    let (generation, document) = exported_layout(&mut h);
    assert!(
        document
            .nodes
            .iter()
            .any(|node| matches!(node, fux::layout::LayoutNode::Split { ratio: 7000, .. }))
    );
    input_request(
        &mut h,
        "default",
        layout_request(
            Some(generation),
            LayoutAction::Zoom {
                pane: Some(PaneId(1)),
            },
        ),
    );
    h.control(a, options(Axis::Vertical, 500, false));
    h.complete_spawns();
    assert_eq!(h.last_frame(a).zoomed, Some(PaneId(1)));
    assert_eq!(h.last_frame(a).focused, Some(PaneId(1)));
    let (_, document) = exported_layout(&mut h);
    assert!(
        document
            .nodes
            .iter()
            .any(|node| matches!(node, fux::layout::LayoutNode::Split { ratio: 500, .. }))
    );
    // Explicit focus creation reveals the new pane without retargeting the other viewer.
    h.control(a, options(Axis::Vertical, 9500, true));
    h.complete_spawns();
    assert_eq!(h.last_frame(a).zoomed, None);
    assert_eq!(h.last_frame(a).focused, Some(PaneId(4)));
    assert_eq!(h.last_frame(b).focused, Some(PaneId(1)));
}

#[test]
fn split_options_focus_selects_hidden_tab_for_workspace_control_only() {
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
    assert_eq!(h.last_frame(viewer).active_tab, Some(TabId(2)));
    for (focus, expected_tab) in [(false, TabId(2)), (true, TabId(1))] {
        let mut request = split(2, Axis::Horizontal);
        if let Request::Split {
            target,
            focus: value,
            ..
        } = &mut request
        {
            *target = Some(PaneId(1));
            *value = focus;
        }
        h.step(vec![Inbound::ControlRequest {
            workspace: "default".into(),
            request,
            token: 901,
        }]);
        h.complete_spawns();
        let fresh = h.attach("default", 24, 80);
        assert_eq!(h.last_frame(fresh).active_tab, Some(expected_tab));
        assert_eq!(
            h.last_frame(fresh).focused,
            Some(if focus { PaneId(4) } else { PaneId(2) })
        );
        assert_eq!(
            h.last_frame(viewer).active_tab,
            Some(TabId(2)),
            "workspace controls must not select a viewer's tab"
        );
    }
}

#[test]
fn transfer_ratio_keeps_target_share_on_every_side_and_rejects_before_mutation() {
    use fux::layout::Direction;
    use fux::proto::control::{
        LayoutAction, PaneDestination, WorkspaceDestination, WorkspaceTransfer,
    };
    for cross in [false, true] {
        for side in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let mut h = Harness::new();
            h.create_workspace("default");
            let source_viewer = h.attach("default", 24, 80);
            h.control(source_viewer, split(1, Axis::Horizontal));
            h.complete_spawns();
            let destination_viewer = if cross {
                h.create_workspace("other");
                h.attach("other", 24, 80)
            } else {
                h.control(
                    source_viewer,
                    Request::Tab {
                        id: 1,
                        instance: None,
                        action: TabAction::New { name: None },
                    },
                );
                h.complete_spawns();
                source_viewer
            };
            let destination_generation = h.last_frame(destination_viewer).layout_generation;
            let stream = h.last_frame(destination_viewer).workspace_stream;
            let source_generation = exported_layout(&mut h).0;
            let before_location = pane_location(&mut h, "test-instance", PaneId(2));
            let source_before = exported_layout(&mut h);
            let destination_before = h.last_frame(destination_viewer).clone();
            let apply = |h: &mut Harness, ratio| {
                let destination = PaneDestination::Tab {
                    tab: TabId(2),
                    generation: destination_generation,
                    target: PaneId(3),
                    ratio,
                };
                if cross {
                    transfer_request(
                        h,
                        WorkspaceTransfer {
                            focus: false,
                            follow: None,
                            instance: "test-instance".into(),
                            source: TabId(1),
                            generation: source_generation,
                            pane: PaneId(2),
                            workspace: WorkspaceDestination::Existing {
                                name: "other".into(),
                                stream,
                            },
                            destination,
                            side,
                        },
                    )
                } else {
                    input_request(
                        h,
                        "default",
                        layout_request(
                            Some(source_generation),
                            LayoutAction::Transfer {
                                focus: false,
                                pane: PaneId(2),
                                destination,
                                side,
                            },
                        ),
                    )
                }
            };
            for ratio in [0, 499, 9501, u16::MAX] {
                let start = h.effects.len();
                assert!(
                    matches!(apply(&mut h, ratio), Reply::Failed { error, .. } if error.code == ErrorCode::InvalidRequest)
                );
                assert_eq!(exported_layout(&mut h), source_before);
                assert_eq!(
                    h.last_frame(destination_viewer).layout,
                    destination_before.layout
                );
                assert!(!h.effects[start..].iter().any(|e| matches!(
                    e,
                    Effect::SpawnPane { .. } | Effect::Terminate { .. } | Effect::ResizePty { .. }
                )));
            }
            assert!(matches!(apply(&mut h, 7000), Reply::Completed { .. }));
            let frame = h.last_frame(destination_viewer);
            let target = frame
                .layout
                .iter()
                .find(|entry| entry.pane == PaneId(3))
                .unwrap()
                .rect;
            let moved = frame
                .layout
                .iter()
                .find(|entry| entry.pane == PaneId(2))
                .unwrap()
                .rect;
            match side {
                Direction::Left | Direction::Right => {
                    // Integer rounding assigns the remainder to the second child.
                    assert!((55..=56).contains(&target.width));
                    assert!((23..=24).contains(&moved.width));
                    assert_eq!(target.width + moved.width + 1, 80);
                    assert_eq!(moved.x < target.x, side == Direction::Left);
                }
                Direction::Up | Direction::Down => {
                    assert!((15..=16).contains(&target.height));
                    assert!((6..=7).contains(&moved.height));
                    assert_eq!(target.height + moved.height + 1, 23);
                    assert_eq!(moved.y < target.y, side == Direction::Up);
                }
            }
            let after_location = pane_location(&mut h, "test-instance", PaneId(2));
            let (
                Reply::Completed {
                    result: CommandResult::PaneLocation { location: before },
                    ..
                },
                Reply::Completed {
                    result: CommandResult::PaneLocation { location: after },
                    ..
                },
            ) = (before_location, after_location)
            else {
                panic!("pane location");
            };
            assert_eq!(before.pid, after.pid);
            assert_eq!(before.origin_workspace, after.origin_workspace);
            assert_eq!(after.tab, TabId(2));
        }
    }
}

#[test]
fn transfer_focus_respects_requester_scope_zoom_and_atomic_history() {
    use fux::proto::control::{LayoutAction, PaneDestination};
    for focus in [false, true] {
        for attached in [false, true] {
            let mut h = Harness::new();
            h.create_workspace("default");
            let viewer = h.attach("default", 24, 80);
            h.control(viewer, split(1, Axis::Horizontal));
            h.complete_spawns();
            h.control(
                viewer,
                Request::Tab {
                    id: 1,
                    instance: None,
                    action: TabAction::New { name: None },
                },
            );
            h.complete_spawns();
            let observer = h.attach("default", 24, 80);
            h.control(
                viewer,
                Request::Focus {
                    id: 1,
                    instance: None,
                    target: FocusTarget::Pane(PaneId(2)),
                },
            );
            let destination_generation = h.last_frame(observer).layout_generation;
            input_request(
                &mut h,
                "default",
                Request::Layout {
                    id: 1,
                    instance: Some("test-instance".into()),
                    tab: TabId(2),
                    generation: Some(destination_generation),
                    action: LayoutAction::Zoom {
                        pane: Some(PaneId(3)),
                    },
                },
            );
            let destination_generation = h.last_frame(observer).layout_generation;
            assert_eq!(
                h.last_frame(observer).zoomed,
                Some(PaneId(3)),
                "zoom setup failed"
            );
            let source_generation = exported_layout(&mut h).0;
            let request = |generation| {
                layout_request(
                    Some(source_generation),
                    LayoutAction::Transfer {
                        focus,
                        pane: PaneId(2),
                        destination: PaneDestination::Tab {
                            tab: TabId(2),
                            generation,
                            target: PaneId(3),
                            ratio: 5000,
                        },
                        side: fux::layout::Direction::Right,
                    },
                )
            };
            // Failed requests cannot alter either focus or zoom.
            h.control(viewer, request(destination_generation + 1));
            assert!(
                matches!(h.replies(viewer).last(), Some(Reply::Failed {error,..}) if error.code == ErrorCode::Conflict)
            );
            assert_eq!(h.last_frame(viewer).focused, Some(PaneId(2)));
            assert_eq!(h.last_frame(observer).zoomed, Some(PaneId(3)));
            if attached {
                h.control(viewer, request(destination_generation));
            } else {
                assert!(matches!(
                    input_request(&mut h, "default", request(destination_generation)),
                    Reply::Completed { .. }
                ));
            }
            assert_eq!(
                h.last_frame(observer).zoomed,
                if focus { None } else { Some(PaneId(3)) }
            );
            assert_eq!(h.last_frame(observer).focused, Some(PaneId(3)));
            assert_eq!(
                h.last_frame(viewer).focused,
                Some(if focus && attached {
                    PaneId(2)
                } else {
                    PaneId(1)
                })
            );
            let fresh = h.attach("default", 24, 80);
            assert_eq!(
                h.last_frame(fresh).focused,
                Some(if focus { PaneId(2) } else { PaneId(1) })
            );
            if focus && attached {
                h.control(
                    viewer,
                    Request::Focus {
                        id: 1,
                        instance: None,
                        target: FocusTarget::Last,
                    },
                );
                assert_eq!(
                    h.last_frame(viewer).focused,
                    Some(PaneId(3)),
                    "atomic transfer recorded a transient source fallback"
                );
            }
        }
    }
}

#[test]
fn transfer_focus_and_following_keep_other_viewers_private_and_enforce_admission() {
    use fux::proto::control::{
        LayoutAction, PaneDestination, WorkspaceDestination, WorkspaceTransfer,
    };
    for focus in [false, true] {
        for follow in [false, true] {
            let mut h = Harness::new();
            h.create_workspace("default");
            let source_viewer = h.attach("default", 24, 80);
            h.control(source_viewer, split(1, Axis::Horizontal));
            h.complete_spawns();
            h.create_workspace("other");
            let observer = h.attach("other", 24, 80);
            let generation = h.last_frame(observer).layout_generation;
            input_request(
                &mut h,
                "other",
                Request::Layout {
                    id: 1,
                    instance: Some("test-instance".into()),
                    tab: TabId(2),
                    generation: Some(generation),
                    action: LayoutAction::Zoom {
                        pane: Some(PaneId(3)),
                    },
                },
            );
            assert_eq!(
                h.last_frame(observer).zoomed,
                Some(PaneId(3)),
                "zoom setup failed"
            );
            let source_generation = exported_layout(&mut h).0;
            let transfer = WorkspaceTransfer {
                focus,
                follow: follow.then_some(source_viewer),
                instance: "test-instance".into(),
                source: TabId(1),
                generation: source_generation,
                pane: PaneId(2),
                workspace: WorkspaceDestination::Existing {
                    name: "other".into(),
                    stream: h.last_frame(observer).workspace_stream,
                },
                destination: PaneDestination::Tab {
                    tab: TabId(2),
                    generation: h.last_frame(observer).layout_generation,
                    target: PaneId(3),
                    ratio: 5000,
                },
                side: fux::layout::Direction::Right,
            };
            assert!(matches!(
                transfer_request(&mut h, transfer),
                Reply::Completed { .. }
            ));
            assert_eq!(h.last_frame(observer).focused, Some(PaneId(3)));
            assert_eq!(
                h.last_frame(observer).zoomed,
                if focus || follow {
                    None
                } else {
                    Some(PaneId(3))
                }
            );
            assert_eq!(
                h.last_frame(source_viewer).workspace,
                if follow { "other" } else { "default" }
            );
            assert_eq!(
                h.last_frame(source_viewer).focused,
                Some(if follow { PaneId(2) } else { PaneId(1) })
            );
            let fresh = h.attach("other", 24, 80);
            assert_eq!(
                h.last_frame(fresh).focused,
                Some(if focus || follow {
                    PaneId(2)
                } else {
                    PaneId(3)
                })
            );
        }
    }
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.create_workspace("full");
    let observer = h.attach("full", 24, 80);
    for _ in 1..fux::proto::attach::MAX_VIEWERS_PER_WORKSPACE {
        h.attach("full", 24, 80);
    }
    let before = exported_layout(&mut h);
    let transfer = WorkspaceTransfer {
        focus: true,
        follow: Some(viewer),
        instance: "test-instance".into(),
        source: TabId(1),
        generation: before.0,
        pane: PaneId(1),
        workspace: WorkspaceDestination::Existing {
            name: "full".into(),
            stream: h.last_frame(observer).workspace_stream,
        },
        destination: PaneDestination::NewTab { label: None },
        side: fux::layout::Direction::Right,
    };
    assert!(
        matches!(transfer_request(&mut h, transfer), Reply::Failed {error,..} if error.code == ErrorCode::Limit)
    );
    assert_eq!(exported_layout(&mut h), before);
    assert_eq!(h.last_frame(viewer).workspace, "default");
}
