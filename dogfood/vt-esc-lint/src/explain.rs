//! Plain-English meanings of controls and sequences.
use crate::Flag;
use crate::scan::{Ender, Introducer, Kind};
use fux_vt::Mode;

/// What the stream leaves set that fux-vt's screen does not say: the open
/// hyperlink and the character sets.
#[derive(Default)]
pub struct State {
    link: Option<String>,
    /// What G0 and G1 hold, if not ASCII.
    sets: [Option<&'static str>; 2],
    /// Whether SO shifted G1 in.
    shifted: bool,
}

impl State {
    pub fn left(&self) -> Vec<String> {
        let mut left = Vec::new();
        if let Some(uri) = &self.link {
            left.push(format!("hyperlink left open: {uri} (OSC 8 ; ; ST closes it)"));
        }
        for (g, set) in self.sets.iter().enumerate() {
            if let Some(set) = set {
                left.push(format!("G{g} left as {set}"));
            }
        }
        if self.shifted {
            left.push("SO left in force: G1 is printing (SI undoes it)".into());
        }
        left
    }
}

type Explained = (String, Option<Flag>);

fn ok(text: impl Into<String>) -> Explained {
    (text.into(), None)
}

fn unknown(text: impl Into<String>) -> Explained {
    let text = text.into();
    (text.clone(), Some(Flag::Unknown(text)))
}

fn malformed(text: impl Into<String>) -> Explained {
    let text = text.into();
    (text.clone(), Some(Flag::Malformed(text)))
}

pub fn explain(kind: &Kind, state: &mut State) -> Explained {
    match kind {
        Kind::Text => ok("text"),
        Kind::Control(byte) => control(*byte, state),
        Kind::Esc { intermediates, last } => escape(intermediates, *last, state),
        Kind::Csi { params, intermediates, last, controls, disordered } => {
            let (mut text, mut flag) = if *disordered {
                malformed("CSI with its bytes out of order: terminals ignore it")
            } else {
                csi(params, intermediates, *last)
            };
            if !controls.is_empty() && flag.is_none() {
                let what = format!(
                    "{text}; holds controls {controls:02x?}, which run in the middle of it"
                );
                (text, flag) = malformed(what);
            }
            (text, flag)
        }
        Kind::String { introducer, body, ender } => string(*introducer, body, *ender, state),
        Kind::Broken(why) => malformed(why.clone()),
    }
}

fn control(byte: u8, state: &mut State) -> Explained {
    match byte {
        0x00 => ok("NUL: ignored"),
        0x07 => ok("BEL: ring the bell"),
        0x08 => ok("BS: cursor left one column"),
        0x09 => ok("HT: cursor to the next tab stop"),
        0x0a => ok("LF: line feed (down one line, scrolling at the bottom)"),
        0x0b => ok("VT: vertical tab, a line feed"),
        0x0c => ok("FF: form feed, a line feed"),
        0x0d => ok("CR: cursor to the start of the line"),
        0x0e => {
            state.shifted = true;
            ok("SO: shift out, print with G1 (often line drawing)")
        }
        0x0f => {
            state.shifted = false;
            ok("SI: shift in, print with G0 again")
        }
        0x18 | 0x1a => malformed("CAN/SUB with no sequence to cancel"),
        0x7f => ok("DEL: ignored"),
        _ => unknown(format!("C0 control 0x{byte:02x}, which terminals ignore")),
    }
}

fn charset(last: u8) -> Option<&'static str> {
    Some(match last {
        b'B' => "ASCII",
        b'0' => "DEC Special Graphics (line drawing)",
        b'A' => "British",
        b'<' => "DEC Supplemental",
        b'>' => "DEC Technical",
        b'4' => "Dutch",
        b'5' | b'C' => "Finnish",
        b'R' | b'f' => "French",
        b'K' => "German",
        _ => return None,
    })
}

