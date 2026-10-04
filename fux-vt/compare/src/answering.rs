//! An engine as the terminal a program talks to, for `esctest`: what the
//! program writes goes to the engine, and the engine's replies
//! ([`Engine::replies`]) go back, in order, as a terminal sends them up its
//! PTY.
//!
//! esctest reads the screen back cell by cell with DECRQCRA, a checksum of
//! a rectangle (VT420; ctlseqs, "CSI Pi ; Pg ; Pt ; Pl ; Pb ; Pr * y").
//! fux-vt answers it ([`fux_vt::Options::rectangle_checksums`], added for
//! esctest); no other engine here does: Ghostty's core, alacritty and
//! libvterm ignore it. It is how the test reads the screen, not what the
//! test is about, so here it is answered from the engine's own screen, read
//! as every comparison of the harness reads it ([`Engine::snapshot`]), with
//! fux-vt's sum, which is xterm's ([`checksum`]): what is compared is then
//! the engine's screen, as with fux-vt. A DECRQCRA never reaches the engine.
//!
//! In origin mode DECRQCRA's rectangle is relative to the margins, which no
//! engine's snapshot gives, so it is not answered: esctest's read times out,
//! and esctest's run counts that test as one whose cells cannot be read.
use crate::engine::Engine;
use crate::snapshot::{Cell, Snapshot, Width};

const ESC: u8 = 0x1b;

/// An engine answering a program.
pub struct Answering {
    engine: Box<dyn Engine>,
    /// How many bytes of the engine's replies have been sent.
    answered: usize,
    /// The end of what the program wrote that may be the start of a
    /// DECRQCRA, held until it is complete or is not one.
    held: Vec<u8>,
}

/// Where the next DECRQCRA is in what the program wrote.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// None, and nothing that may start one.
    None,
    /// One, at `start`, `len` bytes long, with its parameters.
    Request {
        start: usize,
        len: usize,
        params: Vec<u16>,
    },
    /// The bytes from `start` on may start one.
    Partial { start: usize },
}

/// What `rest`, starting with ESC, is: a whole DECRQCRA (its length and
/// parameters), the start of one, or not one.
enum Shape {
    Whole(usize, Vec<u16>),
    Start,
    Not,
}

/// The shape of the sequence at the start of `rest` (which starts with
/// ESC): DECRQCRA is `ESC [`, parameters (digits and `;`), `*` and `y`. A
/// parameter is read as fux-vt reads one, saturating at 65535.
fn shape(rest: &[u8]) -> Shape {
    let mut bytes = rest.iter().enumerate().skip(1);
    match bytes.next() {
        None => return Shape::Start,
        Some((_, b'[')) => {}
        Some(_) => return Shape::Not,
    }
    let mut params = vec![0u16];
    for (i, &b) in bytes.by_ref() {
        match b {
            b'0'..=b'9' => {
                if let Some(last) = params.last_mut() {
                    *last = last
                        .saturating_mul(10)
                        .saturating_add(u16::from(b.saturating_sub(b'0')));
                }
            }
            b';' => params.push(0),
            b'*' => {
                return match rest.get(i.saturating_add(1)) {
                    None => Shape::Start,
                    Some(b'y') => Shape::Whole(i.saturating_add(2), params),
                    Some(_) => Shape::Not,
                };
            }
            _ => return Shape::Not,
        }
    }
    Shape::Start
}

/// The first DECRQCRA in `bytes`, or the start of one at their end.
fn find(bytes: &[u8]) -> Found {
    let mut at = 0usize;
    while let Some(start) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == ESC))
        .and_then(|i| i.checked_add(at))
    {
        match shape(bytes.get(start..).unwrap_or_default()) {
            Shape::Whole(len, params) => return Found::Request { start, len, params },
            Shape::Start => return Found::Partial { start },
            Shape::Not => at = start.saturating_add(1),
        }
    }
    Found::None
}

impl Answering {
    pub fn new(engine: Box<dyn Engine>) -> Answering {
        Answering {
            engine,
            answered: 0,
            held: Vec::new(),
        }
    }

    /// The engine's replies not sent yet.
    fn new_replies(&mut self) -> Vec<u8> {
        let all = self.engine.replies();
        let new = all.get(self.answered..).unwrap_or_default().to_vec();
        self.answered = all.len();
        new
    }

