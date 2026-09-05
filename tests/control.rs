#![allow(
    dead_code,
    unused_imports,
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used
)]

#[path = "../src/control/mod.rs"]
mod control;

use control::{
    AgentStatus, Axis, ClientIdentity, CommandResult, ErrorCode, Event, EventKind, EventQueue,
    FocusTarget, PeerAuthorization, PublishOutcome, Reply, ReplyState, Request, TabAction,
    WorkspaceAction, bind_control_socket, control_socket_path, decode_key_bytes,
    decode_request_frame, error_reply, read_request, write_frame,
};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

#[test]
fn every_command_has_a_strict_bounded_json_schema_and_echoes_its_id() {
    // Phase F5 socket: every documented command carries the request id.
    let requests = vec![
        Request::New {
            id: 1,
            cwd: None,
            argv: vec!["sh".into()],
            env: BTreeMap::new(),
        },
        Request::Split {
            id: 2,
            axis: Axis::Horizontal,
            target: Some(1),
            argv: vec!["sh".into()],
            env: BTreeMap::new(),
        },
        Request::Focus {
            id: 3,
            target: FocusTarget::Left,
        },
        Request::Zoom { id: 4, pane: None },
        Request::Kill { id: 5, pane: 1 },
        Request::Resize {
            id: 6,
            pane: 1,
            delta: 3,
        },
        Request::SendKeys {
            id: 7,
            pane: 1,
            keys: "hello\\n".into(),
        },
        Request::Capture {
            id: 8,
            pane: 1,
            attrs: true,
            scrollback: 20,
            max_bytes: 4096,
        },
        Request::List { id: 9 },
        Request::Tab {
            id: 10,
            action: TabAction::Next,
        },
        Request::Workspace {
            id: 11,
            action: WorkspaceAction::List,
        },
        Request::SetStatus {
            id: 12,
            segment: "agent".into(),
            text: "idle".into(),
        },
        Request::Popup {
            id: 13,
            rows: Some(20),
            cols: Some(60),
            argv: vec!["fzf".into()],
            env: BTreeMap::new(),
        },
        Request::Subscribe {
            id: 14,
            events: vec![EventKind::AgentState],
        },
    ];
    for request in requests {
        let decoded = decode_request_frame(&serde_json::to_vec(&request).expect("serialize"))
            .expect("decode");
        assert_eq!(decoded, request);
        assert_eq!(decoded.id(), request.id());
    }
}

