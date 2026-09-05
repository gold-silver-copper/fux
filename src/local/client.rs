//! Local viewer connection, independent of network identities.
use super::{
    ClientMessage, FRAME_TIMEOUT, MAX_CLIENT_FRAME, MAX_SERVER_FRAME, ServerMessage, VERSION,
    read_frame, write_frame,
};
use crate::client::ClientTerminal;
use crate::client::view::Overlay;
use crate::state::WorkspaceState;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct Connection {
    stream: UnixStream,
    updates: Option<tokio::sync::watch::Sender<Option<std::sync::Arc<WorkspaceState>>>>,
    replies: Option<mpsc::Sender<crate::control::Reply>>,
    copy_views: Option<mpsc::Sender<super::CopyViewReply>>,
    copy_repaint: Option<tokio::sync::watch::Receiver<crate::client::copy::CopyUi>>,
    repaint: Option<tokio::sync::watch::Receiver<Option<crate::client::hints::HintPanel>>>,
}
impl Connection {
    /// Validate the service path and kernel peer before negotiating the version. Terminal raw
    /// mode and input readers must be started only after this returns successfully.
    pub async fn connect(path: &Path) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("local socket has no parent"))?;
        let directory = std::fs::symlink_metadata(parent)?;
        let socket = std::fs::symlink_metadata(path)?;
        let owner = nix::unistd::geteuid().as_raw();
        anyhow::ensure!(
            directory.is_dir()
                && directory.uid() == owner
                && directory.permissions().mode() & 0o077 == 0,
            "unsafe local socket directory"
        );
        anyhow::ensure!(
            socket.file_type().is_socket()
                && socket.uid() == owner
                && socket.permissions().mode() & 0o077 == 0,
            "unsafe local socket path"
        );
        let stream = tokio::time::timeout(FRAME_TIMEOUT, UnixStream::connect(path)).await??;
        let stream = stream.into_std()?;
        crate::control::authorize_peer(&stream)?;
        let mut stream = UnixStream::from_std(stream)?;
        write_frame(
            &mut stream,
            &ClientMessage::Hello {
                version: VERSION,
                rows: 24,
                columns: 80,
            },
            MAX_CLIENT_FRAME,
        )
        .await?;
        match tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut stream, MAX_SERVER_FRAME))
            .await??
        {
            ServerMessage::Hello { version: VERSION } => Ok(Self {
                stream,
                repaint: None,
                updates: None,
                replies: None,
                copy_views: None,
                copy_repaint: None,
            }),
            ServerMessage::Error { message } => anyhow::bail!("{message}"),
            _ => anyhow::bail!(
                "incompatible fux session server; use matching versions or restart it after saving your work"
            ),
        }
    }
    pub fn with_repaint(
        mut self,
        receiver: tokio::sync::watch::Receiver<Option<crate::client::hints::HintPanel>>,
    ) -> Self {
        self.repaint = Some(receiver);
        self
    }

    pub fn with_updates(
        mut self,
        updates: tokio::sync::watch::Sender<Option<std::sync::Arc<WorkspaceState>>>,
        replies: mpsc::Sender<crate::control::Reply>,
    ) -> Self {
        self.updates = Some(updates);
        self.replies = Some(replies);
        self
    }

    pub fn with_copy_repaint(
        mut self,
        receiver: tokio::sync::watch::Receiver<crate::client::copy::CopyUi>,
    ) -> Self {
        self.copy_repaint = Some(receiver);
        self
    }

    pub fn with_copy_views(mut self, replies: mpsc::Sender<super::CopyViewReply>) -> Self {
        self.copy_views = Some(replies);
        self
    }

    pub async fn run<T: ClientTerminal<WorkspaceState>>(
        self,
        terminal: T,
        input: mpsc::Receiver<Vec<u8>>,
        resize: mpsc::Receiver<()>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Option<u32>> {
        self.run_input(terminal, input, resize, shutdown).await
    }

    pub async fn run_interactive<T: ClientTerminal<WorkspaceState>>(
        self,
        terminal: T,
        input: mpsc::Receiver<ClientMessage>,
        resize: mpsc::Receiver<()>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Option<u32>> {
        self.run_input(terminal, input, resize, shutdown).await
    }

    async fn run_input<T: ClientTerminal<WorkspaceState>, I: Into<ClientMessage>>(
        self,
        mut terminal: T,
        mut input: mpsc::Receiver<I>,
        mut resize: mpsc::Receiver<()>,
        shutdown: CancellationToken,
    ) -> anyhow::Result<Option<u32>> {
        let mut repaint = self.repaint;
        let mut copy_repaint = self.copy_repaint;
        let mut last_state = None;
        let (mut reader, mut writer) = self.stream.into_split();
        let (states_tx, mut states_rx) = mpsc::channel(1);
        let mut readers = tokio::task::JoinSet::new();
        readers.spawn(async move {
            loop {
                let message = read_frame::<_, ServerMessage>(&mut reader, MAX_SERVER_FRAME).await;
                let failed = message.is_err();
                if states_tx.send(message).await.is_err() || failed {
                    break;
                }
            }
        });
        let (rows, columns) = terminal.size()?;
        write_frame(
            &mut writer,
            &ClientMessage::Resize { rows, columns },
            MAX_CLIENT_FRAME,
        )
        .await?;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return Ok(None),
                message = states_rx.recv() => {
                    let message = message.ok_or_else(|| anyhow::anyhow!("local session server disconnected"))??;
                    match message {
                        ServerMessage::State { state } => {
                            let state: std::sync::Arc<WorkspaceState> = std::sync::Arc::from(state);
                            if let Some(updates) = &self.updates { updates.send_replace(Some(state.clone())); }
                            terminal.render(&state, &Overlay::empty(), None)?;
                            if let Some(code) = state.metadata().exit_code { return Ok(Some(code)); }
                            last_state = Some(state);
                        }
                        ServerMessage::Reply { reply } => {
                            if let Some(replies) = &self.replies { replies.try_send(reply)
                                .map_err(|_| anyhow::anyhow!("viewer reply queue is full or closed"))?; }
                        }
                        ServerMessage::CopyView { reply } => {
                            let replies = self.copy_views.as_ref()
                                .ok_or_else(|| anyhow::anyhow!("unexpected copy viewport reply"))?;
                            replies.try_send(reply)
                                .map_err(|_| anyhow::anyhow!("viewer copy reply queue is full or closed"))?;
                        }
                        ServerMessage::Exited { code } => return Ok(code),
                        ServerMessage::Error { message } => anyhow::bail!("{message}"),
                        _ => anyhow::bail!("unexpected local server message"),
                    }
                }
                changed = async {
                    match repaint.as_mut() {
                        Some(receiver) => receiver.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if changed.is_err() { repaint = None; }
                    else if let Some(state) = &last_state { terminal.render(state.as_ref(), &Overlay::empty(), None)?; }
                }
                changed = async {
                    match copy_repaint.as_mut() {
                        Some(receiver) => receiver.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if changed.is_err() { copy_repaint = None; }
                    else if let Some(state) = &last_state { terminal.render(state.as_ref(), &Overlay::empty(), None)?; }
                }
                bytes = input.recv() => {
                    let Some(bytes) = bytes else { return Ok(None) };
                    match bytes.into() {
                        ClientMessage::Input { bytes } => {
                            for chunk in bytes.chunks(4096) { write_frame(&mut writer, &ClientMessage::Input { bytes: chunk.to_vec() }, MAX_CLIENT_FRAME).await?; }
                        }
                        ClientMessage::PaneInput { bytes } => {
                            for chunk in bytes.chunks(4096) { write_frame(&mut writer, &ClientMessage::PaneInput { bytes: chunk.to_vec() }, MAX_CLIENT_FRAME).await?; }
                        }
                        message => write_frame(&mut writer, &message, MAX_CLIENT_FRAME).await?,
                    }
                }
                Some(()) = resize.recv() => {
                    let (rows, columns) = terminal.size()?;
                    write_frame(&mut writer, &ClientMessage::Resize { rows, columns }, MAX_CLIENT_FRAME).await?;
                    if let Some(state) = &last_state { terminal.render(state.as_ref(), &Overlay::empty(), None)?; }
                }
            }
        }
    }
}
