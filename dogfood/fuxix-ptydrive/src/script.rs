//! A script of steps for a [`Driver`], one per line:
//!
//! ```text
//! # a comment; blank lines are skipped
//! type ls -l\r          text, with \r \n \t \e \\ and \xHH
//! key up up enter ctrl-c named keys, space-separated
//! resize 40x120         rows x columns
//! settle 100 5000       quiet ms, then the limit in ms (default 5000)
//! wait $ 5000           until the output since the last step holds the text
//! sleep 200             ms
//! ```
use crate::{Driver, Settled};
use std::time::Duration;

/// One step of a script.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    Type(Vec<u8>),
    Keys(Vec<Key>),
    Resize(u16, u16),
    Settle { quiet: Duration, limit: Duration },
    Wait { text: Vec<u8>, limit: Duration },
    Sleep(Duration),
}

/// A named key: a cursor key's bytes depend on the mode the program set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Bytes(&'static [u8]),
    Ctrl(u8),
    /// The final byte of a cursor key: `A` up, `B` down, `C` right, `D` left,
    /// `H` home, `F` end.
    Cursor(u8),
}

/// The step as a script line would say it, bytes escaped.
impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Type(bytes) => write!(f, "type {}", bytes.escape_ascii()),
            Step::Keys(keys) => {
                f.write_str("key")?;
                for key in keys {
                    match *key {
                        Key::Bytes(bytes) => write!(f, " {}", bytes.escape_ascii())?,
                        Key::Ctrl(byte) => {
                            write!(f, " ctrl-{}", char::from(byte.wrapping_add(b'a' - 1)))?
                        }
                        Key::Cursor(last) => write!(f, " cursor-{}", char::from(last))?,
                    }
                }
                Ok(())
            }
            Step::Resize(rows, cols) => write!(f, "resize {rows}x{cols}"),
            Step::Settle { quiet, limit } => write!(f, "settle {quiet:?} {limit:?}"),
            Step::Wait { text, limit } => write!(f, "wait {} {limit:?}", text.escape_ascii()),
            Step::Sleep(time) => write!(f, "sleep {time:?}"),
        }
    }
}

const DEFAULT_LIMIT: Duration = Duration::from_secs(5);

/// A script line it could not read: its number and why.
#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub reason: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// The steps of `text`.
pub fn parse(text: &str) -> Result<Vec<Step>, ParseError> {
    let mut steps = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let step = step(word, rest).map_err(|reason| ParseError {
            line: index.saturating_add(1),
            reason,
        })?;
        steps.push(step);
    }
    Ok(steps)
}

fn step(word: &str, rest: &str) -> Result<Step, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    match word {
        "type" => Ok(Step::Type(unescape(rest)?)),
        "key" => words
            .iter()
            .map(|name| key(name))
            .collect::<Result<_, _>>()
            .map(Step::Keys),
        "resize" => {
            let (rows, cols) = rest
                .trim()
                .split_once('x')
                .ok_or_else(|| format!("resize wants ROWSxCOLS, not {rest:?}"))?;
            Ok(Step::Resize(number(rows)?, number(cols)?))
        }
        "settle" => match words.as_slice() {
            [quiet] => Ok(Step::Settle {
                quiet: millis(quiet)?,
                limit: DEFAULT_LIMIT,
            }),
            [quiet, limit] => Ok(Step::Settle {
                quiet: millis(quiet)?,
                limit: millis(limit)?,
            }),
            _ => Err("settle wants QUIET_MS [LIMIT_MS]".into()),
        },
        "wait" => {
            // The last word is the limit if it is a number.
            let (text, limit) = match rest.rsplit_once(' ') {
                Some((text, limit)) if limit.parse::<u64>().is_ok() => (text, millis(limit)?),
                _ => (rest, DEFAULT_LIMIT),
            };
            Ok(Step::Wait {
                text: unescape(text)?,
                limit,
            })
        }
        "sleep" => Ok(Step::Sleep(millis(rest.trim())?)),
        _ => Err(format!("no step {word:?}")),
    }
}

fn number<T: std::str::FromStr>(text: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))
}

fn millis(text: &str) -> Result<Duration, String> {
    number(text).map(Duration::from_millis)
}

