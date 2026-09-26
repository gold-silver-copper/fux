//! A step of a walk, as it was taken: concrete ids and bytes, never the
//! choices that produced it, so a trace replays the same steps whatever the
//! seed. Each is one line of a trace, in fux's word grammar.
use fux::words;

/// What a pane's program does of its own accord, through the fixture's
/// script (`act.sh`), which prints a marker when it is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Enters the alternate screen, writes, and leaves it.
    Alternate,
    /// Turns on mouse reporting.
    Mouse,
    /// Turns on bracketed paste.
    Bracketed,
    /// Turns on application cursor keys.
    AppCursor,
    /// Tells its terminal another size.
    Stty(u16, u16),
    /// Writes this many lines at once.
    Burst(u32),
    /// Stops reading its input for this many seconds.
    Deaf(u32),
}

/// A configuration file to write before a `reload`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Config {
    Valid,
    Invalid,
    PrefixA,
    PrefixB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Stop,
    Cont,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Bytes typed into a real client's terminal.
    Keys { client: String, bytes: Vec<u8> },
    /// A command from the command line.
    Cli(Vec<String>),
    /// A client's terminal resized.
    Resize {
        client: String,
        rows: u16,
        cols: u16,
    },
    /// A new real client.
    Attach { rows: u16, cols: u16 },
    /// A client's terminal hung up (SIGHUP).
    Hangup { client: String },
    /// A pane's program acting on its own.
    Child { pane: String, act: Act },
    /// A pane's shell exiting with this status.
    Exit { pane: String, code: u8 },
    /// A pane's shell stopped or continued.
    Signal { pane: String, stop: Stop },
    /// The configuration rewritten, then reloaded.
    Config(Config),
    /// A client that asks for 9000 by 9000 and must get 4096 by 4096.
    Clamp,
}

/// Bytes as a word: printable ASCII as it is, a backslash doubled, and
/// everything else `\xNN`.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(*b)),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

fn unescape(word: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                out.push(u8::from_str_radix(&hex, 16).map_err(|e| format!("\\x{hex}: {e}"))?);
            }
            other => return Err(format!("a bad escape in {word:?}: {other:?}")),
        }
    }
    Ok(out)
}

impl Step {
    /// The step as a trace line.
    pub fn line(&self) -> String {
        match self {
            Step::Keys { client, bytes } => {
                format!("keys {client} {}", words::quote(&escape(bytes)))
            }
            Step::Cli(args) => format!("cli {}", words::join(args)),
            Step::Resize { client, rows, cols } => format!("resize {client} {rows} {cols}"),
            Step::Attach { rows, cols } => format!("attach {rows} {cols}"),
            Step::Hangup { client } => format!("hangup {client}"),
            Step::Child { pane, act } => format!(
                "child {pane} {}",
                match act {
                    Act::Alternate => "alternate".to_owned(),
                    Act::Mouse => "mouse".to_owned(),
                    Act::Bracketed => "bracketed".to_owned(),
                    Act::AppCursor => "appcursor".to_owned(),
                    Act::Stty(r, c) => format!("stty {r} {c}"),
                    Act::Burst(n) => format!("burst {n}"),
                    Act::Deaf(s) => format!("deaf {s}"),
                }
            ),
            Step::Exit { pane, code } => format!("exit {pane} {code}"),
            Step::Signal { pane, stop } => format!(
                "signal {pane} {}",
                match stop {
                    Stop::Stop => "stop",
                    Stop::Cont => "cont",
                }
            ),
            Step::Config(config) => format!(
                "config {}",
                match config {
                    Config::Valid => "valid",
                    Config::Invalid => "invalid",
                    Config::PrefixA => "prefix-a",
                    Config::PrefixB => "prefix-b",
                }
            ),
            Step::Clamp => "clamp".to_owned(),
        }
    }

    /// A trace line as a step.
    pub fn parse(line: &str) -> Result<Step, String> {
        let words = words::split(line)?;
        let text: Vec<&str> = words.iter().map(String::as_str).collect();
        let number = |w: &str| w.parse::<u16>().map_err(|e| format!("{w:?}: {e}"));
        Ok(match text.as_slice() {
            ["keys", client, bytes] => Step::Keys {
                client: (*client).to_owned(),
                bytes: unescape(bytes)?,
            },
            ["cli", args @ ..] => Step::Cli(args.iter().map(|a| (*a).to_owned()).collect()),
            ["resize", client, rows, cols] => Step::Resize {
                client: (*client).to_owned(),
                rows: number(rows)?,
                cols: number(cols)?,
            },
            ["attach", rows, cols] => Step::Attach {
                rows: number(rows)?,
                cols: number(cols)?,
            },
            ["hangup", client] => Step::Hangup {
                client: (*client).to_owned(),
            },
            ["child", pane, rest @ ..] => Step::Child {
                pane: (*pane).to_owned(),
                act: match rest {
                    ["alternate"] => Act::Alternate,
                    ["mouse"] => Act::Mouse,
                    ["bracketed"] => Act::Bracketed,
                    ["appcursor"] => Act::AppCursor,
                    ["stty", r, c] => Act::Stty(number(r)?, number(c)?),
                    ["burst", n] => Act::Burst(n.parse().map_err(|e| format!("{n:?}: {e}"))?),
                    ["deaf", s] => Act::Deaf(s.parse().map_err(|e| format!("{s:?}: {e}"))?),
                    other => return Err(format!("an unknown act {other:?}")),
                },
            },
            ["exit", pane, code] => Step::Exit {
                pane: (*pane).to_owned(),
                code: code.parse().map_err(|e| format!("{code:?}: {e}"))?,
            },
            ["signal", pane, "stop"] => Step::Signal {
                pane: (*pane).to_owned(),
                stop: Stop::Stop,
            },
            ["signal", pane, "cont"] => Step::Signal {
                pane: (*pane).to_owned(),
                stop: Stop::Cont,
            },
            ["config", "valid"] => Step::Config(Config::Valid),
            ["config", "invalid"] => Step::Config(Config::Invalid),
            ["config", "prefix-a"] => Step::Config(Config::PrefixA),
            ["config", "prefix-b"] => Step::Config(Config::PrefixB),
            ["clamp"] => Step::Clamp,
            other => return Err(format!("an unknown step {other:?}")),
        })
    }

    /// Whether the step is keys typed into a client, which may open or
    /// close an overlay by design.
    pub fn types(&self) -> bool {
        matches!(self, Step::Keys { .. })
    }
}
