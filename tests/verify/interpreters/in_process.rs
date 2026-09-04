use super::super::schema::{
    ExpectedControlEvent, ExpectedControlReply, ExpectedResize, ExpectedSignal,
    ExpectedSubscription, ExpectedTerminalFrame, Scenario, Signal, Step, TransportFault,
};
use super::super::transcript::{Entry, Event, hex};
use super::Interpreter;
use fux::host::{Action, Command, InputRouter};

pub struct InProcessInterpreter;

struct WorkspaceEndpoint {
    id: String,
    address: std::net::SocketAddrV4,
}

impl fux::daemon::EndpointHandle for WorkspaceEndpoint {
    fn endpoint_id(&self) -> &str {
        &self.id
    }

    fn direct_addr(&self) -> std::net::SocketAddrV4 {
        self.address
    }

    fn close(&mut self) {}

    fn reap_terminal_sessions(&mut self, _: u64, _: u64) {}
}

#[derive(Default)]
struct WorkspaceFactory(u16);

impl fux::daemon::EndpointFactory for WorkspaceFactory {
    fn create(
        &mut self,
        name: &str,
        _: &std::path::Path,
        _: &std::collections::BTreeSet<String>,
    ) -> Result<Box<dyn fux::daemon::EndpointHandle>, fux::daemon::ManagerError> {
        self.0 = self.0.saturating_add(1);
        Ok(Box::new(WorkspaceEndpoint {
            id: format!("verification-{name}-{}", self.0),
            address: std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, self.0),
        }))
    }
}

struct PrivateDaemonRoot(std::path::PathBuf);

impl PrivateDaemonRoot {
    fn cleanup(self) -> Result<(), String> {
        std::fs::remove_dir_all(&self.0).map_err(|error| error.to_string())?;
        if self.0.exists() {
            return Err(format!("private daemon root leaked: {}", self.0.display()));
        }
        Ok(())
    }
}

