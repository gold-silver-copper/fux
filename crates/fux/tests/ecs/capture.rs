//! Coherent captures, event replay and server identity guards.
use super::*;

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

/// A control-socket read refreshes the pane grid the viewer is waiting to be sent. Output paced
/// behind the frame interval must still reach the viewer when that read lands in the same step.
#[test]
fn a_control_read_during_paced_output_does_not_strand_the_viewer_frame() {
    let mut h = Harness::new();
    h.create_workspace("default");
    let viewer = h.attach("default", 24, 80);
    h.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"first\r\n".to_vec(),
    }]);
    let shows = |h: &Harness, text: &str| {
        h.last_frame(viewer).panes[&PaneId(1)]
            .text_between((0, 0), (23, 79))
            .contains(text)
    };
    assert!(shows(&h, "first"));
    // Inside the frame interval: the output is paced, and a capture reads the pane meanwhile.
    h.step_after(
        1,
        vec![
            Inbound::PaneOutput {
                pane: PaneId(1),
                bytes: b"second\r\n".to_vec(),
            },
            Inbound::ControlRequest {
                workspace: "default".into(),
                request: serde_json::from_value(serde_json::json!({
                    "command":"capture", "id":990, "instance":"test-instance", "pane":1,
                    "max_bytes":4096
                }))
                .expect("capture request"),
                token: 990,
            },
        ],
    );
    assert!(
        h.session.next_deadline_ms().is_some(),
        "paced output must schedule the viewer's frame"
    );
    h.step_after(20, Vec::new());
    assert!(
        shows(&h, "second"),
        "the viewer never received the paced output"
    );
}
