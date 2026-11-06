//! Lists each control sequence in a byte stream with a plain-English
//! meaning, flags malformed and unknown ones, and says what the stream
//! leaves set at its end. The stream is split into sequences here, as
//! ECMA-48 frames them, and each is fed to a `fux_vt::Parser`, which says
//! what it does not implement and holds the terminal's state at the end.
mod explain;
mod scan;

use fux_vt::{Event, Feature, Mode, Options, Parser, Sink, Size, Unhandled};
pub use scan::{Token, tokens};

/// What is wrong with a sequence, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flag {
    /// Not a well-formed sequence: terminals may drop or misread it.
    Malformed(String),
    /// Well-formed, but with no known meaning.
    Unknown(String),
}

/// One sequence (or run of text) found.
#[derive(Clone, Debug)]
pub struct Finding {
    pub start: usize,
    pub end: usize,
    pub meaning: String,
    pub flag: Option<Flag>,
    pub text: bool,
}

/// All that was found, and what the stream leaves set.
#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub left: Vec<String>,
}

impl Report {
    pub fn count(&self, malformed: bool) -> usize {
        self.findings
            .iter()
            .filter(|f| match &f.flag {
                Some(Flag::Malformed(_)) => malformed,
                Some(Flag::Unknown(_)) => !malformed,
                None => false,
            })
            .count()
    }
}

/// What the parser told while one token was fed to it.
#[derive(Default)]
struct Heard {
    unhandled: bool,
    icon: Option<String>,
}

impl Sink for Heard {
    fn event(&mut self, event: Event<'_>) {
        if let Event::IconName(kept) = event {
            self.icon = Some(String::from_utf8_lossy(kept.bytes).into());
        }
    }
    fn unhandled(&mut self, _sequence: Unhandled<'_>) {
        self.unhandled = true;
    }
}

/// The features whose state is worth reporting: everything but pictures,
/// which need a host's cell size.
fn options() -> Options {
    Feature::ALL.into_iter().filter(|f| *f != Feature::Pictures).collect()
}

/// Lints `bytes` as written to a terminal of `size`.
pub fn lint(bytes: &[u8], size: Size) -> Result<Report, fux_vt::Error> {
    let mut parser = Parser::with_options(size, 0, options())?;
    let fresh = Parser::with_options(size, 0, options())?;
    let mut state = explain::State::default();
    let mut report = Report::default();
    let mut icon = None;
    for token in tokens(bytes) {
        let raw = bytes.get(token.start..token.end).unwrap_or_default();
        let mut heard = Heard::default();
        // Room refused is counted by the parser, and the rest read.
        let _ = parser.process_all(raw, &mut heard);
        icon = heard.icon.or(icon);
        let (meaning, mut flag) = match token.kind {
            scan::Kind::Text => text(raw),
            ref kind => explain::explain(kind, &mut state),
        };
        if heard.unhandled && flag.is_none() {
            flag = Some(Flag::Unknown("fux-vt does not implement it".into()));
        }
        report.findings.push(Finding {
            start: token.start,
            end: token.end,
            meaning,
            flag,
            text: matches!(token.kind, scan::Kind::Text),
        });
    }
    report.left = left(&parser, &fresh, &state, icon);
    Ok(report)
}

/// A run of printable bytes: flagged if it is not UTF-8, or holds C1
/// controls encoded as UTF-8, which UTF-8 terminals ignore.
fn text(raw: &[u8]) -> (String, Option<Flag>) {
    let mut at = 0;
    for chunk in raw.utf8_chunks() {
        if let Some(c) = chunk.valid().chars().find(|c| ('\u{80}'..='\u{9f}').contains(c)) {
            let why = format!("text holding C1 control U+{:04X} as UTF-8, which is ignored", u32::from(c));
            return (why.clone(), Some(Flag::Unknown(why)));
        }
        at += chunk.valid().len();
        if let Some(&byte) = chunk.invalid().first() {
            let why = match byte {
                0x80..=0x9f => format!("text with a raw 8-bit C1 byte 0x{byte:02x} at +{at}, not a control in UTF-8"),
                _ => format!("text with invalid UTF-8 (0x{byte:02x}) at +{at}"),
            };
            return (why.clone(), Some(Flag::Malformed(why)));
        }
    }
    (format!("text, {} bytes", raw.len()), None)
}

/// A setting as DECRQSS reports it (`m` the pen, `r` the scroll region,
/// `s` the left and right margins), asked of a copy of `parser`: fux-vt
/// keeps them private. CAN first ends a sequence the stream left open.
fn setting(parser: &Parser, request: &str) -> String {
    #[derive(Default)]
    struct Replies(Vec<u8>);
    impl Sink for Replies {
        fn reply(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }
    let mut copy = parser.clone();
    let mut replies = Replies::default();
    let _ = copy.process_all(format!("\x18\x1bP$q{request}\x1b\\").as_bytes(), &mut replies);
    let reply = String::from_utf8_lossy(&replies.0);
    let reply = reply.strip_prefix("\x1bP1$r").unwrap_or_default();
    reply.strip_suffix("\x1b\\").unwrap_or_default().to_owned()
}

/// What the stream leaves different from a fresh terminal.
fn left(
    parser: &Parser,
    fresh: &Parser,
    state: &explain::State,
    icon: Option<String>,
) -> Vec<String> {
    let (now, then) = (parser.screen(), fresh.screen());
    let mut left = Vec::new();
    for mode in Mode::ALL {
        if now.mode(mode) != then.mode(mode) {
            let (private, n) = mode.number();
            let set = if now.mode(mode) { "set" } else { "reset" };
            let mark = if private { "?" } else { "" };
            left.push(format!("mode {mark}{n} ({mode:?}) left {set}"));
        }
    }
    for (request, what) in [("m", "SGR"), ("r", "scroll region"), ("s", "left/right margins")] {
        let set = setting(parser, request);
        if set != setting(fresh, request) {
            let (params, last) = set.split_at(set.len().saturating_sub(1));
            left.push(format!("{what} left: CSI {params} {last}"));
        }
    }
    if now.cursor_shape() != then.cursor_shape() {
        left.push(format!("cursor shape left {:?}", now.cursor_shape()));
    }
    if now.kitty_keyboard_flags() != 0 {
        left.push(format!("kitty keyboard flags left {}", now.kitty_keyboard_flags()));
    }
    let palette: Vec<String> = (0..=255u8)
        .filter(|&i| now.palette_color(i).is_some())
        .map(|i| i.to_string())
        .collect();
    if !palette.is_empty() {
        left.push(format!("palette colours changed: {}", palette.join(",")));
    }
    let dynamic: Vec<String> = (10..=19u8)
        .filter(|&n| now.dynamic_color(n).is_some())
        .map(|n| n.to_string())
        .collect();
    if !dynamic.is_empty() {
        left.push(format!("dynamic colours (OSC 10-19) changed: {}", dynamic.join(",")));
    }
    let title = now.title().bytes;
    if !title.is_empty() {
        left.push(format!("window title {:?}", String::from_utf8_lossy(title)));
    }
    if let Some(icon) = icon {
        left.push(format!("icon name {icon:?}"));
    }
    left.extend(state.left());
    let (row, col) = now.cursor_position();
    left.push(format!("cursor at row {}, column {}", row + 1, col + 1));
    left
}