fn escape(intermediates: &[u8], last: u8, state: &mut State) -> Explained {
    let g = |i: u8| match i {
        b'(' => Some(0),
        b')' => Some(1),
        b'*' => Some(2),
        b'+' => Some(3),
        _ => None,
    };
    match (intermediates, last) {
        ([], b'7') => ok("DECSC: save cursor, pen and character sets"),
        ([], b'8') => ok("DECRC: restore what DECSC saved"),
        ([], b'c') => {
            *state = State::default();
            ok("RIS: full reset of the terminal")
        }
        ([], b'D') => ok("IND: index, down one line, scrolling at the bottom"),
        ([], b'E') => ok("NEL: next line"),
        ([], b'H') => ok("HTS: set a tab stop at the cursor"),
        ([], b'M') => ok("RI: reverse index, up one line, scrolling at the top"),
        ([], b'=') => ok("DECKPAM: keypad sends application sequences"),
        ([], b'>') => ok("DECKPNM: keypad sends numbers"),
        ([], b'N') => ok("SS2: next character from G2"),
        ([], b'O') => ok("SS3: next character from G3"),
        ([], b'n') => ok("LS2: G2 into GL"),
        ([], b'o') => ok("LS3: G3 into GL"),
        ([], b'Z') => ok("DECID: asks the terminal to identify itself"),
        ([], b'\\') => malformed("ST with no string to end"),
        ([b'#'], b'8') => ok("DECALN: fill the screen with E"),
        ([b'#'], b'3') => ok("DECDHL: line is double height, top half"),
        ([b'#'], b'4') => ok("DECDHL: line is double height, bottom half"),
        ([b'#'], b'5') => ok("DECSWL: line is single width"),
        ([b'#'], b'6') => ok("DECDWL: line is double width"),
        ([b' '], b'F') => ok("S7C1T: send 7-bit controls"),
        ([b' '], b'G') => ok("S8C1T: send 8-bit controls"),
        ([b'%'], b'G') => ok("select UTF-8"),
        ([b'%'], b'@') => ok("select the default character set (not UTF-8)"),
        ([i], last) if g(*i).is_some() => {
            let n = g(*i).unwrap_or(0);
            match charset(last) {
                Some(name) => {
                    if let Some(slot) = state.sets.get_mut(n) {
                        *slot = (last != b'B').then_some(name);
                    }
                    ok(format!("designate G{n} as {name}"))
                }
                None => unknown(format!("designate G{n} as unknown set {:?}", char::from(last))),
            }
        }
        _ => unknown(format!(
            "unknown escape sequence ESC {}{}",
            String::from_utf8_lossy(intermediates),
            char::from(last)
        )),
    }
}

/// A CSI's parameters: each with its colon subparameters, `None` empty.
fn parse(params: &[u8]) -> Result<Vec<Vec<Option<u32>>>, String> {
    if params.is_empty() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(params);
    text.split(';')
        .map(|p| {
            p.split(':')
                .map(|s| match s {
                    "" => Ok(None),
                    s => match s.parse::<u32>() {
                        Ok(n) if n <= 65535 => Ok(Some(n)),
                        _ => Err(format!("parameter {s:?} is past 65535 or no number")),
                    },
                })
                .collect()
        })
        .collect()
}