    /// Takes what the program wrote; what the terminal answers, in order.
    pub fn take(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = std::mem::take(&mut self.held);
        input.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut rest = input.as_slice();
        loop {
            match find(rest) {
                Found::None => {
                    self.engine.process(rest)?;
                    break;
                }
                Found::Partial { start } => {
                    let (now, later) = rest.split_at_checked(start).unwrap_or((rest, &[]));
                    self.engine.process(now)?;
                    self.held = later.to_vec();
                    break;
                }
                Found::Request { start, len, params } => {
                    let (now, later) = rest.split_at_checked(start).unwrap_or((rest, &[]));
                    self.engine.process(now)?;
                    out.extend(self.new_replies());
                    let screen = self.engine.snapshot(0)?;
                    if let Some(reply) = checksum(&screen, &params) {
                        out.extend_from_slice(reply.as_bytes());
                    }
                    rest = later.get(len..).unwrap_or_default();
                }
            }
        }
        out.extend(self.new_replies());
        Ok(out)
    }
}

/// DEC Special Graphics, 0x5f to 0x7e, as fux-vt draws them (fux-vt's
/// `screen.rs`, `special_graphics`, as xterm draws them).
const GRAPHICS: [char; 32] = [
    ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻', '─',
    '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
];

/// What a cell adds to the checksum, as fux-vt counts it (its `screen.rs`,
/// `checksum_of`, xterm's default): its character, a Special Graphics glyph
/// as the code it is drawn from (0x60 to 0x7e), any other past Latin-1 as
/// ESC, a blank as a space and a wide glyph's second half as ESC; its
/// combining marks as they are; and its VT100 attributes: 0x08 hidden, 0x10
/// underlined, 0x20 inverse, 0x40 blinking, 0x80 bold; a wide glyph's
/// second half has none, as in fux-vt.
fn cell_sum(cell: &Cell) -> u16 {
    let style = &cell.style;
    const SPACE: u16 = 0x20;
    let mut chars = cell.text.chars();
    let base = match chars.next() {
        None if cell.width == Width::Tail => u16::from(ESC),
        None => SPACE,
        Some(c) => match u16::try_from(u32::from(c)) {
            Ok(code @ 0x20..=0xff) => code,
            _ => GRAPHICS
                .iter()
                .zip(0x5fu16..)
                .find(|(g, code)| **g == c && *code >= 0x60)
                .map_or(u16::from(ESC), |(_, code)| code),
        },
    };
    let marks = chars.fold(0u16, |sum, c| {
        sum.wrapping_add(u16::try_from(u32::from(c) & 0xffff).unwrap_or(0))
    });
    let rendition = [
        (style.hidden, 0x08),
        (style.underline, 0x10),
        (style.inverse, 0x20),
        (style.blink, 0x40),
        (style.bold, 0x80),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0u16, |sum, (_, bit)| sum | bit);
    base.wrapping_add(marks).wrapping_add(rendition)
}

/// The reply to DECRQCRA with `params` (`Pi ; Pg ; Pt ; Pl ; Pb ; Pr`), as
/// fux-vt gives it (its `screen.rs`, `rectangle_checksum`): DECCKSR, `DCS
/// Pi ! ~ xxxx ST`, the cells' sum in 16 bits, negated, as four upper-case
/// hex digits. The page is ignored; the coordinates are one-based, clamped
/// to the screen, absent or 0 meaning its whole extent; a rectangle whose
/// top is below its bottom, or left past its right, sums nothing. `None` in
/// origin mode: see the module documentation.
pub fn checksum(screen: &Snapshot, params: &[u16]) -> Option<String> {
    if screen.origin {
        return None;
    }
    let param = |i: usize| params.get(i).copied().unwrap_or(0);
    let last_row = screen.rows.saturating_sub(1);
    let last_col = screen.cols.saturating_sub(1);
    let at = |i: usize, default: u16, last: u16| match param(i) {
        0 => default,
        n => n.saturating_sub(1).min(last),
    };
    let (top, left) = (at(2, 0, last_row), at(3, 0, last_col));
    let (bottom, right) = (at(4, last_row, last_row), at(5, last_col, last_col));
    let mut sum = 0u16;
    let rows = usize::from(bottom.saturating_sub(top)).saturating_add(1);
    let cols = usize::from(right.saturating_sub(left)).saturating_add(1);
    for line in screen
        .screen
        .iter()
        .skip(usize::from(top))
        .take(rows)
        .filter(|_| top <= bottom)
    {
        for cell in line.cells.iter().skip(usize::from(left)).take(cols) {
            if left <= right {
                sum = sum.wrapping_add(cell_sum(cell));
            }
        }
    }
    Some(format!(
        "\x1bP{}!~{:04X}\x1b\\",
        param(0),
        sum.wrapping_neg()
    ))
}

