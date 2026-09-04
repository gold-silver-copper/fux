use super::super::schema::{
    ExpectedControlEvent, ExpectedControlReply, ExpectedResize, ExpectedSubscription, Scenario,
    Step,
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
        let mut commands = Vec::new();
        let mut subscriptions: Vec<ExpectedSubscription> = Vec::new();
        let mut control_events = Vec::new();
        let mut control_replies = Vec::new();
        let mut pty_resizes = Vec::new();
        for step in &scenario.steps {
            match step {
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
                Step::Resize { client: _, size } => {
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
                    push(
                        &mut transcript,
                        Event::Resize {
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