fn csi(raw: &[u8], intermediates: &[u8], last: u8) -> Explained {
    let (private, numbers) = match raw.first() {
        Some(&m @ b'<'..=b'?') => (Some(m), raw.get(1..).unwrap_or_default()),
        _ => (None, raw),
    };
    let params = match parse(numbers) {
        Ok(p) => p,
        Err(why) => return malformed(format!("CSI {}: {why}", char::from(last))),
    };
    let first = |i: usize, default: u32| -> u32 {
        match params.get(i).and_then(|p| p.first()).copied().flatten() {
            None | Some(0) => default,
            Some(n) => n,
        }
    };
    let n = first(0, 1);
    let ps = params.first().and_then(|p| p.first()).copied().flatten().unwrap_or(0);
    let p = private.map(char::from);
    let i = intermediates;
    match (p, i, last) {
        (None, [], b'A') => ok(format!("CUU: cursor up {n}")),
        (None, [], b'B') => ok(format!("CUD: cursor down {n}")),
        (None, [], b'C') => ok(format!("CUF: cursor right {n}")),
        (None, [], b'D') => ok(format!("CUB: cursor left {n}")),
        (None, [], b'E') => ok(format!("CNL: cursor down {n}, to the line's start")),
        (None, [], b'F') => ok(format!("CPL: cursor up {n}, to the line's start")),
        (None, [], b'G' | b'`') => ok(format!("CHA: cursor to column {n}")),
        (None, [], b'H' | b'f') => ok(format!("CUP: cursor to row {n}, column {}", first(1, 1))),
        (None, [], b'd') => ok(format!("VPA: cursor to row {n}")),
        (None, [], b'J') => ok(match ps {
            0 => "ED: erase from the cursor to the end of the screen",
            1 => "ED: erase from the start of the screen to the cursor",
            2 => "ED: erase the whole screen",
            3 => "ED: erase the scrollback",
            _ => return unknown(format!("ED {ps}: no such erase")),
        }),
        (None, [], b'K') => ok(match ps {
            0 => "EL: erase from the cursor to the end of the line",
            1 => "EL: erase from the start of the line to the cursor",
            2 => "EL: erase the whole line",
            _ => return unknown(format!("EL {ps}: no such erase")),
        }),
        (None, [], b'@') => ok(format!("ICH: insert {n} blank characters")),
        (None, [], b'P') => ok(format!("DCH: delete {n} characters")),
        (None, [], b'X') => ok(format!("ECH: erase {n} characters")),
        (None, [], b'L') => ok(format!("IL: insert {n} lines")),
        (None, [], b'M') => ok(format!("DL: delete {n} lines")),
        (None, [], b'S') => ok(format!("SU: scroll up {n} lines")),
        (None, [], b'T') => ok(format!("SD: scroll down {n} lines")),
        (None, [], b'I') => ok(format!("CHT: cursor forward {n} tab stops")),
        (None, [], b'Z') => ok(format!("CBT: cursor back {n} tab stops")),
        (None, [], b'b') => ok(format!("REP: repeat the last character {n} times")),
        (None, [], b'g') => ok(if ps == 3 { "TBC: clear all tab stops" } else { "TBC: clear the tab stop here" }),
        (None, [], b'm') => sgr(&params),
        (None | Some('?'), [], b'h' | b'l') => modes(&params, private.is_some(), last == b'h'),
        (None, [], b'n') => match ps {
            5 => ok("DSR: asks for the terminal's status"),
            6 => ok("DSR: asks for the cursor position"),
            _ => unknown(format!("DSR {ps}")),
        },
        (Some('?'), [], b'n') => ok(format!("DSR (DEC) {ps}: a status query")),
        (None, [], b'c') => ok("DA: asks what the terminal is (primary device attributes)"),
        (Some('>'), [], b'c') => ok("DA2: asks the terminal's type and version"),
        (Some('='), [], b'c') => ok("DA3: asks the terminal's unit ID"),
        (None, [], b'r') if params.is_empty() => ok("DECSTBM: scroll region reset to the whole screen"),
        (None, [], b'r') => ok(format!("DECSTBM: scroll region rows {n} to {}", first(1, 0))),
        (None, [], b's') if params.is_empty() => ok("SCOSC: save the cursor (or DECSLRM, with mode ?69)"),
        (None, [], b's') => ok(format!("DECSLRM: left/right margins {n} to {}", first(1, 0))),
        (None, [], b'u') => ok("SCORC: restore the cursor SCOSC saved"),
        (None, [], b't') => window(&params),
        (None, [], b'x') => ok("DECREQTPARM: asks the terminal's parameters"),
        (None, [b' '], b'q') => ok(match ps {
            0 | 1 => "DECSCUSR: cursor a blinking block",
            2 => "DECSCUSR: cursor a steady block",
            3 => "DECSCUSR: cursor a blinking underline",
            4 => "DECSCUSR: cursor a steady underline",
            5 => "DECSCUSR: cursor a blinking bar",
            6 => "DECSCUSR: cursor a steady bar",
            _ => return unknown(format!("DECSCUSR {ps}: no such cursor")),
        }),
        (None, [b'!'], b'p') => ok("DECSTR: soft terminal reset"),
        (None, [b'"'], b'p') => ok("DECSCL: set the conformance level"),
        (None, [b'"'], b'q') => ok("DECSCA: set character protection"),
        (None | Some('?'), [b'$'], b'p') => ok(format!("DECRQM: asks whether mode {ps} is set")),
        (Some('?'), [], b's') => ok("XTSAVE: save DEC private modes"),
        (Some('?'), [], b'r') => ok("XTRESTORE: restore DEC private modes"),
        (Some('?'), [], b'u') => ok("asks for the kitty keyboard flags"),
        (Some('>'), [], b'u') => ok(format!("kitty keyboard: push flags {ps}")),
        (Some('<'), [], b'u') => ok(format!("kitty keyboard: pop {n} entries")),
        (Some('='), [], b'u') => ok(format!("kitty keyboard: set flags {ps}")),
        (Some('>'), [], b'm') => ok("XTMODKEYS: set modifyOtherKeys"),
        (Some('>'), [], b'q') => ok("XTVERSION: asks the terminal's name and version"),
        (None, [b'$'], b'x' | b'z' | b'{' | b'v' | b'r' | b't') => ok("a rectangular area operation (DECFRA, DECERA, DECSERA, DECCRA, DECCARA, DECRARA)"),
        (None, [b'*'], b'x') => ok("DECSACE: how rectangle operations apply"),
        (None, [b'*'], b'y') => ok("DECRQCRA: asks a checksum of a rectangle"),
        _ => unknown(format!(
            "unknown CSI {}{}{}{}",
            p.map(String::from).unwrap_or_default(),
            String::from_utf8_lossy(numbers),
            String::from_utf8_lossy(intermediates),
            char::from(last)
        )),
    }
}

