#[derive(Default)]
pub struct PrefixOracle {
    prefix: u8,
    armed: bool,
    pending_ms: u64,
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
            pending_ms: 0,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Outcome> {
        let mut output = Vec::new();
        for byte in bytes {
            if self.armed {
                self.armed = false;
                self.pending_ms = 0;
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
                self.pending_ms = 0;
            } else {
                forward(&mut output, *byte);
            }
        }
        output
    }

    pub fn advance_clock(&mut self, milliseconds: u64) -> Vec<Outcome> {
        if self.armed {
            self.pending_ms = self.pending_ms.saturating_add(milliseconds);
        }
        if self.armed && self.pending_ms >= 40 {
            self.armed = false;
            self.pending_ms = 0;
            vec![Outcome::Forward(vec![self.prefix])]
        } else {
            Vec::new()
        }
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
