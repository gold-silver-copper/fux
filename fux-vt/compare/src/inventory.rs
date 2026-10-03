//! The inventory: every escape sequence the corpus's programs sent,
//! normalized (numbers that only position or colour become `n`), how often,
//! from which programs, and what fux-vt does with it.
//!
//! What fux-vt does is found by feeding each sequence to a fux-vt parser
//! set up as fux sets up a pane, as the recording plays: a sequence it
//! reports through `Sink::unhandled`, or answers through `Sink::reply`, is
//! seen doing so. What it consumes without a word is told from its source,
//! and named here: private modes outside `Screen::mode`, SGR parameters
//! `Screen::sgr` passes over or reads only in part, the OSC numbers
//! `Parser::dispatch_osc` drops, and every DCS, APC, PM and SOS string,
//! whose payload the parser never keeps. Those lists are fux-vt's as of
//! this harness, and must change with it.
use crate::corpus::{self, Recording};
use crate::escape;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// The private modes `Screen::mode` keeps (fux-vt `src/screen.rs`).
const MODES: &[u16] = &[
    1, 6, 7, 9, 25, 47, 1000, 1002, 1003, 1004, 1005, 1006, 1047, 1048, 1049, 2004, 2026, 2031,
    2048,
];

/// Sequences fux-vt reports as unhandled that fux answers itself, from
/// what it knows as the host, and how.
const HOST_ANSWERS: &[(&str, &str)] = &[
    (
        "CSI ? 996 n",
        "reported as unhandled; fux answers with its client terminal's scheme (src/outer.rs)",
    ),
    (
        "CSI 22;n t",
        "reported as unhandled; fux's pane pushes its title (src/pane.rs)",
    ),
    (
        "CSI 22;n;n t",
        "reported as unhandled; fux's pane pushes its title (src/pane.rs)",
    ),
    (
        "CSI 23;n t",
        "reported as unhandled; fux's pane pops its title (src/pane.rs)",
    ),
    (
        "CSI 23;n;n t",
        "reported as unhandled; fux's pane pops its title (src/pane.rs)",
    ),
];

