//! The viewer process: private terminal, private menu/history/selection state, one attachment
//! connection. The server frame is the only shared truth; everything here is a view of it.

pub mod backend;
pub mod controller;
pub mod copy;
pub mod hints;
pub mod input;
pub mod io;
pub mod render;
pub mod screen;

use crate::commands::Action;
use crate::config::Config;
use crate::proto::attach::{
    ClientMessage, FRAME_TIMEOUT, MAX_CLIENT_FRAME, MAX_INPUT_CHUNK, MAX_SERVER_FRAME,
    ServerMessage, VERSION, read_frame, write_frame,
};
use crate::proto::control::{FocusTarget, Reply, Request, TabAction};
use crate::view::Frame;
use controller::{Controller, MouseDisposition};
use hints::HintPanel;
use input::{InputEvent, PrefixFilter};
use screen::Screen;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// How long a lone Escape waits for the rest of a sequence before it is taken literally.
const ESCAPE_DELAY: Duration = Duration::from_millis(35);

/// Remembers when a pending Escape started, so frames arriving faster than the disambiguation
/// window cannot postpone the decision indefinitely.
#[derive(Default)]
struct EscapeTimer {
    deadline: Option<tokio::time::Instant>,
}

impl EscapeTimer {
    fn update(&mut self, pending: bool, now: tokio::time::Instant) -> Option<tokio::time::Instant> {
        if !pending {
            self.deadline = None;
        } else if self.deadline.is_none() {
            self.deadline = Some(now + ESCAPE_DELAY);
        }
        self.deadline
    }
}
/// Input buffered while a request awaits its reply; beyond this the viewer disconnects.
const MAX_PENDING_INPUT: usize = 64 * 1024;

pub struct AttachOptions {
    /// Manager socket for workspace choosing/creation; `None` for explicit socket attachments.
    pub manager_socket: Option<PathBuf>,
}

/// Authenticates and negotiates before touching the terminal.
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub async fn connect(path: &Path, rows: u16, columns: u16) -> anyhow::Result<Self> {
        crate::proto::socket::check_private_socket_path(path)?;
        let stream = tokio::time::timeout(FRAME_TIMEOUT, UnixStream::connect(path)).await??;
        let stream = stream.into_std()?;
        crate::proto::socket::authorize_peer(&stream)?;
        let mut stream = UnixStream::from_std(stream)?;
        write_frame(
            &mut stream,
            &ClientMessage::Hello {
                version: VERSION,
                rows,
                columns,
            },
            MAX_CLIENT_FRAME,
        )
        .await?;
        match tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut stream, MAX_SERVER_FRAME))
            .await??
        {
            ServerMessage::Hello { version: VERSION } => Ok(Self { stream }),
            ServerMessage::Error { message } => anyhow::bail!("{message}"),
            _ => anyhow::bail!(
                "incompatible fux session server; use matching versions or restart it after saving your work"
            ),
        }
    }
}

enum Outstanding {
    Control,
    View,
}

