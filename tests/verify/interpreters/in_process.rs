use super::super::schema::{Scenario, Step};
use super::super::transcript::{Entry, Event, hex};
use super::Interpreter;
use fux::host::{Action, Command, InputRouter};

pub struct InProcessInterpreter;

impl Interpreter for InProcessInterpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String> {
        scenario.validate()?;
        let mut router = InputRouter::new(0x02, 25);
        let mut now = 0_u64;
        let mut transcript = Vec::new();
        for step in &scenario.steps {
            match step {
                Step::Input { client, bytes } => {
                    push(
                        &mut transcript,
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    for action in router.feed(bytes, now) {
                        append_action(&mut transcript, action)?;
                    }
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
                        append_action(&mut transcript, action)?;
                    }
                }
                Step::Expect { expected } => push(
                    &mut transcript,
                    Event::Cleanup {
                        owned_resources: expected.owned_resources,
                    },
                ),
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

fn append_action(transcript: &mut Vec<Entry>, action: Action) -> Result<(), String> {
    match action {
        Action::Forward(bytes) => push(
            transcript,
            Event::PtyWrite {
                pane: "pane-1".into(),
                bytes_hex: hex(&bytes),
            },
        ),
        Action::Command(Command::SplitHorizontal) => push(
            transcript,
            Event::Command {
                name: "split_horizontal".into(),
            },
        ),
        other => return Err(format!("unexpected production action: {other:?}")),
    }
    Ok(())
}

fn push(transcript: &mut Vec<Entry>, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: "model".into(),
        event,
    });
}