/// `text` with its backslash escapes replaced by the bytes they stand for.
pub fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut utf8 = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            continue;
        }
        bytes.push(match chars.next() {
            Some('r') => b'\r',
            Some('n') => b'\n',
            Some('t') => b'\t',
            Some('e') => 0x1b,
            Some('\\') => b'\\',
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                u8::from_str_radix(&hex, 16).map_err(|_| format!("\\x{hex} is not a byte"))?
            }
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        });
    }
    Ok(bytes)
}

fn key(name: &str) -> Result<Key, String> {
    if let Some(letter) = name.strip_prefix("ctrl-") {
        return match letter.as_bytes() {
            [c @ b'a'..=b'z'] => Ok(Key::Ctrl(c - b'a' + 1)),
            _ => Err(format!("ctrl-{letter}: only ctrl-a to ctrl-z")),
        };
    }
    Ok(match name {
        "enter" => Key::Bytes(b"\r"),
        "tab" => Key::Bytes(b"\t"),
        "esc" => Key::Bytes(b"\x1b"),
        "backspace" => Key::Bytes(b"\x7f"),
        "space" => Key::Bytes(b" "),
        "up" => Key::Cursor(b'A'),
        "down" => Key::Cursor(b'B'),
        "right" => Key::Cursor(b'C'),
        "left" => Key::Cursor(b'D'),
        "home" => Key::Cursor(b'H'),
        "end" => Key::Cursor(b'F'),
        "pgup" => Key::Bytes(b"\x1b[5~"),
        "pgdn" => Key::Bytes(b"\x1b[6~"),
        "delete" => Key::Bytes(b"\x1b[3~"),
        "f1" => Key::Bytes(b"\x1bOP"),
        "f2" => Key::Bytes(b"\x1bOQ"),
        "f3" => Key::Bytes(b"\x1bOR"),
        "f4" => Key::Bytes(b"\x1bOS"),
        _ => return Err(format!("no key {name:?}")),
    })
}

/// What one step did, for a log.
#[derive(Debug)]
pub enum Outcome {
    Done,
    Settled(Settled),
    Found,
    NotFound,
}

/// Runs `step` on `driver`. `mark` is where the output stood after the
/// last step, which `wait` searches from; it is moved to the end.
pub fn run(driver: &mut Driver, step: &Step, mark: &mut usize) -> std::io::Result<Outcome> {
    let outcome = match step {
        Step::Type(bytes) => driver.send(bytes).map(|()| Outcome::Done)?,
        Step::Keys(keys) => {
            for key in keys {
                let bytes = match *key {
                    Key::Bytes(bytes) => bytes.to_vec(),
                    Key::Ctrl(byte) => vec![byte],
                    Key::Cursor(last) if driver.application_cursor() => vec![0x1b, b'O', last],
                    Key::Cursor(last) => vec![0x1b, b'[', last],
                };
                driver.send(&bytes)?;
            }
            Outcome::Done
        }
        Step::Resize(rows, cols) => driver.resize(*rows, *cols).map(|()| Outcome::Done)?,
        Step::Settle { quiet, limit } => Outcome::Settled(driver.settle(*quiet, *limit)?),
        Step::Wait { text, limit } => match driver.wait_for(*mark, text, *limit)? {
            true => Outcome::Found,
            false => Outcome::NotFound,
        },
        Step::Sleep(time) => {
            std::thread::sleep(*time);
            Outcome::Done
        }
    };
    *mark = driver.output().len();
    Ok(outcome)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "a test fails by panicking")]
mod tests {
    use super::*;

    #[test]
    fn parses_every_step() {
        let steps = parse("# hi\n\ntype a\\x41\\e\\r\nkey up ctrl-c\nresize 10x40\nsettle 50\nwait $ 9 100\nsleep 3\n")
            .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Type(b"aA\x1b\r".to_vec()),
                Step::Keys(vec![Key::Cursor(b'A'), Key::Ctrl(3)]),
                Step::Resize(10, 40),
                Step::Settle {
                    quiet: Duration::from_millis(50),
                    limit: DEFAULT_LIMIT
                },
                Step::Wait {
                    text: b"$ 9".to_vec(),
                    limit: Duration::from_millis(100)
                },
                Step::Sleep(Duration::from_millis(3)),
            ]
        );
        assert_eq!(parse("type ok\nfly away").unwrap_err().line, 2);
    }
}
