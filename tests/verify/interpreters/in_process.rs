use super::super::schema::{Scenario, Step};
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
                        append_action(&mut transcript, action, &mut forwarded, &mut commands)?;
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
                    if forwarded != expected.forwarded || commands != expected.commands {
                        return Err(format!(
                            "production expectation mismatch: forwarded={forwarded:?}, commands={commands:?}"
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
