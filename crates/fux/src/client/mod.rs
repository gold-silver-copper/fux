//! The viewer process: private terminal, private menu/history/selection state, one attachment
//! connection. The server frame is the only shared truth; everything here is a view of it.

pub mod backend;
mod capture;
mod context;
pub mod controller;
pub mod copy;
mod drag;
mod effects;
pub mod hints;
mod history;
pub mod input;
mod interaction;
pub mod io;
mod popup;
mod read_window;
pub mod render;
pub mod screen;
pub mod text;

use crate::commands::{Action, Target};
use crate::config::Config;
use crate::proto::attach::{
    ClientMessage, FRAME_TIMEOUT, MAX_CLIENT_FRAME, MAX_INPUT_CHUNK, MAX_SERVER_FRAME,
    ServerMessage, read_frame, write_frame,
};
use crate::proto::control::{FocusTarget, Reply, Request, TabAction};
use crate::view::Frame;
use controller::{Controller, MouseDisposition};
use hints::HintPanel;
use input::{InputEvent, PrefixFilter, ScrollBy};
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
    pub initial: Option<crate::proto::attach::InitialTarget>,
    /// Manager socket for workspace choosing/creation; `None` for explicit socket attachments.
    pub manager_socket: Option<PathBuf>,
}

/// Authenticates and negotiates before touching the terminal.
pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    pub async fn connect(path: &Path, rows: u16, columns: u16) -> anyhow::Result<Self> {
        Self::connect_target(path, rows, columns, None).await
    }
    pub async fn connect_target(
        path: &Path,
        rows: u16,
        columns: u16,
        initial: Option<crate::proto::attach::InitialTarget>,
    ) -> anyhow::Result<Self> {
        crate::proto::socket::check_private_socket_path(path)?;
        let stream = tokio::time::timeout(FRAME_TIMEOUT, UnixStream::connect(path)).await??;
        let stream = stream.into_std()?;
        crate::proto::socket::authorize_peer(&stream)?;
        let mut stream = UnixStream::from_std(stream)?;
        write_frame(
            &mut stream,
            &ClientMessage::Hello {
                rows,
                columns,
                initial,
            },
            MAX_CLIENT_FRAME,
        )
        .await?;
        let answer = tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut stream, MAX_SERVER_FRAME))
            .await?
            .map_err(|error| {
                anyhow::anyhow!(
                    "session server answered the hello with an unreadable frame ({error}); restart it if it is older than this fux"
                )
            })?;
        match answer {
            ServerMessage::Hello {} => Ok(Self { stream }),
            ServerMessage::Error { message } => anyhow::bail!("session server: {message}"),
            _ => anyhow::bail!(
                "the session server did not answer the hello; restart it if it is older than this fux"
            ),
        }
    }
}

struct WaitingCommand {
    action: Action,
    target: Option<Target>,
    epoch: u64,
    origin: effects::Identity,
    follows_workspace: bool,
}

/// Effects waiting for the server, plus the pane bytes gathered ahead of them. While a manager
/// mutation is in flight, input is pinned to the frame it was typed against so it cannot land in
/// a replacement workspace.
#[derive(Default)]
struct Outbox {
    effects: effects::Queue,
    pane_bytes: Vec<u8>,
    manager_mutations: usize,
}

impl Outbox {
    fn flush_input(&mut self, frame: &Frame) -> anyhow::Result<()> {
        let pinned = self.manager_mutations > 0 || self.effects.manager_pending();
        self.effects
            .input(&mut self.pane_bytes, pinned.then_some(frame))
    }
    /// Queues an effect behind the input typed before it.
    fn push(&mut self, effect: effects::Effect, frame: &Frame) -> anyhow::Result<()> {
        self.flush_input(frame)?;
        self.effects.push(effect, Some(frame))
    }
    fn idle(&self) -> bool {
        self.manager_mutations == 0 && self.effects.is_empty()
    }
}

/// The process signals that end a viewer.
pub struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl Signals {
    fn install() -> anyhow::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }
    /// Resolves when any of the three arrives.
    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}

struct OutstandingControl {
    request: u64,
    navigates_workspace: bool,
    deadline: tokio::time::Instant,
}

