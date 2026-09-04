use super::super::schema::{
    ExpectedControlEvent, ExpectedControlReply, ExpectedResize, ExpectedSubscription, Scenario,
    Size, Step,
};
use super::super::transcript::{Entry, Event, hex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservedAction {
    Forward(Vec<u8>),
    Command(String),
    Mouse {
        code: u16,
        column: u16,
        row: u16,
        release: bool,
    },
    ControlEvent(ExpectedControlEvent),
}

pub trait BinaryDriver {
    fn attach(&mut self, client: &str) -> Result<(), String>;
    fn input(&mut self, bytes: &[u8]) -> Result<Vec<ObservedAction>, String>;
    fn mouse_input(&mut self, bytes: &[u8]) -> Result<ObservedAction, String>;
    fn subscribe(
        &mut self,
        request_id: u64,
        events: &[String],
    ) -> Result<ExpectedSubscription, String>;
    fn control(&mut self, request: &serde_json::Value) -> Result<ExpectedControlReply, String>;
    fn resize(&mut self, client: &str, size: Size) -> Result<ExpectedResize, String>;
    fn cleanup(&mut self) -> Result<usize, String>;
}

pub struct BinaryInterpreter<D> {
    driver: D,
}

impl<D: BinaryDriver> BinaryInterpreter<D> {
    pub fn new(driver: D) -> Self {
        Self { driver }
    }

    pub fn run(mut self, scenario: &Scenario) -> Result<Vec<Entry>, String> {
        scenario.validate()?;
        let mut transcript = Vec::new();
        let mut forwarded = Vec::new();
        let mut commands = Vec::new();
        let mut subscriptions = Vec::new();
        let mut control_events = Vec::new();
        let mut control_replies = Vec::new();
        let mut pty_resizes = Vec::new();
        for step in &scenario.steps {
            match step {
                Step::Attach { client } => {
                    self.driver.attach(client)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "attached".into(),
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
                    for action in self.driver.input(&bytes)? {
                        match action {
                            ObservedAction::Forward(bytes) => {
                                forwarded.push(bytes.clone());
                                push(
                                    &mut transcript,
                                    Event::PtyWrite {
                                        pane: "pane-1".into(),
                                        bytes_hex: hex(&bytes),
                                    },
                                );
                            }
                            ObservedAction::Command(name) => {
                                commands.push(name.clone());
                                push(&mut transcript, Event::Command { name });
                            }
                            ObservedAction::ControlEvent(event) => {
                                control_events.push(event.clone());
                                push(
                                    &mut transcript,
                                    Event::ControlWire {
                                        name: event.name,
                                        request_id: event.request_id,
                                        subscription_id: event.subscription_id,
                                    },
                                );
                            }
                            other => {
                                return Err(format!("unexpected binary input action: {other:?}"));
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
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(&framed),
                        },
                    );
                    for action in self.driver.input(&framed)? {
                        match action {
                            ObservedAction::Forward(bytes) => {
                                forwarded.push(bytes.clone());
                                push(
                                    &mut transcript,
                                    Event::PtyWrite {
                                        pane: "pane-1".into(),
                                        bytes_hex: hex(&bytes),
                                    },
                                );
                            }
                            ObservedAction::Command(name) => {
                                commands.push(name.clone());
                                push(&mut transcript, Event::Command { name });
                            }
                            other => {
                                return Err(format!("unexpected binary paste action: {other:?}"));
                            }
                        }
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
                    match self.driver.mouse_input(bytes)? {
                        ObservedAction::Mouse {
                            code,
                            column,
                            row,
                            release,
                        } => push(
                            &mut transcript,
                            Event::Mouse {
                                code,
                                column,
                                row,
                                release,
                            },
                        ),
                        other => return Err(format!("unexpected binary mouse action: {other:?}")),
                    }
                }
                Step::Subscribe { request_id, events } => {
                    let subscription = self.driver.subscribe(*request_id, events)?;
                    subscriptions.push(subscription.clone());
                    push(
                        &mut transcript,
                        Event::Subscription {
                            request_id: subscription.request_id,
                            events: subscription.events,
                        },
                    );
                }
                Step::Control { request } => {
                    let name = request
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "control request omitted command".to_owned())?;
                    let request_id = request
                        .get("id")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| "control request omitted id".to_owned())?;
                    push(
                        &mut transcript,
                        Event::ControlRequest {
                            name: name.into(),
                            request_id,
                        },
                    );
                    let reply = self.driver.control(request)?;
                    control_replies.push(reply.clone());
                    push(
                        &mut transcript,
                        Event::ControlReply {
                            request_id: reply.request_id,
                            status: reply.status,
                            result_kind: reply.result_kind,
                        },
                    );
                }
                Step::Resize { client, size } => {
                    let resize = self.driver.resize(client, *size)?;
                    pty_resizes.push(resize);
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
                Step::Expect { expected } => {
                    if forwarded != expected.forwarded
                        || commands != expected.commands
                        || subscriptions != expected.subscriptions
                        || control_events != expected.control_events
                        || control_replies != expected.control_replies
                        || pty_resizes != expected.pty_resizes
                    {
                        return Err(format!(
                            "binary expectation mismatch: forwarded={forwarded:?}, commands={commands:?}, subscriptions={subscriptions:?}"
                        ));
                    }
                    let owned_resources = self.driver.cleanup()?;
                    if owned_resources != expected.owned_resources {
                        return Err(format!(
                            "binary cleanup mismatch: owned_resources={owned_resources}"
                        ));
                    }
                    push(&mut transcript, Event::Cleanup { owned_resources });
                }
                _ => return Err("scenario step is not supported by the binary interpreter".into()),
            }
        }
        Ok(transcript)
    }
}

fn push(transcript: &mut Vec<Entry>, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: "model".into(),
        event,
    });
}