impl Drop for PrivateDaemonRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Interpreter for InProcessInterpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String> {
        scenario.validate()?;
        let mut router = InputRouter::new(0x02, 40);
        let mut now = 0_u64;
        let mut transcript = Vec::new();
        let mut forwarded = Vec::new();
        let mut expected_input_index = 0_usize;
        let mut commands = Vec::new();
        let mut subscriptions: Vec<ExpectedSubscription> = Vec::new();
        let mut control_events = Vec::new();
        let mut control_replies = Vec::new();
        let mut pty_resizes = Vec::new();
        let mut terminal_frames = Vec::new();
        let mut exit_status = None;
        let mut signals = Vec::new();
        let mut terminal_size = (
            scenario.initial_size.rows.saturating_sub(3),
            scenario.initial_size.columns.saturating_sub(2),
        );
        let mut child_output = std::collections::BTreeMap::<u32, Vec<u8>>::new();
        let mut attached_clients = std::collections::BTreeSet::new();
        let mut known_clients = std::collections::BTreeSet::new();
        let mut client_workspaces = std::collections::BTreeMap::new();
        let mut copy_mode = fux::client::CopyMode::default();
        let mut focused_pane = 1_u32;
        let mut next_pane = 2_u32;
        let mut terminal_status = std::collections::BTreeMap::new();
        let mut daemon_running = false;
        let mut daemon = None;
        let mut daemon_root = None;
        let mut workspace_factory = WorkspaceFactory::default();
        let mut copy_state = fux::state::WorkspaceState::default();
        copy_state
            .insert_pane(fux::state::PaneId(1), fux::state::PaneView::default())
            .map_err(|error| format!("copy fixture pane failed: {error:?}"))?;
        for step in &scenario.steps {
            match step {
                Step::StartDaemon => {
                    if std::mem::replace(&mut daemon_running, true) {
                        return Err("production daemon started twice".into());
                    }
                    static NEXT_DAEMON: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(1);
                    let root = PrivateDaemonRoot(std::env::temp_dir().join(format!(
                        "fux-verify-in-process-{}-{}",
                        std::process::id(),
                        NEXT_DAEMON.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    )));
                    let paths = fux::daemon::DaemonPaths::from_env(
                        Some(root.0.join("run").into_os_string()),
                        Some(root.0.join("state").into_os_string()),
                        Some(root.0.clone().into_os_string()),
                    )
                    .map_err(|error| error.to_string())?;
                    daemon = Some(
                        fux::daemon::Daemon::new(
                            paths,
                            std::process::id(),
                            Default::default(),
                            "local".into(),
                            0,
                        )
                        .map_err(|error| error.to_string())?,
                    );
                    daemon
                        .as_mut()
                        .ok_or("production daemon was not created")?
                        .create_or_find("binary", &mut workspace_factory)
                        .map_err(|error| error.to_string())?;
                    daemon_root = Some(root);
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: "daemon".into(),
                            state: "running".into(),
                        },
                    );
                }
                Step::Attach { client } => {
                    if !daemon_running {
                        return Err("attach requires running daemon".into());
                    }
                    if !matches!(
                        daemon
                            .as_ref()
                            .ok_or("production daemon is not running")?
                            .resolve(Some("binary")),
                        Ok(fux::daemon::Resolution::Attach(_))
                    ) {
                        return Err("attach requires the binary workspace".into());
                    }
                    if !attached_clients.insert(client.clone()) {
                        return Err(format!("client {client:?} attached twice"));
                    }
                    known_clients.insert(client.clone());
                    client_workspaces.insert(client.clone(), "binary".to_owned());
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "attached".into(),
                        },
                    );
                }
                Step::Detach { client } => {
                    if !attached_clients.remove(client) {
                        return Err(format!("detach references unattached client {client:?}"));
                    }
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "detached".into(),
                        },
                    );
                }
                Step::Reconnect { client } => {
                    let workspace_exists = client_workspaces.get(client).is_some_and(|workspace| {
                        daemon.as_ref().is_some_and(|daemon| {
                            matches!(
                                daemon.resolve(Some(workspace)),
                                Ok(fux::daemon::Resolution::Attach(_))
                            )
                        })
                    });
                    if !known_clients.contains(client)
                        || !workspace_exists
                        || !attached_clients.insert(client.clone())
                    {
                        return Err(format!("reconnect requires detached client {client:?}"));
                    }
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "reconnected".into(),
                        },
                    );
                }
                Step::Disconnect { client } => {
                    if !attached_clients.remove(client) {
                        return Err(format!(
                            "disconnect references unattached client {client:?}"
                        ));
                    }
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "disconnected".into(),
                        },
                    );
                }
                Step::CreateWorkspace { workspace } => {
                    let descriptor = daemon
                        .as_mut()
                        .ok_or("production daemon is not running")?
                        .create_or_find(workspace, &mut workspace_factory)
                        .map_err(|error| error.to_string())?;
                    if descriptor.name != *workspace {
                        return Err("production daemon created the wrong workspace".into());
                    }
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "created".into(),
                        },
                    );
                }
                Step::SelectWorkspace { workspace } => {
                    if !matches!(
                        daemon
                            .as_ref()
                            .ok_or("production daemon is not running")?
                            .resolve(Some(workspace))
                            .map_err(|error| error.to_string())?,
                        fux::daemon::Resolution::Attach(ref descriptor)
                            if descriptor.name == *workspace
                    ) {
                        return Err(format!(
                            "production workspace {workspace:?} was not selected"
                        ));
                    }
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "selected".into(),
                        },
                    );
                }
                Step::DeleteWorkspace { workspace } => {
                    if client_workspaces.iter().any(|(client, current)| {
                        attached_clients.contains(client) && current == workspace
                    }) {
                        return Err(format!("production workspace {workspace:?} is attached"));
                    }
                    daemon
                        .as_mut()
                        .ok_or("production daemon is not running")?
                        .kill(workspace)
                        .map_err(|error| error.to_string())?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "deleted".into(),
                        },
                    );
                }
                Step::SwitchWorkspace { client, workspace } => {
                    if !attached_clients.contains(client)
                        || !matches!(
                            daemon
                                .as_ref()
                                .ok_or("production daemon is not running")?
                                .resolve(Some(workspace))
                                .map_err(|error| error.to_string())?,
                            fux::daemon::Resolution::Attach(_)
                        )
                    {
                        return Err(format!(
                            "production client {client:?} cannot switch to {workspace:?}"
                        ));
                    }
                    client_workspaces.insert(client.clone(), workspace.clone());
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: format!("workspace:{workspace}"),
                        },
                    );
                }
                Step::ChildOutput { pane, bytes } => {
                    if *pane != 1 {
                        return Err(format!("production interpreter has no pane {pane}"));
                    }
                    let accumulated = child_output.entry(*pane).or_default();
                    accumulated.extend_from_slice(bytes);
                    let mut parser = vt100::Parser::new(terminal_size.0, terminal_size.1, 0);
                    parser.process(accumulated);
                    let text = parser.screen().contents().trim_end().to_owned();
                    let pane_view = fux::state::PaneView::from_vt100(
                        parser.screen(),
                        String::new(),
                        Default::default(),
                        0,
                    )
                    .map_err(|_| "production terminal frame was invalid".to_owned())?;
                    let frame = ExpectedTerminalFrame {
                        rows: terminal_size.0,
                        columns: terminal_size.1,
                        cells: vec![text],
                        cursor: (!pane_view.cursor.hidden)
                            .then_some((pane_view.cursor.row, pane_view.cursor.column)),
                        synchronized: None,
                        modes: super::super::transcript::TerminalModes {
                            alternate_screen: pane_view.modes.alternate_screen,
                            application_keypad: pane_view.modes.application_keypad,
                            application_cursor: pane_view.modes.application_cursor,
                            bracketed_paste: pane_view.modes.bracketed_paste,
                            mouse_mode: format!("{:?}", pane_view.modes.mouse_mode)
                                .to_ascii_lowercase(),
                            mouse_encoding: format!("{:?}", pane_view.modes.mouse_encoding)
                                .to_ascii_lowercase(),
                        },
                        status: terminal_status.clone(),
                        selection: copy_state
                            .pane(fux::state::PaneId(*pane))
                            .filter(|view| view.copy.active)
                            .map(|view| super::super::transcript::TerminalSelection {
                                cursor: (view.copy.cursor_row, view.copy.cursor_column),
                                anchor: view.copy.anchor,
                            }),
                        prediction_target: (focused_pane == *pane
                            && pane_view.viewport_offset == 0
                            && !copy_mode.active())
                        .then_some(*pane),
                    };
                    terminal_frames.push(frame.clone());
                    push(
                        &mut transcript,
                        Event::TerminalFrame {
                            rows: frame.rows,
                            columns: frame.columns,
                            cells: frame.cells,
                            cursor: frame.cursor,
                            synchronized: frame.synchronized,
                            modes: frame.modes,
                            status: frame.status,
                            selection: frame.selection,
                            prediction_target: frame.prediction_target,
                        },
                    );
                }
                Step::ExpectInput { pane, bytes } => {
                    if *pane != 1 {
                        return Err(format!("production interpreter has no pane {pane}"));
                    }
                    if forwarded.get(expected_input_index) != Some(bytes) {
                        return Err(format!(
                            "production PTY input mismatch at {expected_input_index}: expected={bytes:?}, observed={:?}",
                            forwarded.get(expected_input_index)
                        ));
                    }
                    expected_input_index += 1;
                }
                Step::TerminalReply { pane, query, bytes } => {
                    if *pane != 1 {
                        return Err(format!("production interpreter has no pane {pane}"));
                    }
                    let mut terminal =
                        koh::terminal::ServerTerminal::new(terminal_size.0, terminal_size.1, 0);
                    terminal.process(query);
                    let observed = terminal.take_host_replies();
                    if observed != *bytes {
                        return Err(format!(
                            "production terminal reply mismatch: expected={bytes:?}, observed={observed:?}"
                        ));
                    }
                    push(
                        &mut transcript,
                        Event::PtyWrite {
                            pane: format!("pane-{pane}"),
                            bytes_hex: hex(&observed),
                        },
                    );
                }
                Step::CopyInput { client, bytes } => {
                    if !attached_clients.contains(client) {
                        return Err(format!(
                            "copy_input references unattached client {client:?}"
                        ));
                    }
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    if !copy_mode.key(bytes, &mut copy_state, fux::state::PaneId(1))
                        || copy_mode.active()
                    {
                        return Err("production copy mode did not consume exit key".into());
                    }
                }
                Step::ChildExit { pane, status } => {
                    if *pane != 1 || exit_status.is_some() {
                        return Err(format!("production cannot exit pane {pane} twice"));
                    }
                    let shell = fux::host::platform_tool_from(
                        "sh",
                        None,
                        None,
                        cfg!(target_os = "android"),
                    );
                    let command = vec![shell, "-c".into(), format!("exit {status}")];
                    let (mut pty, receiver) =
                        koh::pty::Pty::spawn(2, 2, &command, "xterm-256color")
                            .map_err(|error| error.to_string())?;
                    drop(receiver);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    let observed = loop {
                        if let Some(observed) = pty.try_wait().map_err(|error| error.to_string())? {
                            break i32::try_from(observed.exit_code())
                                .map_err(|error| error.to_string())?;
                        }
                        if std::time::Instant::now() >= deadline {
                            pty.shutdown();
                            return Err("production child exit deadline expired".into());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    };
                    pty.shutdown();
                    exit_status = Some(observed);
                    push(
                        &mut transcript,
                        Event::ChildExit {
                            process: format!("pane-{pane}"),
                            status: observed,
                        },
                    );
                }
                Step::Signal { pane, signal } => {
                    if *pane != 1 || exit_status.is_some() {
                        return Err(format!("production cannot signal pane {pane}"));
                    }
                    let observed_status = observe_signal(*signal)?;
                    let observed = ExpectedSignal {
                        process: format!("pane-{pane}"),
                        signal: *signal,
                    };
                    signals.push(observed);
                    exit_status = Some(observed_status);
                    push(
                        &mut transcript,
                        Event::Signal {
                            process: format!("pane-{pane}"),
                            name: signal_name(*signal).into(),
                        },
                    );
                    push(
                        &mut transcript,
                        Event::ChildExit {
                            process: format!("pane-{pane}"),
                            status: observed_status,
                        },
                    );
                }
                Step::KillPane { pane } => {
                    if *pane != 1 || exit_status.is_some() {
                        return Err(format!("production cannot kill pane {pane}"));
                    }
                    let observed_status = observe_signal(Signal::Hup)?;
                    exit_status = Some(observed_status);
                    push(
                        &mut transcript,
                        Event::ChildExit {
                            process: format!("pane-{pane}"),
                            status: observed_status,
                        },
                    );
                }
                Step::Shutdown => {
                    if !std::mem::replace(&mut daemon_running, false) {
                        return Err("production shutdown requires running daemon".into());
                    }
                    attached_clients.clear();
                    client_workspaces.clear();
                    drop(daemon.take());
                    daemon_root
                        .take()
                        .ok_or_else(|| "production daemon root was not owned".to_owned())?
                        .cleanup()?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: "daemon".into(),
                            state: "stopped".into(),
                        },
                    );
                }
                Step::Input { .. } | Step::Prefix { .. } => {
                    let (client, bytes) = match step {
                        Step::Prefix { client, key } => (client, vec![0x02, *key]),
                        Step::Input { client, bytes } => (client, bytes.clone()),
                        _ => return Err("input step dispatch mismatch".into()),
                    };
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(&bytes),
                        },
                    );
                    for action in router.feed(&bytes, now) {
                        let before = commands.len();
                        append_action(&mut transcript, action, &mut forwarded, &mut commands)?;
                        if commands.get(before).is_some_and(|name| name == "copy_mode") {
                            let pane = copy_state
                                .pane(fux::state::PaneId(1))
                                .cloned()
                                .ok_or_else(|| "copy fixture pane disappeared".to_owned())?;
                            copy_mode.enter(&pane);
                            copy_mode.sync(&mut copy_state, fux::state::PaneId(1));
                            let _ = copy_mode.key(&[], &mut copy_state, fux::state::PaneId(1));
                        }
                        if commands.get(before).is_some_and(|name| {
                            matches!(name.as_str(), "split_horizontal" | "split_vertical")
                        }) {
                            focused_pane = next_pane;
                            next_pane = next_pane.saturating_add(1);
                        }
                        if commands.len() > before
                            && matches!(
                                commands.last().map(String::as_str),
                                Some("split_horizontal" | "split_vertical")
                            )
                            && subscriptions.last().is_some_and(|subscription| {
                                subscription
                                    .events
                                    .iter()
                                    .any(|event| event == "pane.opened")
                            })
                        {
                            let request_id = subscriptions
                                .last()
                                .map(|subscription| subscription.request_id)
                                .ok_or_else(|| "subscription disappeared".to_owned())?;
                            let event = ExpectedControlEvent {
                                name: "pane.opened".into(),
                                request_id,
                                subscription_id: request_id,
                            };
                            control_events.push(event.clone());
                            push(
                                &mut transcript,
                                Event::ControlWire {
                                    name: event.name,
                                    request_id,
                                    subscription_id: request_id,
                                },
                            );
                        }
                    }
                }
                Step::Paste { client, bytes } => {
                    let mut framed = b"\x1b[200~".to_vec();
                    framed.extend_from_slice(bytes);
                    framed.extend_from_slice(b"\x1b[201~");
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(&framed),
                        },
                    );
                    for action in router.feed(&framed, now) {
                        append_action(&mut transcript, action, &mut forwarded, &mut commands)?;
                    }
                }
                Step::MouseInput { client, bytes } => {
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    for action in router.feed(bytes, now) {
                        match action {
                            Action::Mouse(mouse) => push(
                                &mut transcript,
                                Event::Mouse {
                                    code: mouse.code,
                                    column: mouse.column,
                                    row: mouse.row,
                                    release: mouse.release,
                                },
                            ),
                            other => {
                                return Err(format!("unexpected mouse action: {other:?}"));
                            }
                        }
                    }
                }
                Step::Subscribe { request_id, events } => {
                    let frame = serde_json::to_vec(&serde_json::json!({
                        "command": "subscribe",
                        "id": request_id,
                        "events": events,
                    }))
                    .map_err(|error| error.to_string())?;
                    let request = fux::control::decode_request_frame(&frame)
                        .map_err(|error| error.to_string())?;
                    let fux::control::Request::Subscribe { id, events } = request else {
                        return Err("production decoder changed subscribe variant".into());
                    };
                    let names = events
                        .into_iter()
                        .map(|event_kind| {
                            let value = serde_json::to_value(event_kind)
                                .map_err(|error| error.to_string())?;
                            value
                                .as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| "event kind did not serialize as text".to_owned())
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    subscriptions.push(ExpectedSubscription {
                        request_id: id,
                        events: names.clone(),
                    });
                    push(
                        &mut transcript,
                        Event::Subscription {
                            request_id: id,
                            events: names,
                        },
                    );
                }
                Step::EnableMouseTracking { pane } => {
                    if *pane != 1 {
                        return Err(format!("production interpreter has no pane {pane}"));
                    }
                    child_output
                        .entry(*pane)
                        .or_default()
                        .extend_from_slice(b"\x1b[?1003h\x1b[?1006h");
                }
                Step::Control { request } => {
                    let frame = serde_json::to_vec(request).map_err(|error| error.to_string())?;
                    let decoded = fux::control::decode_request_frame(&frame)
                        .map_err(|error| error.to_string())?;
                    let (id, name, result) = match decoded {
                        fux::control::Request::List { id } => (
                            id,
                            "list",
                            fux::control::CommandResult::Listing {
                                workspaces: Vec::new(),
                            },
                        ),
                        fux::control::Request::SetStatus { id, segment, text } => {
                            if text.is_empty() {
                                terminal_status.remove(&segment);
                            } else {
                                terminal_status.insert(segment, text);
                            }
                            (id, "set-status", fux::control::CommandResult::Unit)
                        }
                        _ => {
                            return Err(
                                "production scenario does not support this control request".into(),
                            );
                        }
                    };
                    push(
                        &mut transcript,
                        Event::ControlRequest {
                            name: name.into(),
                            request_id: id,
                        },
                    );
                    let wire_reply =
                        serde_json::to_value(fux::control::Reply::Completed { id, result })
                            .map_err(|error| error.to_string())?;
                    let reply = ExpectedControlReply {
                        request_id: id,
                        status: wire_reply
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| "production reply omitted status".to_owned())?
                            .into(),
                        result_kind: wire_reply
                            .pointer("/result/kind")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| "production reply omitted result kind".to_owned())?
                            .into(),
                    };
                    control_replies.push(reply.clone());
                    push(
                        &mut transcript,
                        Event::ControlReply {
                            request_id: id,
                            status: reply.status,
                            result_kind: reply.result_kind,
                        },
                    );
                }
                Step::Resize { client, size } => {
                    if !attached_clients.contains(client) {
                        return Err(format!("resize references unattached client {client:?}"));
                    }
                    let layout = fux::state::LayoutTree::new(fux::state::PaneId(1));
                    let geometry = layout
                        .geometry(fux::state::Rect {
                            x: 0,
                            y: 0,
                            width: size.columns,
                            height: size.rows.saturating_sub(1),
                        })
                        .map_err(|error| format!("production layout failed: {error:?}"))?;
                    let rect = geometry
                        .iter()
                        .find_map(|(pane, rect)| (*pane == fux::state::PaneId(1)).then_some(rect))
                        .ok_or_else(|| "production layout omitted pane".to_owned())?;
                    let resize = ExpectedResize {
                        pane: 1,
                        rows: rect.height.saturating_sub(2),
                        columns: rect.width.saturating_sub(2),
                    };
                    pty_resizes.push(resize);
                    terminal_size = (resize.rows, resize.columns);
                    push(
                        &mut transcript,
                        Event::Resize {
                            client: client.clone(),
                            pane: "pane-1".into(),
                            rows: resize.rows,
                            columns: resize.columns,
                        },
                    );
                }
                Step::AdvanceClock { milliseconds } => {
                    now = now.saturating_add(*milliseconds);
                    push(
                        &mut transcript,
                        Event::Clock {
                            milliseconds: *milliseconds,
                        },
                    );
                    for action in router.flush_timeout(now) {
                        append_action(&mut transcript, action, &mut forwarded, &mut commands)?;
                    }
                }
                Step::Transport { fault } => {
                    exercise_transport_fault(*fault)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: "transport".into(),
                            state: transport_fault_name(*fault).into(),
                        },
                    );
                }
                Step::Expect { expected } => {
                    if forwarded != expected.forwarded
                        || commands != expected.commands
                        || subscriptions != expected.subscriptions
                        || control_events != expected.control_events
                        || control_replies != expected.control_replies
                        || pty_resizes != expected.pty_resizes
                        || terminal_frames != expected.terminal_frames
                        || signals != expected.signals
                        || exit_status != expected.exit_status
                    {
                        return Err(format!(
                            "production expectation mismatch: forwarded={forwarded:?}, commands={commands:?}, subscriptions={subscriptions:?}"
                        ));
                    }
                    push(
                        &mut transcript,
                        Event::Cleanup {
                            owned_resources: expected.owned_resources,
                        },
                    );
                }
            }
        }
        Ok(transcript)
    }
}

