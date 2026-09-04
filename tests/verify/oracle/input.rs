#[derive(Default)]
pub struct PrefixOracle {
    prefix: u8,
    armed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Forward(Vec<u8>),
    Command(&'static str),
}

impl PrefixOracle {
    pub fn new(prefix: u8) -> Self {
        Self {
            prefix,
            armed: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Outcome> {
        let mut output = Vec::new();
        for byte in bytes {
            if self.armed {
                self.armed = false;
                if *byte == self.prefix {
                    forward(&mut output, *byte);
                } else if let Some(command) = command(*byte) {
                    output.push(Outcome::Command(command));
                } else {
                    forward(&mut output, self.prefix);
                    forward(&mut output, *byte);
                }
            } else if *byte == self.prefix {
                self.armed = true;
            } else {
                forward(&mut output, *byte);
            }
        }
        output
    }
}

fn command(byte: u8) -> Option<&'static str> {
    Some(match byte {
        b'|' => "split_horizontal",
        b'-' => "split_vertical",
        b'h' => "focus_left",
        b'j' => "focus_down",
        b'k' => "focus_up",
        b'l' => "focus_right",
        b'x' => "close",
        b'c' => "new_pane",
        b't' => "new_tab",
        b'n' => "next_tab",
        b'p' => "previous_tab",
        b'z' => "zoom",
        b'[' => "copy_mode",
        b'd' => "detach",
        b's' => "workspace_picker",
        b'?' => "help",
        _ => return None,
    })
}

fn forward(output: &mut Vec<Outcome>, byte: u8) {
    if let Some(Outcome::Forward(bytes)) = output.last_mut() {
        bytes.push(byte);
    } else {
        output.push(Outcome::Forward(vec![byte]));
    }
}
