//! Tracked input operations, receipts and manager creation.
use super::*;

#[test]
fn manager_create_rejects_reserved_and_existing_names_without_borrowing() {
    let mut h = Harness::new();
    h.create_workspace("default");
    h.step(vec![
        Inbound::Manager {
            request: ManagerRequest::Create {
                name: "owned".into(),
            },
            token: 600,
        },
        Inbound::Manager {
            request: ManagerRequest::Create {
                name: "owned".into(),
            },
            token: 601,
        },
    ]);
    assert_eq!(h.pending_spawns.len(), 1);
    assert!(h.manager.iter().any(|(token, result)| *token == 601
        && matches!(
            result,
            ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
        )));
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
        request: ManagerRequest::Create {
            name: "owned".into(),
        },
        token: 602,
    }]);
    assert!(matches!(
        h.manager.last(),
        Some((
            602,
            ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
        ))
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
