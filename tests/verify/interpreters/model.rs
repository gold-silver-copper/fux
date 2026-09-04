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
        let mut forwarded = Vec::new();
        let mut commands = Vec::new();
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
                    if forwarded != expected.forwarded || commands != expected.commands {
                        return Err(format!(
                            "model expectation mismatch: forwarded={forwarded:?}, commands={commands:?}"
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

fn push(transcript: &mut Vec<Entry>, source: &str, event: Event) {
    transcript.push(Entry {
        sequence: transcript.len() as u64,
        source: source.into(),
        event,
    });
}