/// Attaches to a workspace socket and runs the viewer until detach, workspace retirement or a
/// signal. Returns the exit code to propagate (`None` for detach).
pub async fn attach(
    socket: &Path,
    config: &Config,
    options: AttachOptions,
) -> anyhow::Result<Option<u32>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let bindings = crate::commands::configured_bindings(config)?;
    let (rows, cols) = backend::TerminaBackend::new()
        .and_then(|backend| backend::TerminalBackend::size(&backend))
        .unwrap_or((24, 80));
    let connection = tokio::select! {
        result = Connection::connect(socket, rows, cols) => result?,
        _ = interrupt.recv() => return Ok(None),
        _ = terminate.recv() => return Ok(None),
        _ = hangup.recv() => return Ok(None),
    };
    // Negotiation succeeded: only now does the terminal enter raw mode.
    let mut screen = Screen::enter_default(config.clipboard.writes())?;
    let mut io = io::ClientIo::spawn()?;
    let result = run(
        connection,
        &mut screen,
        &mut io,
        bindings,
        options,
        &mut interrupt,
        &mut terminate,
        &mut hangup,
    )
    .await;
    drop(screen);
    io.shutdown().await?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run(
    connection: Connection,
    screen: &mut Screen<backend::TerminaBackend>,
    io: &mut io::ClientIo,
    bindings: crate::commands::ClientBindings,
    options: AttachOptions,
    interrupt: &mut tokio::signal::unix::Signal,
    terminate: &mut tokio::signal::unix::Signal,
    hangup: &mut tokio::signal::unix::Signal,
) -> anyhow::Result<Option<u32>> {
    let (mut reader, mut writer) = connection.stream.into_split();
    let (message_tx, mut message_rx) = mpsc::channel::<std::io::Result<ServerMessage>>(4);
    let reader_task = tokio::spawn(async move {
        loop {
            let message = read_frame::<_, ServerMessage>(&mut reader, MAX_SERVER_FRAME).await;
            let failed = message.is_err();
            if message_tx.send(message).await.is_err() || failed {
                break;
            }
        }
    });
    let workspaces_enabled = options.manager_socket.is_some();
    let mut filter = PrefixFilter::new(bindings);
    let mut controller = Controller::new(workspaces_enabled);
    let mut frame: Option<Frame> = None;
    let mut pending: VecDeque<u8> = VecDeque::new();
    let mut resolved: VecDeque<InputEvent> = VecDeque::new();
    let mut outstanding: Option<Outstanding> = None;
    let mut hint_page: usize = 0;
    let mut lookups = tokio::task::JoinSet::new();
    let mut detaching = false;
    let (rows, cols) = screen.size()?;
    write_frame(
        &mut writer,
        &ClientMessage::Resize {
            rows,
            columns: cols,
        },
        MAX_CLIENT_FRAME,
    )
    .await?;
    let mut escape_timer = EscapeTimer::default();
    let outcome = loop {
        let escape_deadline = escape_timer.update(
            filter.escape_pending() || controller.escape_pending(),
            tokio::time::Instant::now(),
        );
        let request_deadline = outstanding
            .as_ref()
            .map(|_| tokio::time::Instant::now() + FRAME_TIMEOUT);
        tokio::select! {
            biased;
            _ = interrupt.recv() => break Ok(None),
            _ = terminate.recv() => break Ok(None),
            _ = hangup.recv() => break Ok(None),
            message = message_rx.recv() => {
                let Some(message) = message else { break Err(anyhow::anyhow!("session server disconnected")) };
                match message? {
                    ServerMessage::State { state } => {
                        anyhow::ensure!(state.valid(), "invalid frame from session server");
                        filter.configure(state.bindings.clone());
                        controller.reconcile(&state);
                        if let Some(previous) = &frame
                            && previous.workspace != state.workspace
                        {
                            // Workspace identity is shown transiently on switches, not permanently.
                            controller.report_info(format!("Workspace {}", state.workspace));
                        }
                        frame = Some(*state);
                    }
                    ServerMessage::Reply { reply } => {
                        if matches!(outstanding, Some(Outstanding::Control)) {
                            outstanding = None;
                        }
                        if let Reply::Failed { error, .. } = reply {
                            controller.report_error(error.message);
                            filter.show_commands();
                        }
                    }
                    ServerMessage::View { reply } => {
                        if matches!(outstanding, Some(Outstanding::View)) {
                            outstanding = None;
                        }
                        controller.install_view(reply);
                    }
                    ServerMessage::Exited { code } => break Ok(code),
                    ServerMessage::Error { message } => break Err(anyhow::anyhow!("{message}")),
                    ServerMessage::Hello { .. } => break Err(anyhow::anyhow!("unexpected server hello")),
                }
            }
            chunk = io.input_rx.recv() => {
                let Some(chunk) = chunk else { break Ok(None) };
                if detaching { continue; }
                if pending.len() + chunk.len() > MAX_PENDING_INPUT {
                    break Err(anyhow::anyhow!("input buffered beyond limit while waiting for the server"));
                }
                pending.extend(chunk);
            }
            Some(()) = io.resize_rx.recv() => {
                let (rows, cols) = screen.size()?;
                write_frame(&mut writer, &ClientMessage::Resize { rows, columns: cols }, MAX_CLIENT_FRAME).await?;
                screen.invalidate();
            }
            Some(result) = lookups.join_next(), if !lookups.is_empty() => {
                let current = frame.as_ref().map(|frame| frame.workspace.clone()).unwrap_or_default();
                controller.workspaces_loaded(result.map_err(anyhow::Error::from).and_then(|result| result), &current);
                let replay = controller.take_loading_input();
                for byte in replay.into_iter().rev() { pending.push_front(byte); }
            }
            () = async {
                match escape_deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await }
            } => {
                if controller.owns_input() { controller.resolve_escape(); }
                else {
                    // Resolved events are dispatched as they are; feeding the bytes back through
                    // the filter would buffer the Escape again and never deliver it.
                    resolved.extend(filter.resolve_escape());
                }
            }
            () = async {
                match request_deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await }
            } => {
                break Err(anyhow::anyhow!("session server did not answer a request in time"));
            }
        }
        // Apply as much buffered input as the outstanding-request rule allows.
        let Some(current) = frame.as_ref() else {
            continue;
        };
        let mut pane_bytes = Vec::new();
        while outstanding.is_none() && !detaching {
            let events = if let Some(event) = resolved.pop_front() {
                vec![event]
            } else {
                let Some(byte) = pending.pop_front() else {
                    break;
                };
                if controller.owns_input() {
                    controller.clear_error();
                    if let Some(request) = controller.feed(byte, current) {
                        flush(&mut writer, &mut pane_bytes).await?;
                        write_frame(
                            &mut writer,
                            &ClientMessage::Control { request },
                            MAX_CLIENT_FRAME,
                        )
                        .await?;
                        outstanding = Some(Outstanding::Control);
                    }
                    Vec::new()
                } else {
                    if !filter.command_pending() {
                        // A fresh keypress outside command mode dismisses notices; inside command
                        // mode a failure explanation stays visible next to the popup.
                        controller.clear_error();
                    }
                    filter.feed(&[byte])
                }
            };
            {
                for event in events {
                    match event {
                        InputEvent::Bytes(bytes) => {
                            controller.clear_error();
                            pane_bytes.extend(bytes);
                        }
                        InputEvent::Command(action) => {
                            flush(&mut writer, &mut pane_bytes).await?;
                            controller.clear_error();
                            match dispatch(
                                action,
                                current,
                                &mut controller,
                                &mut filter,
                                workspaces_enabled,
                            ) {
                                Dispatch::Send(request) => {
                                    write_frame(
                                        &mut writer,
                                        &ClientMessage::Control { request },
                                        MAX_CLIENT_FRAME,
                                    )
                                    .await?;
                                    outstanding = Some(Outstanding::Control);
                                }
                                Dispatch::Detach => {
                                    write_frame(
                                        &mut writer,
                                        &ClientMessage::Detach,
                                        MAX_CLIENT_FRAME,
                                    )
                                    .await?;
                                    detaching = true;
                                    pending.clear();
                                }
                                Dispatch::LoadWorkspaces => {
                                    if let Some(path) = options.manager_socket.clone()
                                        && lookups.is_empty()
                                    {
                                        lookups.spawn_blocking(move || {
                                            crate::daemon::workspace_names(&path)
                                        });
                                    }
                                }
                                Dispatch::Local => {}
                            }
                        }
                        InputEvent::Mouse(event, raw) => match controller.mouse(event, current) {
                            MouseDisposition::Local | MouseDisposition::Ignore => {}
                            MouseDisposition::Forward => {
                                flush(&mut writer, &mut pane_bytes).await?;
                                let _ = raw;
                                write_frame(
                                    &mut writer,
                                    &ClientMessage::Mouse {
                                        event,
                                        generation: current.generation,
                                    },
                                    MAX_CLIENT_FRAME,
                                )
                                .await?;
                            }
                        },
                        InputEvent::Unknown => {
                            hint_page = if filter.revealed() { hint_page } else { 0 };
                        }
                        InputEvent::Cancel => {
                            hint_page = 0;
                            controller.clear_error();
                        }
                        InputEvent::Page(delta) => {
                            let pages = HintPanel::commands(
                                filter.bindings(),
                                workspaces_enabled,
                                0,
                                current,
                            )
                            .page_count(screen.size()?.0);
                            hint_page = (hint_page as i64 + i64::from(delta))
                                .rem_euclid(pages.max(1) as i64)
                                as usize;
                        }
                    }
                }
            }
            if controller.take_back() {
                filter.show_commands();
                hint_page = 0;
            }
            if let Some(text) = controller.take_copied() {
                match screen.copy_to_clipboard(&text)? {
                    true => controller.report_info(format!(
                        "Copied {} bytes to the terminal clipboard",
                        text.len()
                    )),
                    false if !screen.clipboard_enabled() => controller.report_error(
                        "Clipboard writes are disabled (config: clipboard = \"write-only\")",
                    ),
                    false => controller.report_error(
                        "Selection exceeds the clipboard limit; select a smaller region",
                    ),
                }
            }
            if let Some((request, pane, offset)) = controller.take_read() {
                flush(&mut writer, &mut pane_bytes).await?;
                write_frame(
                    &mut writer,
                    &ClientMessage::View {
                        request,
                        pane,
                        offset,
                    },
                    MAX_CLIENT_FRAME,
                )
                .await?;
                outstanding = Some(Outstanding::View);
            }
        }
        flush(&mut writer, &mut pane_bytes).await?;
        if let Some((request, pane, offset)) = controller.take_read()
            && outstanding.is_none()
        {
            write_frame(
                &mut writer,
                &ClientMessage::View {
                    request,
                    pane,
                    offset,
                },
                MAX_CLIENT_FRAME,
            )
            .await?;
            outstanding = Some(Outstanding::View);
        }
        if controller.take_back() {
            filter.show_commands();
            hint_page = 0;
        }
        // Paint once per loop turn, after every ready event has been applied.
        let panel = controller.panel().or_else(|| {
            filter.popup_visible().then(|| {
                HintPanel::commands(filter.bindings(), workspaces_enabled, hint_page, current)
            })
        });
        let local = controller.local_view();
        screen.render(current, local.as_ref(), panel.as_ref())?;
    };
    reader_task.abort();
    let _ = reader_task.await;
    outcome
}

