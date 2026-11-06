//! vt-a11y-reader: replays terminal output through fux-vt and prints, one
//! per line, plain-text announcements of what changed, for a screen reader:
//! new lines, the cursor line as it is edited, bells and title changes.
//! Redraws that leave the text as it was say nothing, and a line that churns
//! in place (a spinner, a progress count) is told at most every two seconds.
//!
//! ```text
//! vt-a11y-reader [--size ROWSxCOLS] [--settle MS] [--timing FILE] [FILE]
//! ```
//! With no FILE it reads stdin live: what arrives is announced once output
//! has been quiet for `--settle` ms (default 100), or every 500 ms of
//! steady output. With `--timing` (util-linux `script -T`), FILE is replayed
//! on the recorded clock, so its pauses settle as they did live.

use fux_vt::{Event, Feature, Mode, Options, Parser, RowId, Sink, Size};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// How long one line churning in place stays quiet between announcements.
const CHURN_GAP: Duration = Duration::from_secs(2);
/// The longest steady output goes unannounced.
const MAX_BATCH: Duration = Duration::from_millis(500);
/// History kept between settles: rows scrolled off are read from it.
const HISTORY_BYTES: usize = 16 << 20;

/// What the program's output asked of the terminal beyond the screen.
#[derive(Default)]
struct Events {
    bells: u32,
    title: Option<String>,
}

impl Sink for Events {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Bell => self.bells = self.bells.saturating_add(1),
            Event::Title(kept) => self.title = Some(String::from_utf8_lossy(kept.bytes).into_owned()),
            _ => {}
        }
    }
}

/// The reader: a screen, and what has been announced of it.
struct Reader {
    parser: Parser,
    events: Events,
    /// The text last announced for each line on the screen, by its first row.
    told: HashMap<RowId, String>,
    /// The other screen's, kept while this one is shown.
    told_other: HashMap<RowId, String>,
    /// When each line was last told as churn.
    churned: HashMap<RowId, Duration>,
    /// History rows numbered below this have been read.
    history_end: u64,
    title: String,
    alternate: bool,
}

/// A logical line: its rows' joined text, keyed by its first row.
struct Line {
    id: RowId,
    text: String,
    on_screen: bool,
    has_cursor: bool,
}

impl Reader {
    fn new(size: Size) -> Result<Reader, String> {
        let options = Options::new().with(Feature::Events);
        let parser = Parser::with_options(size, HISTORY_BYTES, options)
            .map_err(|e| format!("cannot make a {}x{} screen: {e:?}", size.rows(), size.cols()))?;
        Ok(Reader {
            parser,
            events: Events::default(),
            told: HashMap::new(),
            told_other: HashMap::new(),
            churned: HashMap::new(),
            history_end: 0,
            title: String::new(),
            alternate: false,
        })
    }

    fn feed(&mut self, bytes: &[u8]) {
        // A refusal loses only a history row or a picture; the screen reads on.
        let _refused = self.parser.process_all(bytes, &mut self.events);
    }

    /// The lines scrolled into history since the last settle, then the
    /// screen's, each soft-wrapped line joined.
    fn lines(&self) -> (Vec<Line>, u64) {
        let screen = self.parser.screen();
        let numbers = screen.history_numbers();
        let first = self.history_end.max(numbers.start);
        let lost = first.saturating_sub(self.history_end);
        let skip = usize::try_from(first.saturating_sub(numbers.start)).unwrap_or(usize::MAX);
        let history = screen.history_len();
        let (cursor_row, _) = screen.cursor_position();
        let mut lines: Vec<Line> = Vec::new();
        let mut open = false;
        for (index, row) in screen.rows().enumerate().skip(skip) {
            let on_screen = index >= history;
            let screen_row = index.checked_sub(history);
            let has_cursor = screen_row == Some(usize::from(cursor_row));
            match lines.last_mut() {
                Some(line) if open => {
                    line.text.push_str(&row.text());
                    line.on_screen |= on_screen;
                    line.has_cursor |= has_cursor;
                }
                _ => lines.push(Line { id: row.id(), text: row.text(), on_screen, has_cursor }),
            }
            open = row.continues();
        }
        (lines, lost)
    }

