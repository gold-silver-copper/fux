//! vt-capture-pane: `tmux capture-pane -p` for a recorded output stream.
//! Feeds a program's raw output (a file, or stdin) through fux-vt at a
//! given size and prints the screen, and as much of its history as asked.
//!
//!   vt-capture-pane [-e] [-J] [-S START] [-E END] [-x COLS] [-y ROWS]
//!                   [--history BYTES] [FILE]
//!
//! Lines are numbered as tmux numbers them: 0 is the screen's top line,
//! negative numbers are history (-1 the newest history line), and `-` is
//! history's oldest line for `-S`, the screen's last for `-E`. The default
//! is the screen alone.
//!
//! `-e` keeps colours and attributes, as SGR escapes. `-J` joins rows the
//! output wrapped onto into one line each.
use fux_vt::{Attributes, Parser, Row, Size};
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: vt-capture-pane [-e] [-J] [-S START] [-E END] [-x COLS] [-y ROWS] \
                     [--history BYTES] [FILE]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// `-`: history's oldest line, or the screen's last.
    Edge,
    /// A line number: 0 the screen's top, negative history.
    Line(i64),
}

#[derive(Debug)]
struct Options {
    escapes: bool,
    join: bool,
    start: Bound,
    end: Bound,
    rows: u16,
    cols: u16,
    history: usize,
    file: Option<String>,
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("vt-capture-pane: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let input: Box<dyn Read> = match &options.file {
        None => Box::new(std::io::stdin().lock()),
        Some(path) => match std::fs::File::open(path) {
            Ok(file) => Box::new(file),
            Err(e) => {
                eprintln!("vt-capture-pane: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let result = capture(input, &options).and_then(|out| {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(&out)
            .and_then(|()| stdout.flush())
            .map_err(|e| format!("writing: {e}"))
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // A closed pipe (`| head`) is not worth a message.
        Err(message) if message.contains("Broken pipe") => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vt-capture-pane: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        escapes: false,
        join: false,
        start: Bound::Line(0),
        end: Bound::Edge,
        rows: 24,
        cols: 80,
        history: 64 << 20,
        file: None,
    };
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-e" => options.escapes = true,
            "-J" => options.join = true,
            "-S" => options.start = bound(&value("-S")?)?,
            "-E" => options.end = bound(&value("-E")?)?,
            "-x" => options.cols = number(&value("-x")?, "-x")?,
            "-y" => options.rows = number(&value("-y")?, "-y")?,
            "--history" => options.history = number(&value("--history")?, "--history")?,
            "-h" | "--help" => return Err("help".into()),
            _ if arg.starts_with('-') && arg != "-" => return Err(format!("unknown option {arg}")),
            _ if options.file.is_none() => options.file = (arg != "-").then_some(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    Ok(options)
}

fn bound(text: &str) -> Result<Bound, String> {
    if text == "-" {
        return Ok(Bound::Edge);
    }
    text.parse()
        .map(Bound::Line)
        .map_err(|_| format!("not a line number: {text}"))
}

fn number<T: std::str::FromStr>(text: &str, name: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{name}: not a number: {text}"))
}

/// Reads `input` to its end through a terminal, and renders the lines asked.
fn capture(mut input: impl Read, options: &Options) -> Result<Vec<u8>, String> {
    let size = Size::new(options.rows, options.cols).map_err(|e| format!("bad size: {e}"))?;
    let mut parser = Parser::with_options(size, options.history, fux_vt::Options::default())
        .map_err(|e| format!("a {}x{} terminal: {e}", options.cols, options.rows))?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("reading: {e}")),
        };
        // Replies go nowhere: there is no program to answer. A refusal of
        // room loses only what it was for, and is counted; the rest is read.
        let _ = parser.process_all(buffer.get(..n).unwrap_or_default(), &mut ());
    }
    Ok(render(&parser, options))
}

/// The rows from `-S` to `-E`, as text.
fn render(parser: &Parser, options: &Options) -> Vec<u8> {
    let screen = parser.screen();
    let history = screen.history_len();
    let rows: Vec<Row<'_>> = screen.rows().collect();
    let last = rows.len().saturating_sub(1);
    // A line number as an index into `rows`, clamped to them.
    let index = |bound: Bound, edge: usize| match bound {
        Bound::Edge => edge,
        Bound::Line(line) => {
            let at = i64::try_from(history)
                .unwrap_or(i64::MAX)
                .saturating_add(line);
            usize::try_from(at.max(0)).unwrap_or(usize::MAX).min(last)
        }
    };
    let (first, mut end) = (index(options.start, 0), index(options.end, last));
    // Joined, a line the range ends inside is printed whole.
    while options.join && end < last && rows.get(end).is_some_and(Row::continues) {
        end = end.saturating_add(1);
    }
    let mut out = Vec::new();
    let mut line = Line::default();
    for row in rows.get(first..=end).unwrap_or_default() {
        let joined = options.join && row.continues();
        line.push_row(row, joined, options.escapes);
        if !joined {
            line.finish(&mut out);
        }
    }
    line.finish(&mut out);
    out
}

/// A line being built from its rows: cells as text, with the pen's changes
/// as SGR when asked, and trailing blanks held back until something follows.
#[derive(Default)]
struct Line {
    text: Vec<u8>,
    /// Blanks after the text, written only if more text follows them.
    blanks: Vec<u8>,
    /// The pen as last written to `text`.
    pen: Attributes,
    started: bool,
}

impl Line {
    fn push_row(&mut self, row: &Row<'_>, joined: bool, escapes: bool) {
        self.started = true;
        // A wide glyph that did not fit at a wrapped row's end left a blank
        // there; `glyphs` leaves it out, and the line reads on without it.
        for (_, cell) in row.glyphs() {
            let attributes = if escapes {
                cell.attributes()
            } else {
                Attributes::default()
            };
            let text = cell.shown();
            // A blank in the default pen may be trailing: held back. One in
            // a colour is drawn, as tmux draws it.
            if text == " " && attributes == Attributes::default() {
                self.blanks.push(b' ');
                continue;
            }
            if !self.blanks.is_empty() {
                // The blanks are in the default pen, not the text's before.
                self.reset_pen();
                self.text.append(&mut self.blanks);
            }
            attributes.write_sgr_change(self.pen, &mut self.text, true);
            self.pen = attributes;
            self.text.extend_from_slice(text.as_bytes());
        }
        if !joined {
            self.reset_pen();
        }
    }

