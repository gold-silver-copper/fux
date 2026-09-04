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
    prefix: Vec<u8>,
    notifications: Option<crate::config::NotificationPolicy>,
) -> anyhow::Result<Option<u32>> {
    match connect_workspace_with_picker(config, prefix, notifications, false).await? {
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
    prefix: Vec<u8>,
    notifications: Option<crate::config::NotificationPolicy>,
    enable_picker: bool,
) -> anyhow::Result<ConnectOutcome> {
    let mut filter = DetachFilter::new(prefix)
        .ok_or_else(|| anyhow::anyhow!("detach prefix must contain 1-16 bytes"))?;
    filter.set_workspace_picker_enabled(enable_picker);
    let clipboard = config.clipboard;
    let (channels, tasks) = koh::client::spawn_client_io()?;
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(64);
    let (picker_tx, mut picker_rx) = tokio::sync::mpsc::channel(1);
    let mut source = channels.input_rx;
    let bridge = tokio::spawn(async move {
        while let Some(chunk) = source.recv().await {
            let filtered = filter.process_terminal_input(&chunk);
            if enable_picker && filter.take_workspace_picker() {
                let _ = picker_tx.try_send(());
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
    let connection = koh::client::connect_with(
        config,
        crate::FUX_ALPN,
        move || {
            WorkspaceTerminal::enter_default(clipboard, notifications.clone())
                .map_err(|error| anyhow::anyhow!("entering fux terminal: {error}"))
        },
        input_rx,
        channels.resize_rx,
    );
    tokio::pin!(connection);
    let (result, picker) = tokio::select! {
        result = &mut connection => (Some(result), false),
        value = picker_rx.recv(), if enable_picker => (None, value.is_some()),
    };
    bridge.abort();
    let _ = bridge.await;
    let shutdown = tasks.shutdown().await;
    match (result, shutdown, picker) {
        (Some(Err(error)), _, _) => Err(error),
        (_, Err(error), _) => Err(error.context("stopping terminal input producers")),
        (Some(Ok(exit)), Ok(()), false) => Ok(ConnectOutcome::Exited(exit)),
        (None, Ok(()), true) => Ok(ConnectOutcome::WorkspacePicker),
        _ => Err(anyhow::anyhow!("workspace connection ended unexpectedly")),
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
