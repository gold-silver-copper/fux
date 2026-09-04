use super::super::schema::{
    ExpectedControlEvent, ExpectedControlReply, ExpectedResize, ExpectedSignal,
    ExpectedSubscription, ExpectedTerminalFrame, Scenario, Signal, Size, Step,
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
    fn start_daemon(&mut self) -> Result<(), String>;
    fn attach(&mut self, client: &str) -> Result<(), String>;
    fn detach(&mut self, client: &str) -> Result<(), String>;
    fn reconnect(&mut self, client: &str) -> Result<(), String>;
    fn disconnect(&mut self, client: &str) -> Result<(), String>;
    fn create_workspace(&mut self, workspace: &str) -> Result<(), String>;
    fn select_workspace(&mut self, workspace: &str) -> Result<(), String>;
    fn delete_workspace(&mut self, workspace: &str) -> Result<(), String>;
    fn switch_workspace(&mut self, client: &str, workspace: &str) -> Result<(), String>;
    fn child_output(&mut self, pane: u32, bytes: &[u8]) -> Result<ExpectedTerminalFrame, String>;
    fn terminal_reply(
        &mut self,
        pane: u32,
        query: &[u8],
        expected: &[u8],
    ) -> Result<Vec<u8>, String>;
    fn copy_input(&mut self, client: &str, bytes: &[u8]) -> Result<(), String>;
    fn child_exit(&mut self, pane: u32, status: i32) -> Result<i32, String>;
    fn signal(&mut self, pane: u32, signal: Signal) -> Result<i32, String>;
    fn kill_pane(&mut self, pane: u32) -> Result<i32, String>;
    fn input(&mut self, bytes: &[u8]) -> Result<Vec<ObservedAction>, String>;
    fn mouse_input(&mut self, bytes: &[u8]) -> Result<ObservedAction, String>;
    fn enable_mouse_tracking(&mut self, pane: u32) -> Result<(), String>;
    fn subscribe(
        &mut self,
        request_id: u64,
        events: &[String],
    ) -> Result<ExpectedSubscription, String>;
    fn control(&mut self, request: &serde_json::Value) -> Result<ExpectedControlReply, String>;
    fn resize(&mut self, client: &str, size: Size) -> Result<ExpectedResize, String>;
    fn shutdown(&mut self) -> Result<usize, String>;
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
        let mut expected_input_index = 0_usize;
        let mut commands = Vec::new();
        let mut subscriptions = Vec::new();
        let mut control_events = Vec::new();
        let mut control_replies = Vec::new();
        let mut pty_resizes = Vec::new();
        let mut terminal_frames = Vec::new();
        let mut exit_status = None;
        let mut signals = Vec::new();
        let mut owned_resources = None;
        for step in &scenario.steps {
            match step {
                Step::StartDaemon => {
                    self.driver.start_daemon()?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: "daemon".into(),
                            state: "running".into(),
                        },
                    );
                }
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
                Step::Detach { client } => {
                    self.driver.detach(client)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "detached".into(),
                        },
                    );
                }
                Step::Reconnect { client } => {
                    self.driver.reconnect(client)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "reconnected".into(),
                        },
                    );
                }
                Step::Disconnect { client } => {
                    self.driver.disconnect(client)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: "disconnected".into(),
                        },
                    );
                }
                Step::CreateWorkspace { workspace } => {
                    self.driver.create_workspace(workspace)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "created".into(),
                        },
                    );
                }
                Step::SelectWorkspace { workspace } => {
                    self.driver.select_workspace(workspace)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "selected".into(),
                        },
                    );
                }
                Step::DeleteWorkspace { workspace } => {
                    self.driver.delete_workspace(workspace)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("workspace:{workspace}"),
                            state: "deleted".into(),
                        },
                    );
                }
                Step::SwitchWorkspace { client, workspace } => {
                    self.driver.switch_workspace(client, workspace)?;
                    push(
                        &mut transcript,
                        Event::Lifecycle {
                            resource: format!("client:{client}"),
                            state: format!("workspace:{workspace}"),
                        },
                    );
                }
                Step::ChildOutput { pane, bytes } => {
                    let frame = self.driver.child_output(*pane, bytes)?;
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
                Step::EnableMouseTracking { pane } => {
                    self.driver.enable_mouse_tracking(*pane)?;
                }
                Step::ExpectInput { pane, bytes } => {
                    if *pane != 1 {
                        return Err(format!("binary interpreter has no pane {pane}"));
                    }
                    if forwarded.get(expected_input_index) != Some(bytes) {
                        return Err(format!(
                            "binary PTY input mismatch at {expected_input_index}: expected={bytes:?}, observed={:?}",
                            forwarded.get(expected_input_index)
                        ));
                    }
                    expected_input_index += 1;
                }
                Step::TerminalReply { pane, query, bytes } => {
                    let observed = self.driver.terminal_reply(*pane, query, bytes)?;
                    if observed != *bytes {
                        return Err(format!(
                            "binary terminal reply mismatch: expected={bytes:?}, observed={observed:?}"
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
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    self.driver.copy_input(client, bytes)?;
                }
                Step::ChildExit { pane, status } => {
                    let observed = self.driver.child_exit(*pane, *status)?;
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
                    let observed_status = self.driver.signal(*pane, *signal)?;
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
                    let observed_status = self.driver.kill_pane(*pane)?;
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
                    let remaining = self.driver.shutdown()?;
                    owned_resources = Some(remaining);
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
                        || terminal_frames != expected.terminal_frames
                        || signals != expected.signals
                        || exit_status != expected.exit_status
                    {
                        return Err(format!(
                            "binary expectation mismatch: forwarded={forwarded:?}, commands={commands:?}, subscriptions={subscriptions:?}, terminal_frames={terminal_frames:?}, expected_terminal_frames={:?}",
                            expected.terminal_frames,
                        ));
                    }
                    let owned_resources = owned_resources
                        .ok_or_else(|| "binary expect requires shutdown".to_owned())?;
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

fn signal_name(signal: Signal) -> &'static str {
    match signal {
        Signal::Hup => "hup",
        Signal::Int => "int",
        Signal::Term => "term",
        Signal::Kill => "kill",
    }
}

fn push(transcript: &mut Vec<Entry>, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: "model".into(),
        event,
    });
}
