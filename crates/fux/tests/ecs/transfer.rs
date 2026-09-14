//! Cross-workspace transfers, pane location, focus and split options.
use super::*;

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
            request: ManagerRequest::ReleasePanePin {
                instance: instance.into(),
                pane,
                pid,
            },
        }]);
        let ManagerOutcome::Reply(ManagerReply::ReleasePanePin { result: reply }) =
            &h.manager.last().unwrap().1
        else {
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
    fn request(h: &mut Harness, action: ManagerRequest) -> ManagerOutcome {
        h.step(vec![Inbound::Manager {
            request: action,
            token: 994,
        }]);
        h.manager.last().unwrap().1.clone()
    }
    fn export(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        let ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) =
            request(h, ManagerRequest::ExportLayout)
        else {
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
                ManagerRequest::ApplyLayout {
                    expected: original.clone(),
                    archive: invalid
                }
            ),
            ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
        ));
        assert_eq!(export(&mut h), original);
    }
    let effects = h.effects.len();
    let ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: applied }) = request(
        &mut h,
        ManagerRequest::ApplyLayout {
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
            ManagerRequest::ApplyLayout {
                expected: applied,
                archive: original.clone()
            }
        ),
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
    ));
    assert_eq!(
        h.last_frame(viewer).panes[&PaneId(2)].label.as_deref(),
        Some("concurrent")
    );
    let current = export(&mut h);
    assert!(matches!(
        request(
            &mut h,
            ManagerRequest::ApplyLayout {
                expected: current,
                archive: original
            }
        ),
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
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
        request: ManagerRequest::ExportLayout,
        token: 981,
    }]);
    let before = match &h.manager.last().unwrap().1 {
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) => archive.clone(),
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
        request: ManagerRequest::ApplyLayout {
            expected: before,
            archive: desired,
        },
        token: 982,
    }]);
    assert!(matches!(
        &h.manager.last().unwrap().1,
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
    ));
    assert_eq!(h.last_frame(b).active_tab, Some(TabId(1)));
    assert_eq!(h.last_frame(b).focused, Some(PaneId(3)));
    h.step(vec![Inbound::Manager {
        request: ManagerRequest::ExportLayout,
        token: 983,
    }]);
    let ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) =
        &h.manager.last().unwrap().1
    else {
        panic!("archive missing")
    };
    assert_eq!(archive.workspaces[0].name, "other");
}

#[test]
fn archive_apply_restores_order_membership_and_selection_atomically() {
    fn request(h: &mut Harness, action: ManagerRequest) -> ManagerOutcome {
        h.step(vec![Inbound::Manager {
            request: action,
            token: 989,
        }]);
        h.manager.last().unwrap().1.clone()
    }
    fn export(h: &mut Harness) -> fux::proto::control::LayoutArchive {
        match request(h, ManagerRequest::ExportLayout) {
            ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) => archive,
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
                ManagerRequest::ApplyLayout {
                    expected: before.clone(),
                    archive: invalid
                }
            ),
            ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
        ));
        assert_eq!(export(&mut h), before);
    }
    let effects = h.effects.len();
    assert!(matches!(
        request(
            &mut h,
            ManagerRequest::ApplyLayout {
                expected: before.clone(),
                archive: desired
            }
        ),
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
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
            ManagerRequest::ApplyLayout {
                expected: before,
                archive: after.clone()
            }
        ),
        ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
    ));
    let effects = h.effects.len();
    assert!(matches!(
        request(
            &mut h,
            ManagerRequest::ApplyLayout {
                expected: after.clone(),
                archive: after.clone()
            }
        ),
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
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
            request: ManagerRequest::ExportLayout,
            token: 990,
        }]);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::SpawnPane { .. } | Effect::ResizePty { .. } | Effect::WorkspaceOpened { .. }
        )));
        match &h.manager.last().unwrap().1 {
            ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) => archive.clone(),
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
        request: ManagerRequest::Reorder {
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
            request: ManagerRequest::Catalog,
            token: 991,
        }]);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::SpawnPane { .. } | Effect::WorkspaceOpened { .. }
        )));
        match &h.manager.last().unwrap().1 {
            ManagerOutcome::Reply(ManagerReply::Catalog { catalog }) => catalog.clone(),
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
        request: ManagerRequest::Reorder {
            name: "beta".into(),
            before: Some("alpha".into()),
        },
        token: 992,
    }]);
    assert_eq!(catalog(&mut h).entries[0].name, "beta");
    h.step(vec![Inbound::Manager {
        request: ManagerRequest::Kill {
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
