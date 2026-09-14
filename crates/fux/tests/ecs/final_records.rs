//! Retained final records: exit evidence, retention and expiry.
use super::*;

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
        request: ManagerRequest::Kill {
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