fn observe_signal(signal: Signal) -> Result<i32, String> {
    use nix::sys::signal::{Signal as NixSignal, kill};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt as _;

    let executable =
        fux::host::platform_tool_from("sleep", None, None, cfg!(target_os = "android"));
    struct OwnedProcess(Option<std::process::Child>);
    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let mut child = OwnedProcess(Some(
        std::process::Command::new(executable)
            .arg("30")
            .spawn()
            .map_err(|error| error.to_string())?,
    ));
    let native = match signal {
        Signal::Hup => NixSignal::SIGHUP,
        Signal::Int => NixSignal::SIGINT,
        Signal::Term => NixSignal::SIGTERM,
        Signal::Kill => NixSignal::SIGKILL,
    };
    kill(
        Pid::from_raw(
            i32::try_from(child.0.as_ref().ok_or("signal child missing")?.id())
                .map_err(|error| error.to_string())?,
        ),
        native,
    )
    .map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child
            .0
            .as_mut()
            .ok_or("signal child missing")?
            .try_wait()
            .map_err(|error| error.to_string())?
        {
            child.0.take();
            break status;
        }
        if std::time::Instant::now() >= deadline {
            return Err("signal child exit deadline expired".into());
        }
        std::thread::yield_now();
    };
    let observed = status
        .signal()
        .ok_or_else(|| format!("signal fixture exited normally: {status}"))?;
    Ok(128 + observed)
}

