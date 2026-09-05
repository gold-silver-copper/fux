#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
use fux::local::{self, ClientMessage, ServerMessage, read_frame, write_frame};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

struct Endpoint {
    server: local::server::LocalEndpoint,
    root: PathBuf,
}
impl Endpoint {
    fn new() -> Self {
        Self::with_command(vec!["/bin/sh".into()], 0)
    }
    fn with_command(command: Vec<String>, history: usize) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(format!(
            "/tmp/flp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private directory");
        let host = fux::host::WorkspaceHost::spawn(command, history, None).expect("local host");
        let server = local::server::LocalEndpoint::bind(&root.join("attach.sock"), host)
            .expect("local endpoint");
        Self { server, root }
    }
    async fn peer(&self) -> UnixStream {
        UnixStream::connect(self.server.path())
            .await
            .expect("connect peer")
    }
    async fn healthy(&self) {
        let mut peer = self.peer().await;
        hello(&mut peer, local::VERSION).await;
        let reply: ServerMessage = read_frame(&mut peer, local::MAX_SERVER_FRAME)
            .await
            .expect("hello reply");
        assert!(matches!(
            reply,
            ServerMessage::Hello {
                version: local::VERSION
            }
        ));
        let reply: ServerMessage = read_frame(&mut peer, local::MAX_SERVER_FRAME)
            .await
            .expect("state reply");
        assert!(matches!(reply, ServerMessage::State { .. }));
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.server.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn hello(peer: &mut UnixStream, version: u32) {
    write_frame(
        peer,
        &ClientMessage::Hello {
            version,
            rows: 24,
            columns: 80,
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("hello");
}
async fn rejected(mut peer: UnixStream) {
    tokio::time::timeout(local::FRAME_TIMEOUT + Duration::from_secs(2), async {
        for _ in 0..8 {
            match read_frame::<_, ServerMessage>(&mut peer, local::MAX_SERVER_FRAME).await {
                Err(_) => return,
                Ok(ServerMessage::Error { .. }) => return,
                Ok(_) => {}
            }
        }
        panic!("invalid peer continued receiving frames");
    })
    .await
    .expect("peer rejection deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_frames_and_invalid_message_order_leave_other_viewers_usable() {
    let endpoint = Endpoint::new();
    for bytes in [vec![0, 0, 0, 0], vec![0xff; 4], vec![0, 0, 0, 1, b'{']] {
        let mut peer = endpoint.peer().await;
        peer.write_all(&bytes).await.expect("malformed frame");
        rejected(peer).await;
        endpoint.healthy().await;
    }
    for message in [
        ClientMessage::Input { bytes: vec![b'x'] },
        ClientMessage::Detach,
        ClientMessage::Hello {
            version: 0,
            rows: 24,
            columns: 80,
        },
    ] {
        let mut peer = endpoint.peer().await;
        write_frame(&mut peer, &message, local::MAX_CLIENT_FRAME)
            .await
            .expect("invalid first message");
        rejected(peer).await;
    }
    for message in [
        ClientMessage::Input {
            bytes: vec![b'x'; 4097],
        },
        ClientMessage::Hello {
            version: local::VERSION,
            rows: 24,
            columns: 80,
        },
    ] {
        let mut peer = endpoint.peer().await;
        hello(&mut peer, local::VERSION).await;
        write_frame(&mut peer, &message, local::MAX_CLIENT_FRAME)
            .await
            .expect("invalid established message");
        rejected(peer).await;
        endpoint.healthy().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_and_partial_handshakes_expire_and_release_all_connection_slots() {
    let endpoint = Endpoint::new();
    let mut peers = Vec::new();
    for index in 0..fux::daemon::MAX_CONNECTIONS_PER_WORKSPACE {
        let mut peer = endpoint.peer().await;
        if index % 3 == 0 {
            peer.write_all(&[0]).await.expect("partial header");
        }
        if index % 3 == 1 {
            peer.write_all(&[0, 0, 0, 20, b'{'])
                .await
                .expect("partial body");
        }
        peers.push(peer);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while endpoint.server.active_tasks() < fux::daemon::MAX_CONNECTIONS_PER_WORKSPACE {
        assert!(
            tokio::time::Instant::now() < deadline,
            "connections not accepted"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    rejected(endpoint.peer().await).await;
    let deadline = tokio::time::Instant::now() + local::FRAME_TIMEOUT + Duration::from_secs(2);
    while endpoint.server.active_tasks() != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stalled handshake leaked a connection slot"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    endpoint.healthy().await;
    drop(peers);
}

#[tokio::test]
async fn blocked_frame_writer_has_a_deadline() {
    let (mut writer, _unread) = tokio::io::duplex(1);
    let error = tokio::time::timeout(
        local::FRAME_TIMEOUT + Duration::from_secs(2),
        write_frame(
            &mut writer,
            &ClientMessage::Input {
                bytes: vec![1; 4096],
            },
            local::MAX_CLIENT_FRAME,
        ),
    )
    .await
    .expect("outer deadline")
    .expect_err("blocked write must end");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn private_copy_view_replies_are_correlated_and_do_not_change_other_viewers() {
    let endpoint = Endpoint::with_command(
        vec!["/bin/sh".into(), "-c".into(), "seq 1 80; sleep 10".into()],
        32,
    );
    let mut first = endpoint.peer().await;
    hello(&mut first, local::VERSION).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerMessage::State { state } = read_frame(&mut first, local::MAX_SERVER_FRAME)
                .await
                .expect("initial state")
                && state.panes().values().any(|pane| {
                    pane.cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("80")
                })
            {
                break;
            }
        }
    })
    .await
    .expect("output deadline");
    let mut second = endpoint.peer().await;
    hello(&mut second, local::VERSION).await;
    write_frame(
        &mut first,
        &ClientMessage::CopyView {
            request: 41,
            pane: 1,
            offset: 3,
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("history request");
    write_frame(
        &mut second,
        &ClientMessage::CopyView {
            request: 42,
            pane: 1,
            offset: 0,
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("live request");
    async fn copy_reply(peer: &mut UnixStream, expected: u64, pane: u32) -> local::CopyViewReply {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match read_frame(peer, local::MAX_SERVER_FRAME)
                    .await
                    .expect("copy reply frame")
                {
                    ServerMessage::CopyView { reply } => {
                        assert_eq!(reply.request, expected, "reply crossed viewer boundaries");
                        assert_eq!(reply.pane, pane);
                        return reply;
                    }
                    ServerMessage::State { state } => {
                        for pane in state.panes().values() {
                            assert_eq!(
                                pane.viewport_offset, 0,
                                "private scroll changed shared viewport"
                            );
                            assert!(!pane.copy.active);
                        }
                    }
                    ServerMessage::Hello { .. } => {}
                    message => panic!("unexpected reply: {message:?}"),
                }
            }
        })
        .await
        .expect("copy reply deadline")
    }
    let history = copy_reply(&mut first, 41, 1)
        .await
        .view
        .expect("history pane");
    let live = copy_reply(&mut second, 42, 1)
        .await
        .view
        .expect("live pane");
    assert_eq!(history.viewport_offset, 3);
    assert_eq!(live.viewport_offset, 0);
    assert_ne!(history.cells, live.cells);
    write_frame(
        &mut first,
        &ClientMessage::CopyView {
            request: 43,
            pane: 1,
            offset: u32::MAX,
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("oldest request");
    assert!(
        copy_reply(&mut first, 43, 1)
            .await
            .view
            .expect("oldest")
            .viewport_offset
            <= 32
    );
    write_frame(
        &mut first,
        &ClientMessage::CopyView {
            request: 44,
            pane: u32::MAX,
            offset: 0,
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("missing pane request");
    assert!(copy_reply(&mut first, 44, u32::MAX).await.view.is_none());
}

#[test]
fn copy_reply_rejects_invalid_pane_geometry_before_rendering() {
    let invalid = local::CopyViewReply {
        request: 7,
        pane: 1,
        view: Some(Box::new(fux::state::PaneView {
            rows: 2,
            columns: 2,
            cells: Vec::new(),
            ..Default::default()
        })),
    };
    let bytes = serde_json::to_vec(&ServerMessage::CopyView { reply: invalid })
        .expect("encode malformed frame");
    assert!(serde_json::from_slice::<ServerMessage>(&bytes).is_err());
    let missing = ServerMessage::CopyView {
        reply: local::CopyViewReply {
            request: 8,
            pane: 1,
            view: None,
        },
    };
    assert!(
        serde_json::from_slice::<ServerMessage>(
            &serde_json::to_vec(&missing).expect("encode missing pane")
        )
        .is_ok()
    );
}

#[tokio::test]
async fn terminal_input_at_frame_boundaries_cannot_consume_another_viewers_command_key() {
    let endpoint = Endpoint::with_command(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; cat".into(),
        ],
        0,
    );
    let mut first = endpoint.peer().await;
    hello(&mut first, local::VERSION).await;
    async fn text(peer: &mut UnixStream, needle: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match read_frame(peer, local::MAX_SERVER_FRAME)
                    .await
                    .expect("viewer frame")
                {
                    ServerMessage::State { state } => {
                        assert_eq!(
                            state.panes().len(),
                            1,
                            "terminal bytes executed a multiplexer command"
                        );
                        if state.panes().values().any(|pane| {
                            pane.cells
                                .iter()
                                .map(|cell| cell.text.as_str())
                                .collect::<String>()
                                .contains(needle)
                        }) {
                            break;
                        }
                    }
                    ServerMessage::Hello { .. } => {}
                    message => panic!("unexpected viewer message: {message:?}"),
                }
            }
        })
        .await
        .expect("pane output deadline");
    }
    text(&mut first, "READY").await;
    let mut second = endpoint.peer().await;
    hello(&mut second, local::VERSION).await;
    let mut bytes = vec![b'a'; 4095];
    bytes.push(1);
    write_frame(
        &mut first,
        &ClientMessage::PaneInput { bytes },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("frame-ending literal prefix");
    write_frame(
        &mut first,
        &ClientMessage::Control {
            request: fux::control::Request::Capture {
                id: 71,
                pane: 1,
                attrs: false,
                scrollback: 0,
                max_bytes: 256,
            },
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("input ordering barrier");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match read_frame(&mut first, local::MAX_SERVER_FRAME)
                .await
                .expect("barrier frame")
            {
                ServerMessage::Reply { reply } => {
                    assert_eq!(reply.id(), 71);
                    break;
                }
                ServerMessage::State { .. } => {}
                message => panic!("unexpected barrier response: {message:?}"),
            }
        }
    })
    .await
    .expect("input barrier deadline");
    write_frame(
        &mut second,
        &ClientMessage::Input {
            bytes: b"xINPUT_OK".to_vec(),
        },
        local::MAX_CLIENT_FRAME,
    )
    .await
    .expect("other viewer's ordinary x");
    text(&mut second, "xINPUT_OK").await;
    endpoint.healthy().await;
}