#[cfg(test)]
mod tests {
    use super::{Answering, Found, find};
    use crate::engine::{ENGINES, Setup};
    use crate::rng::Rng;

    /// fux-vt answering DECRQCRA itself, as esctest's direct run sets it
    /// up: the oracle for the checksum read from a snapshot.
    fn fux_vt(rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
        fux_vt::Parser::with_options(rows, cols, 0, crate::esctest::options())
            .map_err(|e| e.to_string())
    }

    #[derive(Default)]
    struct Replies(Vec<u8>);

    impl fux_vt::Sink for Replies {
        fn reply(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }

    /// fux-vt as an engine, which does not answer DECRQCRA: every
    /// checksum then comes from its snapshot.
    fn answering(rows: u16, cols: u16) -> Result<Answering, String> {
        let engine = crate::engines::fux_vt::make(&Setup {
            rows,
            cols,
            history: 0,
            reflow: false,
        })?;
        Ok(Answering::new(engine))
    }

    /// Both answers to `bytes`, fed `chunk` bytes at a time to the
    /// snapshot's side.
    fn both(
        rows: u16,
        cols: u16,
        bytes: &[u8],
        chunk: usize,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut oracle = fux_vt(rows, cols)?;
        let mut sink = Replies::default();
        oracle
            .process_with(bytes, &mut sink)
            .map_err(|e| e.to_string())?;
        let mut terminal = answering(rows, cols)?;
        let mut got = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (now, later) = rest
                .split_at_checked(chunk.min(rest.len()))
                .unwrap_or((rest, &[]));
            got.extend(terminal.take(now)?);
            rest = later;
        }
        Ok((sink.0, got))
    }

    /// A DECRQCRA of every cell, one at a time as esctest asks, then of the
    /// whole screen, of each row, of a rectangle past the screen's edge and
    /// of an empty one.
    fn reads(rows: u16, cols: u16) -> Vec<u8> {
        let mut out = String::new();
        let mut id = 1u32;
        for y in 1..=rows {
            for x in 1..=cols {
                out.push_str(&format!("\x1b[{id};0;{y};{x};{y};{x}*y"));
                id = id.saturating_add(1);
            }
            out.push_str(&format!("\x1b[{id};0;{y};1;{y};{cols}*y"));
        }
        out.push_str("\x1b[7*y\x1b[8;0;0;0;0;0*y\x1b[9;1;2;3;999;999*y\x1b[10;0;3;3;2;2*y");
        out.into_bytes()
    }

    /// Screens to read: attributes (each of the five counted, and colours,
    /// which are not), wide glyphs (bold too), combining marks, Special
    /// Graphics, Latin-1 and past it, erasing with a pen, a wide glyph cut
    /// at the right edge, and the cursor's own reports beside the
    /// checksums.
    const SCREENS: &[&str] = &[
        "abc",
        "\x1b[1ma\x1b[4mb\x1b[5mc\x1b[7md\x1b[8me\x1b[0m\x1b[31;42mf\x1b[2;3;9mg\x1b[0m",
        "\x1b[1m漢字\x1b[0mx\x1b[7m字",
        "e\u{301}\u{302}a\u{20dd}",
        "\x1b(0`abjklmnqtuvwx~_\x1b(Bq",
        "é£ÿ€\u{1F600}\u{0100}",
        "abcdef\x1b[1;3H\x1b[1;7m\x1b[K\x1b[2;1H\x1b[4mxyz\x1b[2J",
        "abcdefg漢",
        "\x1b[2;3Hx\x1b[6n\x1b[5n",
    ];

    /// The checksum read from fux-vt's snapshot is fux-vt's own DECRQCRA
    /// reply, byte for byte, on every screen, whatever the chunks the bytes
    /// arrive in: so an engine's checksum is fux-vt's sum of the engine's
    /// screen.
    #[test]
    fn checksums_from_a_snapshot_are_fux_vts() -> Result<(), String> {
        let (rows, cols) = (3u16, 8u16);
        for screen in SCREENS {
            let mut bytes = screen.as_bytes().to_vec();
            bytes.extend(reads(rows, cols));
            for chunk in [1, 2, 3, 7, bytes.len()] {
                let (want, got) = both(rows, cols, &bytes, chunk)?;
                assert!(!want.is_empty());
                assert_eq!(
                    String::from_utf8_lossy(&got),
                    String::from_utf8_lossy(&want),
                    "{screen:?} in chunks of {chunk}"
                );
            }
        }
        Ok(())
    }

