//! vt-cast-player: replays an asciinema v2 `.cast` file through fux-vt at
//! the cast's size and prints the screen as text: the final screen, or the
//! screen as it was at `--at SECONDS`.
//!
//! Usage: vt-cast-player [--at SECONDS] FILE   (FILE `-` reads stdin)
mod json;

use fux_vt::{Options, Parser, Size};
use json::Json;
use std::io::{Read, Write};
use std::process::ExitCode;

const USAGE: &str = "usage: vt-cast-player [--at SECONDS] FILE";

/// A cast's header and events, as far as playing it needs.
#[derive(Debug, PartialEq)]
enum Event {
    Output(String),
    Resize(Size),
}

struct Cast {
    size: Size,
    events: Vec<(f64, Event)>,
}

fn size_of(cols: f64, rows: f64) -> Option<Size> {
    let fit = |n: f64| (n.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(&n)).then_some(n as u16);
    Size::new(fit(rows)?, fit(cols)?).ok()
}

/// Reads a cast: a header line, then an event a line. Events other than
/// output and resizes (input, markers) are skipped.
fn read_cast(text: &str) -> Result<Cast, String> {
    let mut lines = text.lines().enumerate().filter(|(_, line)| !line.trim().is_empty());
    let (_, header) = lines.next().ok_or("empty file: no header")?;
    let header = json::parse(header).map_err(|e| format!("line 1: header: {e}"))?;
    match header.get("version").and_then(Json::as_f64) {
        Some(2.0) => {}
        Some(other) => return Err(format!("asciicast version {other}: only version 2 is played")),
        None => return Err(String::from("line 1: not an asciicast header")),
    }
    let number = |key| header.get(key).and_then(Json::as_f64);
    let size = number("width")
        .zip(number("height"))
        .and_then(|(cols, rows)| size_of(cols, rows))
        .ok_or("line 1: no valid width and height")?;
    let mut events = Vec::new();
    for (index, line) in lines {
        let at = |e: String| format!("line {}: {e}", index.saturating_add(1));
        let Json::Array(items) = json::parse(line).map_err(at)? else {
            return Err(at(String::from("an event is an array")));
        };
        let (Some(time), Some(kind), Some(data)) = (
            items.first().and_then(Json::as_f64),
            items.get(1).and_then(Json::as_str),
            items.get(2).and_then(Json::as_str),
        ) else {
            return Err(at(String::from("an event is [time, code, data]")));
        };
        let event = match kind {
            "o" => Event::Output(data.to_owned()),
            "r" => {
                let resized = data
                    .split_once('x')
                    .and_then(|(c, r)| size_of(c.trim().parse().ok()?, r.trim().parse().ok()?))
                    .ok_or_else(|| at(format!("bad resize {data:?}")))?;
                Event::Resize(resized)
            }
            _ => continue,
        };
        events.push((time, event));
    }
    Ok(Cast { size, events })
}

/// Plays `cast` up to and including `until` seconds (all of it if `None`).
fn play(cast: &Cast, until: Option<f64>) -> Result<Parser, fux_vt::Error> {
    let mut parser = Parser::with_options(cast.size, 0, Options::default())?;
    for (time, event) in &cast.events {
        if until.is_some_and(|until| *time > until) {
            break;
        }
        match event {
            // No program listens, so replies go nowhere; a refusal of room
            // loses only what it was for: the screen goes on.
            Event::Output(text) => {
                let _ = parser.process_all(text.as_bytes(), &mut ());
            }
            Event::Resize(size) => parser.resize(*size)?,
        }
    }
    Ok(parser)
}

/// The screen as text: a line a row, trailing blanks and blank rows cut.
fn screen_text(parser: &Parser) -> String {
    let window = parser.screen().window();
    let mut lines: Vec<String> = window
        .iter()
        .map(|(_, row)| row.text().trim_end_matches(' ').to_owned())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn run(args: &[String]) -> Result<String, String> {
    let (mut file, mut until) = (None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--at" => {
                let seconds = args.next().ok_or("--at needs SECONDS")?;
                let seconds: f64 = seconds.parse().map_err(|_| format!("--at {seconds}: not a number"))?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err(format!("--at {seconds}: not a time"));
                }
                until = Some(seconds);
            }
            path if file.is_none() && (path == "-" || !path.starts_with('-')) => file = Some(path),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let path = file.ok_or("no cast file given")?;
    let text = if path == "-" {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).map_err(|e| format!("stdin: {e}"))?;
        text
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?
    };
    let cast = read_cast(&text).map_err(|e| format!("{path}: {e}"))?;
    let parser = play(&cast, until).map_err(|e| format!("{path}: {e}"))?;
    Ok(screen_text(&parser))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(screen) => {
            let mut stdout = std::io::stdout();
            match writeln!(stdout, "{screen}") {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Err(error) => {
            eprintln!("vt-cast-player: {error}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAST: &str = r#"{"version": 2, "width": 10, "height": 3, "timestamp": 0}
[0.1, "o", "hello\r\nworld"]
[0.5, "i", "ignored"]
[1.0, "o", "\u001b[2J\u001b[Hcafé 漢字 | \u001b[31mred"]
[2.0, "r", "6x2"]
[2.5, "o", "\r\nabcdefghij"]
"#;

    #[test]
    fn plays_to_a_time_and_to_the_end() -> Result<(), String> {
        let cast = read_cast(CAST)?;
        let at = |t| play(&cast, t).map(|p| screen_text(&p)).map_err(|e| e.to_string());
        assert_eq!(at(Some(0.0))?, "");
        assert_eq!(at(Some(0.9))?, "hello\nworld");
        // The wide glyphs take two columns each, and the line ends at the
        // tenth: `|` and what follows are cut by wrapping onto row 2.
        assert_eq!(at(Some(1.5))?, "café 漢字\n| red");
        // Six columns, two rows, then a line feed scrolls and text wraps.
        assert_eq!(at(None)?, "abcdef\nghij");
        Ok(())
    }

    #[test]
    fn refuses_what_it_cannot_play() {
        assert!(read_cast("").is_err());
        assert!(read_cast(r#"{"version": 1, "width": 80, "height": 24}"#).is_err());
        assert!(read_cast(r#"{"version": 2, "width": 0, "height": 24}"#).is_err());
        assert!(read_cast("{\"version\": 2, \"width\": 8, \"height\": 2}\n[1, \"o\"]").is_err());
    }
}
