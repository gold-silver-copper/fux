//! UTF-8 decoding against an independent model, `String::from_utf8_lossy`,
//! which substitutes one U+FFFD for each maximal subpart of an ill-formed
//! sequence (the Unicode Standard 17.0, 3.9, "U+FFFD Substitution of
//! Maximal Subparts"): random bytes, fed in random pieces, print what it
//! decodes them to, a character a cell, wrapping at the right edge. The
//! model's C1 characters (U+0080 to U+009F, encoded in UTF-8) are dropped,
//! as fux-vt ignores them (README, "Text, UTF-8"), and so is DEL, a control
//! that prints nothing.
//!
//! Every character generated is one column wide, so the screen is the
//! model's text cut into rows of the screen's width; and input ends with an
//! ASCII letter, which completes or breaks off a sequence left open, as the
//! model, which sees the end, does.

use fux_vt::{Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

/// A small deterministic generator (splitmix64), so a failure names its case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    /// Below `n`, or 0 for an `n` of 0.
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }
    fn byte(&mut self, low: u8, high: u8) -> u8 {
        let span = usize::from(high.saturating_sub(low)).saturating_add(1);
        low.saturating_add(u8::try_from(self.below(span)).unwrap_or(0))
    }
}

/// Well-formed characters one column wide in every width rule (none of
/// them East Asian ambiguous, wide or combining), of each encoded length.
const WIDE_ONE: &[char] = &['ĉ', 'ա', 'א', 'ก', 'ｱ', '\u{10348}', '\u{1d400}'];

/// Ill-formed forms the tables name: overlong, surrogate, past U+10FFFF.
const ILL_FORMED: &[&[u8]] = &[
    b"\xc0\xaf",
    b"\xc1\xbf",
    b"\xe0\x80\xaf",
    b"\xe0\x9f\xbf",
    b"\xed\xa0\x80",
    b"\xed\xbf\xbf",
    b"\xf0\x80\x80\xaf",
    b"\xf0\x8f\xbf\xbf",
    b"\xf4\x90\x80\x80",
    b"\xf5\x80\x80\x80",
];

/// One piece of input: a character, whole or cut short, a C1 character,
/// DEL, an ill-formed form or a byte anywhere from 0x80 up.
fn piece(r: &mut Rng, out: &mut Vec<u8>) {
    let mut buffer = [0; 4];
    match r.below(8) {
        0 | 1 => out.push(r.byte(0x20, 0x7e)),
        2 => out.push(0x7f),
        3 => out.extend_from_slice(&[0xc2, r.byte(0x80, 0x9f)]),
        4 => {
            let c = WIDE_ONE
                .get(r.below(WIDE_ONE.len()))
                .copied()
                .unwrap_or('a');
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
        5 => {
            let c = WIDE_ONE
                .get(r.below(WIDE_ONE.len()))
                .copied()
                .unwrap_or('a');
            let bytes = c.encode_utf8(&mut buffer).as_bytes();
            let cut = r.below(bytes.len().saturating_sub(1)).saturating_add(1);
            out.extend_from_slice(bytes.get(..cut).unwrap_or_default());
        }
        6 => out.extend_from_slice(
            ILL_FORMED
                .get(r.below(ILL_FORMED.len()))
                .copied()
                .unwrap_or_default(),
        ),
        _ => out.push(r.byte(0x80, 0xff)),
    }
}

/// Random pieces ending in `Z`. Bytes from 0x80 up may happen to make a
/// well-formed character, which may be wide or combining: input whose
/// model has a character other than ASCII, U+FFFD, C1 and `WIDE_ONE` is
/// drawn again.
fn input(r: &mut Rng) -> Vec<u8> {
    loop {
        let mut bytes = Vec::new();
        for _ in 0..=r.below(24) {
            piece(r, &mut bytes);
        }
        bytes.push(b'Z');
        let known = |c: char| {
            c.is_ascii()
                || c == '\u{fffd}'
                || ('\u{80}'..='\u{9f}').contains(&c)
                || WIDE_ONE.contains(&c)
        };
        if String::from_utf8_lossy(&bytes).chars().all(known) {
            return bytes;
        }
    }
}

/// The model's screen: `from_utf8_lossy`'s text without C1 and DEL, cut
/// into rows of `cols`, each row's blank tail trimmed.
fn model(bytes: &[u8], rows: u16, cols: u16) -> Vec<String> {
    let text: Vec<char> = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|&c| !('\u{7f}'..='\u{9f}').contains(&c))
        .collect();
    let mut screen = Vec::new();
    let mut rest = text.as_slice();
    while let Some((row, tail)) = rest.split_at_checked(usize::from(cols).min(rest.len()))
        && !row.is_empty()
    {
        screen.push(row.iter().collect::<String>().trim_end().to_owned());
        rest = tail;
    }
    screen.resize(usize::from(rows), String::new());
    screen
}

/// Random input in random pieces prints what the model decodes it to.
#[test]
fn utf8_decodes_as_from_utf8_lossy_in_any_chunking() -> Result {
    let mut r = Rng(0x0075_7466_0038_0001);
    let mut substituted = 0usize;
    for case in 0..20_000 {
        let bytes = input(&mut r);
        let cols = u16::try_from(r.below(16).saturating_add(5))?;
        // Rows enough that nothing scrolls, the last one empty.
        let rows = u16::try_from(bytes.len().div_ceil(usize::from(cols)).saturating_add(1))?;
        let mut parser = Parser::new(Size::new(rows, cols)?, 0)?;
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let size = if r.below(4) == 0 {
                rest.len()
            } else {
                r.below(6).saturating_add(1)
            };
            let (head, tail) =
                (rest.split_at_checked(size.min(rest.len()))).ok_or("a split past the end")?;
            parser.process(head)?;
            rest = tail;
        }
        let want = model(&bytes, rows, cols);
        substituted = substituted
            .saturating_add(want.iter().map(|row| row.matches('\u{fffd}').count()).sum());
        assert_eq!(
            lines(&parser),
            want,
            "case {case}: {cols} columns, bytes {bytes:02x?}"
        );
    }
    // The generator reaches what it is for.
    assert!(substituted > 20_000, "{substituted} substitutions");
    Ok(())
}
