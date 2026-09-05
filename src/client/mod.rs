//! Workspace client state, compositor, terminal adapter, and detach input filter.

mod compositor;
mod input;
mod terminal;

pub use compositor::{ComposedFrame, Compositor, Selection};
pub use input::{CopyMode, CopyPoint, DetachFilter};
pub use terminal::{
    CaptureBackend, ClientNotificationGate, WorkspaceTerminal, client_notification_command,
};

use crate::state::{Color, PaneView, WorkspaceState};
use koh::client::{ClientState, InputModes, WindowState};
use koh::predict::{CellView, ScreenView};

/// Connects the fux workspace state over its static ALPN with owned terminal I/O producers.
pub async fn connect_workspace(
    config: koh::client::ConnectConfig,
    notifications: Option<crate::config::NotificationPolicy>,
) -> anyhow::Result<Option<u32>> {
    match connect_workspace_with_picker(config, notifications, false).await? {
        ConnectOutcome::Exited(exit) => Ok(exit),
        ConnectOutcome::WorkspacePicker => Ok(None),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectOutcome {
    Exited(Option<u32>),
    WorkspacePicker,
}

pub async fn connect_workspace_with_picker(
    config: koh::client::ConnectConfig,
    notifications: Option<crate::config::NotificationPolicy>,
    enable_picker: bool,
) -> anyhow::Result<ConnectOutcome> {
    let secret = koh::identity::load_client(config.key_file.as_deref())?;
    connect_workspace_with_secret(config, notifications, enable_picker, secret).await
}

/// Connect using an identity already unlocked before terminal input producers start.
pub async fn connect_workspace_with_secret(
    config: koh::client::ConnectConfig,
    notifications: Option<crate::config::NotificationPolicy>,
    enable_picker: bool,
    secret: koh::identity::Identity,
) -> anyhow::Result<ConnectOutcome> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut filter = DetachFilter::new(vec![crate::commands::ClientBindings::default().prefix()])
        .ok_or_else(|| anyhow::anyhow!("detach prefix must contain 1-16 bytes"))?;
    filter.set_workspace_picker_enabled(enable_picker);
    let clipboard = config.clipboard;
    let prepared = tokio::select! {
        result = koh::embed::Connection::connect(&config, crate::FUX_ALPN, &secret) => result?,
        _ = interrupt.recv() => return Ok(ConnectOutcome::Exited(None)),
        _ = terminate.recv() => return Ok(ConnectOutcome::Exited(None)),
        _ = hangup.recv() => return Ok(ConnectOutcome::Exited(None)),
    };
    let (channels, tasks) = koh::client::spawn_client_io()?;
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(64);
    let (action_tx, mut action_rx) = tokio::sync::mpsc::channel(1);
    let mut source = channels.input_rx;
    let (policy_tx, mut policy_rx) = tokio::sync::watch::channel(None);
    let bridge = tokio::spawn(async move {
        // Wait for the workspace's first authoritative policy before interpreting input.
        if policy_rx.wait_for(|policy| policy.is_some()).await.is_err() {
            return;
        }
        while let Some(chunk) = source.recv().await {
            let mut filtered = policy_rx
                .borrow_and_update()
                .clone()
                .map_or_else(Vec::new, |policy| filter.configure(policy));
            filtered.extend(filter.process_terminal_input(&chunk));
            let detach = filter.take_detach();
            let picker = enable_picker && filter.take_workspace_picker();
            if detach || picker {
                let _ = action_tx.try_send(picker && !detach);
                // Keep input_tx alive until the picker branch cancels this bridge. Otherwise
                // run_client may observe EOF and finish while the select is polling it, racing
                // the picker notification even with a biased select.
                std::future::pending::<()>().await;
                break;
            }
            if !filtered.is_empty() && input_tx.send(filtered).await.is_err() {
                break;
            }
        }
        let tail = filter.flush();
        if !tail.is_empty() {
            let _ = input_tx.send(tail).await;
        }
    });
    // Scope the connection future so its terminal is restored before input producers are joined
    // and before the caller opens the workspace picker.
    let cancel = tokio_util::sync::CancellationToken::new();
    let (result, picker) = {
        let connection = async {
            let term = WorkspaceTerminal::enter_default(clipboard, notifications)
                .map_err(|error| anyhow::anyhow!("entering fux terminal: {error}"))?
                .with_input_policy(policy_tx);
            prepared
                .run(
                    term,
                    input_rx,
                    channels.resize_rx,
                    cancel.clone(),
                    config.bell_command.map(koh::client::BellHook::new),
                )
                .await
        };
        tokio::pin!(connection);
        tokio::select! {
            biased;
            Some(picker) = action_rx.recv() => {
                cancel.cancel();
                (connection.await, picker)
            },
            result = &mut connection => (result, false),
            _ = interrupt.recv() => { cancel.cancel(); (connection.await, false) },
            _ = terminate.recv() => { cancel.cancel(); (connection.await, false) },
            _ = hangup.recv() => { cancel.cancel(); (connection.await, false) },
        }
    };
    bridge.abort();
    let _ = bridge.await;
    let shutdown = tasks.shutdown().await;
    match (result, shutdown, picker) {
        (Err(error), _, _) => Err(error),
        (_, Err(error), _) => Err(error.context("stopping terminal input producers")),
        (Ok(exit), Ok(()), false) => Ok(ConnectOutcome::Exited(exit)),
        (Ok(_), Ok(()), true) => Ok(ConnectOutcome::WorkspacePicker),
    }
}

impl ScreenView for PaneView {
    fn size(&self) -> (u16, u16) {
        (self.rows, self.columns)
    }

    fn cursor_position(&self) -> (u16, u16) {
        (self.cursor.row, self.cursor.column)
    }

    fn cell(&self, row: u16, col: u16) -> Option<CellView<'_>> {
        let cell = self.cell(row, col)?;
        Some(CellView {
            // Peer state is validated before acceptance, but never let a malformed cell become
            // an output-side terminal escape even if a future construction path misses validation.
            contents: if cell.valid() { &cell.text } else { "" },
            fg: vt_color(cell.style.foreground),
            bg: vt_color(cell.style.background),
        })
    }
}