async fn flush(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    bytes: &mut Vec<u8>,
) -> anyhow::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
        write_frame(
            writer,
            &ClientMessage::Input {
                bytes: chunk.to_vec(),
            },
            MAX_CLIENT_FRAME,
        )
        .await?;
    }
    bytes.clear();
    Ok(())
}

enum Dispatch {
    Send(Request),
    Detach,
    LoadWorkspaces,
    Local,
}

/// Turns a bound action into a request, a mode entry or a local effect, honouring availability.
fn dispatch(
    action: Action,
    frame: &Frame,
    controller: &mut Controller,
    filter: &mut PrefixFilter,
    workspaces: bool,
) -> Dispatch {
    if let Some(reason) = action.unavailable(frame, workspaces) {
        controller.report_error(reason);
        filter.show_commands();
        return Dispatch::Local;
    }
    match action {
        Action::Detach => Dispatch::Detach,
        Action::Help => {
            filter.show_commands();
            Dispatch::Local
        }
        Action::SplitSide | Action::SplitStack => Dispatch::Send(Request::Split {
            id: 0,
            axis: if action == Action::SplitSide {
                crate::layout::Axis::Horizontal
            } else {
                crate::layout::Axis::Vertical
            },
            target: frame.focused,
            cwd: None,
            argv: Vec::new(),
        }),
        Action::FocusLeft => Dispatch::Send(Request::Focus {
            id: 0,
            target: FocusTarget::Left,
        }),
        Action::FocusRight => Dispatch::Send(Request::Focus {
            id: 0,
            target: FocusTarget::Right,
        }),
        Action::FocusUp => Dispatch::Send(Request::Focus {
            id: 0,
            target: FocusTarget::Up,
        }),
        Action::FocusDown => Dispatch::Send(Request::Focus {
            id: 0,
            target: FocusTarget::Down,
        }),
        Action::NewTab => Dispatch::Send(Request::Tab {
            id: 0,
            action: TabAction::New { name: None },
        }),
        Action::NextTab => Dispatch::Send(Request::Tab {
            id: 0,
            action: TabAction::Next,
        }),
        Action::PreviousTab => Dispatch::Send(Request::Tab {
            id: 0,
            action: TabAction::Previous,
        }),
        Action::ChooseWorkspace => {
            controller.enter(action, frame);
            Dispatch::LoadWorkspaces
        }
        Action::CopyMode
        | Action::ChooseTab
        | Action::RenameTab
        | Action::CloseTab
        | Action::ClosePane
        | Action::ResizeMode
        | Action::NewWorkspace => {
            if !controller.enter(action, frame) {
                controller.report_error("That command is not available right now");
                filter.show_commands();
            }
            Dispatch::Local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_deadline_is_fixed_when_the_escape_arrives() {
        let start = tokio::time::Instant::now();
        let mut timer = EscapeTimer::default();
        let first = timer.update(true, start);
        assert_eq!(first, Some(start + ESCAPE_DELAY));
        // Frames arriving inside the window must not postpone the decision.
        assert_eq!(timer.update(true, start + Duration::from_millis(20)), first);
        assert_eq!(timer.update(false, start + Duration::from_millis(40)), None);
        let later = start + Duration::from_millis(50);
        assert_eq!(timer.update(true, later), Some(later + ESCAPE_DELAY));
    }
}
