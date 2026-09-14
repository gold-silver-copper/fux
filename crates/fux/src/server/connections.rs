//! Socket-facing tasks: attachment viewers, control clients and the manager socket. Each task
//! forwards typed inbound events to the owner loop and writes replies/frames it is handed.

use super::adapter::{Subscriber, ViewerOutbox};
use crate::daemon::{DaemonPaths, ManagerIdentity, ManagerReply, ManagerRequest};
use crate::ecs::{Inbound, ManagerOutcome, ViewerRequest};
use crate::ids::ViewerId;
use crate::proto::attach::{
    ClientMessage, FRAME_TIMEOUT, MAX_CLIENT_FRAME, MAX_INPUT_CHUNK, MAX_SERVER_FRAME,
    ServerMessage, read_frame, write_frame,
};
use crate::proto::control::{
    self, CONTROL_PREFACE, ErrorCode, MAX_FRAME_BYTES, MAX_SUBSCRIBER_QUEUE, Reply, Request,
};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinSet;

/// A reply channel or outbox a socket task hands the owner before sending the event that will
/// answer through it.
pub enum Register {
    ControlReply(u64, oneshot::Sender<Reply>),
    ManagerReply(u64, oneshot::Sender<ManagerOutcome>),
    Outbox(ViewerId, ViewerOutbox),
}

/// Shared by every accept loop: how to reach the owner and how to hand it reply channels.
#[derive(Clone)]
pub struct Owner {
    pub paths: DaemonPaths,
    pub identity: ManagerIdentity,
    pub inbound: mpsc::Sender<Inbound>,
    pub tokens: Arc<AtomicU64>,
    pub register: mpsc::Sender<Register>,
    pub viewer_ids: Arc<AtomicU64>,
}

impl Owner {
    fn token(&self) -> u64 {
        self.tokens.fetch_add(1, Ordering::Relaxed)
    }
    fn instance(&self) -> &str {
        &self.identity.instance_nonce
    }
}

fn authenticate(stream: UnixStream) -> std::io::Result<UnixStream> {
    let stream = stream.into_std()?;
    crate::proto::socket::authorize_peer(&stream)?;
    UnixStream::from_std(stream)
}

/// Accepts authenticated connections until `stop` fires or the listener fails; each admitted
/// stream is served on its own task. Returns the tasks still running.
async fn accept_loop<F>(
    listener: UnixListener,
    stop: &Notify,
    admit: impl Fn(&JoinSet<()>) -> bool,
    mut serve: impl FnMut(UnixStream) -> F,
) -> JoinSet<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.notified() => break,
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break };
                let Ok(stream) = authenticate(stream) else { continue };
                if !admit(&tasks) {
                    continue;
                }
                tasks.spawn(serve(stream));
            }
        }
    }
    tasks
}

