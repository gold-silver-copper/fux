//! `ctl`'s commands: one short line each, so each reaches the FIFO whole.
use crate::Failure;
use crate::files::{MAX_COLS, MAX_ROWS, PIPE_BUF};
use std::path::PathBuf;

/// The most bytes `send` and `paste` take from one file.
pub const MAX_SEND: usize = 16 << 20;

/// A command, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `resize COLS ROWS`: the terminal's size in cells.
    Resize { cols: u16, rows: u16 },
    /// `send PATH`: a file's bytes, whole, as typed.
    Send(PathBuf),
    /// `paste PATH`: a file's text, whole, as a paste: bracketed if the
    /// program asked for that, its own end markers taken out.
    Paste(PathBuf),
    /// `cell WIDTH HEIGHT`: a cell's size in pixels, for pictures.
    Cell { width: u16, height: u16 },
    /// `signal NAME`: a signal (`INT`, `TERM`, `HUP`, …) to the program's
    /// foreground process group.
    Signal(String),
    /// `kill`: end the program and all it started.
    Kill,
}

/// Reads one line (its newline gone).
pub fn parse(line: &str) -> Result<Command, Failure> {
    let mut words = line.split_whitespace();
    let verb = words.next().unwrap_or_default();
    let rest: Vec<&str> = words.collect();
    let number = |text: &str, most: u16, what: &str| -> Result<u16, Failure> {
        match text.parse::<u16>() {
            Ok(n) if (1..=most).contains(&n) => Ok(n),
            _ => Err(Failure::new(format!("{what} {text:?}: 1 to {most}"))),
        }
    };
    let path = |rest: &[&str]| -> Result<PathBuf, Failure> {
        // The path is the rest of the line, spaces and all.
        let text = line.trim_start().split_once(' ').map(|(_, p)| p.trim());
        let path = PathBuf::from(text.unwrap_or_default());
        if rest.is_empty() || !path.is_absolute() {
            return Err(Failure::new("it takes an absolute path"));
        }
        Ok(path)
    };
    let command = match (verb, rest.as_slice()) {
        ("resize", [cols, rows]) => Command::Resize {
            cols: number(cols, MAX_COLS, "columns")?,
            rows: number(rows, MAX_ROWS, "rows")?,
        },
        ("send", rest) => Command::Send(path(rest)?),
        ("paste", rest) => Command::Paste(path(rest)?),
        ("cell", [width, height]) => Command::Cell {
            width: number(width, 1000, "width")?,
            height: number(height, 1000, "height")?,
        },
        ("signal", [name]) => Command::Signal(name.trim_start_matches("SIG").to_owned()),
        ("kill", []) => Command::Kill,
        ("resize" | "cell" | "signal" | "kill", _) => {
            return Err(Failure::new("wrong arguments; see the README"));
        }
        _ => {
            return Err(Failure::new(
                "no such command; the commands are resize, send, paste, signal, cell and kill",
            ));
        }
    };
    Ok(command)
}

/// Splits what `ctl` gave into lines, keeping a part line for the next
/// read. A line longer than [`PIPE_BUF`] is no command: it is given back
/// as an error, and what follows it to its newline is dropped.
#[derive(Default)]
pub struct Lines {
    part: Vec<u8>,
    overlong: bool,
}

impl Lines {
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Result<String, Failure>>) {
        for &byte in bytes {
            if byte == b'\n' {
                let line = std::mem::take(&mut self.part);
                if std::mem::take(&mut self.overlong) {
                    out.push(Err(Failure::new(format!(
                        "a line over {PIPE_BUF} bytes: no command is that long"
                    ))));
                } else if !line.iter().all(u8::is_ascii_whitespace) {
                    out.push(String::from_utf8(line).map_err(|_| Failure::new("not UTF-8")));
                }
                continue;
            }
            if self.part.len() >= PIPE_BUF {
                self.overlong = true;
                self.part.clear();
            }
            if !self.overlong {
                self.part.push(byte);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_reads_and_bad_ones_say_why() {
        assert_eq!(
            parse("resize 100 30"),
            Ok(Command::Resize {
                cols: 100,
                rows: 30
            })
        );
        assert_eq!(
            parse("send /a b/c"),
            Ok(Command::Send(PathBuf::from("/a b/c")))
        );
        assert_eq!(parse("kill"), Ok(Command::Kill));
        assert!(parse("resize 0 30").is_err());
        assert!(parse("resize 100").is_err());
        assert!(parse("send relative").is_err());
        assert!(parse("explode").is_err());
    }

    #[test]
    fn an_overlong_line_is_refused_and_the_next_one_taken() {
        let mut lines = Lines::default();
        let mut out = Vec::new();
        lines.push(&vec![b'x'; PIPE_BUF + 10], &mut out);
        lines.push(b"\nkill\n", &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.first().is_some_and(Result::is_err));
        assert_eq!(out.get(1), Some(&Ok("kill".to_owned())));
    }
}