    /// The same on random screens: printable text, wide glyphs, marks,
    /// Special Graphics, the five attributes on and off, cursor moves and
    /// erases.
    #[test]
    fn checksums_from_a_snapshot_are_fux_vts_at_random() -> Result<(), String> {
        const PIECES: &[&str] = &[
            "a",
            "Z",
            " ",
            "~",
            "é",
            "ÿ",
            "漢",
            "\u{1F600}",
            "\u{301}",
            "\u{FE0F}",
            "\x1b(0",
            "\x1b(B",
            "q",
            "`",
            "\x1b[1m",
            "\x1b[4m",
            "\x1b[5m",
            "\x1b[7m",
            "\x1b[8m",
            "\x1b[0m",
            "\x1b[22m",
            "\x1b[24m",
            "\x1b[27m",
            "\x1b[H",
            "\x1b[2;5H",
            "\x1b[K",
            "\x1b[1K",
            "\x1b[2X",
            "\x1b[P",
            "\x1b[@",
            "\r\n",
            "\x08",
            "\x1b[J",
            "\x1b[3;1H",
        ];
        let (rows, cols) = (4u16, 6u16);
        let mut rng = Rng::new(7);
        for _ in 0..300 {
            let mut bytes = Vec::new();
            for _ in 0..rng.below(40) {
                if let Some(piece) = rng.pick(PIECES) {
                    bytes.extend_from_slice(piece.as_bytes());
                }
            }
            bytes.extend(reads(rows, cols));
            let chunk = rng.below(9).saturating_add(1);
            let (want, got) = both(rows, cols, &bytes, chunk)?;
            assert_eq!(
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(&want),
                "{:?} in chunks of {chunk}",
                String::from_utf8_lossy(&bytes)
            );
        }
        Ok(())
    }

    /// In origin mode DECRQCRA is not answered; everything else still is,
    /// in order.
    #[test]
    fn origin_mode_is_not_read() -> Result<(), String> {
        let mut terminal = answering(5, 10)?;
        let got = terminal.take(b"\x1b[2;4r\x1b[?6h\x1b[1;0;1;1;1;1*y\x1b[6n")?;
        assert_eq!(got, b"\x1b[1;1R");
        let got = terminal.take(b"\x1b[?6l\x1b[2;0;1;1;1;1*y\x1b[6n")?;
        assert_eq!(got, b"\x1bP2!~FFE0\x1b\\\x1b[1;1R");
        Ok(())
    }

    /// Finding a DECRQCRA: a whole one, the start of one at the end, and
    /// sequences that only begin like one.
    #[test]
    fn decrqcra_is_found() {
        assert_eq!(find(b"ab"), Found::None);
        assert_eq!(
            find(b"a\x1b[1;;3*yb"),
            Found::Request {
                start: 1,
                len: 8,
                params: vec![1, 0, 3]
            }
        );
        assert_eq!(
            find(b"\x1b[99999*y"),
            Found::Request {
                start: 0,
                len: 9,
                params: vec![u16::MAX]
            }
        );
        for partial in [&b"a\x1b"[..], b"a\x1b[", b"a\x1b[1;2", b"a\x1b[1*"] {
            assert_eq!(find(partial), Found::Partial { start: 1 }, "{partial:?}");
        }
        for not in [
            &b"\x1b[6n"[..],
            b"\x1b[?1*y",
            b"\x1b[1*x",
            b"\x1b7",
            b"\x1b[1:2*y",
        ] {
            assert_eq!(find(not), Found::None, "{not:?}");
        }
        assert_eq!(
            find(b"\x1b[6n\x1b[*y"),
            Found::Request {
                start: 4,
                len: 4,
                params: vec![0]
            }
        );
    }

    /// Ghostty's core as esctest's terminal: it answers a cursor report
    /// and, given its size, the size reports esctest asks before every
    /// test; its cells are read by checksum.
    #[test]
    fn ghostty_answers_through_its_screen() -> Result<(), String> {
        let ghostty = crate::engine::find("ghostty")
            .and_then(|i| ENGINES.get(i))
            .ok_or("no ghostty")?;
        assert_eq!(ghostty.name, "ghostty");
        let mut terminal = Answering::new(crate::engines::ghostty::answering(25, 80)?);
        let got = terminal.take(b"ab\x1b[1;0;1;2;1;2*y\x1b[6n\x1b[18t\x1b[14t\x1b[16t")?;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1bP1!~FF9E\x1b\\\x1b[1;3R\x1b[8;25;80t\x1b[4;400;640t\x1b[6;16;8t"
        );
        Ok(())
    }
}
