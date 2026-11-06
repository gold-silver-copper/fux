//! vt-log-clean: prints the plain text a person would have seen on a
//! terminal while a build log was written to it.
//!
//! The log is played through a terminal emulator (fux-vt), so `\r`
//! progress bars, backspaces, line erases, cursor movement and colours end
//! up as they looked. Lines that scroll off the top of the emulated screen
//! are printed as they leave it, so a log of any length streams through in
//! bounded memory; what is left on the screen at the end is printed last.
//!
//! A pipe's log has bare `\n` line ends, which on a terminal the tty driver
//! turns into `\r\n`; the emulator is put in newline mode (LNM) to do the same.

use fux_vt::{Parser, Row, Size};
use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: vt-log-clean [--cols N] [--rows N] [FILE...]
Reads each FILE (or stdin, or `-`) as raw terminal output and prints the
text a person would have seen, one line per line, scrolled-off lines included.
  --cols N   the emulated terminal's width (default 1000): longer lines are
             wrapped there and joined again on output
  --rows N   its height (default 100): how far back cursor movement can
             redraw lines that are still on screen";

/// History the emulator may hold between two drains. A drain follows each
/// read of input; rows let go before one are counted and reported.
const HISTORY_BYTES: usize = 64 << 20;

/// Newline mode: LF also returns the carriage, as a tty's ONLCR does.
const NEWLINE_MODE: &[u8] = b"\x1b[20h";

/// A log being played through a terminal, printing each line once it is
/// final: scrolled off the screen, or left on it at the end.
struct Cleaner {
    parser: Parser,
    /// The start of a line whose rows have begun to scroll off while its
    /// end is still on the screen.
    open: String,
    /// The number of the next history row to print.
    seen: u64,
}

impl Cleaner {
    fn new(size: Size) -> Result<Cleaner, fux_vt::Error> {
        let parser = Parser::with_options(size, HISTORY_BYTES, fux_vt::Options::default())?;
        let mut cleaner = Cleaner {
            parser,
            open: String::new(),
            seen: 0,
        };
        // Replies to queries (cursor reports, device attributes) go
        // nowhere: there is no program to answer. Five bytes of a mode
        // never need room.
        let _ = cleaner.parser.process_all(NEWLINE_MODE, &mut ());
        Ok(cleaner)
    }

    /// Plays `bytes`, printing the lines they scroll off.
    fn feed(&mut self, bytes: &[u8], out: &mut impl Write) -> io::Result<()> {
        // A refusal of room (`Err`) loses only what it was for, and every
        // byte was read: the log goes on.
        let _ = self.parser.process_all(bytes, &mut ());
        self.drain(out)
    }

    /// Prints the rows that have scrolled into history, and lets them go.
    fn drain(&mut self, out: &mut impl Write) -> io::Result<()> {
        let screen = self.parser.screen();
        // The primary's history, whichever screen a program shows.
        let numbers = screen.history_numbers();
        if numbers.start > self.seen {
            let lost = numbers.start - self.seen;
            eprintln!("vt-log-clean: {lost} lines left the history budget before they were printed");
        }
        self.seen = numbers.end;
        if numbers.is_empty() {
            return Ok(());
        }
        let history = usize::try_from(numbers.end - numbers.start).unwrap_or(usize::MAX);
        for row in screen.main_rows().take(history) {
            push_row(&row, &mut self.open);
            if !row.continues() {
                print_line(&mut self.open, out)?;
            }
        }
        self.parser.clear_history();
        Ok(())
    }

    /// Prints what is left on the screen, but the blank lines at its end.
    fn finish(mut self, out: &mut impl Write) -> io::Result<()> {
        self.drain(out)?;
        let mut lines = Vec::new();
        for row in self.parser.screen().main_rows() {
            push_row(&row, &mut self.open);
            if !row.continues() {
                lines.push(std::mem::take(&mut self.open));
            }
        }
        lines.push(std::mem::take(&mut self.open));
        let shown = lines
            .iter()
            .rposition(|line| !line.trim_end().is_empty())
            .map_or(0, |last| last.saturating_add(1));
        for mut line in lines.into_iter().take(shown) {
            print_line(&mut line, out)?;
        }
        Ok(())
    }
}

/// Adds the text of `row` to `line`: whole if its line goes on below, as
/// its trailing blanks are part of the line; else up to its last stored
/// cell, as [`Row::text`] pads to the row's width, which at 1000 columns
/// made the cleaner 26 times slower.
fn push_row(row: &Row<'_>, line: &mut String) {
    let end = if row.continues() { usize::MAX } else { row.stored_len() };
    line.extend(row.glyphs().take_while(|(col, _)| usize::from(*col) < end).map(|(_, cell)| cell.shown()));
}

