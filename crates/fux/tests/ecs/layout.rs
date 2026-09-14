//! Layout edits, archives, zoom and tab order.
use super::*;

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
            request: ManagerRequest::ExportLayout,
            token: 994,
        }]);
        let ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive }) =
            h.manager.last().unwrap().1.clone()
        else {
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
        request: ManagerRequest::Catalog,
        token: 994,
    }]);
    let ManagerOutcome::Reply(ManagerReply::Catalog { catalog }) = &h.manager.last().unwrap().1
    else {
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
        request: ManagerRequest::ApplyLayout {
            expected: original.clone(),
            archive: original.clone(),
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
    ));
    assert_eq!(archive(&mut h), named);
    let mut invalid = original.clone();
    invalid.workspaces[0].label = Some("bad\nlabel".into());
    h.step(vec![Inbound::Manager {
        token: 994,
        request: ManagerRequest::ApplyLayout {
            expected: named.clone(),
            archive: invalid,
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
    ));
    assert_eq!(archive(&mut h), named);
    h.step(vec![Inbound::Manager {
        token: 994,
        request: ManagerRequest::ApplyLayout {
            expected: named,
            archive: original.clone(),
        },
    }]);
    assert!(matches!(
        h.manager.last().unwrap().1,
        ManagerOutcome::Reply(ManagerReply::LayoutArchive { archive: _ })
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
        request: ManagerRequest::Reorder {
            name: "gamma".into(),
            before: Some("alpha".into()),
        },
    }]);
    assert!(
        matches!(h.manager.last(), Some((801, ManagerOutcome::Reply(ManagerReply::Names { names }))) if names == &vec!["gamma".to_owned(), "alpha".to_owned(), "beta".to_owned()])
    );
    h.step(vec![Inbound::Manager {
        token: 802,
        request: ManagerRequest::Reorder {
            name: "alpha".into(),
            before: Some("missing".into()),
        },
    }]);
    assert!(matches!(
        h.manager.last(),
        Some((
            802,
            ManagerOutcome::Reply(ManagerReply::Failed { message: _ })
        ))
    ));
    h.step(vec![Inbound::Manager {
        token: 803,
        request: ManagerRequest::List,
    }]);
    assert!(
        matches!(h.manager.last(), Some((803, ManagerOutcome::Reply(ManagerReply::Names { names }))) if names == &vec!["gamma".to_owned(), "alpha".to_owned(), "beta".to_owned()])
    );
    assert_eq!(h.last_frame(viewer).workspace, "beta");
    assert_eq!(h.last_frame(viewer).focused, focused);
    h.step(vec![Inbound::Manager {
        token: 804,
        request: ManagerRequest::Reorder {
            name: "gamma".into(),
            before: None,
        },
    }]);
    assert!(
        matches!(h.manager.last(), Some((804, ManagerOutcome::Reply(ManagerReply::Names { names }))) if names == &vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()])
    );
}
