//! Workspace client state, compositor, terminal adapter, and detach input filter.
pub mod copy;

pub mod backend;
mod compositor;
pub mod hints;
mod input;
pub mod interaction;
mod io;
mod terminal;
mod workspace_picker;
pub use workspace_picker::pick_workspace;
pub mod view;

pub use compositor::{ComposedFrame, Compositor, Selection};
pub use input::{CopyMode, CopyPoint, DetachFilter};
pub use terminal::{
    CaptureBackend, ClientNotificationGate, WorkspaceTerminal, client_notification_command,
};

use crate::state::{Color, PaneView, WorkspaceState};
pub use view::{ClientState, ClientTerminal, InputModes, WindowState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectOutcome {
    Exited(Option<u32>),
    Workspace { name: String, pending: Vec<u8> },
}

/// Attach a local viewer after authenticating the session server, before terminal raw mode.
pub async fn connect_local_workspace(
    socket: &std::path::Path,
    clipboard: bool,
    notifications: Option<crate::config::NotificationPolicy>,
    enable_picker: bool,
) -> anyhow::Result<ConnectOutcome> {
    connect_local_workspace_with_pending(
        socket,
        clipboard,
        notifications,
        enable_picker,
        Vec::new(),
    )
    .await
}

pub async fn connect_local_workspace_with_pending(
    socket: &std::path::Path,
    clipboard: bool,
    notifications: Option<crate::config::NotificationPolicy>,
    enable_picker: bool,
    pending: Vec<u8>,
) -> anyhow::Result<ConnectOutcome> {
    use tokio::signal::unix::{SignalKind, signal};
    let manager = if enable_picker {
        Some(crate::daemon::DaemonPaths::discover()?.manager_socket)
    } else {
        None
    };
    let hint_preferences = crate::config::Config::load()?.hints;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut filter = DetachFilter::new(vec![crate::commands::ClientBindings::default().prefix()])
        .ok_or_else(|| anyhow::anyhow!("detach prefix must contain 1-16 bytes"))?;
    filter.set_workspace_picker_enabled(enable_picker);
    filter.enable_contextual_help();
    let prepared = tokio::select! {
        result = crate::local::client::Connection::connect(socket) => result?,
        _ = interrupt.recv() => return Ok(ConnectOutcome::Exited(None)),
        _ = terminate.recv() => return Ok(ConnectOutcome::Exited(None)),
        _ = hangup.recv() => return Ok(ConnectOutcome::Exited(None)),
    };
    let (channels, tasks) = io::spawn_client_io()?;
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(64);
    let (action_tx, mut action_rx) = tokio::sync::mpsc::channel(1);
    let mut source = channels.input_rx;
    let (state_tx, mut state_rx) =
        tokio::sync::watch::channel::<Option<std::sync::Arc<WorkspaceState>>>(None);
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(32);
    let (copy_reply_tx, mut copy_reply_rx) =
        tokio::sync::mpsc::channel::<crate::local::CopyViewReply>(1);
    let (copy_ui_tx, copy_ui_rx) = tokio::sync::watch::channel(copy::CopyUi::default());
    let copy_repaint = copy_ui_rx.clone();
    let (mouse_layout_tx, mouse_layout_rx) = tokio::sync::watch::channel(copy::MouseLayout::new());
    let (policy_tx, mut policy_rx) = tokio::sync::watch::channel(None);
    let (hint_tx, hint_rx) = tokio::sync::watch::channel(None);
    let repaint = hint_rx.clone();
    let bridge = tokio::spawn(async move {
        // Wait for the workspace's first authoritative policy before interpreting input.
        if policy_rx.wait_for(|policy| policy.is_some()).await.is_err() {
            return;
        }
        if let Some(policy) = policy_rx.borrow_and_update().clone() {
            let _ = filter.configure(policy);
        }
        let mut interaction = interaction::Interaction::default();
        let mut copy_request = 0_u64;
        let mut workspace_lookup = tokio::task::JoinSet::new();
        let mut queued = (!pending.is_empty()).then_some(pending);
        let mut started = None;
        let mut prefix_visible = false;
        let mut escape_started = None;
        let mut epoch = filter.prefix_epoch();
        loop {
            let hint_deadline = started
                .filter(|_| hint_preferences.automatic && !prefix_visible)
                .map(|time| time + std::time::Duration::from_millis(hint_preferences.delay_ms));
            let escape_deadline =
                escape_started.map(|time| time + std::time::Duration::from_millis(35));
            let deadline = hint_deadline.into_iter().chain(escape_deadline).min();
            let mut filtered = Vec::new();
            let mut pending_chunk = None;
            let mut selected_workspace = None;
            let mut detach = false;
            tokio::select! {
                chunk = async {
                    if let Some(chunk) = queued.take() { Some(chunk) } else { source.recv().await }
                } => {
                    let Some(chunk) = chunk else { break };
                    pending_chunk = Some(chunk);
                }
                Some(result) = workspace_lookup.join_next(), if !workspace_lookup.is_empty() => {
                    interaction.workspaces_loaded(result.map_err(anyhow::Error::from).and_then(|result| result));
                    let input = interaction.take_loading_input();
                    if interaction.active() && !input.is_empty() { pending_chunk = Some(input); }
                }
                reply = reply_rx.recv() => {
                    if let Some(crate::control::Reply::Failed { error, .. }) = reply { interaction.report_error(error.message); }
                }
                changed = state_rx.changed() => { if changed.is_err() { break; } }
                changed = policy_rx.changed() => { if changed.is_err() { break; } }
                () = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    if escape_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
                        if interaction.active() { interaction.resolve_escape(); }
                        else { filtered.extend(filter.resolve_escape()); }
                    }
                }
            }
            if let Some(policy) = policy_rx.borrow_and_update().clone() {
                filtered.extend(filter.configure(policy));
            }
            if let Some(chunk) = pending_chunk {
                for (offset, byte) in chunk.iter().copied().enumerate() {
                    let state = state_rx.borrow_and_update().clone();
                    if let Some(state) = state.as_deref() {
                        interaction.set_mouse_layout(mouse_layout_rx.borrow().clone());
                        let message = if interaction.active() {
                            interaction
                                .feed(byte, state)
                                .map(|request| crate::local::ClientMessage::Control { request })
                        } else {
                            interaction.clear_error();
                            filtered.extend(filter.process_terminal_input(&[byte]));
                            let mouse = filter.take_mouse().and_then(|(event, _)| {
                                (!interaction.mouse(event, state))
                                    .then_some(crate::local::ClientMessage::Mouse { event })
                            });
                            if let Some(key) = filter.take_external_binding() {
                                Some(crate::local::ClientMessage::Binding { key })
                            } else {
                                filter
                                    .take_viewer_action()
                                    .and_then(|action| {
                                        if let Some(reason) =
                                            action.unavailable(state, enable_picker)
                                        {
                                            filter.show_commands();
                                            interaction.report_error(reason.into());
                                            return None;
                                        }
                                        if let Some(request) = action
                                            .command()
                                            .and_then(|command| command.request(None))
                                        {
                                            Some(crate::local::ClientMessage::Control { request })
                                        } else {
                                            interaction.enter(action, state);
                                            None
                                        }
                                    })
                                    .or(mouse)
                            }
                        };
                        if let Some(message) = message {
                            if !filtered.is_empty() {
                                let _ = input_tx
                                    .send(crate::local::ClientMessage::PaneInput {
                                        bytes: std::mem::take(&mut filtered),
                                    })
                                    .await;
                            }
                            let acknowledged = matches!(
                                message,
                                crate::local::ClientMessage::Control { .. }
                                    | crate::local::ClientMessage::Binding { .. }
                            );
                            let _ = input_tx.send(message).await;
                            // State precedes acknowledgements for state-dependent next keys.
                            if acknowledged
                                && let Some(crate::control::Reply::Failed { error, .. }) =
                                    reply_rx.recv().await
                            {
                                interaction.report_error(error.message);
                            }
                        }
                        refresh_copy_read(
                            &mut interaction,
                            &input_tx,
                            &mut copy_reply_rx,
                            &mut copy_request,
                            &mut filtered,
                        )
                        .await;
                        if enable_picker && filter.take_workspace_picker() {
                            interaction.loading_workspaces();
                            if workspace_lookup.is_empty()
                                && let Some(path) = manager.clone()
                            {
                                workspace_lookup
                                    .spawn_blocking(move || crate::daemon::workspace_names(&path));
                            }
                            hint_tx.send_replace(interaction.panel());
                        }
                        if let Some(name) = interaction.take_workspace() {
                            selected_workspace =
                                Some((name, chunk.get(offset + 1..).unwrap_or_default().to_vec()));
                            break;
                        }
                        if filter.take_detach() {
                            detach = true;
                            break;
                        }
                        if interaction.take_back() {
                            filter.show_commands();
                        }
                    }
                }
            }
            let current = state_rx.borrow_and_update().clone();
            if let Some(current) = &current {
                interaction.reconcile_copy(current);
            }
            refresh_copy_read(
                &mut interaction,
                &input_tx,
                &mut copy_reply_rx,
                &mut copy_request,
                &mut filtered,
            )
            .await;
            if interaction.take_back() {
                filter.show_commands();
            }
            let now = tokio::time::Instant::now();
            if filter.prefix_epoch() != epoch {
                started = Some(now);
                epoch = filter.prefix_epoch();
            }
            if !filter.command_pending() {
                started = None;
            }
            escape_started = if filter.escape_pending() || interaction.escape_pending() {
                Some(escape_started.unwrap_or(now))
            } else {
                None
            };
            let show = filter.hints_requested()
                || hint_preferences.automatic
                    && started.is_some_and(|time| {
                        now.duration_since(time)
                            >= std::time::Duration::from_millis(hint_preferences.delay_ms)
                    });
            prefix_visible = show;
            let next = interaction
                .panel()
                .or_else(|| {
                    if show {
                        current.as_deref().map(|state| {
                            hints::HintPanel::commands(
                                filter.bindings(),
                                enable_picker,
                                filter.hint_page(),
                                state,
                            )
                        })
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    state_rx
                        .borrow()
                        .as_ref()
                        .filter(|state| !state.popups().is_empty())
                        .map(|_| hints::HintPanel::popup(filter.bindings()))
                });
            let copy_ui = interaction.copy_ui();
            copy_ui_tx.send_if_modified(|current| {
                if *current == copy_ui {
                    false
                } else {
                    *current = copy_ui;
                    true
                }
            });
            hint_tx.send_if_modified(|current| {
                if *current == next {
                    false
                } else {
                    *current = next;
                    true
                }
            });
            let detach = detach || filter.take_detach();
            let selected = selected_workspace;
            if detach || selected.is_some() {
                if !filtered.is_empty() {
                    let _ = input_tx
                        .send(crate::local::ClientMessage::PaneInput {
                            bytes: std::mem::take(&mut filtered),
                        })
                        .await;
                    // A read-only ordered acknowledgement drains preceding input
                    // before cancellation closes this attachment.
                    let _ = input_tx
                        .send(crate::local::ClientMessage::Control {
                            request: crate::control::Request::List { id: 0 },
                        })
                        .await;
                    let _ =
                        tokio::time::timeout(crate::local::FRAME_TIMEOUT, reply_rx.recv()).await;
                }
                let _ = action_tx.try_send(if detach { None } else { selected });
                // Keep input_tx alive until the picker branch cancels this bridge. Otherwise
                // run_client may observe EOF and finish while the select is polling it, racing
                // the picker notification even with a biased select.
                std::future::pending::<()>().await;
                break;
            }
            if !filtered.is_empty()
                && input_tx
                    .send(crate::local::ClientMessage::PaneInput { bytes: filtered })
                    .await
                    .is_err()
            {
                break;
            }
        }
        let tail = filter.flush();
        if !tail.is_empty() {
            let _ = input_tx
                .send(crate::local::ClientMessage::PaneInput { bytes: tail })
                .await;
        }
    });
    // Scope the connection future so its terminal is restored before input producers are joined
    // and before the caller opens the workspace picker.
    let cancel = tokio_util::sync::CancellationToken::new();
    let (result, picker) = {
        let connection = async {
            let term = WorkspaceTerminal::enter_default(clipboard, notifications)
                .map_err(|error| anyhow::anyhow!("entering fux terminal: {error}"))?
                .with_input_policy(policy_tx)
                .with_hints(hint_rx)
                .with_copy_ui(copy_ui_rx)
                .with_mouse_layout(mouse_layout_tx);
            prepared
                .with_repaint(repaint)
                .with_updates(state_tx, reply_tx)
                .with_copy_views(copy_reply_tx)
                .with_copy_repaint(copy_repaint)
                .run_interactive(term, input_rx, channels.resize_rx, cancel.clone())
                .await
        };
        tokio::pin!(connection);
        tokio::select! {
            biased;
            Some(picker) = action_rx.recv() => {
                cancel.cancel();
                (connection.await, picker)
            },
            result = &mut connection => (result, None),
            _ = interrupt.recv() => { cancel.cancel(); (connection.await, None) },
            _ = terminate.recv() => { cancel.cancel(); (connection.await, None) },
            _ = hangup.recv() => { cancel.cancel(); (connection.await, None) },
        }
    };
    bridge.abort();
    let _ = bridge.await;
    let shutdown = tasks.shutdown().await;
    match (result, shutdown, picker) {
        (Err(error), _, _) => Err(error),
        (_, Err(error), _) => Err(error.context("stopping terminal input producers")),
        (Ok(exit), Ok(()), None) => Ok(ConnectOutcome::Exited(exit)),
        (Ok(_), Ok(()), Some(name)) => Ok(ConnectOutcome::Workspace {
            name: name.0,
            pending: name.1,
        }),
    }
}

