#[derive(Default)]
pub struct PrefixOracle {
    prefix: u8,
    armed: bool,
}

#[derive(Debug, Eq, PartialEq)]
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
                } else if *byte == b'|' {
                    output.push(Outcome::Command("split_horizontal"));
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

fn forward(output: &mut Vec<Outcome>, byte: u8) {
    if let Some(Outcome::Forward(bytes)) = output.last_mut() {
        bytes.push(byte);
    } else {
        output.push(Outcome::Forward(vec![byte]));
    }
}
