//! Workspace attachment over an authenticated local socket.
use super::{
    ClientMessage, FRAME_TIMEOUT, MAX_CLIENT_FRAME, MAX_SERVER_FRAME, ServerMessage, VERSION,
    read_frame, write_frame,
};
use crate::control::{BoundControlSocket, authorize_peer, bind_local_socket};
use crate::host::session::{ChangeSignal, ClientId, SessionHost};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

pub struct LocalEndpoint {
    path: PathBuf,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    active: Arc<AtomicUsize>,
}
impl LocalEndpoint {
    pub fn bind<H: SessionHost<State = crate::state::WorkspaceState>>(
        path: &Path,
        mut host: H,
    ) -> anyhow::Result<Self> {
        let bound = bind_local_socket(path)?;
        bound.listener().set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(bound.listener().try_clone()?)?;
        let changed = ChangeSignal::default();
        host.attach_notify(changed.clone());
        let host = Arc::new(Mutex::new(host));
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&active);
        let task = tokio::spawn(async move {
            let _bound: BoundControlSocket = bound;
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        if clients.len() >= crate::daemon::MAX_CONNECTIONS_PER_WORKSPACE { continue; }
                        let Ok(stream) = authenticate(stream) else { continue };
                        let host = Arc::clone(&host);
                        let changed = changed.clone();
                        let active = Arc::clone(&count);
                        clients.spawn(async move {
                            active.fetch_add(1, Ordering::AcqRel);
                            let _active = ActiveGuard(active);
                            if let Err(error) = serve_client(stream, host, changed).await {
                                tracing::debug!(%error, "local viewer disconnected");
                            }
                        });
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            path: path.to_owned(),
            cancel,
            task,
            active,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn active_tasks(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
    pub fn close(&mut self) {
        self.cancel.cancel();
    }
}
impl Drop for LocalEndpoint {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
struct ActiveGuard(Arc<AtomicUsize>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn authenticate(stream: UnixStream) -> std::io::Result<UnixStream> {
    let stream = stream.into_std()?;
    authorize_peer(&stream)?;
    UnixStream::from_std(stream)
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
struct Viewer<H: SessionHost> {
    host: Arc<Mutex<H>>,
    id: ClientId,
}
impl<H: SessionHost> Drop for Viewer<H> {
    fn drop(&mut self) {
        lock(&self.host).client_detached(self.id);
    }
}
async fn serve_client<H: SessionHost<State = crate::state::WorkspaceState>>(
    mut stream: UnixStream,
    host: Arc<Mutex<H>>,
    changed: ChangeSignal,
) -> anyhow::Result<()> {
    let hello: ClientMessage =
        tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut stream, MAX_CLIENT_FRAME)).await??;
    let (rows, columns) = match hello {
        ClientMessage::Hello {
            version: VERSION,
            rows,
            columns,
        } => (rows, columns),
        _ => {
            write_frame(&mut stream, &ServerMessage::Error { message: "incompatible local attachment protocol; use matching fux versions or restart the session server after saving your work".into() }, MAX_SERVER_FRAME).await?;
            anyhow::bail!("incompatible local handshake");
        }
    };
    write_frame(
        &mut stream,
        &ServerMessage::Hello { version: VERSION },
        MAX_SERVER_FRAME,
    )
    .await?;
    let viewer = Viewer {
        host,
        id: ClientId::next(),
    };
    lock(&viewer.host).resize(viewer.id, rows, columns);
    let mut changes = changed.subscribe();
    let (mut reader, mut writer) = stream.into_split();
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(8);
    let mut readers = tokio::task::JoinSet::new();
    readers.spawn(async move {
        loop {
            let result = read_frame::<_, ClientMessage>(&mut reader, MAX_CLIENT_FRAME).await;
            let failed = result.is_err();
            if input_tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    let mut pending_reply = None;
    loop {
        changes.borrow_and_update();
        let (state, alive) = {
            let mut host = lock(&viewer.host);
            (host.snapshot(), host.alive())
        };
        let exit_code = state.metadata().exit_code;
        write_frame(
            &mut writer,
            &ServerMessage::State {
                state: Box::new(state),
            },
            MAX_SERVER_FRAME,
        )
        .await?;
        // The snapshot is the action's ordering barrier. Send it before the
        // acknowledgement without adding a duplicate full-state repaint.
        if let Some(reply) = pending_reply.take() {
            write_frame(
                &mut writer,
                &ServerMessage::Reply { reply },
                MAX_SERVER_FRAME,
            )
            .await?;
        }
        if !alive {
            write_frame(
                &mut writer,
                &ServerMessage::Exited { code: exit_code },
                MAX_SERVER_FRAME,
            )
            .await?;
            break;
        }
        tokio::select! {
            result = changes.changed() => { result?; },
            message = input_rx.recv() => {
                let Some(message) = message else { break };
                match message? {
                    ClientMessage::Input { bytes } if bytes.len() <= 4096 => lock(&viewer.host).pane_input(&bytes),
                    ClientMessage::PaneInput { bytes } if bytes.len() <= 4096 => lock(&viewer.host).pane_input(&bytes),
                    ClientMessage::Mouse { event } if event.column > 0 && event.row > 0 => lock(&viewer.host).application_mouse(event),
                    ClientMessage::Binding { key } => {
                        let accepted = lock(&viewer.host).external_binding(key);
                        pending_reply = Some(if accepted {
                            crate::control::Reply::Accepted { id: 0 }
                        } else {
                            crate::control::Reply::Failed { id: 0, error: crate::control::ReplyError {
                                code: crate::control::ErrorCode::NotFound,
                                message: "External binding is no longer configured".into(),
                            }}
                        });
                    }
                    ClientMessage::Control { request } => {
                        pending_reply = Some(lock(&viewer.host).control(request)
                            .ok_or_else(|| anyhow::anyhow!("host does not support viewer control"))?);
                    }
                    ClientMessage::CopyView { request, pane, offset } => {
                        let view = lock(&viewer.host).copy_view(pane, offset).map(Box::new);
                        let reply = super::CopyViewReply { request, pane, view };
                        write_frame(&mut writer, &ServerMessage::CopyView { reply }, MAX_SERVER_FRAME).await?;
                    }
                    ClientMessage::Resize { rows, columns } => lock(&viewer.host).resize(viewer.id, rows, columns),
                    ClientMessage::Detach => break,
                    _ => anyhow::bail!("invalid local client message"),
                }
            }
        }
        // Coalesce output bursts and bound repaint frequency without retaining frame queues.
        tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    }
    Ok(())
}
