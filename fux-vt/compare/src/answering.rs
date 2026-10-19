//! An engine as the terminal a program talks to, for `esctest`: what the
//! program writes goes to the engine, and the engine's replies
//! ([`Engine::replies`]) go back, in order, as a terminal sends them up its
//! PTY.
//!
//! Two reports are answered here, from the engine's own screen, read as
//! every comparison of the harness reads it ([`Engine::snapshot`]), where
//! the engine gives none. Each such answer is counted ([`Answering::told`]):
//! a test that needed one is adapter-observed, not the engine's native
//! support.
//!
//! - DECRQCRA, a checksum of a rectangle (VT420; ctlseqs, "CSI Pi ; Pg ;
//!   Pt ; Pl ; Pb ; Pr * y"), with which esctest reads the screen back cell
//!   by cell. fux-vt answers it ([`fux_vt::Feature::RectangleChecksums`],
//!   added for esctest); no other engine here does by default: Ghostty's
//!   core, alacritty and libvterm ignore it, and wezterm-term's is off. It
//!   is how the test reads the screen, not what the test is about, so it
//!   is answered from the snapshot with fux-vt's sum, which is xterm's
//!   ([`checksum`]): what is compared is then the engine's screen, as with
//!   fux-vt. A DECRQCRA never reaches the engine.
//! - The text area's size in characters (XTWINOPS 18, `CSI 18 t`), which
//!   esctest asks before every test: the request goes to the engine, and
//!   only if it does not answer is `CSI 8 ; rows ; cols t` given from the
//!   snapshot.
//!
//! In origin mode DECRQCRA's rectangle is relative to the margins, which no
//! engine's snapshot gives, so it is not answered: esctest's read times out,
//! and the test is adapter-limited, its cells unreadable
//! ([`Answering::unreadable`]). For an engine whose origin mode cannot be
//! read, origin mode is taken to be on from the program's DECSET 6 until
//! its DECRST 6, DECSTR or RIS, so that no rectangle is read as absolute
//! that the engine may hold relative.
//!
//! The requests are found in the bytes as the program wrote them, not as
//! the engine parsed them: one inside a string the engine swallows is
//! answered all the same.
use crate::engine::Engine;
use crate::snapshot::{Cell, Snapshot, Width};

const ESC: u8 = 0x1b;

/// An engine answering a program.
pub struct Answering {
    engine: Box<dyn Engine>,
    /// Whether the engine's snapshot says if origin mode is on.
    reads_origin: bool,
    /// For an engine that cannot say: whether the program may have set
    /// origin mode.
    origin_set: bool,
    /// How many bytes of the engine's replies have been sent.
    answered: usize,
    /// The end of what the program wrote that may be the start of a
    /// request, held until it is complete or is not one.
    held: Vec<u8>,
    /// How many reports were answered here, from the snapshot, or by the
    /// engine's adapter for its host (`Engine::host_answers`).
    pub told: usize,
    /// The engine's host answers counted so far.
    hosted: usize,
    /// How many DECRQCRAs were left unanswered: in origin mode.
    pub unreadable: usize,
}

/// A sequence the program wrote that is answered or watched here.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// DECRQCRA, with its parameters.
    Checksum(Vec<u16>),
    /// XTWINOPS 18: the text area's size in characters.
    Size,
    /// DECSET or DECRST of origin mode (6): whether set.
    Origin(bool),
    /// DECSTR or RIS, either of which resets origin mode.
    Reset,
}

/// Where the next request is in what the program wrote.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// None, and nothing that may start one.
    None,
    /// One, at `start`, `len` bytes long.
    Request {
        start: usize,
        len: usize,
        request: Request,
    },
    /// The bytes from `start` on may start one.
    Partial { start: usize },
}

/// What `rest`, starting with ESC, is: a whole request (its length), the
/// start of a sequence, or neither.
enum Shape {
    Whole(usize, Request),
    Start,
    Not,
}