    fn reset_pen(&mut self) {
        if self.pen != Attributes::default() {
            self.text.extend_from_slice(b"\x1b[0m");
            self.pen = Attributes::default();
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.started {
            return;
        }
        self.reset_pen();
        out.append(&mut self.text);
        out.push(b'\n');
        self.blanks.clear();
        self.started = false;
    }
}

#[cfg(test)]
#[expect(clippy::panic, reason = "a test fails by panicking")]
mod tests {
    use super::*;

    fn run(args: &[&str], input: &[u8]) -> String {
        let options = parse(args.iter().map(|a| (*a).to_owned())).unwrap_or_else(|e| panic!("{e}"));
        let out = capture(input, &options).unwrap_or_else(|e| panic!("{e}"));
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn history_ranges_count_as_tmux_counts_them() {
        let input = b"one\r\ntwo\r\nthree\r\nfour\r\nfive";
        // Three rows: "one" and "two" scrolled into history.
        assert_eq!(run(&["-y", "3", "-x", "10"], input), "three\nfour\nfive\n");
        assert_eq!(
            run(&["-y", "3", "-x", "10", "-S", "-"], input),
            "one\ntwo\nthree\nfour\nfive\n"
        );
        assert_eq!(
            run(&["-y", "3", "-x", "10", "-S", "-1", "-E", "0"], input),
            "two\nthree\n"
        );
    }

    #[test]
    fn joining_reads_a_wrapped_line_whole_and_escapes_keep_styles() {
        let input = b"abcdefgh\x1b[1;31mij\x1b[m  x\r\nnext";
        let plain = ["-y", "4", "-x", "5"];
        assert_eq!(run(&plain, input), "abcde\nfghij\n  x\nnext\n");
        assert_eq!(
            run(&["-y", "4", "-x", "5", "-J"], input),
            "abcdefghij  x\nnext\n"
        );
        assert_eq!(
            run(&["-y", "4", "-x", "5", "-J", "-e"], input),
            "abcdefgh\x1b[1;31mij\x1b[0m  x\nnext\n"
        );
        // A range ending inside a wrapped line takes the line whole.
        assert_eq!(
            run(&["-y", "4", "-x", "5", "-J", "-E", "0"], input),
            "abcdefghij  x\n"
        );
    }
}
