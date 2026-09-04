use super::super::schema::{Scenario, Step};
use super::super::transcript::{Entry, Event, hex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservedAction {
    Forward(Vec<u8>),
    Command(String),
}

pub trait BinaryDriver {
    fn input(&mut self, bytes: &[u8]) -> Result<Vec<ObservedAction>, String>;
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
                    for action in self.driver.input(bytes)? {
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
                        }
                    }
                }
                Step::Expect { expected } => {
                    if forwarded != expected.forwarded || commands != expected.commands {
                        return Err(format!(
                            "binary expectation mismatch: forwarded={forwarded:?}, commands={commands:?}"
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