/// Prints `line` without its trailing blanks, and empties it.
fn print_line(line: &mut String, out: &mut impl Write) -> io::Result<()> {
    out.write_all(line.trim_end_matches(' ').as_bytes())?;
    out.write_all(b"\n")?;
    line.clear();
    Ok(())
}

/// Plays all of `input` through a fresh terminal of `size`.
fn clean(mut input: impl Read, size: Size, out: &mut impl Write) -> io::Result<()> {
    let mut cleaner = Cleaner::new(size).map_err(io::Error::other)?;
    let mut buffer = vec![0u8; 64 << 10];
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        cleaner.feed(buffer.get(..read).unwrap_or_default(), out)?;
    }
    cleaner.finish(out)
}

struct Args {
    size: Size,
    files: Vec<String>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut rows, mut cols, mut files) = (100u16, 1000u16, Vec::new());
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> Result<u16, String> {
            let value = args.next().ok_or(format!("{name} needs a number"))?;
            value
                .parse()
                .map_err(|_| format!("{name}: not a number from 1 to 65535: {value}"))
        };
        match arg.as_str() {
            "--rows" => rows = number("--rows")?,
            "--cols" => cols = number("--cols")?,
            "-h" | "--help" => return Err(String::new()),
            "--" => files.extend(args.by_ref()),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => files.push(arg),
        }
    }
    let size = Size::new(rows, cols).map_err(|_| "--rows and --cols must not be 0".to_owned())?;
    if !fux_vt::fits(size) {
        return Err(format!("a {rows}x{cols} terminal is more cells than fux-vt holds"));
    }
    if files.is_empty() {
        files.push("-".to_owned());
    }
    Ok(Args { size, files })
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(why) if why.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("vt-log-clean: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut out = BufWriter::new(io::stdout().lock());
    let mut failed = false;
    for file in &args.files {
        let done = if file == "-" {
            clean(io::stdin().lock(), args.size, &mut out)
        } else {
            std::fs::File::open(file).and_then(|f| clean(f, args.size, &mut out))
        };
        match done.and_then(|()| out.flush()) {
            Ok(()) => {}
            // A reader that stopped reading (`| head`) is no failure.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("vt-log-clean: {file}: {error}");
                failed = true;
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `log` cleaned at `rows` by `cols`, fed `chunk` bytes at a time.
    fn cleaned(log: &[u8], rows: u16, cols: u16, chunk: usize) -> String {
        let size = Size::new(rows, cols).unwrap_or_else(|_| unreachable!());
        let mut out = Vec::new();
        let mut cleaner = Cleaner::new(size).unwrap_or_else(|_| unreachable!());
        for part in log.chunks(chunk) {
            assert!(cleaner.feed(part, &mut out).is_ok());
        }
        assert!(cleaner.finish(&mut out).is_ok());
        String::from_utf8(out).unwrap_or_default()
    }

    #[test]
    fn progress_bars_backspaces_and_colours_end_as_they_looked() {
        let log = b"\x1b[1;32m   Compiling\x1b[0m foo v0.1.0\n\
            [  0%] fetch\r[ 50%] fetch\r\x1b[K[100%] done\n\
            typo\x08\x08po!\n\
            a: 0%\nb: 0%\n\x1b[2A\x1b[2Ka: 100%\n\x1b[2Kb: 100%\n\
            \tindented\n\n\n";
        for chunk in [1, 3, 4096] {
            assert_eq!(
                cleaned(log, 10, 80, chunk),
                "   Compiling foo v0.1.0\n[100%] done\ntypo!\na: 100%\nb: 100%\n        indented\n",
                "fed {chunk} bytes at a time"
            );
        }
    }

    #[test]
    fn scrolled_off_lines_are_kept_in_order_and_wrapped_lines_joined() {
        let mut log = Vec::new();
        let mut want = String::new();
        for n in 0..2000 {
            let line = format!("line {n}:{}", "x".repeat(n % 37));
            log.extend_from_slice(format!("{line}\r\n").as_bytes());
            want.push_str(&line);
            want.push('\n');
        }
        // 3 rows of 16 columns: most lines wrap, and wrap across the top.
        assert_eq!(cleaned(&log, 3, 16, 777), want);
        // A wide glyph that does not fit the last column moves down whole.
        assert_eq!(cleaned("abc漢字\n".as_bytes(), 2, 4, 64), "abc漢字\n");
    }
}
