//! Spawn completion ordering and workspace kill races.
use super::*;

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
        request: ManagerRequest::Resolve {
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
        request: ManagerRequest::Kill {
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
