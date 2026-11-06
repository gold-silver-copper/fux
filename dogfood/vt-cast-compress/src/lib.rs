//! Minimizes an asciinema cast (v2 or v3): replays it through fux-vt and
//! keeps an output event only where the screen a viewer sees changes. The
//! bytes of an event that changes nothing visible move on to the next one
//! that does (so no hidden state is lost), output left over at the end that
//! never shows is dropped, and with a merge window, frames closer together
//! than it become one. [`verify`] replays both casts and compares the
//! screens at every sampled time.
pub mod json;

use fux_vt::{Attributes, LineRendition, Mode, Parser, Size};

/// One event: when (microseconds from the start), its code (`o`, `i`, `m`,
/// `r`, `x`), and its data.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub time: u64,
    pub code: String,
    pub data: String,
}

#[derive(Clone, Debug)]
pub struct Cast {
    /// The header line, written back as it was.
    pub header: String,
    /// 2: times from the start; 3: times since the event before.
    pub version: u8,
    pub size: Size,
    pub events: Vec<Event>,
}

/// "12.5" or "0.000301" as microseconds; more digits are cut.
fn micros(raw: &str) -> Option<u64> {
    let (whole, frac) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut micros: u64 = 0;
    for i in 0..6 {
        let digit = frac.as_bytes().get(i).map_or(0, |b| u64::from(b - b'0'));
        micros = micros * 10 + digit;
    }
    whole.parse::<u64>().ok()?.checked_mul(1_000_000)?.checked_add(micros)
}

fn seconds(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// "80x24" as a size.
fn resize_of(data: &str) -> Option<Size> {
    let (cols, rows) = data.split_once('x')?;
    Size::new(rows.trim().parse().ok()?, cols.trim().parse().ok()?).ok()
}

impl Cast {
    pub fn parse(text: &str) -> Result<Cast, String> {
        let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let (_, header) = lines.next().ok_or("an empty file")?;
        let head = json::parse(header).map_err(|e| format!("line 1: {e}"))?;
        let version = match head.get("version").and_then(json::Value::as_u64) {
            Some(2) => 2,
            Some(3) => 3,
            other => return Err(format!("line 1: cast version {other:?}, not 2 or 3")),
        };
        let term = if version == 3 { head.get("term") } else { Some(&head) };
        let dimension = |v2: &str, v3: &str| {
            let key = if version == 3 { v3 } else { v2 };
            term.and_then(|t| t.get(key))
                .and_then(json::Value::as_u64)
                .and_then(|n| u16::try_from(n).ok())
                .ok_or(format!("line 1: no {key}"))
        };
        let (cols, rows) = (dimension("width", "cols")?, dimension("height", "rows")?);
        let size = Size::new(rows, cols).map_err(|e| format!("line 1: {e:?}"))?;
        let mut events = Vec::new();
        let mut clock: u64 = 0;
        for (index, line) in lines {
            // v3 has comment lines.
            if line.starts_with('#') {
                continue;
            }
            let at = |e: &str| format!("line {}: {e}", index + 1);
            let json::Value::Array(fields) = json::parse(line).map_err(|e| at(&e))? else {
                return Err(at("an event is not an array"));
            };
            let [json::Value::Number(time), json::Value::Str(code), json::Value::Str(data)] =
                fields.as_slice()
            else {
                return Err(at("an event is not [time, code, data]"));
            };
            let time = micros(time).ok_or_else(|| at("a bad time"))?;
            clock = if version == 3 { clock.saturating_add(time) } else { time };
            events.push(Event {
                time: clock,
                code: code.clone(),
                data: data.clone(),
            });
        }
        Ok(Cast {
            header: header.to_owned(),
            version,
            size,
            events,
        })
    }

    pub fn write(&self) -> String {
        let mut out = String::with_capacity(self.events.len() * 32);
        out.push_str(&self.header);
        out.push('\n');
        let mut last = 0;
        for event in &self.events {
            let time = if self.version == 3 {
                event.time.saturating_sub(last)
            } else {
                event.time
            };
            last = event.time;
            out.push('[');
            out.push_str(&seconds(time));
            out.push_str(", ");
            json::write_string(&mut out, &event.code);
            out.push_str(", ");
            json::write_string(&mut out, &event.data);
            out.push_str("]\n");
        }
        out
    }
}

/// What a viewer sees: each cell's text and look, each row's width, the
/// cursor if shown, and reverse video. Links, the title, modes a viewer
/// can't see and history are not part of it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Shot {
    text: String,
    looks: Vec<(Attributes, bool)>,
    renditions: Vec<LineRendition>,
    cursor: Option<(u16, u16)>,
    reverse: bool,
}