    /// What changed since the last settle, at input time `now`; `last`
    /// tells all churn, as nothing more will come.
    fn settle(&mut self, now: Duration, last: bool) -> Vec<String> {
        let mut said = Vec::new();
        if let Some(title) = self.events.title.take().filter(|t| *t != self.title) {
            said.push(format!("title: {title}"));
            self.title = title;
        }
        let alternate = self.parser.screen().mode(Mode::AlternateScreen);
        if alternate != self.alternate {
            self.alternate = alternate;
            std::mem::swap(&mut self.told, &mut self.told_other);
            said.push(if alternate { "full-screen program started" } else { "full-screen program ended" }.to_string());
        }
        let (lines, lost) = self.lines();
        if lost > 0 {
            said.push(format!("{lost} lines skipped"));
        }
        let mut told = HashMap::new();
        for line in lines {
            let text = line.text.trim_end().to_string();
            let before = self.told.get(&line.id).cloned().unwrap_or_default();
            if text == before {
                if line.on_screen {
                    told.insert(line.id, text);
                }
                continue;
            }
            let churn = line.on_screen && !before.is_empty() && is_churn(&before, &text);
            let quiet = self.churned.get(&line.id).is_none_or(|at| now.saturating_sub(*at) >= CHURN_GAP);
            if churn && !quiet && !last {
                told.insert(line.id, before); // told later, once quiet
                continue;
            }
            if churn {
                self.churned.insert(line.id, now);
            }
            match text.strip_prefix(before.as_str()) {
                // Row::text drops the blanks a line ends in, so a typed
                // space shows only with what follows it.
                // A short addition at the cursor is typing (or its echo); a
                // long one is output, told as the whole line.
                Some(typed) if line.has_cursor && !before.is_empty() && typed.chars().count() <= 16 => {
                    said.push(format!("typed: {}", typed.trim_start()));
                }
                _ if text.is_empty() => {}
                _ => said.push(text.clone()),
            }
            if line.on_screen {
                told.insert(line.id, text);
            }
        }
        self.churned.retain(|id, _| told.contains_key(id));
        self.told = told;
        self.history_end = self.parser.screen().history_numbers().end;
        if self.events.bells > 0 {
            said.push(match self.events.bells {
                1 => "bell".to_string(),
                n => format!("bell x{n}"),
            });
            self.events.bells = 0;
        }
        said
    }
}

/// Whether `after` is `before` changed in place: the same length, a few
/// characters differing (a spinner, a counter), not a new line of text.
fn is_churn(before: &str, after: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (before.chars().collect(), after.chars().collect());
    if a.len() != b.len() {
        return false;
    }
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    differ.saturating_mul(4) <= a.len().max(4)
}

struct Args {
    size: Size,
    settle: Duration,
    timing: Option<String>,
    file: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { size: Size::new(24, 80).map_err(|e| format!("{e:?}"))?, settle: Duration::from_millis(100), timing: None, file: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--size" => args.size = parse_size(&value()?)?,
            "--settle" => {
                let ms = value()?.parse::<u64>().map_err(|e| format!("--settle: {e}"))?;
                args.settle = Duration::from_millis(ms);
            }
            "--timing" => args.timing = Some(value()?),
            "-h" | "--help" => return Err("usage: vt-a11y-reader [--size ROWSxCOLS] [--settle MS] [--timing FILE] [FILE]".into()),
            _ if args.file.is_none() && !arg.starts_with("--") => args.file = Some(arg),
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    Ok(args)
}

fn parse_size(text: &str) -> Result<Size, String> {
    let (rows, cols) = text.split_once('x').ok_or("size is ROWSxCOLS")?;
    let rows = rows.parse().map_err(|e| format!("rows: {e}"))?;
    let cols = cols.parse().map_err(|e| format!("cols: {e}"))?;
    Size::new(rows, cols).map_err(|e| format!("size: {e:?}"))
}

/// A typescript's output without its header and footer lines, if it has
/// them, and the size the header names.
fn strip_header(bytes: &[u8]) -> (&[u8], Option<Size>) {
    if !bytes.starts_with(b"Script started on ") {
        return (bytes, None);
    }
    let end = bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| n.saturating_add(1));
    let (header, rest) = bytes.split_at(end);
    // And the footer `script` adds after the program's output.
    let footer = b"\nScript done on ";
    let body = rest.windows(footer.len()).rposition(|w| w == footer).unwrap_or(rest.len());
    let rest = rest.get(..body).unwrap_or(rest);
    let header = String::from_utf8_lossy(header);
    let number = |key: &str| -> Option<u16> {
        let from = header.find(key)?.checked_add(key.len())?;
        header.get(from..)?.split('"').next()?.parse().ok()
    };
    let size = number("LINES=\"").zip(number("COLUMNS=\"")).and_then(|(r, c)| Size::new(r, c).ok());
    (rest, size)
}

/// An output error as said, or nothing for a reader that went away
/// (`| head`), which ends the program quietly.
fn io_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::BrokenPipe => String::new(),
        _ => error.to_string(),
    }
}

fn say(out: &mut impl Write, said: &[String]) -> std::io::Result<()> {
    for line in said {
        writeln!(out, "{line}")?;
    }
    out.flush()
}

