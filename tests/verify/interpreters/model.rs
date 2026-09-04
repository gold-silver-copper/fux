use super::super::oracle::input::{Outcome, PrefixOracle};
use super::super::schema::{Scenario, Step};
use super::super::transcript::{Entry, Event, hex};
use super::Interpreter;

pub struct ModelInterpreter;

impl Interpreter for ModelInterpreter {
    fn run(&self, scenario: &Scenario) -> Result<Vec<Entry>, String> {
        scenario.validate()?;
        let mut oracle = PrefixOracle::new(0x02);
        let mut transcript = Vec::new();
        for step in &scenario.steps {
            match step {
                Step::Input { client, bytes } => {
                    push(
                        &mut transcript,
                        "model",
                        Event::Input {
                            client: client.clone(),
                            bytes_hex: hex(bytes),
                        },
                    );
                    for outcome in oracle.feed(bytes) {
                        match outcome {
                            Outcome::Forward(bytes) => push(
                                &mut transcript,
                                "model",
                                Event::PtyWrite {
                                    pane: "pane-1".into(),
                                    bytes_hex: hex(&bytes),
                                },
                            ),
                            Outcome::Command(name) => push(
                                &mut transcript,
                                "model",
                                Event::Command { name: name.into() },
                            ),
                        }
                    }
                }
                Step::AdvanceClock { milliseconds } => push(
                    &mut transcript,
                    "model",
                    Event::Clock {
                        milliseconds: *milliseconds,
                    },
                ),
                Step::Expect { expected } => {
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

fn push(transcript: &mut Vec<Entry>, source: &str, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: source.into(),
        event,
    });
}