/// What fux-vt does with a sequence.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Does {
    /// Reported through `Sink::unhandled`.
    Unhandled,
    /// Consumed with no state, reply or report.
    Ignored(&'static str),
    /// Read in part.
    Partly(&'static str),
    /// Answered.
    Answers(String),
    Implemented(&'static str),
}

impl Does {
    fn text(&self) -> String {
        match self {
            Does::Unhandled => "not implemented: reported as unhandled".into(),
            Does::Ignored(why) => format!("not implemented: {why}"),
            Does::Partly(why) => format!("partly: {why}"),
            Does::Answers(reply) => format!("answers (first with `{reply}`)"),
            Does::Implemented("") => "implemented".into(),
            Does::Implemented(what) => format!("implemented: {what}"),
        }
    }

    fn missing(&self) -> bool {
        matches!(self, Does::Unhandled | Does::Ignored(_) | Does::Partly(_))
    }
}

/// A sequence, cut from a recording.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    /// Text, controls, or a sequence cancelled by CAN or SUB.
    Other(&'a [u8]),
    /// `CSI`: its private marker and parameter bytes, intermediates, final.
    Csi {
        params: &'a [u8],
        intermediates: &'a [u8],
        action: u8,
        whole: &'a [u8],
    },
    Esc {
        intermediates: &'a [u8],
        action: u8,
        whole: &'a [u8],
    },
    /// OSC, DCS, APC, PM or SOS: which (`]`, `P`, `_`, `^`, `X`), and the
    /// string between the introducer and its end.
    String {
        kind: u8,
        body: &'a [u8],
        whole: &'a [u8],
    },
}

/// Cuts `bytes` into tokens as a VT500-style parser reads them: a string
/// ends at ST (or BEL, for OSC), or at ESC, which starts the next
/// sequence; CAN and SUB cancel. A sequence cut short at the end is left
/// as `Other`.
fn tokens(bytes: &[u8]) -> Vec<Token<'_>> {
    fn flush<'a>(out: &mut Vec<Token<'a>>, bytes: &'a [u8], from: usize, to: usize) {
        if let Some(text) = bytes.get(from..to).filter(|t| !t.is_empty()) {
            out.push(Token::Other(text));
        }
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut text_from = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b != 0x1b {
            i = i.saturating_add(1);
            continue;
        }
        let start = i;
        let Some(&kind) = bytes.get(i.saturating_add(1)) else {
            break;
        };
        match kind {
            b'[' => {
                let body = i.saturating_add(2);
                let mut j = body;
                let mut end = None;
                while let Some(&c) = bytes.get(j) {
                    match c {
                        0x40..=0x7e => {
                            end = Some(j);
                            break;
                        }
                        0x18 | 0x1a | 0x1b => break,
                        _ => j = j.saturating_add(1),
                    }
                }
                let Some(end) = end else {
                    i = j;
                    continue;
                };
                let inner = bytes.get(body..end).unwrap_or_default();
                let split = inner
                    .iter()
                    .position(|c| (0x20..=0x2f).contains(c))
                    .unwrap_or(inner.len());
                let (params, intermediates) = inner.split_at_checked(split).unwrap_or((inner, &[]));
                flush(&mut out, bytes, text_from, start);
                out.push(Token::Csi {
                    params,
                    intermediates,
                    action: bytes.get(end).copied().unwrap_or(0),
                    whole: bytes.get(start..=end).unwrap_or_default(),
                });
                i = end.saturating_add(1);
                text_from = i;
            }
            b']' | b'P' | b'_' | b'^' | b'X' => {
                let body = i.saturating_add(2);
                let mut j = body;
                let mut found = None;
                while let Some(&c) = bytes.get(j) {
                    match c {
                        0x07 if kind == b']' => {
                            found = Some((j, j.saturating_add(1)));
                            break;
                        }
                        0x18 | 0x1a => break,
                        0x1b => {
                            let after = if bytes.get(j.saturating_add(1)) == Some(&b'\\') {
                                j.saturating_add(2)
                            } else {
                                j
                            };
                            found = Some((j, after));
                            break;
                        }
                        _ => j = j.saturating_add(1),
                    }
                }
                let Some((end, after)) = found else {
                    i = j.max(body);
                    continue;
                };
                flush(&mut out, bytes, text_from, start);
                out.push(Token::String {
                    kind,
                    body: bytes.get(body..end).unwrap_or_default(),
                    whole: bytes.get(start..after).unwrap_or_default(),
                });
                i = after;
                text_from = i;
            }
            _ => {
                let mut j = i.saturating_add(1);
                while bytes.get(j).is_some_and(|c| (0x20..=0x2f).contains(c)) {
                    j = j.saturating_add(1);
                }
                match bytes.get(j) {
                    Some(&action @ 0x30..=0x7e) => {
                        flush(&mut out, bytes, text_from, start);
                        out.push(Token::Esc {
                            intermediates: bytes.get(i.saturating_add(1)..j).unwrap_or_default(),
                            action,
                            whole: bytes.get(start..=j).unwrap_or_default(),
                        });
                        i = j.saturating_add(1);
                        text_from = i;
                    }
                    _ => i = j,
                }
            }
        }
    }
    flush(&mut out, bytes, text_from, bytes.len());
    out
}

/// What a fux-vt pane's parser said while reading one sequence.
#[derive(Default)]
struct Heard {
    unhandled: bool,
    replies: Vec<u8>,
}