/// Replays `bytes` on the clock of a `script -T` timing file.
fn replay_timed(args: &Args, bytes: &[u8], timing: &str, out: &mut impl Write) -> Result<(), String> {
    let (mut rest, size) = strip_header(bytes);
    let mut reader = Reader::new(size.unwrap_or(args.size))?;
    let (mut now, mut since) = (Duration::ZERO, Duration::ZERO);
    for (number, line) in timing.lines().enumerate() {
        let mut fields = line.split_whitespace();
        // The advanced format leads with the stream: only output (O) is read.
        let first = fields.next().unwrap_or_default();
        let delay = match first {
            "O" => fields.next().unwrap_or_default(),
            "I" | "S" | "H" => continue,
            delay => delay,
        };
        let bad = || format!("timing line {}: {line:?}", number.saturating_add(1));
        let delay = Duration::try_from_secs_f64(delay.parse::<f64>().map_err(|_| bad())?).map_err(|_| bad())?;
        let count: usize = fields.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
        if delay >= args.settle || now.saturating_sub(since) >= MAX_BATCH {
            say(out, &reader.settle(now, false)).map_err(io_error)?;
            since = now;
        }
        now = now.saturating_add(delay);
        let (chunk, tail) = rest.split_at(count.min(rest.len()));
        reader.feed(chunk);
        rest = tail;
    }
    reader.feed(rest);
    say(out, &reader.settle(now, true)).map_err(io_error)
}

/// Reads stdin as it comes, announcing at each quiet spell.
fn live(args: &Args, out: &mut impl Write) -> Result<(), String> {
    let mut reader = Reader::new(args.size)?;
    // A pipe's lines end in LF alone, where a terminal's driver would add
    // the CR (ONLCR): LNM has LF return the carriage too.
    reader.feed(b"\x1b[20h");
    let (send, receive) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = vec![0u8; 1 << 16];
        while let Ok(n @ 1..) = stdin.read(&mut buffer) {
            if send.send(buffer.get(..n).unwrap_or_default().to_vec()).is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    let mut batch: Option<Instant> = None;
    loop {
        match receive.recv_timeout(args.settle) {
            Ok(bytes) => {
                reader.feed(&bytes);
                let begun = *batch.get_or_insert_with(Instant::now);
                if begun.elapsed() < MAX_BATCH {
                    continue;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if batch.is_none() => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return say(out, &reader.settle(start.elapsed(), true)).map_err(io_error);
            }
        }
        batch = None;
        say(out, &reader.settle(start.elapsed(), false)).map_err(io_error)?;
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let mut out = std::io::stdout().lock();
    match (&args.file, &args.timing) {
        (Some(file), timing) => {
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let timing = match timing {
                Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
                None => String::new(),
            };
            replay_timed(&args, &bytes, &timing, &mut out)
        }
        (None, Some(_)) => Err("--timing needs the typescript FILE".into()),
        (None, None) => live(&args, &mut out),
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) if message.is_empty() => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vt-a11y-reader: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn reader(rows: u16, cols: u16) -> Reader {
        Reader::new(Size::new(rows, cols).unwrap()).unwrap()
    }

    #[test]
    fn every_line_is_told_once_however_it_scrolls() {
        let mut r = reader(3, 20);
        r.feed(b"one\r\ntwo\r\n");
        assert_eq!(r.settle(Duration::ZERO, false), ["one", "two"]);
        // Scrolling `one` and `two` off tells them no more; the rest once each.
        r.feed(b"three\r\nfour\r\nfive\r\nsix");
        assert_eq!(r.settle(Duration::from_secs(1), false), ["three", "four", "five", "six"]);
        assert!(r.settle(Duration::from_secs(2), true).is_empty());
    }

    #[test]
    fn redraws_say_nothing_but_bells_titles_and_edits_do() {
        let mut r = reader(4, 30);
        r.feed(b"\x1b]2;editor\x07status: ok\r\n$ ");
        assert_eq!(r.settle(Duration::ZERO, false), ["title: editor", "status: ok", "$"]);
        // The whole screen cleared and painted again as it was.
        r.feed(b"\x1b[H\x1b[2Jstatus: ok\r\n$ \x1b]2;editor\x07");
        assert!(r.settle(Duration::from_secs(1), false).is_empty());
        // Typing at the prompt: the cursor line's new text, and a bell.
        r.feed(b"ls -l\x07");
        assert_eq!(r.settle(Duration::from_secs(2), false), ["typed: ls -l", "bell"]);
    }

    #[test]
    fn a_line_churning_in_place_is_told_at_most_every_two_seconds() {
        let mut r = reader(2, 30);
        r.feed(b"copying 10%");
        assert_eq!(r.settle(Duration::ZERO, false), ["copying 10%"]);
        r.feed(b"\rcopying 20%");
        assert_eq!(r.settle(Duration::from_millis(100), false), ["copying 20%"]);
        r.feed(b"\rcopying 30%");
        assert!(r.settle(Duration::from_millis(200), false).is_empty());
        r.feed(b"\rcopying 90%");
        assert!(r.settle(Duration::from_millis(300), false).is_empty());
        // The last state is told at the end, quiet or not.
        assert_eq!(r.settle(Duration::from_millis(400), true), ["copying 90%"]);
    }
}