async fn send_control(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    mut request: Request,
    next: &mut u64,
) -> anyhow::Result<OutstandingControl> {
    let id = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("control request IDs exhausted"))?;
    request.set_id(id);
    let navigates_workspace = effects::navigates_workspace(&request);
    send(writer, &ClientMessage::Control { request }).await?;
    Ok(OutstandingControl {
        request: id,
        navigates_workspace,
        deadline: tokio::time::Instant::now() + FRAME_TIMEOUT,
    })
}

/// Generic viewer termination evidence for a supervising process.
#[derive(serde::Serialize)]
pub struct AttachExit {
    pub fux_attach_exit: u8,
    pub code: Option<u32>,
    pub detached: bool,
}

/// Run a viewer and return its optional process exit code.
pub async fn attach(
    socket: &Path,
    config: &Config,
    options: AttachOptions,
) -> anyhow::Result<Option<u32>> {
    Ok(attach_reported(socket, config, options).await?.code)
}

/// Run a viewer while preserving explicit detach evidence separately from its exit code.
pub async fn attach_reported(
    socket: &Path,
    config: &Config,
    options: AttachOptions,
) -> anyhow::Result<AttachExit> {
    let mut signals = Signals::install()?;
    let bindings = crate::commands::configured_bindings(config)?;
    let (rows, cols) = backend::TerminaBackend::new()
        .and_then(|backend| backend::TerminalBackend::size(&backend))
        .unwrap_or((24, 80));
    let connection = tokio::select! {
        result = Connection::connect_target(socket, rows, cols, options.initial.clone()) => result?,
        () = signals.recv() => return Ok(AttachExit { fux_attach_exit: 1, code: None, detached: false }),
    };
    // Negotiation succeeded: only now does the terminal enter raw mode.
    let mut screen = Screen::enter_default(
        config.clipboard.writes(),
        render::Palette::from(&config.style),
    )?;
    let mut io = io::ClientIo::spawn()?;
    let result = run(
        connection,
        &mut screen,
        &mut io,
        bindings,
        options,
        config.final_records.retain_ms,
        &mut signals,
    )
    .await;
    drop(screen);
    io.shutdown().await?;
    result
}