async fn refresh_copy_read(
    interaction: &mut interaction::Interaction,
    input: &tokio::sync::mpsc::Sender<crate::local::ClientMessage>,
    replies: &mut tokio::sync::mpsc::Receiver<crate::local::CopyViewReply>,
    sequence: &mut u64,
    filtered: &mut Vec<u8>,
) {
    let Some((pane, offset)) = interaction.take_copy_read() else {
        return;
    };
    if !filtered.is_empty() {
        let _ = input
            .send(crate::local::ClientMessage::PaneInput {
                bytes: std::mem::take(filtered),
            })
            .await;
    }
    *sequence = sequence.wrapping_add(1);
    let request = *sequence;
    let _ = input
        .send(crate::local::ClientMessage::CopyView {
            request,
            pane,
            offset,
        })
        .await;
    let reply = tokio::time::timeout(crate::local::FRAME_TIMEOUT, async {
        while let Some(reply) = replies.recv().await {
            if reply.request == request && reply.pane == pane {
                return Some(reply);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    interaction.install_copy_view(reply.unwrap_or(crate::local::CopyViewReply {
        request,
        pane,
        view: None,
    }));
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

pub(crate) const fn rat_color(color: Color) -> ratatui_core::style::Color {
    match color {
        Color::Default => ratatui_core::style::Color::Reset,
        Color::Indexed(index) => ratatui_core::style::Color::Indexed(index),
        Color::Rgb(red, green, blue) => ratatui_core::style::Color::Rgb(red, green, blue),
    }
}