impl fux_vt::Sink for Heard {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
    fn unhandled(&mut self, _: fux_vt::Unhandled<'_>) {
        self.unhandled = true;
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Numbers in a parameter list as `n`, separators kept: `12;3:4` is
/// `n;n:n`, an empty parameter stays empty.
fn numbers_as_n(params: &str) -> String {
    let mut out = String::new();
    let mut digits = false;
    for c in params.chars() {
        if c.is_ascii_digit() {
            if !digits {
                out.push('n');
            }
            digits = true;
        } else {
            digits = false;
            out.push(c);
        }
    }
    out
}

/// The SGR parameters, one entry per attribute: a colour with its
/// semicolon parameters taken together.
fn sgr_groups(params: &str) -> Vec<String> {
    let list: Vec<&str> = if params.is_empty() {
        vec!["0"]
    } else {
        params.split(';').collect()
    };
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&p) = list.get(i) {
        i = i.saturating_add(1);
        let take = match p {
            "38" | "48" | "58" => match list.get(i).copied() {
                Some("5") => 2,
                Some("2") => 4,
                _ => 0,
            },
            _ => 0,
        };
        let mut group = vec![p];
        for _ in 0..take {
            if let Some(&q) = list.get(i) {
                group.push(q);
                i = i.saturating_add(1);
            }
        }
        out.push(group.join(";"));
    }
    out
}

/// One SGR attribute, normalized, and what fux-vt does with it
/// (`Screen::sgr`).
fn sgr(group: &str) -> (String, Does) {
    let code = group
        .split([':', ';'])
        .next()
        .unwrap_or_default()
        .parse::<u16>()
        .unwrap_or(0);
    let colon = group.contains(':');
    let sub = group.split(':').nth(1).and_then(|s| s.parse::<u16>().ok());
    let name = match code {
        30..=37 => "SGR 30-37".to_owned(),
        40..=47 => "SGR 40-47".to_owned(),
        90..=97 => "SGR 90-97".to_owned(),
        100..=107 => "SGR 100-107".to_owned(),
        38 | 48 | 58 => {
            let rest = group.get(2..).unwrap_or_default();
            format!("SGR {code}{}", numbers_as_n_keep_kind(rest))
        }
        4 if colon => format!("SGR 4:{}", sub.unwrap_or(0)),
        _ => format!("SGR {code}"),
    };
    let does = match code {
        0..=9 | 22..=25 | 27..=29 | 30..=39 | 40..=49 | 59 | 90..=97 | 100..=107 if !colon => {
            Does::Implemented("")
        }
        4 => match sub {
            Some(0 | 1) => Does::Implemented(""),
            Some(2..=5) => Does::Partly("underline on; the style is not kept"),
            _ => Does::Ignored("passed over"),
        },
        21 => Does::Partly("a double underline, kept as a single one"),
        38 | 48 | 58 => Does::Implemented(""),
        _ => Does::Ignored("passed over without a report"),
    };
    (name, does)
}

/// `;5;n` or `;2;n;n;n` (or the colon forms), the kind kept and the
/// numbers as `n`.
fn numbers_as_n_keep_kind(rest: &str) -> String {
    let mut chars = rest.chars();
    let sep = chars.next();
    let kind: String = chars.clone().take_while(char::is_ascii_digit).collect();
    let tail: String = chars.skip(kind.len()).collect();
    match sep {
        Some(sep) => format!("{sep}{kind}{}", numbers_as_n(&tail)),
        None => String::new(),
    }
}

/// The capability names in an XTGETTCAP request: hex, joined by `;`.
fn capabilities(body: &[u8]) -> String {
    text(body)
        .split(';')
        .map(|hex| {
            let bytes: Vec<u8> = (0..hex.len().div_ceil(2))
                .map(|i| i.saturating_mul(2))
                .filter_map(|i| hex.get(i..i.saturating_add(2)))
                .filter_map(|h| u8::from_str_radix(h, 16).ok())
                .collect();
            text(&bytes)
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A name for a sequence, where it has a well-known one.
fn name(key: &str) -> String {
    let named: &[(&str, &str)] = &[
        ("CSI ? 1 h", "DECCKM, application cursor keys"),
        ("CSI ? 4 m", "XTQMODKEYS, modifyOtherKeys?"),
        ("CSI ? 1016 h", "mouse: SGR pixel encoding"),
        ("CSI ? 7727 h", "application escape key (mintty)"),
        ("CSI > 0 q", "XTVERSION"),
        ("CSI > 4;2 m", "XTMODKEYS: modifyOtherKeys 2"),
        ("CSI > 4; m", "XTMODKEYS: modifyOtherKeys reset"),
        ("CSI > 5 u", "kitty keyboard: push flags 5"),
        ("CSI < 1 u", "kitty keyboard: pop flags"),
        ("CSI ? 7 h", "DECAWM, autowrap"),
        ("CSI ? 12 h", "blinking cursor (att610)"),
        ("CSI ? 12 l", "steady cursor (att610)"),
        ("CSI ? 25 h", "DECTCEM, show the cursor"),
        ("CSI ? 25 l", "DECTCEM, hide the cursor"),
        ("CSI ? 1000 h", "mouse: press and release"),
        ("CSI ? 1002 h", "mouse: button motion"),
        ("CSI ? 1003 h", "mouse: any motion"),
        ("CSI ? 1004 h", "focus reporting"),
        ("CSI ? 1005 h", "mouse: UTF-8 encoding"),
        ("CSI ? 1006 h", "mouse: SGR encoding"),
        ("CSI ? 1015 h", "mouse: urxvt encoding"),
        ("CSI ? 1049 h", "alternate screen, cursor saved"),
        ("CSI ? 2004 h", "bracketed paste"),
        ("CSI ? 2026 h", "synchronized output: begin"),
        ("CSI ? 2026 l", "synchronized output: end"),
        ("CSI ? 2026 $ p", "DECRQM: synchronized output?"),
        ("CSI ? 2027 $ p", "DECRQM: grapheme clusters?"),
        ("CSI ? 2031 h", "colour-scheme change reports"),
        ("CSI ? 2048 h", "in-band resize reports"),
        ("CSI ? 996 n", "colour-scheme query"),
        ("CSI ? u", "kitty keyboard: query flags"),
        ("CSI > u", "kitty keyboard: push flags"),
        ("CSI < u", "kitty keyboard: pop flags"),
        ("CSI = u", "kitty keyboard: set flags"),
        ("CSI c", "DA1, primary device attributes"),
        ("CSI > c", "DA2, secondary device attributes"),
        ("CSI > q", "XTVERSION"),
        ("CSI 6 n", "DSR, cursor position report"),
        ("CSI ? 6 n", "DECXCPR, cursor position report"),
        ("CSI 5 n", "DSR, status"),
        ("CSI > 4 m", "XTMODKEYS: modifyOtherKeys reset"),
        ("CSI 22;n;n t", "XTWINOPS: push title"),
        ("CSI 23;n;n t", "XTWINOPS: pop title"),
        ("CSI 22;n t", "XTWINOPS: push title"),
        ("CSI 23;n t", "XTWINOPS: pop title"),
        ("CSI 14 t", "XTWINOPS: window size in pixels?"),
        ("CSI 16 t", "XTWINOPS: cell size in pixels?"),
        ("CSI 18 t", "XTWINOPS: size in characters?"),
        ("CSI n SP q", "DECSCUSR, cursor style"),
        ("CSI 3 J", "ED 3, erase saved lines"),
        ("CSI n;n r", "DECSTBM, scrolling region"),
        ("CSI n;n H", "CUP"),
        ("CSI H", "CUP, home"),
        ("CSI K", "EL"),
        ("CSI n K", "EL"),
        ("CSI J", "ED"),
        ("CSI n J", "ED"),
        ("CSI n X", "ECH"),
        ("CSI n @", "ICH"),
        ("CSI n P", "DCH"),
        ("CSI n L", "IL"),
        ("CSI n M", "DL"),
        ("CSI n S", "SU"),
        ("CSI n T", "SD"),
        ("CSI n b", "REP"),
        ("OSC 0", "title and icon name"),
        ("OSC 1", "icon name"),
        ("OSC 2", "title"),
        ("OSC 7", "current directory"),
        ("OSC 8 (open)", "hyperlink: start"),
        ("OSC 8 (close)", "hyperlink: end"),
        ("OSC 10 ?", "foreground colour query"),
        ("OSC 11 ?", "background colour query"),
        ("OSC 12 ?", "cursor colour query"),
        ("OSC 52", "clipboard"),
        ("OSC 104", "reset palette"),
        ("OSC 110", "reset foreground"),
        ("OSC 111", "reset background"),
        ("OSC 112", "reset cursor colour"),
        ("OSC 133", "semantic prompt"),
        ("OSC 9;4", "progress"),
        ("DCS $ q", "DECRQSS"),
        ("DCS + q", "XTGETTCAP"),
        ("APC G", "kitty graphics"),
        ("ESC 7", "DECSC"),
        ("ESC 8", "DECRC"),
        ("ESC =", "DECKPAM"),
        ("ESC >", "DECKPNM"),
        ("ESC M", "RI"),
        ("ESC ( B", "G0: ASCII"),
        ("ESC ( 0", "G0: DEC special graphics"),
        ("ESC c", "RIS"),
    ];
    let base = key.split(" (").next().unwrap_or(key);
    let found = |key: &str| {
        named
            .iter()
            .find(|(k, _)| *k == key || *k == base || key.starts_with(&format!("{k} ")))
            .map(|(_, n)| *n)
    };
    // A mode's reset is named after its set.
    let set = key
        .strip_suffix(" l")
        .filter(|_| key.starts_with("CSI ?"))
        .map(|k| format!("{k} h"));
    match (found(key), set) {
        (Some(n), _) => n.to_owned(),
        (None, Some(set)) => found(&set).map_or(String::new(), |n| format!("reset: {n}")),
        (None, None) => String::new(),
    }
}

/// The rows a CSI gives: its normalized form or forms (one for each mode
/// it sets, one for each SGR attribute), and what fux-vt does with each.
fn csi(params: &[u8], intermediates: &[u8], action: u8, heard: &Heard) -> Vec<(String, Does)> {
    let params = text(params);
    let inter = text(intermediates);
    let fin = char::from(action);
    let (private, rest) = match params.chars().next() {
        Some(p @ ('?' | '>' | '<' | '=')) => (Some(p), params.get(1..).unwrap_or_default()),
        _ => (None, params.as_str()),
    };
    let lead = private.map_or(String::new(), |p| format!("{p} "));
    let tail = if inter.is_empty() {
        format!("{fin}")
    } else {
        format!("{} {fin}", inter.replace(' ', "SP"))
    };
    let answered = || Does::Answers(escape::escape(&heard.replies));
    if heard.unhandled {
        let shown = match (private, fin) {
            _ if !inter.is_empty() => rest.to_owned(),
            (_, 'h' | 'l' | 'n' | 'c' | 'q' | 'p' | 'u') | (Some(_), _) => rest.to_owned(),
            (None, 't') => keep_first(rest),
            _ => numbers_as_n(rest),
        };
        let key = format!("CSI {lead}{}{tail}", spaced(&shown));
        let does = HOST_ANSWERS
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(Does::Unhandled, |(_, how)| Does::Implemented(how));
        return vec![(key, does)];
    }
    if !heard.replies.is_empty() {
        return vec![(format!("CSI {lead}{}{tail}", spaced(rest)), answered())];
    }
    match (private, fin, inter.as_str()) {
        (Some('?'), 'h' | 'l', "") => rest
            .split(';')
            .map(|m| {
                let n = m.parse::<u16>().unwrap_or(0);
                let does = if MODES.contains(&n) {
                    Does::Implemented("")
                } else {
                    Does::Ignored("a mode fux-vt does not keep, consumed quietly")
                };
                (format!("CSI ? {m} {fin}"), does)
            })
            .collect(),
        (None, 'm', "") => sgr_groups(rest).iter().map(|g| sgr(g)).collect(),
        (None, 'J' | 'K' | 'g', "") => {
            vec![(format!("CSI {}{tail}", spaced(rest)), Does::Implemented(""))]
        }
        (None, 'q', " ") => vec![(
            format!("CSI {}{tail}", spaced(&numbers_as_n(rest))),
            Does::Implemented("the cursor style is kept"),
        )],
        // The kitty keyboard protocol and modifyOtherKeys, their numbers kept.
        (Some('<' | '>' | '='), 'u', "") | (Some('>'), 'm', "") => vec![(
            format!("CSI {lead}{}{tail}", spaced(rest)),
            Does::Implemented("tracked; fux encodes keys as it asks (src/encode.rs)"),
        )],
        _ => vec![(
            format!("CSI {lead}{}{tail}", spaced(&numbers_as_n(rest))),
            Does::Implemented(""),
        )],
    }
}

/// Parameters followed by a space, if any.
fn spaced(params: &str) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!("{params} ")
    }
}

/// The first parameter as it is, the rest as `n`.
fn keep_first(params: &str) -> String {
    match params.split_once(';') {
        Some((first, rest)) => format!("{first};{}", numbers_as_n(rest)),
        None => params.to_owned(),
    }
}

/// The row an OSC gives (`Parser::dispatch_osc`: 0, 1, 2 and 52 are
/// events, 8 a hyperlink, 133's A and L prompt marks, the rest dropped).
fn osc(body: &[u8]) -> (String, Does) {
    let body = text(body);
    let (number, rest) = body.split_once(';').unwrap_or((body.as_str(), ""));
    let key = match number {
        "8" => {
            let uri = rest.split_once(';').map_or("", |(_, uri)| uri);
            if uri.is_empty() {
                "OSC 8 (close)".to_owned()
            } else {
                "OSC 8 (open)".to_owned()
            }
        }
        "4" if rest.ends_with('?') => "OSC 4 ?".to_owned(),
        "10" | "11" | "12" | "17" | "19" if rest == "?" => format!("OSC {number} ?"),
        "52" if rest.ends_with('?') => "OSC 52 ?".to_owned(),
        "133" => format!("OSC 133 ; {}", rest.chars().next().unwrap_or(' ')),
        "9" if rest.starts_with("4;") => "OSC 9;4".to_owned(),
        "1337" => format!("OSC 1337 ; {}", rest.split('=').next().unwrap_or_default()),
        _ => format!("OSC {number}"),
    };
    let does = match number {
        "0" | "1" | "2" => Does::Implemented("an event; fux sets the pane title"),
        "52" if rest.ends_with('?') => Does::Ignored("a query, dropped unanswered"),
        "10" | "11" if rest == "?" => Does::Implemented(
            "a ColorQuery event; fux answers with its client terminal's colour (src/outer.rs)",
        ),
        "52" => Does::Implemented("a clipboard event"),
        "8" if key == "OSC 8 (close)" => Does::Implemented("ends the open hyperlink"),
        "8" => Does::Implemented("opens a hyperlink, which the cells printed keep (Row::link)"),
        "133" if rest.starts_with('A') => {
            Does::Implemented("a fresh line, and the row marked (Row::starts_prompt)")
        }
        "133" if rest.starts_with('L') => Does::Implemented("a fresh line"),
        "133" => Does::Ignored("consumed: only A and L are kept (Parser::dispatch_osc)"),
        _ if rest.ends_with('?') => Does::Ignored("a query, dropped unanswered"),
        _ => Does::Ignored("dropped (Parser::dispatch_osc)"),
    };
    (key, does)
}

/// The row a DCS, APC, PM or SOS string gives: all are consumed and
/// dropped.
fn string(kind: u8, body: &[u8]) -> (String, Does) {
    let key = match kind {
        b'P' => {
            let len = body
                .iter()
                .position(|c| (0x40..=0x7e).contains(c))
                .unwrap_or(body.len());
            let header = text(body.get(..=len).unwrap_or(body));
            let payload = body.get(len.saturating_add(1)..).unwrap_or_default();
            let header = numbers_as_n(&header);
            match header.as_str() {
                "+q" => format!("DCS + q ({})", capabilities(payload)),
                "$q" => format!("DCS $ q {}", text(payload)),
                _ => format!(
                    "DCS {}",
                    header
                        .chars()
                        .map(String::from)
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            }
        }
        b'_' => format!("APC {}", text(body.get(..1).unwrap_or_default())),
        b'^' => "PM".to_owned(),
        _ => "SOS".to_owned(),
    };
    let does = if kind == b'P' && (key.starts_with("DCS + q") || key.starts_with("DCS $ q")) {
        Does::Ignored("a query, consumed unanswered")
    } else {
        Does::Ignored("consumed, dropped")
    };
    (key, does)
}

/// A feature asked after, and how to know its rows.
type Watched = (&'static str, fn(&str) -> bool);

/// What the work on modern features asks after, each with how to know
/// its rows: who sends these, and who sends none.
const WATCHED: &[Watched] = &[
    (
        "synchronized output (CSI ? 2026 h/l, and its DECRQM)",
        |k| k.starts_with("CSI ? 2026 "),
    ),
    ("hyperlinks (OSC 8)", |k| k.starts_with("OSC 8 ")),
    ("underline styles (SGR 4:n, 21)", |k| {
        k.starts_with("SGR 4:") || k == "SGR 21"
    }),
    ("underline colour (SGR 58, 59)", |k| {
        k.starts_with("SGR 58") || k == "SGR 59"
    }),
    ("semantic prompts (OSC 133)", |k| k.starts_with("OSC 133")),
    ("in-band resize (CSI ? 2048 h/l)", |k| {
        k.starts_with("CSI ? 2048 ")
    }),
    ("XTGETTCAP (DCS + q)", |k| k.starts_with("DCS + q")),
    ("DECRQSS (DCS $ q)", |k| k.starts_with("DCS $ q")),
    ("DECRQM (CSI ? n $ p, CSI n $ p)", |k| k.ends_with("$ p")),
    (
        "kitty keyboard (CSI ? u, CSI > n u, CSI < u, CSI = n u)",
        |k| {
            k.starts_with("CSI ")
                && k.ends_with(" u")
                && k.chars().nth(4).is_some_and(|c| "?<>=".contains(c))
        },
    ),
    ("modifyOtherKeys (CSI > 4 ; n m, CSI ? 4 m)", |k| {
        k.starts_with("CSI > 4") || k == "CSI ? 4 m"
    }),
    ("colour queries (OSC 4/10/11/12 ?)", |k| {
        k.starts_with("OSC ") && k.ends_with(" ?")
    }),
    ("XTVERSION, DA2 (CSI > q, CSI > c)", |k| {
        k.starts_with("CSI > ") && (k.ends_with(" q") || k.ends_with(" c"))
    }),
    ("current directory (OSC 7)", |k| k == "OSC 7"),
    ("clipboard (OSC 52)", |k| k.starts_with("OSC 52")),
    ("kitty graphics (APC G)", |k| k == "APC G"),
];

fn watched(out: &mut String, rows: &BTreeMap<String, Row>) {
    let _ = writeln!(out, "| Feature | Sent as | Programs |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for (feature, matches) in WATCHED {
        let seen: Vec<(&String, &Row)> = rows.iter().filter(|(k, _)| matches(k)).collect();
        if seen.is_empty() {
            let _ = writeln!(out, "| {feature} | not sent by any | |");
            continue;
        }
        let forms: Vec<String> = seen
            .iter()
            .map(|(k, r)| format!("`{}` ×{}", k.replace('|', "\\|"), r.count))
            .collect();
        let programs: BTreeSet<&str> = seen
            .iter()
            .flat_map(|(_, r)| r.programs.iter().map(String::as_str))
            .collect();
        let _ = writeln!(
            out,
            "| {feature} | {} | {} |",
            forms.join(", "),
            programs.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
}

struct Row {
    count: usize,
    programs: BTreeSet<String>,
    does: Does,
}

/// Feeds each recording, sequence by sequence, to a fux-vt pane's parser,
/// and tallies what it sent.
fn tally(recordings: &[Recording]) -> Result<BTreeMap<String, Row>, String> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for r in recordings {
        // As fux's panes are set up.
        let options = fux::pane::OPTIONS;
        let mut parser = fux_vt::Parser::with_options(r.rows, r.cols, 10_000, options)
            .map_err(|e| format!("fux-vt: {e}"))?;
        let bytes = r.bytes();
        for token in tokens(&bytes) {
            let mut heard = Heard::default();
            let whole = match &token {
                Token::Other(b) => b,
                Token::Csi { whole, .. }
                | Token::Esc { whole, .. }
                | Token::String { whole, .. } => whole,
            };
            parser
                .process_with(whole, &mut heard)
                .map_err(|e| format!("fux-vt: {e}"))?;
            let found: Vec<(String, Does)> = match token {
                Token::Other(_) => Vec::new(),
                Token::Csi {
                    params,
                    intermediates,
                    action,
                    ..
                } => csi(params, intermediates, action, &heard),
                Token::Esc {
                    intermediates,
                    action,
                    ..
                } => {
                    let mut key = String::from("ESC");
                    for &c in intermediates {
                        key.push(' ');
                        key.push(char::from(c));
                    }
                    key.push(' ');
                    key.push(char::from(action));
                    let does = if heard.unhandled {
                        Does::Unhandled
                    } else {
                        Does::Implemented("")
                    };
                    vec![(key, does)]
                }
                Token::String {
                    kind: b']', body, ..
                } => vec![osc(body)],
                Token::String { kind, body, .. } => vec![string(kind, body)],
            };
            for (key, does) in found {
                let row = rows.entry(key).or_insert_with(|| Row {
                    count: 0,
                    programs: BTreeSet::new(),
                    does,
                });
                row.count = row.count.saturating_add(1);
                row.programs.insert(r.name.clone());
            }
        }
    }
    Ok(rows)
}

fn table(out: &mut String, rows: &[(&String, &Row)]) {
    let _ = writeln!(out, "| Sequence | What | Count | Programs | fux-vt |");
    let _ = writeln!(out, "| --- | --- | ---: | --- | --- |");
    for (key, row) in rows {
        let programs: Vec<&str> = row.programs.iter().map(String::as_str).collect();
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} |",
            key.replace('|', "\\|"),
            name(key),
            row.count,
            programs.join(", "),
            row.does.text().replace('|', "\\|")
        );
    }
}

/// The inventory of the recordings named (all if none), as Markdown.
pub fn run(names: &[String]) -> Result<bool, String> {
    let recordings = corpus::recordings(names)?;
    let rows = tally(&recordings)?;
    let mut sorted: Vec<(&String, &Row)> = rows.iter().collect();
    sorted.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
    let (missing, done): (Vec<_>, Vec<_>) = sorted.into_iter().partition(|(_, r)| r.does.missing());
    let mut out = String::new();
    let _ = writeln!(out, "# What the corpus's programs send\n");
    let _ = writeln!(
        out,
        "Made by `fux-vt-compare inventory` from the recordings in this directory \
         (see the harness README, \"The corpus\"). Each sequence is normalized: \
         numbers that only place the cursor or pick a colour are `n`, a mode or an \
         SGR attribute is a row of its own, and an XTGETTCAP request shows the \
         capabilities it asks for. \"Count\" counts every time it was sent, in all the \
         recordings. \"fux-vt\" is what a fux pane's parser does with it, as fux sets \
         it up (`fux::pane::OPTIONS`: events, DECRQM, in-band resize, the size query, \
         colour-scheme reports, the kitty keyboard protocol, hyperlinks, prompt marks and \
         fux's identity), with what fux itself answers.\n"
    );
    let _ = writeln!(out, "Recordings:\n");
    for r in &recordings {
        let _ = writeln!(
            out,
            "- `{}`: {}, {} ({} steps, {} bytes)",
            r.name,
            r.program,
            r.version,
            r.steps.len(),
            r.bytes().len()
        );
    }
    let _ = writeln!(out, "\n## Asked after\n");
    watched(&mut out, &rows);
    let _ = writeln!(out, "\n## Not implemented, or in part\n");
    table(&mut out, &missing);
    let _ = writeln!(out, "\n## Implemented\n");
    table(&mut out, &done);
    print!("{out}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{Token, tokens};

    #[test]
    fn sequences_are_cut_as_a_parser_reads_them() {
        let bytes = b"a\x1b[?25lb\x1b]8;;x\x1b\\c\x1b]2;t\x07\x1bPzz\x1b\\\x1b(B\x1b[1;2";
        let got = tokens(bytes);
        assert_eq!(got.first(), Some(&Token::Other(b"a")));
        assert!(matches!(
            got.get(1),
            Some(Token::Csi {
                params: b"?25",
                intermediates: b"",
                action: b'l',
                ..
            })
        ));
        assert!(matches!(
            got.get(3),
            Some(Token::String {
                kind: b']',
                body: b"8;;x",
                ..
            })
        ));
        assert!(matches!(
            got.get(5),
            Some(Token::String {
                kind: b']',
                body: b"2;t",
                ..
            })
        ));
        assert!(matches!(
            got.get(6),
            Some(Token::String {
                kind: b'P',
                body: b"zz",
                ..
            })
        ));
        assert!(matches!(
            got.get(7),
            Some(Token::Esc {
                intermediates: b"(",
                action: b'B',
                ..
            })
        ));
        assert_eq!(got.get(8), Some(&Token::Other(b"\x1b[1;2")));
    }

    #[test]
    fn sgr_attributes_are_rows_of_their_own() {
        assert_eq!(
            super::sgr_groups("1;38;5;196;48;2;1;2;3;4:3;58:2::1:2:3"),
            ["1", "38;5;196", "48;2;1;2;3", "4:3", "58:2::1:2:3"]
        );
        assert_eq!(super::sgr("38;5;196").0, "SGR 38;5;n");
        assert_eq!(super::sgr("58:2::1:2:3").0, "SGR 58:2::n:n:n");
        assert_eq!(super::sgr("4:3").0, "SGR 4:3");
        assert_eq!(super::capabilities(b"536d756c78;5463"), "Smulx,Tc");
    }
}