fn modes(params: &[Vec<Option<u32>>], private: bool, set: bool) -> Explained {
    let (what, verb) = match (private, set) {
        (true, true) => ("DECSET", "set"),
        (true, false) => ("DECRST", "reset"),
        (false, true) => ("SM", "set"),
        (false, false) => ("RM", "reset"),
    };
    let mut parts = Vec::new();
    let mut unknown_modes = Vec::new();
    for n in params.iter().map(|p| p.first().copied().flatten().unwrap_or(0)) {
        let named = Mode::ALL
            .into_iter()
            .find(|m| m.number() == (private, u16::try_from(n).unwrap_or(0)));
        match named {
            Some(mode) => parts.push(format!("{mode:?} ({n})")),
            None => unknown_modes.push(n.to_string()),
        }
    }
    let mut text = format!("{what}: {verb}");
    if !parts.is_empty() {
        text.push_str(&format!(" {}", parts.join(", ")));
    }
    if unknown_modes.is_empty() {
        return ok(text);
    }
    if !parts.is_empty() {
        text.push(';');
    }
    text.push_str(&format!(" unknown mode {}", unknown_modes.join(", ")));
    unknown(text)
}

fn window(params: &[Vec<Option<u32>>]) -> Explained {
    let at = |i: usize| params.get(i).and_then(|p| p.first()).copied().flatten().unwrap_or(0);
    ok(match at(0) {
        8 => return ok(format!("XTWINOPS: resize to {} rows, {} columns", at(1), at(2))),
        14 => "XTWINOPS: asks the text area's size in pixels",
        16 => "XTWINOPS: asks a cell's size in pixels",
        18 => "XTWINOPS: asks the text area's size in characters",
        22 => "XTWINOPS: push the title on a stack",
        23 => "XTWINOPS: pop the title from the stack",
        48 => "in-band resize report",
        n => return unknown(format!("XTWINOPS {n}")),
    })
}

const COLOURS: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

fn colour_name(i: u32) -> &'static str {
    COLOURS.get(usize::try_from(i).unwrap_or(9)).copied().unwrap_or("?")
}