async fn run(
    connection: Connection,
    screen: &mut Screen<backend::TerminaBackend>,
    io: &mut io::ClientIo,
    bindings: crate::commands::ClientBindings,
    options: AttachOptions,
    final_retain_ms: u64,
    signals: &mut Signals,
) -> anyhow::Result<AttachExit> {
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
    let mut outstanding: Option<OutstandingControl> = None;
    let mut next_control = 1;
    let mut reads = read_window::ReadWindow::default();
    let mut hint_scroll: usize = 0;
    let mut popup = popup::Popup::default();
    let mut lookups = tokio::task::JoinSet::new();
    let mut manager_commands = tokio::task::JoinSet::new();
    let mut outbox = Outbox::default();
    let mut waiting_command: Option<WaitingCommand> = None;
    let mut detaching = false;
    let (rows, cols) = screen.size()?;
    send(
        &mut writer,
        &ClientMessage::Resize {
            rows,
            columns: cols,
        },
    )
    .await?;
    let mut escape_timer = EscapeTimer::default();
    let outcome = loop {
        let escape_deadline = escape_timer.update(
            filter.escape_pending() || controller.escape_pending(),
            tokio::time::Instant::now(),
        );
        let request_deadline = outstanding.as_ref().map(|pending| pending.deadline);
        let history_deadline = reads.deadline();
        let notice_deadline = controller
            .notice_deadline(std::time::Instant::now())
            .map(tokio::time::Instant::from_std);
        tokio::select! {
            biased;
            () = signals.recv() => break Ok(AttachExit { fux_attach_exit: 1, code: None, detached: false }),
            () = at(escape_deadline) => {
                if controller.owns_input() { controller.resolve_escape(); }
                else {
                    // Resolved events are dispatched as they are; feeding the bytes back through
                    // the filter would buffer the Escape again and never deliver it.
                    resolved.extend(filter.resolve_history_escape(controller.has_history()));
                }
            }
            () = at(history_deadline) => {
                for (request, pane) in reads.expire(tokio::time::Instant::now()) {
                    controller.history_failed(request, pane);
                }
            }
            () = at(request_deadline) => {
                break Err(anyhow::anyhow!("session server did not answer a request in time"));
            }
            () = std::future::ready(()), if waiting_command.is_some()
                && controller.waiting_command_ready() && outstanding.is_none() && outbox.idle() => {}
            message = message_rx.recv() => {
                let Some(message) = message else { break Err(anyhow::anyhow!("session server disconnected")) };
                match message? {
                    ServerMessage::State { state } => {
                        let mut current = frame.take().unwrap_or_default();
                        let switched = !current.workspace.is_empty() && current.workspace != state.workspace;
                        current
                            .apply(*state)
                            .map_err(|error| anyhow::anyhow!("invalid frame from session server: {error}"))?;
                        controller.reconcile(&current);
                        if switched {
                            // Workspace identity is shown transiently on switches, not permanently.
                            controller.report_info(format!("Workspace {}", current.workspace));
                        }
                        frame = Some(current);
                    }
                    ServerMessage::Bindings { bindings } => filter.configure(bindings),
                    ServerMessage::Reply { reply } => {
                        if outstanding.as_ref().is_some_and(|pending| pending.request == reply.id()) {
                            outstanding = None;
                            if let Reply::Failed { error, .. } = reply {
                                controller.report_error(error.message);
                            }
                        }
                    }
                    ServerMessage::View { reply } => {
                        if reads.complete(reply.request, reply.pane) {
                            controller.install_view(reply);
                        }
                    }
                    ServerMessage::Exited { code } => break Ok(AttachExit { fux_attach_exit: 1, code, detached: detaching && code.is_none() }),
                    ServerMessage::Error { message } => break Err(anyhow::anyhow!("{message}")),
                    ServerMessage::Hello { .. } => break Err(anyhow::anyhow!("unexpected server hello")),
                }
            }
            chunk = io.input_rx.recv() => {
                let Some(chunk) = chunk else { break Ok(AttachExit { fux_attach_exit: 1, code: None, detached: false }) };
                if detaching { continue; }
                if pending.len() + chunk.len() > MAX_PENDING_INPUT {
                    break Err(anyhow::anyhow!("input buffered beyond limit while waiting for the server"));
                }
                pending.extend(chunk);
            }
            Some(()) = io.resize_rx.recv() => {
                let (rows, cols) = screen.size()?;
                send(&mut writer, &ClientMessage::Resize { rows, columns: cols }).await?;
                screen.invalidate();
            }
            Some(result) = manager_commands.join_next(), if !manager_commands.is_empty() => {
                let (epoch, mutating, result) = result.map_err(anyhow::Error::from)?;
                if mutating { outbox.manager_mutations = outbox.manager_mutations.saturating_sub(1); }
                if epoch != controller.interaction_epoch() {
                    // Old results cannot reopen a dialog or replay input into its replacement.
                    if mutating {
                        match result {
                            Ok(crate::daemon::ManagerReply::Failed { message }) => controller.report_error(message),
                            Ok(crate::daemon::ManagerReply::Layout { result: crate::proto::control::Reply::Failed { error, .. } }) => controller.report_error(error.message),
                            Err(error) => controller.report_error(format!("Earlier workspace operation failed: {error}")),
                            Ok(_) => controller.report_info("Earlier workspace operation completed"),
                        }
                    }
                } else { match result {
                    Ok(crate::daemon::ManagerReply::Catalog { catalog }) => {
                        controller.destinations_loaded(catalog);
                        for byte in controller.take_loading_input().into_iter().rev() { pending.push_front(byte); }
                    }
                    Ok(crate::daemon::ManagerReply::Names { .. }) => controller.report_info("Workspace order updated"),
                    Ok(crate::daemon::ManagerReply::Layout { result: crate::proto::control::Reply::Completed { .. } }) => controller.report_info("Pane moved to workspace"),
                    Ok(crate::daemon::ManagerReply::Layout { result: crate::proto::control::Reply::Failed { error, .. } }) => controller.report_error(error.message),
                    Ok(crate::daemon::ManagerReply::Failed { message }) => controller.manager_failed(message),
                    Ok(_) => controller.manager_failed("Unexpected manager reply"),
                    Err(error) => controller.manager_failed(format!("Manager request failed; inspect before retrying: {error}")),
                }}
            }
            Some(result) = lookups.join_next(), if !lookups.is_empty() => {
                let current = frame.as_ref().map(|frame| frame.workspace.clone()).unwrap_or_default();
                let (epoch, result) = result.map_err(anyhow::Error::from)?;
                if controller.workspaces_loaded_for(epoch, result, &current) {
                    let replay = controller.take_loading_input();
                    for byte in replay.into_iter().rev() { pending.push_front(byte); }
                }
            }
            () = at(notice_deadline) => {
                // The bar notice timed out; repaint without it.
                controller.expire_notice(std::time::Instant::now());
            }
        }
        // Apply as much buffered input as the outstanding-request rule allows.
        let Some(current) = frame.as_ref() else {
            continue;
        };
        while !detaching {
            if waiting_command
                .as_ref()
                .is_some_and(|waiting| waiting.epoch != controller.interaction_epoch())
            {
                waiting_command = None;
            }
            if waiting_command.as_ref().is_some_and(|waiting| {
                !waiting.follows_workspace && waiting.origin != effects::Identity::of(current)
            }) {
                waiting_command = None;
                controller.cancel_waiting_command();
                controller.report_error("Waiting command discarded: its workspace changed");
            }
            let ready = outstanding.is_none() && outbox.idle();
            let resumed = if ready && controller.waiting_command_ready() {
                waiting_command.take().map(|waiting| {
                    let replay = controller.finish_waiting_command();
                    (waiting.action, waiting.target, replay)
                })
            } else {
                None
            };
            let contextual = controller.take_action();
            let events = if let Some((action, _, _)) = &resumed {
                vec![InputEvent::Command(*action)]
            } else if let Some((action, _)) = &contextual {
                vec![InputEvent::Command(*action)]
            } else if let Some(event) = resolved.pop_front() {
                vec![event]
            } else {
                let Some(byte) = pending.pop_front() else {
                    break;
                };
                if byte != 27 && byte == filter.bindings().prefix() && controller.prefix_from_copy()
                {
                    filter.feed(&[byte])
                } else if controller.owns_input() {
                    controller.clear_error();
                    if let Some(request) = controller.feed(byte, current) {
                        outbox.push(effects::Effect::Control(request), current)?;
                        if let Some(action) = controller.wait_for_layout_reply() {
                            waiting_command = Some(WaitingCommand {
                                action,
                                target: Some(Target::of(current)),
                                epoch: controller.interaction_epoch(),
                                origin: effects::Identity::of(current),
                                follows_workspace: false,
                            });
                        }
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
                            controller.resume_input(current);
                            outbox.pane_bytes.extend(bytes);
                        }
                        InputEvent::Escape => {
                            if !controller.dismiss_history() {
                                outbox.pane_bytes.push(27);
                            }
                        }
                        InputEvent::Command(action) => {
                            if let Some(button) = popup.take_capture() {
                                controller.adopt_popup_capture(button);
                            }
                            outbox.flush_input(current)?;
                            controller.clear_error();
                            let target = resumed
                                .as_ref()
                                .and_then(|(_, target, _)| *target)
                                .or_else(|| contextual.map(|(_, target)| target));
                            if (outstanding.is_some() || !outbox.idle())
                                && !matches!(action, Action::CopyMode | Action::Detach)
                            {
                                controller.wait_for_command();
                                waiting_command = Some(WaitingCommand {
                                    action,
                                    target,
                                    epoch: controller.interaction_epoch(),
                                    origin: effects::Identity::of(current),
                                    follows_workspace: target.is_none()
                                        && (outbox.effects.navigates_workspace()
                                            || outstanding.as_ref().is_some_and(|pending| {
                                                pending.navigates_workspace
                                            })),
                                });
                                continue;
                            }
                            let outcome = dispatch(
                                action,
                                current,
                                target.unwrap_or_else(|| Target::of(current)),
                                &mut controller,
                                DispatchContext {
                                    workspaces: workspaces_enabled,
                                    final_retain_ms,
                                },
                            );
                            // A contextual command can have waited while its pane/tab
                            // disappeared. Validate its new local mode against live state
                            // before replaying any buffered text into it.
                            controller.reconcile(current);
                            if (matches!(outcome, Dispatch::Send(_) | Dispatch::Detach)
                                || controller.owns_input())
                                && let Some((_, _, replay)) = &resumed
                            {
                                for byte in replay.iter().rev() {
                                    pending.push_front(*byte);
                                }
                            }
                            match outcome {
                                Dispatch::Send(request) => {
                                    outbox.push(effects::Effect::Control(request), current)?;
                                }
                                Dispatch::Detach => {
                                    outbox.effects.push(effects::Effect::Detach, None)?;
                                    detaching = true;
                                    pending.clear();
                                }
                                Dispatch::LoadWorkspaces => {
                                    if let Some(path) = options.manager_socket.clone() {
                                        if lookups.len() < 8 {
                                            let epoch = controller.interaction_epoch();
                                            lookups.spawn_blocking(move || {
                                                (epoch, crate::daemon::workspace_entries(&path))
                                            });
                                        } else {
                                            controller.workspaces_loaded(
                                                Err(anyhow::anyhow!(
                                                    "Too many pending workspace lookups"
                                                )),
                                                &current.workspace,
                                            );
                                        }
                                    }
                                }
                                Dispatch::Local => {}
                            }
                        }
                        InputEvent::PopupMouse(event) => match popup.mouse(event) {
                            popup::Outcome::Ignore => {}
                            popup::Outcome::Dismiss => {
                                filter.cancel();
                                hint_scroll = 0;
                            }
                            popup::Outcome::Scroll(rows) => {
                                resolved.push_back(InputEvent::Scroll(ScrollBy::Rows(rows)));
                            }
                            popup::Outcome::Command(action) => {
                                filter.cancel();
                                resolved.push_back(InputEvent::Command(action));
                            }
                        },
                        InputEvent::Mouse(event) if popup.consume_tail(event) => {}
                        InputEvent::Mouse(event) => match controller.mouse(event, current) {
                            MouseDisposition::Local | MouseDisposition::Ignore => {}
                            MouseDisposition::Request(request) => {
                                outbox.push(effects::Effect::Control(request), current)?;
                            }
                            MouseDisposition::Forward => {
                                outbox.push(
                                    effects::Effect::Mouse {
                                        event,
                                        generation: current.generation,
                                    },
                                    current,
                                )?;
                            }
                        },
                        InputEvent::Unknown => {
                            hint_scroll = if filter.revealed() { hint_scroll } else { 0 };
                        }
                        InputEvent::Cancel => {
                            hint_scroll = 0;
                            controller.clear_error();
                        }
                        InputEvent::Scroll(step) => {
                            let column = HintPanel::commands(
                                filter.bindings(),
                                workspaces_enabled,
                                0,
                                current,
                            );
                            // The column lives above the one-row bar.
                            let rows = screen.size()?.0.saturating_sub(1);
                            hint_scroll = scrolled(hint_scroll, step, &column, rows);
                        }
                    }
                }
            }
            if let Some(event) = controller.take_forwarded_mouse() {
                outbox.push(
                    effects::Effect::Mouse {
                        event,
                        generation: current.generation,
                    },
                    current,
                )?;
            }
            if let Some(request) = controller.take_manager_request() {
                outbox.push(
                    effects::Effect::Manager {
                        request,
                        epoch: controller.interaction_epoch(),
                    },
                    current,
                )?;
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
        }
        outbox.flush_input(current)?;
        while outstanding.is_none() && outbox.manager_mutations == 0 {
            let Some(effect) = outbox.effects.pop(current) else {
                break;
            };
            match effect {
                Err(message) => controller.report_error(message),
                Ok(effects::Effect::Input(mut bytes)) => flush(&mut writer, &mut bytes).await?,
                Ok(effects::Effect::Control(request)) => {
                    outstanding =
                        Some(send_control(&mut writer, request, &mut next_control).await?);
                }
                Ok(effects::Effect::Manager { request, epoch }) => {
                    if let Some(path) = options.manager_socket.clone() {
                        if manager_commands.len() < 8 {
                            let mutating =
                                !matches!(request, crate::daemon::ManagerRequest::Catalog);
                            if mutating {
                                outbox.manager_mutations += 1;
                            }
                            manager_commands.spawn_blocking(move || {
                                (
                                    epoch,
                                    mutating,
                                    crate::daemon::manager_request(&path, &request),
                                )
                            });
                        } else {
                            controller.manager_failed("Too many pending manager operations");
                        }
                    } else {
                        controller.report_error(
                            "Manager commands are not available through this attachment",
                        );
                    }
                }
                Ok(effects::Effect::Mouse { event, generation }) => {
                    send(&mut writer, &ClientMessage::Mouse { event, generation }).await?;
                }
                Ok(effects::Effect::Detach) => send(&mut writer, &ClientMessage::Detach).await?,
            }
        }
        // Drain/coalesce ready input before issuing history reads. Each pane can have
        // one outstanding read, and the bounded window never owns keyboard input.
        reads.retain(|request, pane| controller.history_pending(request, pane));
        while reads.has_capacity() {
            let Some((request, pane, offset)) = controller.take_read() else {
                break;
            };
            send(
                &mut writer,
                &ClientMessage::View {
                    request,
                    pane,
                    offset,
                },
            )
            .await?;
            reads.insert(request, pane, tokio::time::Instant::now());
        }
        // Paint once per loop turn, after every ready event has been applied.
        let panel = if filter.popup_visible() {
            Some(HintPanel::commands(
                filter.bindings(),
                workspaces_enabled,
                hint_scroll,
                current,
            ))
        } else {
            controller
                .drag_panel(current)
                .or_else(|| controller.panel())
        };
        let local = controller.local_views();
        let notice = controller.notice(std::time::Instant::now());
        let tab_regions = screen.render(current, Some(&local), panel.as_ref(), notice.as_ref())?;
        popup.painted(
            &tab_regions,
            filter.popup_visible().then_some(panel.as_ref()).flatten(),
        );
        if controller.set_regions(tab_regions) {
            // Remove the cancelled preview immediately, even if the panes produce no output.
            let notice = controller.notice(std::time::Instant::now());
            let local = controller.local_views();
            let panel = controller.panel();
            let tab_regions =
                screen.render(current, Some(&local), panel.as_ref(), notice.as_ref())?;
            controller.set_regions(tab_regions);
        }
    };
    reader_task.abort();
    let _ = reader_task.await;
    outcome
}

