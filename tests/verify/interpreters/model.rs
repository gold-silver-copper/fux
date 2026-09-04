use super::super::oracle::input::{Outcome, PrefixOracle};
use super::super::schema::{ExpectedControlEvent, ExpectedSubscription, Scenario, Step};
use super::super::transcript::{Entry, Event, hex};
use super::Interpreter;

pub struct ModelInterpreter;

impl Interpreter for ModelInterpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String> {
        scenario.validate()?;
        let mut oracle = PrefixOracle::new(0x02);
        let mut transcript = Vec::new();
        let mut forwarded = Vec::new();
        let mut commands = Vec::new();
        let mut subscriptions: Vec<ExpectedSubscription> = Vec::new();
        let mut control_events = Vec::new();
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
                        "model",
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(&bytes),
                        },
                    );
                    for outcome in oracle.feed(&bytes) {
                        match outcome {
                            Outcome::Forward(bytes) => {
                                forwarded.push(bytes.clone());
                                push(
                                    &mut transcript,
                                    "model",
                                    Event::PtyWrite {
                                        pane: "pane-1".into(),
                                        bytes_hex: hex(&bytes),
                                    },
                                );
                            }
                            Outcome::Command(name) => {
                                commands.push(name.to_owned());
                                push(
                                    &mut transcript,
                                    "model",
                                    Event::Command { name: name.into() },
                                );
                                if matches!(name, "split_horizontal" | "split_vertical")
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
                                        "model",
                                        Event::ControlWire {
                                            name: event.name,
                                            request_id,
                                            subscription_id: request_id,
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                Step::Paste { client, bytes } => {
                    let mut framed = b"\x1b[200~".to_vec();
                    framed.extend_from_slice(bytes);
                    framed.extend_from_slice(b"\x1b[201~");
                    push(
                        &mut transcript,
                        "model",
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(&framed),
                        },
                    );
                    forwarded.push(framed.clone());
                    push(
                        &mut transcript,
                        "model",
                        Event::PtyWrite {
                            pane: "pane-1".into(),
                            bytes_hex: hex(&framed),
                        },
                    );
                }
                Step::MouseInput { client, bytes } => {
                    push(
                        &mut transcript,
                        "model",
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    let (code, column, row, release) = parse_sgr_mouse(bytes)?;
                    push(
                        &mut transcript,
                        "model",
                        Event::Mouse {
                            code,
                            column,
                            row,
                            release,
                        },
                    );
                }
                Step::Subscribe { request_id, events } => {
                    subscriptions.push(ExpectedSubscription {
                        request_id: *request_id,
                        events: events.clone(),
                    });
                    push(
                        &mut transcript,
                        "model",
                        Event::Subscription {
                            request_id: *request_id,
                            events: events.clone(),
                        },
                    );
                }
                Step::AdvanceClock { milliseconds } => {
                    push(
                        &mut transcript,
                        "model",
                        Event::Clock {
                            milliseconds: *milliseconds,
                        },
                    );
                    for outcome in oracle.advance_clock(*milliseconds) {
                        if let Outcome::Forward(bytes) = outcome {
                            forwarded.push(bytes.clone());
                            push(
                                &mut transcript,
                                "model",
                                Event::PtyWrite {
                                    pane: "pane-1".into(),
                                    bytes_hex: hex(&bytes),
                                },
                            );
                        }
                    }
                }
                Step::Expect { expected } => {
                    if forwarded != expected.forwarded
                        || commands != expected.commands
                        || subscriptions != expected.subscriptions
                        || control_events != expected.control_events
                    {
                        return Err(format!(
                            "model expectation mismatch: forwarded={forwarded:?}, commands={commands:?}, subscriptions={subscriptions:?}"
                        ));
                    }
                    push(
                        &mut transcript,
                        "model",
                        Event::Cleanup {
                            owned_resources: expected.owned_resources,
                        },
                    );
                }
                _ => {
                    return Err(
                        "scenario step is not supported by the model interpreter yet".into(),
                    );
                }
            }
        }
        Ok(transcript)
    }
}

fn parse_sgr_mouse(bytes: &[u8]) -> Result<(u16, u16, u16, bool), String> {
    let tail = bytes
        .strip_prefix(b"\x1b[<")
        .ok_or_else(|| "mouse input is not SGR encoded".to_owned())?;
    let (terminator, body) = tail
        .split_last()
        .ok_or_else(|| "mouse input has no SGR terminator".to_owned())?;
    let release = match terminator {
        b'M' => false,
        b'm' => true,
        _ => return Err("mouse input has no SGR terminator".into()),
    };
    let body = std::str::from_utf8(body).map_err(|error| error.to_string())?;
    let mut fields = body.split(';');
    let parse = |value: &str| value.parse::<u16>().map_err(|error| error.to_string());
    let mut next = || {
        fields
            .next()
            .ok_or_else(|| "mouse input must contain three SGR fields".to_owned())
            .and_then(parse)
    };
    let (code, column, row) = (next()?, next()?, next()?);
    if fields.next().is_some() || column == 0 || row == 0 {
        return Err("mouse coordinates are one based".into());
    }
    Ok((code, column, row, release))
}

fn push(transcript: &mut Vec<Entry>, source: &str, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: source.into(),
        event,
    });
}