/// The shape of the sequence at the start of `rest` (which starts with
/// ESC): RIS (`ESC c`), or a CSI, `ESC [`, an optional `?`, parameters
/// (digits and `;`), an optional intermediate and a final byte, of which
/// DECRQCRA (`* y`), XTWINOPS 18 (`18 t`), DECSET and DECRST naming 6, and
/// DECSTR (`! p`) are requests. A parameter is read as fux-vt reads one,
/// saturating at 65535.
fn shape(rest: &[u8]) -> Shape {
    let mut bytes = rest.iter().enumerate().skip(1).peekable();
    match bytes.next() {
        None => return Shape::Start,
        Some((_, b'[')) => {}
        Some((_, b'c')) => return Shape::Whole(2, Request::Reset),
        Some(_) => return Shape::Not,
    }
    let private = bytes.next_if(|(_, b)| **b == b'?').is_some();
    let mut params = vec![0u16];
    let mut intermediate = None;
    for (i, &b) in bytes {
        match b {
            b'0'..=b'9' if intermediate.is_none() => {
                if let Some(last) = params.last_mut() {
                    *last = last
                        .saturating_mul(10)
                        .saturating_add(u16::from(b.saturating_sub(b'0')));
                }
            }
            b';' if intermediate.is_none() => params.push(0),
            b'*' | b'!' if intermediate.is_none() => intermediate = Some(b),
            0x40..=0x7e => {
                let len = i.saturating_add(1);
                let request = match (private, intermediate, b) {
                    (false, Some(b'*'), b'y') => Request::Checksum(params),
                    (false, None, b't') if params == [18] => Request::Size,
                    (true, None, b'h' | b'l') if params.contains(&6) => Request::Origin(b == b'h'),
                    (false, Some(b'!'), b'p') => Request::Reset,
                    _ => return Shape::Not,
                };
                return Shape::Whole(len, request);
            }
            _ => return Shape::Not,
        }
    }
    Shape::Start
}

/// The first request in `bytes`, or the start of one at their end.
fn find(bytes: &[u8]) -> Found {
    let mut at = 0usize;
    while let Some(start) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == ESC))
        .and_then(|i| i.checked_add(at))
    {
        match shape(bytes.get(start..).unwrap_or_default()) {
            Shape::Whole(len, request) => {
                return Found::Request {
                    start,
                    len,
                    request,
                };
            }
            Shape::Start => return Found::Partial { start },
            Shape::Not => at = start.saturating_add(1),
        }
    }
    Found::None
}

impl Answering {
    /// `engine` answering a program; `reads_origin`, whether its snapshot
    /// says if origin mode is on (its [`crate::engine::Can`]).
    pub fn new(engine: Box<dyn Engine>, reads_origin: bool) -> Answering {
        Answering {
            engine,
            reads_origin,
            origin_set: false,
            answered: 0,
            held: Vec::new(),
            told: 0,
            hosted: 0,
            unreadable: 0,
        }
    }

    /// The engine's replies not sent yet.
    fn new_replies(&mut self) -> Result<Vec<u8>, String> {
        let all = self.engine.replies()?;
        let new = all.get(self.answered..).unwrap_or_default().to_vec();
        self.answered = all.len();
        Ok(new)
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
                Found::Request {
                    start,
                    len,
                    request,
                } => {
                    let (now, later) = rest.split_at_checked(start).unwrap_or((rest, &[]));
                    let (sequence, after) = later.split_at_checked(len).unwrap_or((later, &[]));
                    self.engine.process(now)?;
                    match request {
                        Request::Checksum(params) => {
                            out.extend(self.new_replies()?);
                            let screen = self.engine.snapshot(0)?;
                            let origin = screen.origin || (!self.reads_origin && self.origin_set);
                            match checksum(&screen, &params).filter(|_| !origin) {
                                Some(reply) if reply.is_empty() => {}
                                Some(reply) => {
                                    self.told = self.told.saturating_add(1);
                                    out.extend_from_slice(reply.as_bytes());
                                }
                                None => self.unreadable = self.unreadable.saturating_add(1),
                            }
                        }
                        Request::Size => {
                            out.extend(self.new_replies()?);
                            self.engine.process(sequence)?;
                            let new = self.new_replies()?;
                            let native = (0..new.len())
                                .any(|i| new.get(i..).is_some_and(|r| r.starts_with(b"\x1b[8;")));
                            out.extend(new);
                            if !native {
                                let screen = self.engine.snapshot(0)?;
                                self.told = self.told.saturating_add(1);
                                out.extend_from_slice(
                                    format!("\x1b[8;{};{}t", screen.rows, screen.cols).as_bytes(),
                                );
                            }
                        }
                        Request::Origin(set) => {
                            self.engine.process(sequence)?;
                            self.origin_set = set;
                        }
                        Request::Reset => {
                            self.engine.process(sequence)?;
                            self.origin_set = false;
                        }
                    }
                    rest = after;
                }
            }
        }
        out.extend(self.new_replies()?);
        let hosted = self.engine.host_answers();
        self.told = self.told.saturating_add(hosted.saturating_sub(self.hosted));
        self.hosted = hosted;
        Ok(out)
    }
}

