//! The synthetic workloads, as `fux-vt/compare`'s `bench` makes them
//! (`fux-vt/compare/src/bench.rs`, modelled on alacritty's vtebench): the
//! same generators, copied, so both crates time the same bytes. Each is for
//! a 50×200 screen.
use crate::rng::Rng;
use std::fmt::Write;

pub const ROWS: u16 = 50;
pub const COLS: u16 = 200;

/// A workload: its name, what it is, and how to make `bytes` of it.
pub type Generator = (&'static str, &'static str, fn(&mut Rng, usize) -> Vec<u8>);

pub const GENERATORS: &[Generator] = &[
    ("ascii", "lines of plain ASCII text, scrolling", ascii),
    (
        "dense-cells",
        "full screens where every cell has its own 256-colour foreground and background",
        dense_cells,
    ),
    (
        "medium-cells",
        "full screens of words, an SGR change every few cells",
        medium_cells,
    ),
    (
        "cursor-motion",
        "a glyph at a random position, over and over",
        cursor_motion,
    ),
    (
        "scrolling",
        "short lines, scrolling the whole screen",
        scrolling,
    ),
    (
        "scroll-region",
        "lines scrolling inside a region of half the screen",
        scroll_region,
    ),
    (
        "unicode",
        "CJK, accented text, combining marks and emoji, scrolling",
        unicode,
    ),
];

/// `bytes` of the named workload, from seed 1, as `compare` makes it.
pub fn make(name: &str, bytes: usize) -> Option<Vec<u8>> {
    GENERATORS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, make)| make(&mut Rng::new(1), bytes))
}

fn ascii(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let words = [
        "lorem ",
        "ipsum ",
        "dolor ",
        "sit ",
        "amet, ",
        "consectetur ",
        "adipiscing ",
        "elit ",
    ];
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        let mut line = 0usize;
        while line < 150 {
            let word = r.pick(&words).copied().unwrap_or("x ");
            out.extend_from_slice(word.as_bytes());
            line = line.saturating_add(word.len());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn dense_cells(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(b"\x1b[H");
        for _ in 0..usize::from(ROWS).saturating_mul(usize::from(COLS)) {
            let glyph = char::from(b'A'.saturating_add(u8::try_from(r.below(26)).unwrap_or(0)));
            let _ = write!(
                Text(&mut out),
                "\x1b[38;5;{};48;5;{}m{glyph}",
                r.below(256),
                r.below(256)
            );
        }
    }
    out
}

fn medium_cells(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let words = [
        "the ", "quick ", "brown ", "fox ", "jumps ", "over ", "lazy ", "dog ",
    ];
    let sgr = [
        "\x1b[1m",
        "\x1b[0m",
        "\x1b[31m",
        "\x1b[4m",
        "\x1b[7m",
        "\x1b[38;2;10;200;30m",
        "\x1b[44m",
        "\x1b[m",
    ];
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(b"\x1b[H");
        for _ in 0..usize::from(ROWS)
            .saturating_mul(usize::from(COLS))
            .checked_div(6)
            .unwrap_or(1)
        {
            out.extend_from_slice(r.pick(&sgr).copied().unwrap_or("").as_bytes());
            out.extend_from_slice(r.pick(&words).copied().unwrap_or("").as_bytes());
        }
    }
    out
}

fn cursor_motion(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        let (row, col) = (
            r.below(usize::from(ROWS)).saturating_add(1),
            r.below(usize::from(COLS)).saturating_add(1),
        );
        let _ = write!(Text(&mut out), "\x1b[{row};{col}H*");
    }
    out
}

fn scrolling(_: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(b"y\r\n");
    }
    out
}

fn scroll_region(_: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    let _ = write!(
        Text(&mut out),
        "\x1b[{};{}r\x1b[{}H",
        ROWS / 4,
        ROWS / 4 * 3,
        ROWS / 4 * 3
    );
    while out.len() < bytes {
        out.extend_from_slice(b"line in a region\r\n");
    }
    out.extend_from_slice(b"\x1b[r");
    out
}

fn unicode(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let pieces = [
        "界", "全角", "é", "ñandú ", "e\u{301}", "👍", "👍🏽", "🇺🇸", "Ж", "ष्", "a", " ",
    ];
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        for _ in 0..60 {
            out.extend_from_slice(r.pick(&pieces).copied().unwrap_or("").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `write!` into bytes.
struct Text<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Text<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// Each workload is the size asked for, or at most a screen more, and
    /// the same every time it is made.
    #[test]
    fn workloads_are_their_size_and_reproducible() {
        for (name, _, _) in super::GENERATORS {
            let one = super::make(name, 1 << 16).unwrap_or_default();
            assert!(one.len() >= 1 << 16, "{name}");
            assert!(one.len() < (1 << 16) + (1 << 18), "{name}");
            assert_eq!(super::make(name, 1 << 16), Some(one), "{name}");
        }
    }
}