async fn drain(mut tasks: JoinSet<()>) {
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

/// Accepts attachment connections for one workspace until `stop` fires.
pub async fn serve_attachments(
    listener: UnixListener,
    workspace: String,
    owner: Owner,
    stop: Arc<Notify>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    let mut tasks = accept_loop(
        listener,
        &stop,
        |_| active.load(Ordering::Acquire) < crate::proto::attach::MAX_VIEWERS_PER_WORKSPACE,
        |stream| {
            active.fetch_add(1, Ordering::AcqRel);
            let active = Arc::clone(&active);
            let owner = owner.clone();
            let workspace = workspace.clone();
            async move {
                if let Err(error) = serve_viewer(stream, workspace, owner).await {
                    tracing::debug!(%error, "viewer connection ended");
                }
                active.fetch_sub(1, Ordering::AcqRel);
            }
        },
    )
    .await;
    // Viewers were told to exit; let their final frames flush before cutting the connections.
    let grace = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(grace);
    while !tasks.is_empty() {
        tokio::select! {
            _ = tasks.join_next() => {}
            () = &mut grace => break,
        }
    }
    drain(tasks).await;
}

async fn serve_viewer(
    mut stream: UnixStream,
    workspace: String,
    owner: Owner,
) -> anyhow::Result<()> {
    let hello: ClientMessage =
        tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut stream, MAX_CLIENT_FRAME)).await??;
    let ClientMessage::Hello {
        rows,
        columns: cols,
        initial,
    } = hello
    else {
        write_frame(
            &mut stream,
            &ServerMessage::Error {
                message: "the first attachment frame must be a hello".into(),
            },
            MAX_SERVER_FRAME,
        )
        .await?;
        anyhow::bail!("attachment did not start with a hello");
    };
    let viewer = ViewerId(owner.viewer_ids.fetch_add(1, Ordering::Relaxed));
    let outbox = ViewerOutbox::default();
    owner
        .register
        .send(Register::Outbox(viewer, outbox.clone()))
        .await?;
    owner
        .inbound
        .send(Inbound::ViewerAttached {
            initial,
            viewer,
            workspace,
            rows,
            cols,
        })
        .await?;
    let (mut reader, mut writer) = stream.into_split();
    let writer_outbox = outbox.clone();
    let mut writer_task = tokio::spawn(async move {
        while let Some(message) = writer_outbox.next().await {
            let exit = matches!(
                message,
                ServerMessage::Exited { .. } | ServerMessage::Error { .. }
            );
            if write_frame(&mut writer, &message, MAX_SERVER_FRAME)
                .await
                .is_err()
            {
                break;
            }
            if exit {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    let inbound = owner.inbound.clone();
    let result = async {
        loop {
            let message: ClientMessage = read_frame(&mut reader, MAX_CLIENT_FRAME).await?;
            let request = match message {
                ClientMessage::Input { bytes } => {
                    anyhow::ensure!(bytes.len() <= MAX_INPUT_CHUNK, "oversized input chunk");
                    ViewerRequest::Input(bytes)
                }
                ClientMessage::Mouse { event, generation } => {
                    anyhow::ensure!(event.column > 0 && event.row > 0, "invalid mouse report");
                    ViewerRequest::Mouse { event, generation }
                }
                ClientMessage::Control { request } => ViewerRequest::Control(request),
                ClientMessage::View {
                    request,
                    pane,
                    offset,
                } => ViewerRequest::View {
                    request,
                    pane,
                    offset,
                },
                ClientMessage::Resize { rows, columns } => ViewerRequest::Resize {
                    rows,
                    cols: columns,
                },
                ClientMessage::Detach => {
                    inbound
                        .send(Inbound::ViewerRequest {
                            viewer,
                            request: ViewerRequest::Detach,
                        })
                        .await?;
                    // The owner answers with `exited`; the writer task ends after sending it.
                    return Ok::<(), anyhow::Error>(());
                }
                ClientMessage::Hello { .. } => anyhow::bail!("duplicate hello"),
            };
            inbound
                .send(Inbound::ViewerRequest { viewer, request })
                .await?;
        }
    };
    let outcome = tokio::select! {
        result = result => result,
        _ = &mut writer_task => Ok(()),
    };
    // Detach: wait for the exit frame to flush; otherwise the peer vanished.
    let detached = outcome.is_ok();
    if detached {
        let _ = tokio::time::timeout(FRAME_TIMEOUT, &mut writer_task).await;
    }
    writer_task.abort();
    let _ = owner.inbound.send(Inbound::ViewerGone { viewer }).await;
    outbox.close();
    outcome
}

/// Accepts control connections for one workspace until `stop` fires.
pub async fn serve_control(
    listener: UnixListener,
    workspace: String,
    owner: Owner,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    stop: Arc<Notify>,
) {
    let tasks = accept_loop(
        listener,
        &stop,
        |tasks| tasks.len() < control::MAX_CONTROL_CONNECTIONS,
        |stream| {
            let owner = owner.clone();
            let workspace = workspace.clone();
            let subscribers = Arc::clone(&subscribers);
            async move {
                if let Err(error) =
                    serve_control_connection(stream, workspace, owner, subscribers).await
                {
                    tracing::debug!(%error, "control connection ended");
                }
            }
        },
    )
    .await;
    drain(tasks).await;
}

/// Preface exchange with a two-second absolute deadline including idle time.
pub async fn negotiate(stream: &mut UnixStream) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut preface = [0_u8; CONTROL_PREFACE.len()];
        stream.read_exact(&mut preface).await?;
        stream.write_all(CONTROL_PREFACE).await?;
        anyhow::ensure!(&preface == CONTROL_PREFACE, "not a fux control preface");
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("control negotiation timed out"))?
}

async fn read_line(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let limit = MAX_FRAME_BYTES + 1;
    let count = tokio::time::timeout(Duration::from_secs(30), async {
        (&mut *reader)
            .take(limit as u64)
            .read_until(b'\n', &mut line)
            .await
    })
    .await
    .map_err(|_| anyhow::anyhow!("control frame stalled"))??;
    if count == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() > MAX_FRAME_BYTES {
        anyhow::bail!("control frame exceeds limit");
    }
    Ok(Some(line))
}

async fn serve_control_connection(
    mut stream: UnixStream,
    workspace: String,
    owner: Owner,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
) -> anyhow::Result<()> {
    negotiate(&mut stream).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    while let Some(line) = read_line(&mut reader).await? {
        let request = match control::decode_request_frame(&line) {
            Ok(request) => request,
            Err(error) => {
                write_line(&mut writer, &control::error_reply(&error)).await?;
                continue;
            }
        };
        if request
            .instance()
            .is_some_and(|instance| instance != owner.instance())
        {
            write_line(
                &mut writer,
                &Reply::failed(
                    request.id(),
                    control::ErrorCode::Conflict,
                    "server instance changed; rediscover before retrying",
                ),
            )
            .await?;
            continue;
        }
        if let Request::Subscribe { id, after, .. } = request {
            return serve_subscription(reader, writer, subscribers, owner, workspace, id, after)
                .await;
        }
        let reply = dispatch_control(&owner, &workspace, request).await?;
        write_reply(&mut writer, reply).await?;
    }
    Ok(())
}

/// Registers the subscriber, replays from `after` through the ECS, then streams queued events
/// until the peer writes another byte or closes.
async fn serve_subscription(
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    owner: Owner,
    workspace: String,
    id: u64,
    after: Option<control::EventCursor>,
) -> anyhow::Result<()> {
    let (sender, mut receiver) = mpsc::channel(MAX_SUBSCRIBER_QUEUE);
    {
        let mut active = crate::os::lock(&subscribers);
        active.retain(|subscriber| !subscriber.sender.is_closed());
        active.push(Subscriber {
            sender,
            bytes: Arc::new(AtomicUsize::new(0)),
        });
    }
    // Register first, then replay through the authoritative ECS boundary. Events that race
    // this read are either in the replay or in the queue (possibly both).
    let mut boundary = after;
    let mut replay = Vec::new();
    if let Some(after) = after {
        let reply = dispatch_control(
            &owner,
            &workspace,
            Request::Events {
                id,
                instance: Some(owner.instance().to_owned()),
                after,
            },
        )
        .await?;
        match reply {
            Reply::Completed {
                result:
                    control::CommandResult::Events {
                        cursor,
                        events: entries,
                    },
                ..
            } => {
                boundary = Some(cursor);
                replay = entries;
            }
            other => return write_reply(&mut writer, other).await,
        }
    }
    write_line(&mut writer, &Reply::Accepted { id }).await?;
    for entry in replay {
        write_event(&mut writer, entry, id).await?;
    }
    let mut probe = [0_u8; 1];
    loop {
        tokio::select! {
            event = receiver.recv() => {
                let Some(event) = event else { break };
                if boundary.is_some_and(|cursor| cursor.stream == event.entry.cursor.stream
                    && event.entry.cursor.sequence <= cursor.sequence) {
                    continue;
                }
                write_event(&mut writer, (*event.entry).clone(), id).await?;
            }
            read = reader.read(&mut probe) => {
                // Any further byte or EOF ends the subscription.
                let _ = read;
                break;
            }
        }
    }
    Ok(())
}

async fn dispatch_control(
    owner: &Owner,
    workspace: &str,
    request: Request,
) -> anyhow::Result<Reply> {
    let request_id = request.id();
    // Every request is answered within the fixed 30 s window or failed with its own id.
    let answer_window = Duration::from_secs(30);
    let token = owner.token();
    let (sender, receiver) = oneshot::channel();
    owner
        .register
        .send(Register::ControlReply(token, sender))
        .await?;
    owner
        .inbound
        .send(Inbound::ControlRequest {
            workspace: workspace.to_owned(),
            request,
            token,
        })
        .await?;
    let reply = match tokio::time::timeout(answer_window, receiver).await {
        Ok(Ok(reply)) => reply,
        _ => Reply::failed(
            request_id,
            ErrorCode::Internal,
            "control request was not answered",
        ),
    };

    Ok(reply)
}

async fn write_event(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    mut event: control::SequencedEvent,
    id: u64,
) -> anyhow::Result<()> {
    event.event = event.event.with_id(id);
    write_line(writer, &event).await
}

/// Writes a reply, or the `frame-too-large` failure that stands in for one the frame limit
/// cannot carry.
async fn write_reply(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    reply: Reply,
) -> anyhow::Result<()> {
    let bytes = match control::encode_line(&reply) {
        Ok(bytes) => bytes,
        Err(_) => control::encode_line(&Reply::failed(
            reply.id(),
            ErrorCode::FrameTooLarge,
            "control response exceeds the 1 MiB frame limit",
        ))?,
    };
    tokio::time::timeout(FRAME_TIMEOUT, writer.write_all(&bytes)).await??;
    Ok(())
}

/// Writes one newline-delimited JSON frame within the frame limit and timeout.
async fn write_line<T: serde::Serialize>(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &T,
) -> anyhow::Result<()> {
    let bytes = control::encode_line(value)?;
    tokio::time::timeout(FRAME_TIMEOUT, writer.write_all(&bytes)).await??;
    Ok(())
}

/// Serves the manager socket: one negotiated request per connection.
pub async fn serve_manager(listener: UnixListener, owner: Owner, stop: Arc<Notify>) {
    let tasks = accept_loop(
        listener,
        &stop,
        |tasks| tasks.len() < 64,
        |stream| {
            let owner = owner.clone();
            async move {
                if let Err(error) = serve_manager_connection(stream, owner).await {
                    tracing::debug!(%error, "manager connection ended");
                }
            }
        },
    )
    .await;
    drain(tasks).await;
}

async fn serve_manager_connection(mut stream: UnixStream, owner: Owner) -> anyhow::Result<()> {
    negotiate(&mut stream).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let Some(line) = read_line(&mut reader).await? else {
        return Ok(());
    };
    let request: ManagerRequest = match serde_json::from_slice(&line) {
        Ok(request) => request,
        Err(error) => {
            let reply = ManagerReply::Failed {
                message: format!("invalid manager request: {error}"),
            };
            return write_line(&mut writer, &reply).await;
        }
    };
    if let ManagerRequest::Resolve { name: Some(name) }
    | ManagerRequest::Kill { name }
    | ManagerRequest::Create { name } = &request
        && let Err(error) = crate::ids::validate_workspace_name(name)
    {
        let reply = ManagerReply::Failed {
            message: error.to_string(),
        };
        return write_line(&mut writer, &reply).await;
    }
    let token = owner.token();
    let (sender, receiver) = oneshot::channel();
    owner
        .register
        .send(Register::ManagerReply(token, sender))
        .await?;
    owner
        .inbound
        .send(Inbound::Manager { request, token })
        .await?;
    let outcome = match tokio::time::timeout(crate::daemon::MANAGER_DEADLINE, receiver).await {
        Ok(Ok(outcome)) => outcome,
        _ => ManagerOutcome::failed("manager request was not answered"),
    };
    let reply = match outcome {
        ManagerOutcome::Reply(reply) => reply,
        ManagerOutcome::Attach { name, stream, .. } => {
            match super::adapter::descriptor(&owner.paths, &owner.identity, &name, stream) {
                Ok(descriptor) => ManagerReply::Attach { descriptor },
                Err(_) => ManagerReply::Failed {
                    message: "workspace descriptor unavailable".into(),
                },
            }
        }
    };
    write_line(&mut writer, &reply).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_owner(inbound: mpsc::Sender<Inbound>, register: mpsc::Sender<Register>) -> Owner {
        Owner {
            paths: DaemonPaths::from_env(
                Some("/run/user/1".into()),
                Some("/home/u/.local/state".into()),
                Some("/home/u".into()),
            )
            .unwrap_or_else(|error| panic!("test paths: {error}")),
            identity: ManagerIdentity {
                pid: 1,
                instance_nonce: "current-server".into(),
            },
            inbound,
            tokens: Arc::new(AtomicU64::new(1)),
            register,
            viewer_ids: Arc::new(AtomicU64::new(1)),
        }
    }

    #[tokio::test]
    async fn subscription_replay_deduplicates_queued_overlap() -> anyhow::Result<()> {
        use control::{CommandResult, Event, EventCursor, SequencedEvent};
        tokio::time::timeout(Duration::from_secs(5), async {
            let (server, mut client) = UnixStream::pair()?;
            let (inbound, mut inbound_rx) = mpsc::channel(1);
            let (register, mut register_rx) = mpsc::channel(1);
            let owner = test_owner(inbound, register);
            let subscribers = Arc::new(Mutex::new(Vec::new()));
            let serving = tokio::spawn(serve_control_connection(server, "default".into(), owner, Arc::clone(&subscribers)));
            client.write_all(CONTROL_PREFACE).await?;
            let mut preface = [0; CONTROL_PREFACE.len()];
            client.read_exact(&mut preface).await?;
            let (client, mut writer) = client.into_split();
            let mut client = BufReader::new(client);
            let cursor = |sequence| EventCursor { stream: 7, sequence };
            write_line(&mut writer, &Request::Subscribe {
                id: 42, instance: Some("current-server".into()),
                after: Some(cursor(0)),
            }).await?;
            let Some(Register::ControlReply(token, reply)) = register_rx.recv().await else {
                anyhow::bail!("missing reply channel");
            };
            let request = inbound_rx.recv().await.ok_or_else(|| anyhow::anyhow!("missing replay request"))?;
            assert!(matches!(request, Inbound::ControlRequest { token: actual, request: Request::Events { after, .. }, .. } if actual == token && after == cursor(0)));
            // Registration must already exist before the authoritative replay is answered.
            assert_eq!(crate::os::lock(&subscribers).len(), 1);
            let title = |value: &str| Event::TabOpened { id: 0, tab: crate::ids::TabId(1), name: value.into() };
            let publish = |event: Event, cursor: EventCursor| {
                let size = crate::ecs::events::encoded_len(&SequencedEvent { cursor, event: event.clone() });
                super::super::adapter::publish(&subscribers, &event, cursor, size);
            };
            publish(title("overlap"), cursor(2));
            reply.send(Reply::Completed { id: 42, result: CommandResult::Events {
                cursor: cursor(2), events: vec![
                    SequencedEvent { cursor: cursor(1), event: Event::WorkspaceChanged { id: 0 } },
                    SequencedEvent { cursor: cursor(2), event: title("overlap") },
                ],
            }}).map_err(|_| anyhow::anyhow!("replay receiver closed"))?;
            // Queue a later event before reading the acceptance/replay.
            publish(title("later"), cursor(3));
            let accepted = read_line(&mut client).await?.ok_or_else(|| anyhow::anyhow!("accept EOF"))?;
            assert_eq!(serde_json::from_slice::<Reply>(&accepted)?, Reply::Accepted { id: 42 });
            for (sequence, event) in [(1, Event::WorkspaceChanged { id: 0 }), (2, title("overlap")), (3, title("later"))] {
                let line = read_line(&mut client).await?.ok_or_else(|| anyhow::anyhow!("event EOF"))?;
                let entry: SequencedEvent = serde_json::from_slice(&line)?;
                assert_eq!(entry.cursor, cursor(sequence));
                assert_eq!(entry.event, event.with_id(42));
            }
            drop(client);
            drop(writer);
            serving.await??;
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn subscription_checks_incarnation_before_registering() -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (server, client) = UnixStream::pair()?;
            let (inbound, _inbound_rx) = mpsc::channel(1);
            let (register, _register_rx) = mpsc::channel(1);
            let owner = test_owner(inbound, register);
            let subscribers = Arc::new(Mutex::new(Vec::new()));
            let serving = tokio::spawn(serve_control_connection(
                server,
                "default".into(),
                owner,
                Arc::clone(&subscribers),
            ));
            let mut client = client;
            client.write_all(CONTROL_PREFACE).await?;
            let mut preface = [0; CONTROL_PREFACE.len()];
            client.read_exact(&mut preface).await?;
            assert_eq!(&preface, CONTROL_PREFACE);
            let (client, mut writer) = client.into_split();
            let mut client = BufReader::new(client);
            for (instance, expected) in [
                ("old-server", ErrorCode::Conflict),
                ("bad instance", ErrorCode::InvalidRequest),
            ] {
                write_line(
                    &mut writer,
                    &Request::Subscribe {
                        after: None,
                        id: 1,
                        instance: Some(instance.into()),
                    },
                )
                .await?;
                let line = read_line(&mut client)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("reply EOF"))?;
                let response: Reply = serde_json::from_slice(&line)?;
                assert!(matches!(response, Reply::Failed { error, .. } if error.code == expected));
                assert!(crate::os::lock(&subscribers).is_empty());
            }
            write_line(
                &mut writer,
                &Request::Subscribe {
                    after: None,
                    id: 2,
                    instance: Some("current-server".into()),
                },
            )
            .await?;
            let line = read_line(&mut client)
                .await?
                .ok_or_else(|| anyhow::anyhow!("reply EOF"))?;
            assert_eq!(
                serde_json::from_slice::<Reply>(&line)?,
                Reply::Accepted { id: 2 }
            );
            assert_eq!(crate::os::lock(&subscribers).len(), 1);
            drop(client);
            drop(writer);
            serving.await??;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
}
