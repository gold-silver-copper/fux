//! Workspaces, tabs, panes, viewers, frames and lifecycle.
use super::*;

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
fn exact_initial_attachment_selects_only_its_viewer_and_rejects_stale_identity() {
    use fux::proto::attach::InitialTarget;
    let mut h = Harness::new();
    h.create_workspace("default");
    let alice = h.attach("default", 24, 80);
    h.control(alice, split(1, Axis::Horizontal));
    h.complete_spawns();
    let bob = h.attach("default", 24, 80);
    assert_eq!(h.last_frame(bob).focused, Some(PaneId(2)));
    let initial = InitialTarget {
        instance: "test-instance".into(),
        workspace: "default".into(),
        stream: workspace_stream(&mut h, "default"),
        pane: PaneId(1),
        pid: 101,
    };
    let viewer = ViewerId(h.next_viewer);
    h.next_viewer += 1;
    h.step(vec![Inbound::ViewerAttached {
        viewer,
        workspace: "default".into(),
        rows: 24,
        cols: 80,
        initial: Some(initial.clone()),
    }]);
    assert_eq!(h.last_frame(viewer).focused, Some(PaneId(1)));
    assert_eq!(h.last_frame(bob).focused, Some(PaneId(2)));
    let ordinary = h.attach("default", 24, 80);
    assert_eq!(
        h.last_frame(ordinary).focused,
        Some(PaneId(2)),
        "exact attach changed workspace defaults"
    );
    h.request(viewer, ViewerRequest::Input(b"targeted".to_vec()));
    assert_eq!(h.written.last(), Some(&(PaneId(1), b"targeted".to_vec())));
    for field in 0..5 {
        let mut stale = initial.clone();
        match field {
            0 => stale.instance = "old".into(),
            1 => stale.workspace = "other".into(),
            2 => stale.stream += 1,
            3 => stale.pane = PaneId(99),
            _ => stale.pid += 1,
        }
        let rejected = ViewerId(h.next_viewer);
        h.next_viewer += 1;
        let written = h.written.len();
        h.step(vec![
            Inbound::ViewerAttached {
                viewer: rejected,
                workspace: "default".into(),
                rows: 24,
                cols: 80,
                initial: Some(stale),
            },
            Inbound::ViewerRequest {
                viewer: rejected,
                request: ViewerRequest::Input(b"must-not-deliver".to_vec()),
            },
        ]);
        assert_eq!(h.written.len(), written);
        assert!(h.messages.get(&rejected).is_some_and(|messages| {
            messages
                .iter()
                .any(|message| matches!(message, ServerMessage::Error { .. }))
        }));
        assert!(!h.messages.get(&rejected).is_some_and(|messages| {
            messages
                .iter()
                .any(|message| matches!(message, ServerMessage::Hello {}))
        }));
    }
    let written = h.written.len();
    h.step(vec![
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 0,
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"not-to-sibling".to_vec()),
        },
    ]);
    assert_eq!(
        h.written.len(),
        written,
        "exited target redirected input to its sibling"
    );
    assert!(h.messages.get(&viewer).is_some_and(|messages| {
        messages
            .iter()
            .any(|message| matches!(message, ServerMessage::Error { .. }))
    }));
}

#[test]
fn exact_attachment_closes_when_shared_zoom_hides_its_required_pane() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let controller = h.attach("default", 24, 80);
    h.control(controller, split(1, Axis::Horizontal));
    h.complete_spawns();
    let initial = fux::proto::attach::InitialTarget {
        instance: "test-instance".into(),
        workspace: "default".into(),
        stream: workspace_stream(&mut h, "default"),
        pane: PaneId(1),
        pid: 101,
    };
    let viewer = ViewerId(h.next_viewer);
    h.next_viewer += 1;
    h.step(vec![Inbound::ViewerAttached {
        viewer,
        workspace: "default".into(),
        rows: 24,
        cols: 80,
        initial: Some(initial),
    }]);
    h.control(
        controller,
        layout_request(
            Some(h.last_frame(controller).layout_generation),
            fux::proto::control::LayoutAction::Zoom {
                pane: Some(PaneId(2)),
            },
        ),
    );
    let count = h.written.len();
    h.request(
        viewer,
        ViewerRequest::Input(b"must-not-reach-zoomed-sibling".to_vec()),
    );
    assert_eq!(h.written.len(), count);
    assert!(h.messages.get(&viewer).is_some_and(|messages| {
        messages
            .iter()
            .any(|message| matches!(message, ServerMessage::Error { .. }))
    }));
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
        request: ManagerRequest::Resolve { name: None },
        token: 11,
    }]);
    assert!(matches!(
        harness.manager.last(),
        Some((11, ManagerOutcome::Attach { name, created: false, .. })) if name == "other"
    ));
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
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(effect, Effect::CloseViewer { .. }))
            .count(),
        1,
        "one close per overflowed viewer"
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
fn a_detaching_viewer_no_longer_counts_toward_the_workspace_limit() {
    let mut harness = Harness::new();
    harness.create_workspace("full");
    harness.create_workspace("other");
    let limit = fux::proto::attach::MAX_VIEWERS_PER_WORKSPACE;
    let residents: Vec<ViewerId> = (0..limit).map(|_| harness.attach("full", 24, 80)).collect();
    let mover = harness.attach("other", 24, 80);
    // The first resident detaches in the same step the mover selects the full workspace; the
    // detaching viewer is still an entity but no longer holds a seat.
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer: residents[0],
            request: ViewerRequest::Detach,
        },
        Inbound::ViewerRequest {
            viewer: mover,
            request: ViewerRequest::Control(Request::Workspace {
                stream: None,
                instance: None,
                id: 1,
                action: WorkspaceAction::Select {
                    name: "full".into(),
                },
            }),
        },
    ]);
    assert!(
        matches!(
            harness.replies(mover).last(),
            Some(Reply::Completed { id: 1, .. })
        ),
        "{:?}",
        harness.replies(mover).last()
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
            initial: None,
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
            initial: None,
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
            initial: None,
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