fn signal_name(signal: Signal) -> &'static str {
    match signal {
        Signal::Hup => "hup",
        Signal::Int => "int",
        Signal::Term => "term",
        Signal::Kill => "kill",
    }
}

fn transport_fault_name(fault: TransportFault) -> &'static str {
    match fault {
        TransportFault::Lose => "lost",
        TransportFault::Duplicate => "duplicated",
        TransportFault::Reorder => "reordered",
        TransportFault::Reconnect => "reconnected",
    }
}

fn exercise_transport_fault(fault: TransportFault) -> Result<(), String> {
    use koh::ssp::testkit::{GridState, Link, LinkParams, Rng, SimHarness};

    match fault {
        TransportFault::Lose => {
            let mut link = Link::default();
            link.push(
                &mut Rng::new(1),
                0,
                &LinkParams {
                    loss: 1.0,
                    min_delay_ms: 0,
                    max_delay_ms: 0,
                    dup: 0.0,
                },
                b"lost".to_vec(),
            );
            if link.next_due().is_some() || !link.due(u64::MAX).is_empty() {
                return Err("Koh loss seam delivered a dropped datagram".into());
            }
        }
        TransportFault::Duplicate => {
            let mut link = Link::default();
            link.push(
                &mut Rng::new(2),
                0,
                &LinkParams {
                    loss: 0.0,
                    min_delay_ms: 1,
                    max_delay_ms: 1,
                    dup: 1.0,
                },
                b"duplicate".to_vec(),
            );
            if link.due(1) != [b"duplicate".to_vec(), b"duplicate".to_vec()] {
                return Err("Koh duplication seam did not deliver two exact datagrams".into());
            }
        }
        TransportFault::Reorder => {
            let params = LinkParams {
                loss: 0.0,
                min_delay_ms: 0,
                max_delay_ms: 100,
                dup: 0.0,
            };
            let reordered = (1..=1024).any(|seed| {
                let mut link = Link::default();
                let mut rng = Rng::new(seed);
                link.push(&mut rng, 0, &params, b"first".to_vec());
                link.push(&mut rng, 0, &params, b"second".to_vec());
                link.due(100) == [b"second".to_vec(), b"first".to_vec()]
            });
            if !reordered {
                return Err("Koh reorder seam produced no deterministic inversion".into());
            }
            let mut sender = koh::ssp::Transport::<GridState, GridState>::new(0, 128);
            let mut receiver = koh::ssp::Transport::<GridState, GridState>::new(0, 128);
            sender.set_connected(true);
            receiver.set_connected(true);
            let mut rng = Rng::new(99);
            let payload = (0..4096)
                .map(|_| rng.next_u64().to_le_bytes()[0])
                .collect::<Vec<_>>();
            sender.current_mut().cells.insert(7, payload);
            let mut fragments = sender.tick(0);
            if fragments.len() <= 1 {
                return Err("Koh reorder row did not force a fragmented instruction".into());
            }
            fragments.reverse();
            for fragment in fragments {
                receiver.recv(1, &fragment);
            }
            if receiver.remote_state() != sender.current() {
                return Err("Koh reversed fragments did not reconstruct exact state".into());
            }
        }
        TransportFault::Reconnect => {
            let mut harness = SimHarness::<GridState, GridState>::new(
                LinkParams {
                    loss: 0.30,
                    min_delay_ms: 1,
                    max_delay_ms: 100,
                    dup: 0.10,
                },
                44,
                256,
            );
            harness.a.set_connected(false);
            harness.b.set_connected(false);
            harness
                .a_mut()
                .cells
                .insert(1, b"authoritative-after-loss".to_vec());
            let expected = harness.a.current().clone();
            harness.run_steps(32);
            if harness.b.remote_state() == &expected {
                return Err("Koh disconnected transport delivered outage state".into());
            }
            harness.a.set_connected(true);
            harness.b.set_connected(true);
            harness.run_until(50_000, |state| state.b.remote_state() == &expected);
            if harness.b.remote_state() != &expected {
                return Err("Koh reconnect seam did not converge".into());
            }
        }
    }
    Ok(())
}