impl ClientState for WorkspaceState {
    fn window(&self) -> WindowState<'_> {
        WindowState {
            title: &self.metadata().window_title,
            icon: &self.metadata().window_title,
            clipboard: &self.metadata().clipboard_base64,
            bell_count: self.metadata().bell_count,
        }
    }

    fn exit_code(&self) -> Option<u32> {
        self.metadata().exit_code
    }

    fn echo_ack(&self) -> u64 {
        self.metadata().echo_ack
    }

    fn input_modes(&self) -> InputModes {
        let Some(pane) = focused_pane(self) else {
            return InputModes::default();
        };
        InputModes {
            application_keypad: pane.modes.application_keypad,
            application_cursor: pane.modes.application_cursor,
            bracketed_paste: pane.modes.bracketed_paste,
            // Capture the superset locally; the host filters and re-encodes for the focused pane.
            mouse_mode: vt100::MouseProtocolMode::AnyMotion,
            mouse_encoding: vt100::MouseProtocolEncoding::Sgr,
        }
    }

    fn predict_target(&self) -> Option<&dyn ScreenView> {
        let pane = focused_pane(self)?;
        (pane.viewport_offset == 0 && !pane.copy.active).then_some(pane as &dyn ScreenView)
    }
}

pub(crate) fn active_tab(state: &WorkspaceState) -> Option<&crate::state::Tab> {
    let id = state.active_tab()?;
    state.tabs().iter().find(|tab| tab.id == id)
}

pub(crate) fn focused_pane(state: &WorkspaceState) -> Option<&PaneView> {
    if let Some(popup) = state.popups().iter().max_by_key(|popup| popup.z_index) {
        return state.pane(popup.pane).filter(|pane| pane.valid());
    }
    let tab = active_tab(state)?;
    state.pane(tab.focused).filter(|pane| pane.valid())
}

pub(crate) const fn vt_color(color: Color) -> vt100::Color {
    match color {
        Color::Default => vt100::Color::Default,
        Color::Indexed(index) => vt100::Color::Idx(index),
        Color::Rgb(red, green, blue) => vt100::Color::Rgb(red, green, blue),
    }
}

pub(crate) const fn rat_color(color: Color) -> ratatui_core::style::Color {
    match color {
        Color::Default => ratatui_core::style::Color::Reset,
        Color::Indexed(index) => ratatui_core::style::Color::Indexed(index),
        Color::Rgb(red, green, blue) => ratatui_core::style::Color::Rgb(red, green, blue),
    }
}