/// The DEC code fux-vt sums for a Special Graphics or DEC Supplemental
/// glyph past Latin-1 (its `screen.rs`, `dec_code`; xterm 411's
/// `unmap_DEC_Spec_Graphic` and `unmap_DEC_Supp_Graphic`).
fn dec_code(c: char) -> Option<u16> {
    Some(match c {
        '◆' => 0x14,
        '▒' => 0x78,
        '␉' => 0x0a,
        '␌' => 0x0d,
        '␍' => 0x0e,
        '␊' => 0x0b,
        '␤' => 0x15,
        '␋' => 0x0c,
        '┘' => 0x16,
        '┐' => 0x17,
        '┌' => 0x18,
        '└' => 0x19,
        '┼' => 0x1a,
        '⎺' => 0x1b,
        '⎻' => 0x1c,
        '─' => 0x1d,
        '⎼' => 0x1e,
        '⎽' => 0x1f,
        '├' => 0x80,
        '┤' => 0x81,
        '┴' => 0x82,
        '┬' => 0x83,
        '│' => 0x84,
        '≤' => 0x85,
        '≥' => 0x86,
        '≠' => 0x87,
        'Œ' => 0x97,
        'Ÿ' => 0x98,
        'œ' => 0x99,
        'π' => 0xc6,
        _ => return None,
    })
}