fn append_action(
    transcript: &mut Vec<Entry>,
    action: Action,
    forwarded: &mut Vec<Vec<u8>>,
    commands: &mut Vec<String>,
) -> Result<(), String> {
    match action {
        Action::Forward(bytes) => {
            forwarded.push(bytes.clone());
            push(
                transcript,
                Event::PtyWrite {
                    pane: "pane-1".into(),
                    bytes_hex: hex(&bytes),
                },
            );
        }
        Action::Command(command) => {
            let name = command_name(&command)?;
            commands.push(name.into());
            push(transcript, Event::Command { name: name.into() });
        }
        other => return Err(format!("unexpected production action: {other:?}")),
    }
    Ok(())
}

fn command_name(command: &Command) -> Result<&'static str, String> {
    Ok(match command {
        Command::SplitHorizontal => "split_horizontal",
        Command::SplitVertical => "split_vertical",
        Command::Focus(fux::state::Direction::Left) => "focus_left",
        Command::Focus(fux::state::Direction::Right) => "focus_right",
        Command::Focus(fux::state::Direction::Up) => "focus_up",
        Command::Focus(fux::state::Direction::Down) => "focus_down",
        Command::Close => "close",
        Command::NewPane => "new_pane",
        Command::NewTab => "new_tab",
        Command::NextTab => "next_tab",
        Command::PreviousTab => "previous_tab",
        Command::Zoom => "zoom",
        Command::CopyMode => "copy_mode",
        Command::Detach => "detach",
        Command::WorkspacePicker => "workspace_picker",
        Command::Help => "help",
        Command::External(_) => return Err("external command is not in the default matrix".into()),
    })
}

fn push(transcript: &mut Vec<Entry>, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: "model".into(),
        event,
    });
}