impl Shot {
    pub fn of(parser: &Parser) -> Shot {
        let screen = parser.screen();
        let mut shot = Shot {
            cursor: screen.mode(Mode::ShowCursor).then(|| screen.cursor_position()),
            reverse: screen.mode(Mode::ReverseVideo),
            ..Shot::default()
        };
        for (_, row) in screen.window().iter() {
            shot.renditions.push(row.rendition());
            for cell in row.cells() {
                // A blank and a space look alike.
                shot.text.push_str(cell.shown());
                shot.text.push('\0');
                shot.looks.push((cell.attributes(), cell.is_wide()));
            }
        }
        shot
    }

    /// The screen's rows as text, for a report.
    pub fn lines(&self) -> Vec<String> {
        let cols = self.looks.len() / self.renditions.len().max(1);
        let text = self.text.strip_suffix('\0').unwrap_or(&self.text);
        let cells: Vec<&str> = text.split('\0').collect();
        let mut lines: Vec<String> =
            cells.chunks(cols.max(1)).map(|row| row.concat().trim_end().to_owned()).collect();
        lines.push(format!("(cursor {:?}, reverse video {})", self.cursor, self.reverse));
        lines
    }
}

fn new_parser(size: Size) -> Result<Parser, String> {
    // No history: what scrolls off is not seen.
    Parser::with_options(size, 0, fux_vt::Options::default()).map_err(|e| format!("{e:?}"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub events_in: usize,
    pub events_out: usize,
    /// Output events that changed nothing visible: dropped, or moved into
    /// the next.
    pub invisible: usize,
    /// Visible frames joined into a later one by the merge window.
    pub merged: usize,
    pub bytes_in: usize,
    pub bytes_out: usize,
    /// Output that never showed and changed nothing ahead, left out.
    pub bytes_dropped: usize,
    /// Times fux-vt refused room (a picture, say) while replaying.
    pub refused: u64,
}

/// Applies one event to `parser`: output, or a resize.
fn apply(parser: &mut Parser, event: &Event) -> Result<u64, String> {
    match event.code.as_str() {
        "o" => Ok(parser.process_all(event.data.as_bytes(), &mut ())),
        "r" => {
            if let Some(size) = resize_of(&event.data) {
                parser.resize(size).map_err(|e| format!("resize to {}: {e:?}", event.data))?;
            }
            Ok(0)
        }
        _ => Ok(0),
    }
}

/// Whether `with` and `without` (one event apart) show the same screens
/// through the next `lookahead` events.
fn same_ahead(mut with: Parser, mut without: Parser, ahead: &[Event]) -> Result<bool, String> {
    for event in ahead {
        apply(&mut with, event)?;
        apply(&mut without, event)?;
        if event.code == "o" && Shot::of(&with) != Shot::of(&without) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The minimized cast, and what was done. An output event that changes
/// nothing visible is dropped when the next `lookahead` events show the
/// same screens without it, and otherwise moved into the next event that
/// shows something; [`minimize`] checks the result.
pub fn compress(cast: &Cast, merge_window: u64, lookahead: usize) -> Result<(Cast, Stats), String> {
    let mut parser = new_parser(cast.size)?;
    let mut shown = Shot::of(&parser);
    let mut stats = Stats {
        events_in: cast.events.len(),
        ..Stats::default()
    };
    let mut out: Vec<Event> = Vec::new();
    let mut pending = String::new();
    for (index, event) in cast.events.iter().enumerate() {
        match event.code.as_str() {
            "o" => {
                stats.bytes_in += event.data.len();
                let saved = parser.clone();
                let before = parser.screen().mark();
                stats.refused += apply(&mut parser, event)?;
                let (was, screen) = (saved.screen(), parser.screen());
                // The marks say quickly when nothing at all changed.
                let untouched = !screen.full_refresh_since(before)
                    && !screen.window().iter().any(|(_, row)| row.changed_since(before))
                    && screen.cursor_position() == was.cursor_position()
                    && screen.mode(Mode::ShowCursor) == was.mode(Mode::ShowCursor)
                    && screen.mode(Mode::ReverseVideo) == was.mode(Mode::ReverseVideo);
                if !untouched {
                    let now = Shot::of(&parser);
                    if now != shown {
                        shown = now;
                        pending.push_str(&event.data);
                        out.push(Event {
                            time: event.time,
                            code: "o".into(),
                            data: std::mem::take(&mut pending),
                        });
                        continue;
                    }
                }
                stats.invisible += 1;
                let ahead = cast.events.get(index + 1..).unwrap_or_default();
                let ahead = ahead.get(..lookahead.min(ahead.len())).unwrap_or_default();
                if lookahead > 0 && same_ahead(parser.clone(), saved.clone(), ahead)? {
                    stats.bytes_dropped += event.data.len();
                    parser = saved;
                } else {
                    pending.push_str(&event.data);
                }
            }
            "r" => {
                // Output held back must land before the size changes.
                if !pending.is_empty() {
                    out.push(Event {
                        time: event.time,
                        code: "o".into(),
                        data: std::mem::take(&mut pending),
                    });
                }
                apply(&mut parser, event)?;
                shown = Shot::of(&parser);
                out.push(event.clone());
            }
            _ => out.push(event.clone()),
        }
    }
    stats.bytes_dropped += pending.len();
    if merge_window > 0 {
        out = merge(out, merge_window, &mut stats);
    }
    stats.events_out = out.len();
    stats.bytes_out = out.iter().filter(|e| e.code == "o").map(|e| e.data.len()).sum();
    Ok((
        Cast {
            header: cast.header.clone(),
            version: cast.version,
            size: cast.size,
            events: out,
        },
        stats,
    ))
}

/// Compresses `cast` and checks it, dropping invisible output where a
/// look `lookahead` events ahead allows; if the check then fails (a
/// dropped byte mattered further on), compresses again keeping every
/// byte. The cast, what was done, how many screens were compared, and
/// whether it fell back.
pub fn minimize(
    cast: &Cast,
    merge_window: u64,
    lookahead: usize,
) -> Result<(Cast, Stats, usize, bool), String> {
    let (small, stats) = compress(cast, merge_window, lookahead)?;
    match verify(cast, &small, &samples(cast, &small, merge_window)) {
        Ok(n) => Ok((small, stats, n, false)),
        Err(_) if lookahead > 0 => {
            let (small, stats) = compress(cast, merge_window, 0)?;
            let n = verify(cast, &small, &samples(cast, &small, merge_window))?;
            Ok((small, stats, n, true))
        }
        Err(error) => Err(error),
    }
}

/// Joins each output event into the one before it while it comes less
/// than `window` after that one's first frame, unless a resize comes between.
fn merge(events: Vec<Event>, window: u64, stats: &mut Stats) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    // Where in `out` the last output event is, while one may still grow,
    // and when its first frame was shown.
    let mut open: Option<usize> = None;
    let mut start = 0;
    for event in events {
        match event.code.as_str() {
            "o" => {
                if let Some(last) = open.and_then(|i| out.get_mut(i))
                    && event.time.saturating_sub(start) < window
                {
                    last.data.push_str(&event.data);
                    last.time = event.time;
                    stats.merged += 1;
                    // Moved later: it must not pass an event now before it.
                    if let Some(i) = open.filter(|i| i + 1 < out.len()) {
                        let joined = out.remove(i);
                        out.push(joined);
                        open = Some(out.len() - 1);
                    }
                    continue;
                }
                start = event.time;
                out.push(event);
                open = Some(out.len() - 1);
            }
            "r" => {
                open = None;
                out.push(event);
            }
            _ => out.push(event),
        }
    }
    out
}

/// Where two casts first differ: the time, and both screens.
#[derive(Debug)]
pub struct Mismatch {
    pub time: u64,
    pub original: Shot,
    pub compressed: Shot,
}

/// Replays both casts and compares their screens at each of `samples`
/// (microseconds) and at the end: how many were compared.
pub fn verify(original: &Cast, compressed: &Cast, samples: &[u64]) -> Result<usize, String> {
    let mut times: Vec<u64> = samples.to_vec();
    times.sort_unstable();
    times.dedup();
    times.push(u64::MAX);
    let mut a = Replay::new(original)?;
    let mut b = Replay::new(compressed)?;
    for &time in &times {
        a.until(time)?;
        b.until(time)?;
        let (sa, sb) = (Shot::of(&a.parser), Shot::of(&b.parser));
        if sa != sb {
            let shown = |s: &Shot| s.lines().join("\n");
            return Err(format!(
                "screens differ at {}:\n--- original\n{}\n--- compressed\n{}",
                if time == u64::MAX { "the end".into() } else { seconds(time) },
                shown(&sa),
                shown(&sb)
            ));
        }
    }
    Ok(times.len())
}

struct Replay<'a> {
    parser: Parser,
    events: std::slice::Iter<'a, Event>,
    next: Option<&'a Event>,
}

impl<'a> Replay<'a> {
    fn new(cast: &'a Cast) -> Result<Self, String> {
        let mut events = cast.events.iter();
        let next = events.next();
        Ok(Replay {
            parser: new_parser(cast.size)?,
            events,
            next,
        })
    }

    /// Applies every event up to and including `time`.
    fn until(&mut self, time: u64) -> Result<(), String> {
        while let Some(event) = self.next.filter(|e| e.time <= time) {
            // Refusals were counted when compressing; a screen that
            // lost something shows as a mismatch.
            apply(&mut self.parser, event)?;
            self.next = self.events.next();
        }
        Ok(())
    }
}

/// The times to compare at: without a merge window every screen the
/// original showed must be shown at the same moment; with one, the screens
/// at each of the compressed cast's frames.
pub fn samples(original: &Cast, compressed: &Cast, merge_window: u64) -> Vec<u64> {
    let from = if merge_window == 0 { original } else { compressed };
    from.events.iter().map(|e| e.time).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cast(events: &[(u64, &str)]) -> Result<Cast, String> {
        Ok(Cast {
            header: r#"{"version": 2, "width": 20, "height": 3}"#.into(),
            version: 2,
            size: Size::new(3, 20).map_err(|e| format!("{e:?}"))?,
            events: events
                .iter()
                .map(|&(time, data)| Event {
                    time,
                    code: "o".into(),
                    data: data.into(),
                })
                .collect(),
        })
    }

    /// Invisible output (text rewritten as it was, a colour set before
    /// text, a cursor hidden and shown) moves into the next frame, or with
    /// a look ahead goes if nothing later needs it; output at the end that
    /// never shows goes; every original moment's screen is the same.
    #[test]
    fn invisible_frames_go_and_every_screen_stays() -> Result<(), String> {
        let original = cast(&[
            (100_000, "hello"),
            (200_000, "\rhello"),
            (300_000, "\x1b[31m"),
            (400_000, "\x1b[?25l\x1b[?25h"),
            (500_000, " red"),
            (600_000, "\x1b[0m\x1b[s\x1b[u"),
        ])?;
        let (small, stats) = compress(&original, 0, 0)?;
        assert_eq!(
            small.events,
            [
                Event { time: 100_000, code: "o".into(), data: "hello".into() },
                Event {
                    time: 500_000,
                    code: "o".into(),
                    data: "\rhello\x1b[31m\x1b[?25l\x1b[?25h red".into()
                },
            ]
        );
        assert_eq!((stats.invisible, stats.bytes_dropped), (4, 10));
        let checked = verify(&original, &small, &samples(&original, &small, 0))?;
        assert_eq!(checked, 7);
        // Looking ahead, output that changes nothing later goes too.
        let (small, stats, _, fell_back) = minimize(&original, 0, 16)?;
        let frames: Vec<(u64, &str)> =
            small.events.iter().map(|e| (e.time, e.data.as_str())).collect();
        assert_eq!(frames, [(100_000, "hello"), (500_000, "\x1b[31m red")]);
        assert_eq!((stats.bytes_dropped, fell_back), (28, false));
        // A saved cursor is restored past the look ahead: the check catches
        // the drop, and every byte is kept.
        let saved = cast(&[(0, "ab"), (1, "\x1b7"), (2, "\r\nx"), (3, "\x1b8Z")])?;
        let (small, stats, _, fell_back) = minimize(&saved, 0, 1)?;
        assert!(fell_back);
        assert_eq!((stats.bytes_dropped, small.events.len()), (0, 3));
        Ok(())
    }

    /// A merge window joins frames closer than it; the joined frame keeps
    /// the later time, and the screens at the compressed frames match.
    #[test]
    fn a_merge_window_joins_close_frames() -> Result<(), String> {
        let original = cast(&[(0, "a"), (10_000, "b"), (20_000, "c"), (900_000, "d")])?;
        let (small, stats) = compress(&original, 50_000, 0)?;
        let frames: Vec<(u64, &str)> =
            small.events.iter().map(|e| (e.time, e.data.as_str())).collect();
        assert_eq!(frames, [(20_000, "abc"), (900_000, "d")]);
        assert_eq!(stats.merged, 2);
        verify(&original, &small, &samples(&original, &small, 50_000))?;
        // Not every original moment: "a" alone was never shown.
        assert!(verify(&original, &small, &samples(&original, &small, 0)).is_err());
        Ok(())
    }

    /// v2 and v3 casts read and write back with the same events.
    #[test]
    fn casts_round_trip() -> Result<(), String> {
        let v2 = "{\"version\": 2, \"width\": 10, \"height\": 2}\n[0.5, \"o\", \"a\\u001b[1m\\\"\"]\n[1.25, \"r\", \"12x3\"]\n";
        let cast = Cast::parse(v2)?;
        assert_eq!(cast.events.get(1).map(|e| e.time), Some(1_250_000));
        assert_eq!(Cast::parse(&cast.write())?.events, cast.events);
        let v3 = "{\"version\": 3, \"term\": {\"cols\": 10, \"rows\": 2}}\n# c\n[0.5, \"o\", \"a\"]\n[0.25, \"o\", \"b\"]\n";
        let cast = Cast::parse(v3)?;
        assert_eq!(cast.events.get(1).map(|e| e.time), Some(750_000));
        assert!(cast.write().ends_with("[0.250000, \"o\", \"b\"]\n"));
        Ok(())
    }
}