/// What a cell adds to the checksum, as fux-vt counts it (its `screen.rs`,
/// `checksum_of`): its character within Latin-1, a Special Graphics or DEC
/// Supplemental glyph past it as its DEC code ([`dec_code`]), any other
/// past Latin-1 as ESC, a blank as a space and a wide glyph's second half
/// as ESC; its combining marks as they are; and its VT100 attributes: 0x08
/// hidden, 0x10 underlined, 0x20 inverse, 0x40 blinking, 0x80 bold; a wide
/// glyph's second half has none, as in fux-vt.
fn cell_sum(cell: &Cell) -> u16 {
    let style = &cell.style;
    const SPACE: u16 = 0x20;
    let mut chars = cell.text.chars();
    let base = match chars.next() {
        None if cell.width == Width::Tail => u16::from(ESC),
        None => SPACE,
        Some(c) => match u16::try_from(u32::from(c)) {
            Ok(code @ 0x20..=0xff) => code,
            _ => dec_code(c).unwrap_or(u16::from(ESC)),
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
/// top is below its bottom, or left past its right, is ignored, as DEC STD
/// 070's DECRQCRA has it: no reply, empty. `None` in origin mode: see the
/// module documentation.
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
    if top > bottom || left > right {
        return Some(String::new());
    }
    let mut sum = 0u16;
    let rows = usize::from(bottom.saturating_sub(top)).saturating_add(1);
    let cols = usize::from(right.saturating_sub(left)).saturating_add(1);
    for line in screen.screen.iter().skip(usize::from(top)).take(rows) {
        for cell in line.cells.iter().skip(usize::from(left)).take(cols) {
            sum = sum.wrapping_add(cell_sum(cell));
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
    use super::{Answering, Found, Request, find};
    use crate::engine::{ENGINES, Setup};
    use crate::rng::Rng;

    /// fux-vt answering DECRQCRA itself, as esctest's direct run sets it
    /// up: the oracle for the checksum read from a snapshot.
    fn fux_vt(rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
        fux_vt::Parser::with_options(crate::vt_size(rows, cols)?, 0, crate::esctest::options())
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
        Ok(Answering::new(engine, true))
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
        "\x1b(%5W]wA($\x1b(B",
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
            "\x1b(%5",
            "W",
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
                request: Request::Checksum(vec![1, 0, 3])
            }
        );
        assert_eq!(
            find(b"\x1b[99999*y"),
            Found::Request {
                start: 0,
                len: 9,
                request: Request::Checksum(vec![u16::MAX])
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
                request: Request::Checksum(vec![0])
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
        let mut terminal = Answering::new(crate::engines::ghostty::answering(25, 80)?, true);
        let got = terminal.take(b"ab\x1b[1;0;1;2;1;2*y\x1b[6n\x1b[18t\x1b[14t\x1b[16t")?;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1bP1!~FF9E\x1b\\\x1b[1;3R\x1b[8;25;80t\x1b[4;400;640t\x1b[6;16;8t"
        );
        Ok(())
    }

    /// An engine `name` of the comparisons, at 25 by 80, answering.
    fn engine(name: &str) -> Result<Answering, String> {
        let kind = crate::engine::find(name)
            .and_then(|i| ENGINES.get(i))
            .ok_or(format!("no {name}"))?;
        let engine = (kind.make)(&Setup {
            rows: 25,
            cols: 80,
            history: 0,
            reflow: false,
        })?;
        Ok(Answering::new(engine, kind.can.origin))
    }

    /// wezterm-term answers the cursor report and the size report itself,
    /// through its writer: none is the adapter's.
    #[test]
    fn wezterm_answers_for_itself() -> Result<(), String> {
        let mut terminal = engine("wezterm")?;
        let got = terminal.take(b"ab\x1b[6n\x1b[18t")?;
        assert_eq!(String::from_utf8_lossy(&got), "\x1b[1;3R\x1b[8;25;80t");
        assert_eq!(terminal.told, 0);
        Ok(())
    }

    /// libvterm answers the cursor report but not the size report, which
    /// is then answered from its screen, and counted.
    #[test]
    fn a_size_report_the_engine_does_not_give_is_told() -> Result<(), String> {
        let mut terminal = engine("libvterm")?;
        let got = terminal.take(b"ab\x1b[6n\x1b[18t")?;
        assert_eq!(String::from_utf8_lossy(&got), "\x1b[1;3R\x1b[8;25;80t");
        assert_eq!(terminal.told, 1);
        Ok(())
    }

    /// An engine whose origin mode cannot be read (vt100) has no rectangle
    /// read while the program may have set it: from DECSET 6 to DECRST 6,
    /// and not after RIS.
    #[test]
    fn origin_mode_unread_is_not_assumed_off() -> Result<(), String> {
        let mut terminal = engine("vt100")?;
        let got = terminal.take(b"\x1b[2;4r\x1b[?6h\x1b[1;0;1;1;1;1*y")?;
        assert!(got.is_empty(), "{got:?}");
        assert_eq!((terminal.told, terminal.unreadable), (0, 1));
        let got = terminal.take(b"\x1b[?6l\x1b[2;0;1;1;1;1*y\x1b[?6h\x1bc\x1b[3;0;1;1;1;1*y")?;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1bP2!~FFE0\x1b\\\x1bP3!~FFE0\x1b\\"
        );
        assert_eq!((terminal.told, terminal.unreadable), (2, 1));
        Ok(())
    }

    /// The other requests: the size report, origin mode set and reset,
    /// DECSTR and RIS; and what only looks like them.
    #[test]
    fn the_watched_requests_are_found() {
        let at = |bytes: &[u8]| match find(bytes) {
            Found::Request { request, .. } => Some(request),
            Found::None | Found::Partial { .. } => None,
        };
        assert_eq!(at(b"\x1b[18t"), Some(Request::Size));
        assert_eq!(at(b"\x1b[?6;69h"), Some(Request::Origin(true)));
        assert_eq!(at(b"\x1b[?6l"), Some(Request::Origin(false)));
        assert_eq!(at(b"\x1b[!p"), Some(Request::Reset));
        assert_eq!(at(b"x\x1bc"), Some(Request::Reset));
        for not in [
            &b"\x1b[?1006h"[..],
            b"\x1b[1;18t",
            b"\x1b[6h",
            b"\x1b[?18t",
            b"\x1b[>c",
        ] {
            assert_eq!(at(not), None, "{not:?}");
        }
    }
}
