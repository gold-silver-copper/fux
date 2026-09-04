use super::super::schema::{
    ExpectedControlEvent, ExpectedControlReply, ExpectedResize, ExpectedSubscription,
    ExpectedTerminalFrame, Scenario, Step,
};
use super::super::transcript::{Entry, Event, hex};
use super::Interpreter;
use fux::host::{Action, Command, InputRouter};

pub struct InProcessInterpreter;

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
        let mut terminal_size = (
            scenario.initial_size.rows.saturating_sub(3),
            scenario.initial_size.columns.saturating_sub(2),
        );
        let mut child_output = std::collections::BTreeMap::<u32, Vec<u8>>::new();
        let mut attached_clients = std::collections::BTreeSet::new();
        let mut known_clients = std::collections::BTreeSet::new();
        let mut copy_mode = fux::client::CopyMode::default();
        let mut copy_state = fux::state::WorkspaceState::default();
        copy_state
            .insert_pane(fux::state::PaneId(1), fux::state::PaneView::default())
            .map_err(|error| format!("copy fixture pane failed: {error:?}"))?;
        for step in &scenario.steps {
            match step {
                Step::Attach { client } => {
                    if !attached_clients.insert(client.clone()) {
                        return Err(format!("client {client:?} attached twice"));
                    }
                    known_clients.insert(client.clone());
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
                    if !known_clients.contains(client) || !attached_clients.insert(client.clone()) {
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
                Step::ChildOutput { pane, bytes } => {
                    if *pane != 1 {
                        return Err(format!("production interpreter has no pane {pane}"));
                    }
                    let accumulated = child_output.entry(*pane).or_default();
                    accumulated.extend_from_slice(bytes);
                    let mut parser = vt100::Parser::new(terminal_size.0, terminal_size.1, 0);
                    parser.process(accumulated);
                    let text = parser.screen().contents().trim_end().to_owned();
                    let frame = ExpectedTerminalFrame {
                        rows: terminal_size.0,
                        columns: terminal_size.1,
                        cells: vec![text],
                        cursor: None,
                        synchronized: None,
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
                Step::Control { request } => {
                    let frame = serde_json::to_vec(request).map_err(|error| error.to_string())?;
                    let decoded = fux::control::decode_request_frame(&frame)
                        .map_err(|error| error.to_string())?;
                    let fux::control::Request::List { id } = decoded else {
                        return Err("production scenario currently supports list requests".into());
                    };
                    push(
                        &mut transcript,
                        Event::ControlRequest {
                            name: "list".into(),
                            request_id: id,
                        },
                    );
                    let wire_reply = serde_json::to_value(fux::control::Reply::Completed {
                        id,
                        result: fux::control::CommandResult::Listing {
                            workspaces: Vec::new(),
                        },
                    })
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
                Step::Expect { expected } => {
                    if forwarded != expected.forwarded
                        || commands != expected.commands
                        || subscriptions != expected.subscriptions
                        || control_events != expected.control_events
                        || control_replies != expected.control_replies
                        || pty_resizes != expected.pty_resizes
                        || terminal_frames != expected.terminal_frames
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
                _ => {
                    return Err(
                        "scenario step is not supported by the in-process interpreter yet".into(),
                    );
                }
            }
        }
        Ok(transcript)
    }
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