#[test]
fn malformed_and_unknown_requests_return_structured_errors_without_losing_known_ids() {
    // Phase F5 socket: failures are replyable and do not require closing the stream.
    let unknown = decode_request_frame(br#"{"command":"future","id":41}"#).expect_err("unknown");
    assert_eq!(
        (unknown.id, unknown.code),
        (Some(41), ErrorCode::UnknownCommand)
    );
    let extra =
        decode_request_frame(br#"{"command":"list","id":42,"extra":true}"#).expect_err("extra");
    assert_eq!(extra.id, Some(42));
    assert_eq!(error_reply(&extra).id(), 42);
    let defaulted =
        decode_request_frame(br#"{"command":"new","id":43,"cwd":null,"argv":[],"env":{}}"#)
            .expect("empty argv selects the configured default");
    assert_eq!(defaulted.id(), 43);
    let bad = decode_request_frame(br#"{"command":"new","id":44,"cwd":null,"argv":[""],"env":{}}"#)
        .expect_err("argv entry");
    assert_eq!((bad.id, bad.code), (Some(44), ErrorCode::InvalidRequest));
}

#[test]
fn option_like_environment_names_are_rejected_before_spawning_env() {
    for name in ["-u", "--x"] {
        let frame = serde_json::to_vec(&Request::New {
            id: 47,
            cwd: None,
            argv: vec!["true".into()],
            env: BTreeMap::from([(name.to_owned(), "value".to_owned())]),
        })
        .expect("encode");
        let error = decode_request_frame(&frame).expect_err("option-like env name");
        assert_eq!(
            (error.id, error.code),
            (Some(47), ErrorCode::InvalidRequest)
        );
    }
    let portable = Request::New {
        id: 48,
        cwd: None,
        argv: vec!["true".into()],
        env: BTreeMap::from([("_FUX_2".to_owned(), "value".to_owned())]),
    };
    portable.validate().expect("portable environment name");
}

#[test]
fn maximum_capture_payload_always_fits_the_json_frame() {
    let request = Request::Capture {
        id: 45,
        pane: 1,
        attrs: true,
        scrollback: 0,
        max_bytes: control::MAX_CAPTURE_BYTES,
    };
    request.validate().expect("maximum capture");
    let too_large = Request::Capture {
        id: 46,
        pane: 1,
        attrs: true,
        scrollback: 0,
        max_bytes: control::MAX_CAPTURE_BYTES + 1,
    };
    assert!(too_large.validate().is_err());
    let reply = Reply::Completed {
        id: 45,
        result: CommandResult::Capture {
            text: "\u{1b}".repeat(control::MAX_CAPTURE_BYTES),
        },
    };
    assert!(serde_json::to_vec(&reply).expect("encode").len() <= control::MAX_FRAME_BYTES);
}

#[test]
fn popup_dimensions_are_rejected_before_any_host_side_spawn() {
    let error = decode_request_frame(
        br#"{"command":"popup","id":91,"rows":513,"cols":513,"argv":[],"env":{}}"#,
    )
    .expect_err("oversized popup");
    assert_eq!(
        (error.id, error.code),
        (Some(91), ErrorCode::InvalidRequest)
    );
}

#[test]
fn oversized_line_is_drained_so_the_following_request_remains_decodable() {
    // Phase F5 socket: one bad frame does not desynchronise the newline stream.
    let mut bytes = vec![b'x'; control::MAX_FRAME_BYTES + 1];
    bytes.extend_from_slice(b"\n{\"command\":\"list\",\"id\":9}\n");
    let mut reader = Cursor::new(bytes);
    assert_eq!(
        read_request(&mut reader).expect_err("large").code,
        ErrorCode::FrameTooLarge
    );
    assert_eq!(
        read_request(&mut reader).expect("read").expect("frame"),
        Request::List { id: 9 }
    );
}

#[test]
fn replies_and_every_event_are_newline_delimited_and_keep_subscription_ids() {
    // Phase F5 events: streamed messages use the subscription request id.
    let reply = Reply::Completed {
        id: 5,
        result: CommandResult::Unit,
    };
    assert_eq!(reply.state(), ReplyState::Completed);
    let mut output = Vec::new();
    write_frame(&mut output, &reply).expect("write");
    assert_eq!(output.last(), Some(&b'\n'));
    let events = [
        Event::PaneOpened {
            id: 77,
            pane: 1,
            command: vec!["sh".into()],
        },
        Event::PaneClosed {
            id: 77,
            pane: 1,
            exit_status: Some(0),
        },
        Event::PaneFocused { id: 77, pane: 1 },
        Event::PaneTitle {
            id: 77,
            pane: 1,
            title: "shell".into(),
        },
        Event::AgentState {
            id: 77,
            pane: 1,
            agent: Some("claude".into()),
            old_state: AgentStatus::Working,
            new_state: AgentStatus::Idle,
            timestamp_ms: 1,
        },
        Event::PaneOutput { id: 77, pane: 1 },
        Event::WorkspaceResized {
            id: 77,
            rows: 40,
            cols: 120,
        },
        Event::ClientAttached {
            id: 77,
            client: ClientIdentity::Local,
        },
        Event::ClientDetached {
            id: 77,
            client: ClientIdentity::Viewer(2),
        },
    ];
    assert_eq!(
        serde_json::to_value(&events[0])
            .expect("event JSON")
            .get("event")
            .and_then(serde_json::Value::as_str),
        Some("pane.opened")
    );
    assert_eq!(
        serde_json::to_string(&EventKind::AgentState).expect("event kind"),
        "\"agent.state\""
    );
    for event in events {
        assert_eq!(event.id(), 77);
        assert_eq!(
            serde_json::from_slice::<Event>(&serde_json::to_vec(&event).expect("encode"))
                .expect("decode"),
            event
        );
    }
}

#[test]
fn key_escape_decoder_rejects_incomplete_or_unknown_escapes() {
    // Phase F5 send-keys: escapes are defined once for all clients.
    assert_eq!(
        decode_key_bytes("a\\x1b\\nλ").expect("valid"),
        b"a\x1b\n\xce\xbb"
    );
    assert!(decode_key_bytes("\\x1").is_err());
    assert!(decode_key_bytes("\\q").is_err());
    assert!(decode_key_bytes("tail\\").is_err());
}

#[test]
fn output_is_shed_before_state_events_and_persistently_slow_clients_disconnect() {
    // Phase F5 backpressure: output is lossy; state is ordered or the client disconnects.
    let (queue, receiver) = EventQueue::bounded(2).expect("queue");
    assert_eq!(
        queue.publish(Event::PaneOutput { id: 1, pane: 1 }),
        PublishOutcome::Queued
    );
    assert_eq!(
        queue.publish(Event::PaneFocused { id: 1, pane: 1 }),
        PublishOutcome::Queued
    );
    assert_eq!(
        queue.publish(Event::PaneTitle {
            id: 1,
            pane: 1,
            title: "new".into()
        }),
        PublishOutcome::Queued
    );
    assert!(matches!(
        receiver.try_recv(),
        Some(Event::PaneFocused { .. })
    ));
    assert!(matches!(receiver.try_recv(), Some(Event::PaneTitle { .. })));
    queue.publish(Event::PaneFocused { id: 1, pane: 1 });
    queue.publish(Event::PaneTitle {
        id: 1,
        pane: 1,
        title: "x".into(),
    });
    assert_eq!(
        queue.publish(Event::PaneClosed {
            id: 1,
            pane: 1,
            exit_status: None
        }),
        PublishOutcome::DisconnectedSlowClient
    );
    assert!(receiver.is_disconnected());
}

#[test]
fn unix_socket_enforces_private_paths_and_conservative_stale_replacement() {
    // Phase F5 socket: 0700 directory, 0600 socket, no replacement of live/non-sockets.
    let root = test_directory();
    fs::create_dir(&root).expect("root");
    let bound = bind_control_socket(&root, "work").expect("bind");
    assert_eq!(
        bound.peer_authorization(),
        PeerAuthorization::OperatingSystemCredentials
    );
    assert_eq!(
        fs::metadata(root.join("fux"))
            .expect("dir")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let metadata = fs::metadata(bound.path()).expect("socket");
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        bind_control_socket(&root, "work").expect_err("live").kind(),
        std::io::ErrorKind::AddrInUse
    );
    drop(bound);
    let protected = bind_control_socket(&root, "protected").expect("bind protected");
    let protected_path = protected.path().to_owned();
    let moved_path = root.join("fux/protected.old");
    fs::rename(&protected_path, &moved_path).expect("move original socket");
    let replacement = UnixListener::bind(&protected_path).expect("bind replacement");
    drop(protected);
    assert!(
        fs::symlink_metadata(&protected_path)
            .expect("replacement preserved")
            .file_type()
            .is_socket()
    );
    drop(replacement);
    let stale_path = control_socket_path(&root, "stale").expect("path");
    drop(UnixListener::bind(&stale_path).expect("stale"));
    drop(bind_control_socket(&root, "stale").expect("replace"));
    let occupied = control_socket_path(&root, "file").expect("path");
    fs::write(&occupied, "keep").expect("file");
    assert_eq!(
        bind_control_socket(&root, "file").expect_err("file").kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&occupied).expect("preserved"), "keep");
    assert!(control_socket_path(&root, "../escape").is_err());
    fs::remove_dir_all(root).expect("cleanup");
}

fn test_directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!("fux-control-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    path
}
