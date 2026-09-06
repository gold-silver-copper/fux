//! Socket-facing tasks: attachment viewers, control clients and the manager socket. Each task
//! forwards typed inbound events to the owner loop and writes replies/frames it is handed.

use super::adapter::{Subscriber, ViewerOutbox};
use crate::ecs::{Inbound, ManagerAction, ManagerOutcome, ViewerRequest};
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

/// Shared by every accept loop: how to reach the owner and how to hand it reply channels.
#[derive(Clone)]
pub struct Owner {
    pub inbound: mpsc::Sender<Inbound>,
    pub tokens: Arc<AtomicU64>,
    pub control_replies: mpsc::Sender<(u64, oneshot::Sender<Reply>)>,
    pub manager_replies: mpsc::Sender<(u64, oneshot::Sender<ManagerOutcome>)>,
    pub viewer_outboxes: mpsc::Sender<(ViewerId, ViewerOutbox)>,
    pub viewer_ids: Arc<AtomicU64>,
}

impl Owner {
    fn token(&self) -> u64 {
        self.tokens.fetch_add(1, Ordering::Relaxed)
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
    let (rows, cols) = match hello {
        ClientMessage::Hello { rows, columns } => (rows, columns),
        _ => {
            write_frame(
                &mut stream,
                &ServerMessage::Error {
                    message: "the first attachment frame must be a hello".into(),
                },
                MAX_SERVER_FRAME,
            )
            .await?;
            anyhow::bail!("attachment did not start with a hello");
        }
    };
    write_frame(&mut stream, &ServerMessage::Hello {}, MAX_SERVER_FRAME).await?;
    let viewer = ViewerId(owner.viewer_ids.fetch_add(1, Ordering::Relaxed));
    let outbox = ViewerOutbox::default();
    owner.viewer_outboxes.send((viewer, outbox.clone())).await?;
    owner
        .inbound
        .send(Inbound::ViewerAttached {
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
        if let Request::Subscribe { id, events } = request {
            let (sender, mut receiver) = mpsc::channel(MAX_SUBSCRIBER_QUEUE);
            subscribers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Subscriber {
                    id,
                    filters: events,
                    sender,
                });
            write_line(&mut writer, &Reply::Accepted { id }).await?;
            let mut probe = [0_u8; 1];
            loop {
                tokio::select! {
                    event = receiver.recv() => {
                        let Some(event) = event else { break };
                        write_line(&mut writer, &event).await?;
                    }
                    read = reader.read(&mut probe) => {
                        // Any further byte or EOF ends the subscription.
                        let _ = read;
                        break;
                    }
                }
            }
            return Ok(());
        }
        let request_id = request.id();
        let token = owner.token();
        let (sender, receiver) = oneshot::channel();
        owner.control_replies.send((token, sender)).await?;
        owner
            .inbound
            .send(Inbound::ControlRequest {
                workspace: workspace.clone(),
                request,
                token,
            })
            .await?;
        let reply = match tokio::time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(reply)) => reply,
            _ => Reply::failed(
                request_id,
                ErrorCode::Internal,
                "control request was not answered",
            ),
        };
        write_line(&mut writer, &bounded(reply)).await?;
    }
    Ok(())
}

fn bounded(reply: Reply) -> Reply {
    if serde_json::to_vec(&reply).is_ok_and(|bytes| bytes.len() <= MAX_FRAME_BYTES) {
        reply
    } else {
        Reply::failed(
            reply.id(),
            ErrorCode::FrameTooLarge,
            "control response exceeds the 1 MiB frame limit",
        )
    }
}

/// Writes one newline-delimited JSON frame within the frame timeout.
async fn write_line<T: serde::Serialize>(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &T,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
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

/// Answers one manager request. The descriptor for an attach reply is built by the caller-side
/// hook so the manager task never touches the World.
pub type DescriptorHook = Arc<dyn Fn(&str) -> Option<crate::daemon::Descriptor> + Send + Sync>;

async fn serve_manager_connection(mut stream: UnixStream, owner: Owner) -> anyhow::Result<()> {
    negotiate(&mut stream).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let Some(line) = read_line(&mut reader).await? else {
        return Ok(());
    };
    let request: crate::daemon::ManagerRequest = match serde_json::from_slice(&line) {
        Ok(request) => request,
        Err(error) => {
            let reply = crate::daemon::ManagerReply::Failed {
                message: format!("invalid manager request: {error}"),
            };
            return write_line(&mut writer, &reply).await;
        }
    };
    let action = match request {
        crate::daemon::ManagerRequest::List => ManagerAction::List,
        crate::daemon::ManagerRequest::Resolve { name } => ManagerAction::Resolve { name },
        crate::daemon::ManagerRequest::Kill { name } => ManagerAction::Kill { name },
    };
    if let ManagerAction::Resolve { name: Some(name) } | ManagerAction::Kill { name } = &action
        && let Err(error) = crate::ids::validate_workspace_name(name)
    {
        let reply = crate::daemon::ManagerReply::Failed {
            message: error.to_string(),
        };
        return write_line(&mut writer, &reply).await;
    }
    let token = owner.token();
    let (sender, receiver) = oneshot::channel();
    owner.manager_replies.send((token, sender)).await?;
    owner
        .inbound
        .send(Inbound::Manager { action, token })
        .await?;
    let outcome = match tokio::time::timeout(crate::daemon::MANAGER_DEADLINE, receiver).await {
        Ok(Ok(outcome)) => outcome,
        _ => ManagerOutcome::Failed("manager request was not answered".into()),
    };
    let reply = match outcome {
        ManagerOutcome::Names(names) => crate::daemon::ManagerReply::Names { names },
        ManagerOutcome::Failed(message) => crate::daemon::ManagerReply::Failed { message },
        ManagerOutcome::Attach { name, .. } => match (DESCRIPTOR_HOOK.get())(&name) {
            Some(descriptor) => crate::daemon::ManagerReply::Attach { descriptor },
            None => crate::daemon::ManagerReply::Failed {
                message: "workspace descriptor unavailable".into(),
            },
        },
    };
    write_line(&mut writer, &reply).await
}

/// Process-wide descriptor lookup installed by the server before serving the manager socket.
pub struct DescriptorLookup(std::sync::OnceLock<DescriptorHook>);

impl DescriptorLookup {
    pub fn install(&self, hook: DescriptorHook) {
        let _ = self.0.set(hook);
    }
    fn get(&self) -> DescriptorHook {
        self.0.get().cloned().unwrap_or_else(|| Arc::new(|_| None))
    }
}

pub static DESCRIPTOR_HOOK: DescriptorLookup = DescriptorLookup(std::sync::OnceLock::new());