/// SGR, each parameter in turn.
fn sgr(params: &[Vec<Option<u32>>]) -> Explained {
    if params.is_empty() {
        return ok("SGR: reset all attributes");
    }
    let mut parts = Vec::new();
    let mut bad = Vec::new();
    let mut malformed_parts = Vec::new();
    let mut rest = params.iter();
    while let Some(group) = rest.next() {
        let n = group.first().copied().flatten().unwrap_or(0);
        let part = match n {
            0 => "reset".into(),
            1 => "bold".into(),
            2 => "dim".into(),
            3 => "italic".into(),
            4 => match group.get(1).copied().flatten() {
                None if group.len() == 1 => "underline".into(),
                Some(0) | None => "no underline".into(),
                Some(s @ 1..=5) => format!("underline style {s}"),
                Some(s) => {
                    bad.push(format!("4:{s}"));
                    continue;
                }
            },
            5 => "slow blink".into(),
            6 => "rapid blink".into(),
            7 => "inverse".into(),
            8 => "hidden".into(),
            9 => "strikeout".into(),
            21 => "double underline".into(),
            22 => "not bold or dim".into(),
            23 => "not italic".into(),
            24 => "not underlined".into(),
            25 => "not blinking".into(),
            27 => "not inverse".into(),
            28 => "not hidden".into(),
            29 => "not struck out".into(),
            30..=37 => format!("foreground {}", colour_name(n - 30)),
            40..=47 => format!("background {}", colour_name(n - 40)),
            90..=97 => format!("foreground bright {}", colour_name(n - 90)),
            100..=107 => format!("background bright {}", colour_name(n - 100)),
            39 => "default foreground".into(),
            49 => "default background".into(),
            59 => "default underline colour".into(),
            38 | 48 | 58 => {
                let layer = match n {
                    38 => "foreground",
                    48 => "background",
                    _ => "underline colour",
                };
                // Colon form keeps it in one group; semicolon form takes
                // the next parameters.
                let mut values: Vec<u32> = group.iter().skip(1).map(|v| v.unwrap_or(0)).collect();
                let mut short = false;
                if group.len() == 1 {
                    let mut next = || match rest.next() {
                        Some(g) => g.first().copied().flatten().unwrap_or(0),
                        None => {
                            short = true;
                            0
                        }
                    };
                    let kind = next();
                    values.push(kind);
                    let take = if kind == 2 { 3 } else { 1 };
                    for _ in 0..take {
                        values.push(next());
                    }
                } else {
                    let want = if values.first() == Some(&2) { 4 } else { 2 };
                    short = values.len() < want;
                }
                if short {
                    malformed_parts.push(format!("{n} is missing colour values, taken as 0"));
                }
                match values.as_slice() {
                    [5, i, ..] if *i < 256 => format!("{layer} colour {i}"),
                    [2, r, g, b] | [2, _, r, g, b] if *r < 256 && *g < 256 && *b < 256 => {
                        format!("{layer} #{r:02x}{g:02x}{b:02x}")
                    }
                    _ => {
                        bad.push(format!("{n} with {values:?}"));
                        continue;
                    }
                }
            }
            _ => {
                bad.push(n.to_string());
                continue;
            }
        };
        parts.push(part);
    }
    if !bad.is_empty() {
        parts.push(format!("unknown SGR {}", bad.join(", ")));
    }
    parts.extend(malformed_parts.iter().cloned());
    let text = format!("SGR: {}", parts.join(", "));
    if !malformed_parts.is_empty() {
        malformed(text)
    } else if !bad.is_empty() {
        unknown(text)
    } else {
        ok(text)
    }
}

fn shorten(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.chars().count() > 60 {
        format!("{}…", text.chars().take(60).collect::<String>())
    } else {
        text.into()
    }
}

fn string(introducer: Introducer, body: &[u8], ender: Ender, state: &mut State) -> Explained {
    let (text, flag) = match introducer {
        Introducer::Osc => osc(body, state),
        Introducer::Dcs => dcs(body),
        Introducer::Apc if body.first() == Some(&b'G') => ok("APC: kitty graphics command"),
        Introducer::Apc => unknown(format!("APC {}: unknown", shorten(body))),
        Introducer::Pm => ok("PM: privacy message (ignored)"),
        Introducer::Sos => ok("SOS: start of string (ignored)"),
    };
    match ender {
        Ender::St => (text, flag),
        Ender::Bel if introducer == Introducer::Osc => (text, flag),
        Ender::Bel => malformed(format!("{text}; ended by BEL, which only OSC takes")),
        Ender::Esc => malformed(format!("{text}; ended by ESC without \\ (ST)")),
        Ender::Cancelled(_) => malformed(format!("{text}; cancelled by CAN/SUB, so not carried out")),
        Ender::Eof => malformed(format!("{text}; never ended: the input stops inside it")),
    }
}