async fn send(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &ClientMessage,
) -> anyhow::Result<()> {
    write_frame(writer, message, MAX_CLIENT_FRAME).await?;
    Ok(())
}

async fn flush(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    bytes: &mut Vec<u8>,
) -> anyhow::Result<()> {
    for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
        send(
            writer,
            &ClientMessage::Input {
                bytes: chunk.to_vec(),
            },
        )
        .await?;
    }
    bytes.clear();
    Ok(())
}

/// The command column's scroll offset after `step`, clamped to what `rows` can show.
fn scrolled(current: usize, step: ScrollBy, column: &HintPanel, rows: u16) -> usize {
    let delta = match step {
        ScrollBy::Rows(rows) => i64::from(rows),
        ScrollBy::Screens(screens) => {
            i64::from(screens) * i64::try_from(column.screenful(rows)).unwrap_or(1)
        }
    };
    let limit = i64::try_from(column.max_scroll(rows)).unwrap_or(0);
    usize::try_from((i64::try_from(current).unwrap_or(0).min(limit) + delta).clamp(0, limit))
        .unwrap_or(0)
}

/// Resolves at `deadline`, or never.
async fn at(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

enum Dispatch {
    Send(Request),
    Detach,
    LoadWorkspaces,
    Local,
}

/// What an attachment can do beyond the current frame: whether workspace commands are
/// available (a manager socket is known) and the retention CLI-created panes get.
#[derive(Clone, Copy, Debug)]
struct DispatchContext {
    workspaces: bool,
    final_retain_ms: u64,
}

/// Turns a bound action into a request, a mode entry or a local effect, honouring availability.
fn dispatch(
    action: Action,
    frame: &Frame,
    target: Target,
    controller: &mut Controller,
    context: DispatchContext,
) -> Dispatch {
    let DispatchContext {
        workspaces,
        final_retain_ms,
    } = context;
    if let Some(reason) = action.unavailable(frame, target, workspaces) {
        controller.report_error(reason);
        return Dispatch::Local;
    }
    match action {
        Action::Detach => Dispatch::Detach,
        Action::SplitSide | Action::SplitStack => Dispatch::Send(Request::Split {
            stream: None,
            instance: None,
            id: 0,
            axis: if action == Action::SplitSide {
                crate::layout::Axis::Horizontal
            } else {
                crate::layout::Axis::Vertical
            },
            target: target.focused,
            cwd: None,
            argv: Vec::new(),
            env: Vec::new(),
            rows: None,
            columns: None,
            final_retain_ms,
            fixed_workspace: false,
            right_click: Default::default(),
            ratio: 5000,
            focus: true,
        }),
        Action::FocusLeft
        | Action::FocusRight
        | Action::FocusUp
        | Action::FocusDown
        | Action::FocusNext
        | Action::FocusPrevious
        | Action::FocusLast => Dispatch::Send(Request::Focus {
            instance: None,
            id: 0,
            target: match action {
                Action::FocusLast => FocusTarget::Last,
                Action::FocusNext => FocusTarget::Next,
                Action::FocusPrevious => FocusTarget::Previous,
                Action::FocusLeft => FocusTarget::Left,
                Action::FocusRight => FocusTarget::Right,
                Action::FocusUp => FocusTarget::Up,
                _ => FocusTarget::Down,
            },
        }),
        Action::Zoom => match target.tab {
            Some(tab) => Dispatch::Send(Request::Layout {
                id: 0,
                instance: None,
                tab,
                generation: Some(target.generation),
                action: crate::proto::control::LayoutAction::Zoom {
                    pane: if target.zoomed(frame).is_some() {
                        None
                    } else {
                        target.focused
                    },
                },
            }),
            None => Dispatch::Local,
        },
        Action::MoveToNewTab => match target.focused.zip(target.tab) {
            Some((pane, tab)) => Dispatch::Send(Request::Layout {
                id: 0,
                instance: None,
                tab,
                generation: Some(target.generation),
                action: crate::proto::control::LayoutAction::Transfer {
                    focus: false,
                    pane,
                    destination: crate::proto::control::PaneDestination::NewTab { label: None },
                    side: crate::layout::Direction::Right,
                },
            }),
            None => Dispatch::Local,
        },
        Action::NewTab | Action::NextTab | Action::PreviousTab => Dispatch::Send(Request::Tab {
            instance: None,
            id: 0,
            action: match action {
                Action::NewTab => TabAction::New { name: None },
                Action::NextTab => TabAction::Next,
                _ => TabAction::Previous,
            },
        }),
        Action::CycleRightClick => {
            match target
                .focused
                .and_then(|pane| target.focused_pane(frame).map(|view| (pane, view)))
            {
                Some((pane, view)) => Dispatch::Send(Request::PaneInput {
                    id: 0,
                    instance: Some(frame.server_instance.clone()),
                    pane,
                    right_click: view.right_click.next(),
                }),
                None => Dispatch::Local,
            }
        }
        Action::ChooseWorkspace | Action::ReorderWorkspace => {
            controller.enter_at(action, frame, target);
            Dispatch::LoadWorkspaces
        }
        Action::PaneMenu
        | Action::TabMenu
        | Action::WorkspaceMenu
        | Action::RenameWorkspace
        | Action::CloseWorkspace
        | Action::CopyMode
        | Action::ChooseTab
        | Action::MoveToTab
        | Action::ReorderTab
        | Action::RenamePane
        | Action::RenameTab
        | Action::CloseTab
        | Action::ClosePane
        | Action::ResizeMode
        | Action::SwapMode
        | Action::SwapPane
        | Action::MoveMode
        | Action::NewWorkspace
        | Action::MoveToNewWorkspace
        | Action::MoveToWorkspace => {
            if !controller.enter_at(action, frame, target) {
                controller.report_error("That command is not available right now");
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