fn osc(body: &[u8], state: &mut State) -> Explained {
    let text = String::from_utf8_lossy(body);
    let (number, rest) = text.split_once(';').unwrap_or((&text, ""));
    let rest = shorten(rest.as_bytes());
    match number {
        "0" => ok(format!("OSC 0: set the window title and icon name to {rest:?}")),
        "1" => ok(format!("OSC 1: set the icon name to {rest:?}")),
        "2" => ok(format!("OSC 2: set the window title to {rest:?}")),
        "4" if rest.ends_with('?') => ok("OSC 4: asks a palette colour"),
        "4" => ok(format!("OSC 4: set palette colours {rest}")),
        "7" => ok(format!("OSC 7: the current directory is {rest}")),
        "8" => {
            let uri = rest.split_once(';').map_or("", |(_, u)| u).to_string();
            if uri.is_empty() {
                state.link = None;
                ok("OSC 8: end the hyperlink")
            } else {
                state.link = Some(uri.clone());
                ok(format!("OSC 8: start a hyperlink to {uri}"))
            }
        }
        "9" => ok(format!("OSC 9: notification {rest:?}")),
        "777" => ok(format!("OSC 777: notification {rest:?}")),
        "99" => ok("OSC 99: kitty notification"),
        "10" | "11" | "12" | "13" | "14" | "15" | "16" | "17" | "18" | "19" => {
            let what = match number {
                "10" => "foreground",
                "11" => "background",
                "12" => "cursor colour",
                _ => "dynamic colour",
            };
            if rest.starts_with('?') {
                ok(format!("OSC {number}: asks the {what}"))
            } else {
                ok(format!("OSC {number}: set the {what} to {rest}"))
            }
        }
        "52" => ok("OSC 52: set or ask the clipboard"),
        "104" => ok("OSC 104: reset palette colours"),
        "110" | "111" | "112" => ok(format!("OSC {number}: reset a dynamic colour")),
        "133" => ok(match rest.chars().next() {
            Some('A') => "OSC 133 A: a prompt starts",
            Some('B') => "OSC 133 B: the command line starts",
            Some('C') => "OSC 133 C: the command's output starts",
            Some('D') => "OSC 133 D: the command ended",
            _ => "OSC 133: a shell integration mark",
        }),
        "22" => ok(format!("OSC 22: mouse pointer shape {rest:?}")),
        "1337" => ok("OSC 1337: an iTerm2 command"),
        _ => unknown(format!("OSC {number}: unknown")),
    }
}

fn dcs(body: &[u8]) -> Explained {
    if body.starts_with(b"$q") {
        return ok(format!("DECRQSS: asks the setting of {:?}", shorten(body.get(2..).unwrap_or_default())));
    }
    if body.starts_with(b"+q") {
        let names: Vec<String> = String::from_utf8_lossy(body.get(2..).unwrap_or_default())
            .split(';')
            .map(|hex| {
                let bytes: Vec<u8> = (0..hex.len() / 2)
                    .filter_map(|i| hex.get(i * 2..i * 2 + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
                    .collect();
                String::from_utf8_lossy(&bytes).into_owned()
            })
            .collect();
        return ok(format!("XTGETTCAP: asks terminfo capabilities {}", names.join(", ")));
    }
    if body.iter().find(|b| !matches!(b, b'0'..=b'9' | b';')) == Some(&b'q') {
        return ok("DCS q: a sixel picture");
    }
    if body.starts_with(b"tmux;") {
        return ok("DCS tmux: passed through tmux to the outer terminal");
    }
    if body.starts_with(b"=1s") || body.starts_with(b"=2s") {
        return ok("DCS =s: synchronized update (old form)");
    }
    unknown(format!("DCS {}: unknown", shorten(body)))
}
