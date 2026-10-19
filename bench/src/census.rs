//! The census's speed payloads (`fux-vt-best-panel/census.md`, F1, F7,
//! F17, F35 and E2, and the short-line probes): each a named workload of
//! its own, for a 50×200 screen, made the same way by `fux-bench` and by
//! `fux-vt/compare`, which both compile this file (compare by a `#[path]`).
//!
//! - **vtebench** (alacritty/vtebench `ead80032`, Apache-2.0 or MIT): each
//!   benchmark's output as its scripts write it at 50×200 through a PTY
//!   (`\n` sent as `\r\n`): a reset, its setup once, then its benchmark over
//!   and over. The three data files its scripts `cat` are vendored in
//!   `bench/payloads/vtebench/` with their licence.
//! - **kitty** (`kitten __benchmark__`, kovidgoyal/kitty `f7a16a93`): its
//!   six payloads' shapes, made by this code from their description, as
//!   kitty's code is GPL-3.0: random printable ASCII with newlines and tabs;
//!   CJK and other Unicode text; unique combining cells; CSI-heavy text;
//!   kitty-graphics images; long OSC strings. Each on the alternate screen.
//!   fux-vt keeps no kitty images, so `kitty:images` is ignored-protocol
//!   throughput, not image decoding.
//! - **avt** (asciinema/avt `4239deeb`, `benches/data`, Apache-2.0): its
//!   four 1 MiB captures, vendored in `bench/payloads/avt/` with its licence.
//! - **foot** (`scripts/generate-alt-random-writes.py`, dnkl/foot
//!   `cb277178`, MIT): its alternate-screen random writes with every option
//!   but sixel, ported here with this file's random numbers.
//! - **yes**: `y\r\n` and `y\n` over and over on the main screen, the
//!   census's short-line probes.
//! - **sixel**: an animation of whole-screen sixel frames, the shape of
//!   jerch/sixel-bench (Sintel as sixels), drawn and encoded by this code.
//!
//! Each payload is made to at least the size asked, in whole repeats.
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const ROWS: u16 = 50;
pub const COLS: u16 = 200;
/// Each payload's size, as the census fed them.
pub const SIZE: usize = 8 << 20;

/// A payload: its name, what it is, and how to make it, to a size, from
/// the vendored data in a directory.
pub struct Payload {
    pub name: &'static str,
    pub about: &'static str,
    pub make: fn(&Path, usize) -> Result<Vec<u8>, String>,
}

/// The vendored data in a checkout whose root is `root`.
pub fn data(root: &Path) -> PathBuf {
    root.join("bench/payloads")
}

pub const PAYLOADS: &[Payload] = &[
    Payload {
        name: "vtebench:cursor_motion",
        about: "vtebench: a letter at each cell, spiralling in, a CUP before each",
        make: |_, size| Ok(vtebench(b"", &cursor_motion(), size)),
    },
    Payload {
        name: "vtebench:dense_cells",
        about: "vtebench: full alternate screens, each cell with SGR 38;5, 48;5, bold, italic, underline",
        make: |_, size| Ok(vtebench(b"\x1b[?1049h", &dense_cells(), size)),
    },
    Payload {
        name: "vtebench:light_cells",
        about: "vtebench: full alternate screens of one letter, no escapes",
        make: |_, size| Ok(vtebench(b"\x1b[?1049h", &light_cells(), size)),
    },
    Payload {
        name: "vtebench:medium_cells",
        about: "vtebench: an escape-heavy vim session",
        make: |dir, size| {
            Ok(vtebench(
                b"",
                &read(dir, "vtebench/medium_cells.vim_session")?,
                size,
            ))
        },
    },
    Payload {
        name: "vtebench:scrolling",
        about: "vtebench: `y` lines scrolling the main screen into history",
        make: |_, size| Ok(vtebench(&yes_lines(100_001), b"y\n", size)),
    },
    Payload {
        name: "vtebench:scrolling_bottom_region",
        about: "vtebench: `y` lines scrolling a region of all rows but the last",
        make: |_, size| Ok(vtebench(&region(1, ROWS.saturating_sub(1)), b"y\n", size)),
    },
    Payload {
        name: "vtebench:scrolling_bottom_small_region",
        about: "vtebench: `y` lines scrolling the top half",
        make: |_, size| Ok(vtebench(&region(1, ROWS / 2), b"y\n", size)),
    },
    Payload {
        name: "vtebench:scrolling_fullscreen",
        about: "vtebench: full lines of one letter scrolling the main screen",
        make: |_, size| Ok(vtebench(&yes_lines(100_001), &fullscreen(), size)),
    },
    Payload {
        name: "vtebench:scrolling_top_region",
        about: "vtebench: `y` lines scrolling a region of all rows but the first",
        make: |_, size| Ok(vtebench(&region(2, ROWS), b"y\n", size)),
    },
    Payload {
        name: "vtebench:scrolling_top_small_region",
        about: "vtebench: `y` lines scrolling the bottom half",
        make: |_, size| Ok(vtebench(&region(ROWS / 2, ROWS), b"y\n", size)),
    },
    Payload {
        name: "vtebench:sync_medium_cells",
        about: "vtebench: an escape-heavy vim session in synchronized output",
        make: |dir, size| {
            Ok(vtebench(
                b"",
                &read(dir, "vtebench/sync_medium_cells.vim_session")?,
                size,
            ))
        },
    },
    Payload {
        name: "vtebench:unicode",
        about: "vtebench: a file of Unicode symbols on the alternate screen",
        make: |dir, size| {
            Ok(vtebench(
                b"\x1b[?1049h",
                &read(dir, "vtebench/unicode.symbols")?,
                size,
            ))
        },
    },
    Payload {
        name: "kitty:ascii",
        about: "kitty's shape: random printable ASCII, newlines and tabs",
        make: |_, size| Ok(alternate(&kitty_ascii(), size)),
    },
    Payload {
        name: "kitty:unicode",
        about: "kitty's shape: CJK text and other Unicode, newlines and tabs",
        make: |_, size| Ok(alternate(&kitty_unicode(), size)),
    },
    Payload {
        name: "kitty:unique_combining",
        about: "kitty's shape: cells of `a` and three combining marks, each cell unlike the others",
        make: |_, size| Ok(alternate(&kitty_combining(), size)),
    },
    Payload {
        name: "kitty:csi",
        about: "kitty's shape: short text among SGR, cursor, erase and mode sequences",
        make: |_, size| Ok(alternate(&kitty_csi(), size)),
    },
    Payload {
        name: "kitty:images",
        about: "kitty's shape: a 4 MiB kitty-graphics image in 4 KiB APC chunks, then its delete \
                (fux-vt ignores the protocol: ignored-protocol throughput)",
        make: |_, size| Ok(alternate(&kitty_images(), size)),
    },
    Payload {
        name: "kitty:long_escape_codes",
        about: "kitty's shape: OSC 6 strings of 8024 printable bytes",
        make: |_, size| Ok(alternate(&kitty_long_osc(), size)),
    },
    Payload {
        name: "avt:cacademo",
        about: "avt's capture of cacademo (libcaca's demo)",
        make: |dir, size| Ok(repeated(&read(dir, "avt/cacademo.txt")?, size)),
    },
    Payload {
        name: "avt:licenses",
        about: "avt's capture of licence texts printed in a shell",
        make: |dir, size| Ok(repeated(&read(dir, "avt/licenses.txt")?, size)),
    },
    Payload {
        name: "avt:licenses-lolcat",
        about: "avt's capture of licence texts through lolcat (24-bit colour)",
        make: |dir, size| Ok(repeated(&read(dir, "avt/licenses-lolcat.txt")?, size)),
    },
    Payload {
        name: "avt:mixed",
        about: "avt's capture of a mixed shell session",
        make: |dir, size| Ok(repeated(&read(dir, "avt/mixed.txt")?, size)),
    },
    Payload {
        name: "foot:alt-random-writes",
        about: "foot's alternate-screen random writes: colours, attributes, scrolls and regions",
        make: |_, size| Ok(foot(size)),
    },
    Payload {
        name: "yes:crlf",
        about: "`y\\r\\n` over and over on the main screen",
        make: |_, size| Ok(repeated(b"y\r\n", size)),
    },
    Payload {
        name: "yes:lf",
        about: "`y\\n` over and over on the main screen, no carriage return",
        make: |_, size| Ok(repeated(b"y\n", size)),
    },
    Payload {
        name: "sixel:frames",
        about: "sixel-bench's shape: whole-screen 16-colour sixel frames of moving stripes \
                (an engine without sixel discards them: for it, ignored-protocol throughput)",
        make: |_, size| Ok(sixel(size, 800, 600, 16)),
    },
    Payload {
        name: "sixel:256",
        about: "whole-screen 1920x1080 sixel frames in 256 colours, of moving stripes: sixel \
                decode (an engine without sixel discards them: for it, ignored-protocol \
                throughput)",
        make: |_, size| Ok(sixel(size, 1920, 1080, 256)),
    },
];

/// The named payload, made to `size`.
pub fn make(name: &str, dir: &Path, size: usize) -> Option<Result<Vec<u8>, String>> {
    PAYLOADS
        .iter()
        .find(|p| p.name == name)
        .map(|p| (p.make)(dir, size))
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = dir.join(name);
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `bytes` over and over, whole, until there are at least `total`.
fn repeated(bytes: &[u8], total: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total.saturating_add(bytes.len()));
    while !bytes.is_empty() && out.len() < total {
        out.extend_from_slice(bytes);
    }
    out
}

/// What a PTY sends for `bytes` with `onlcr`: each `\n` as `\r\n`.
fn onlcr(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// A vtebench sample: a reset, the setup, then the benchmark over and
/// over to `size`, through a PTY.
fn vtebench(setup: &[u8], benchmark: &[u8], size: usize) -> Vec<u8> {
    let mut out = b"\x1bc".to_vec();
    out.extend(onlcr(setup));
    let rest = size.saturating_sub(out.len());
    out.extend(repeated(&onlcr(benchmark), rest));
    out
}

fn yes_lines(n: usize) -> Vec<u8> {
    repeated(b"y\n", n.saturating_mul(2))
}

fn region(top: u16, bottom: u16) -> Vec<u8> {
    format!("\x1b[?1049h\x1b[{top};{bottom}r").into_bytes()
}

const LETTERS: std::ops::RangeInclusive<u8> = b'A'..=b'Z';

/// vtebench's `cursor_motion`, at 50×200: each letter drawn round the
/// screen's edge and spiralling in, a CUP before each.
fn cursor_motion() -> Vec<u8> {
    let mut out = Vec::new();
    for c in LETTERS.map(char::from) {
        let (mut left, mut right) = (1u16, COLS);
        let (mut top, mut bottom) = (1u16, ROWS);
        loop {
            let (mut column, mut line) = (left, top);
            let mut at = |line: u16, column: u16| {
                let _ = write!(out, "\x1b[{line};{column}H{c}");
            };
            while column < right {
                at(line, column);
                column = column.saturating_add(1);
            }
            while line < bottom {
                at(line, column);
                line = line.saturating_add(1);
            }
            while column > left {
                at(line, column);
                column = column.saturating_sub(1);
            }
            while line > top {
                at(line, column);
                line = line.saturating_sub(1);
            }
            left = left.saturating_add(1);
            top = top.saturating_add(1);
            right = right.saturating_sub(1);
            bottom = bottom.saturating_sub(1);
            if left > right || top > bottom {
                break;
            }
        }
    }
    out
}

/// vtebench's `dense_cells`, at 50×200.
fn dense_cells() -> Vec<u8> {
    let mut out = Vec::new();
    for (offset, c) in LETTERS.map(char::from).enumerate() {
        out.extend_from_slice(b"\x1b[H");
        for line in 1..=usize::from(ROWS) {
            for column in 1..=usize::from(COLS) {
                let index = line.saturating_add(column).saturating_add(offset);
                let step = index.checked_rem(156).unwrap_or(0);
                let fg = step.saturating_add(100);
                let bg = 355usize.saturating_sub(step);
                let _ = write!(out, "\x1b[38;5;{fg};48;5;{bg};1;3;4m{c}");
            }
        }
    }
    out
}

/// vtebench's `light_cells`, at 50×200.
fn light_cells() -> Vec<u8> {
    let cells = usize::from(ROWS).saturating_mul(usize::from(COLS));
    let mut out = Vec::new();
    for c in LETTERS {
        out.extend_from_slice(b"\x1b[H");
        out.extend(std::iter::repeat_n(c, cells));
    }
    out
}

/// vtebench's `scrolling_fullscreen` benchmark, at 200 columns.
fn fullscreen() -> Vec<u8> {
    let mut out = Vec::new();
    for c in LETTERS {
        out.extend(std::iter::repeat_n(c, usize::from(COLS)));
        out.push(b'\n');
    }
    out
}

/// A seeded source of choices (splitmix64): this file's own, so both
/// crates make the same bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `n`; 0 for an empty range.
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }

    fn pick<T: Copy>(&mut self, items: &[T], or: T) -> T {
        items.get(self.below(items.len())).copied().unwrap_or(or)
    }
}

/// Printable ASCII.
const PRINTABLE: std::ops::RangeInclusive<u8> = b' '..=b'~';

/// `n` random bytes of `alphabet`.
fn random(r: &mut Rng, n: usize, alphabet: &[u8]) -> Vec<u8> {
    (0..n).map(|_| r.pick(alphabet, b' ')).collect()
}

/// `\x1b[?1049h`, then `payload` over and over to `size`.
fn alternate(payload: &[u8], size: usize) -> Vec<u8> {
    let mut out = b"\x1b[?1049h".to_vec();
    out.extend(repeated(payload, size.saturating_sub(out.len())));
    out
}

fn kitty_ascii() -> Vec<u8> {
    let mut alphabet: Vec<u8> = PRINTABLE.collect();
    alphabet.extend_from_slice(b"\n\t");
    random(&mut Rng(1), (2 << 20) + 13, &alphabet)
}

/// A line of 640 random CJK ideographs with fullwidth punctuation, other
/// Unicode (quotes, dashes, symbols, Latin-1, Greek, emoji, combining
/// marks), then a newline and a tab; 1024 of them.
fn kitty_unicode() -> Vec<u8> {
    let mut r = Rng(2);
    let mut line = String::new();
    for n in 0..640u32 {
        let ideograph = 0x4e00u32.saturating_add(u32::try_from(r.below(0x5000)).unwrap_or(0));
        line.push(char::from_u32(ideograph).unwrap_or('字'));
        if n.checked_rem(9) == Some(8) {
            line.push(r.pick(&['，', '。', '：', '；', '？', '！', '「', '」'], '，'));
        }
    }
    line.push_str(
        "‘’“”‹›«»‚„ 😀😛😇😈😉😍😎😮👍👎 —–§¶†‡©®™ →⇒•·°±−×÷¼½¾ …µ¢£€¿¡¨´¸ˆ˜ \
         ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖØ ŒŠÙÚÛÜÝŸÞßàáâãäåæçèéêëìíîïðñòóôõöøœšùúûüýÿþ \
         αβγδεζηθΩ∞ e\u{301}n\u{303}o\u{308}a\u{30a}\u{323}x\u{334}\u{35c}",
    );
    line.push_str("\n\t");
    repeated(line.as_bytes(), line.len().saturating_mul(1024))
}

/// 256 Ki cells, each `a` and three combining marks from U+0300–U+036F
/// that spell its number in base 0x70: no two alike.
fn kitty_combining() -> Vec<u8> {
    let mut out = String::new();
    for n in 0..(256u32 << 10) {
        out.push('a');
        let mut q = n;
        for _ in 0..3 {
            let mark = 0x300u32.saturating_add(q.checked_rem(0x70).unwrap_or(0));
            out.push(char::from_u32(mark).unwrap_or('\u{300}'));
            q = q.checked_div(0x70).unwrap_or(0);
        }
    }
    out.into_bytes()
}

/// About 1 MiB of short text among CSI sequences: SGR resets, colours by
/// index and by RGB in both forms, underline styles and colours, cursor
/// motion, line erases, REP and DECCKM.
fn kitty_csi() -> Vec<u8> {
    let mut r = Rng(3);
    let mut text: Vec<u8> = PRINTABLE.collect();
    text.extend_from_slice(b"\n\t");
    let mut out = Vec::new();
    while out.len() < (1 << 20) + 17 {
        match r.below(100) {
            0..10 => {
                let n = r.below(72).saturating_add(1);
                out.extend(random(&mut r, n, &text));
            }
            10..30 => out.extend_from_slice(b"\x1b[m\x1b[?1h\x1b[H"),
            30..40 => out.extend_from_slice(b"\x1b[1;2;3;4:3;31m"),
            40..50 => out.extend_from_slice(b"\x1b[38:5:24;48:2:125:136:147m"),
            50..60 => out.extend_from_slice(b"\x1b[58;5;44;2m"),
            60..80 => out.extend_from_slice(b"\x1b[m\x1b[10A\x1b[3E\x1b[2K"),
            _ => out.extend_from_slice(b"\x1b[39m\x1b[10`a\x1b[100b\x1b[?1l"),
        }
    }
    out.extend_from_slice(b"\x1b[m");
    out
}

/// A 1024×1024 RGBA image of zeros sent by the kitty graphics protocol,
/// its base64 in 4096-byte chunks, then deleted.
fn kitty_images() -> Vec<u8> {
    const PIXELS: usize = 4 << 20;
    // Base64 of zeros: "AAAA" for every three bytes, and the rest padded.
    let mut b64 = vec![b'A'; (PIXELS / 3).saturating_mul(4)];
    b64.extend_from_slice(match PIXELS.checked_rem(3) {
        Some(1) => b"AA==",
        Some(2) => b"AAA=",
        _ => b"",
    });
    let mut out = Vec::new();
    let mut rest = b64.as_slice();
    let mut first = true;
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at_checked(4096).unwrap_or((rest, &[]));
        rest = tail;
        let head = if first {
            "i=12345,q=2,a=t,f=32,s=1024,v=1024,"
        } else {
            ""
        };
        first = false;
        let more = u8::from(!rest.is_empty());
        let _ = write!(out, "\x1b_G{head}m={more};");
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out.extend_from_slice(b"\x1b_Ga=d,d=I,i=12345,q=2\x1b\\");
    out
}

/// 1024 OSC 6 strings of 8024 random printable bytes, each ended by BEL.
fn kitty_long_osc() -> Vec<u8> {
    let mut r = Rng(4);
    let printable: Vec<u8> = PRINTABLE.collect();
    let mut out = Vec::new();
    for _ in 0..1024 {
        out.extend_from_slice(b"\x1b]6;");
        out.extend(random(&mut r, 8024, &printable));
        out.push(0x07);
    }
    out
}

/// foot's `generate-alt-random-writes.py --colors-regular --colors-bright
/// --colors-256 --colors-rgb --scroll --scroll-region --attr-bold
/// --attr-italic --attr-underline` at 50×200: random runs of a random
/// character at random places, in random colours and attributes, with a
/// scroll now and then; to at least `size` bytes.
fn foot(size: usize) -> Vec<u8> {
    let (lines, cols) = (usize::from(ROWS), usize::from(COLS));
    let alphabet: Vec<char> =
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRTSTUVWXYZ0123456789 öäå 👨👩🧒🇸🇪"
            .chars()
            .collect();
    let mut r = Rng(5);
    let mut out = b"\x1b[?1049h".to_vec();
    while out.len() < size {
        if r.below(256) == 0 {
            out.extend_from_slice(b"\x1b[m");
            if r.below(256) == 0 {
                let (top, bottom) = (r.below(3), r.below(3));
                let _ = write!(out, "\x1b[{top};{}r", lines.saturating_sub(bottom));
            }
            let scroll = r.below(lines.saturating_sub(1));
            let reverse = r.below(2) == 1;
            if !reverse && r.below(2) == 1 {
                let _ = write!(out, "\x1b[{lines};{cols}H");
                out.extend(std::iter::repeat_n(b'\n', scroll));
            } else {
                let way = if reverse { 'T' } else { 'S' };
                let _ = write!(out, "\x1b[{}{way}", scroll.saturating_add(1));
            }
            continue;
        }
        let (row, col) = (r.below(lines), r.below(cols));
        let c = r.pick(&alphabet, 'x');
        let repeat = r.below(cols.saturating_sub(col).saturating_add(1));
        let _ = write!(
            out,
            "\x1b[{};{}H",
            row.saturating_add(1),
            col.saturating_add(1)
        );
        let bg = r.below(2) == 1;
        let variant = r.below(5);
        match variant {
            // None, regular, bright, the 256-colour cube, RGB.
            1 | 2 => {
                let base: usize = if bg { 40 } else { 30 };
                let bright = if variant == 2 { 60 } else { 0 };
                let sgr = base.saturating_add(bright).saturating_add(r.below(8));
                let _ = write!(out, "\x1b[{sgr}m");
            }
            3 => {
                let base = if bg { 48 } else { 38 };
                let index = r.below(256);
                if r.below(2) == 1 {
                    let _ = write!(out, "\x1b[{base};5;{index}m");
                } else {
                    let _ = write!(out, "\x1b[{base}:5:{index}m");
                }
            }
            4 => {
                let base = if bg { 48 } else { 38 };
                let (red, green, blue) = (r.below(256), r.below(256), r.below(256));
                if r.below(2) == 1 {
                    let _ = write!(out, "\x1b[{base};2;{red};{green};{blue}m");
                } else {
                    let _ = write!(out, "\x1b[{base}:2::{red}:{green}:{blue}m");
                }
            }
            _ => {}
        }
        for attribute in [1, 3, 4] {
            if r.below(5) == 0 {
                let _ = write!(out, "\x1b[{attribute}m");
            }
        }
        let mut glyph = [0u8; 4];
        let glyph = c.encode_utf8(&mut glyph).as_bytes();
        for _ in 0..repeat {
            out.extend_from_slice(glyph);
        }
        if r.below(2) == 1 {
            let reset: &[u8] = r.pick(&[b"\x1b[m", b"\x1b[39m", b"\x1b[49m"], b"\x1b[m");
            out.extend_from_slice(reset);
        }
    }
    out.extend_from_slice(b"\x1b[m\x1b[r\x1b[?1049l");
    out
}

/// Whole-screen sixel frames, `width` by `height` pixels in `colours`
/// colours, of diagonal stripes that move a little each frame; to at least
/// `size` bytes. Each frame homes the cursor and is one DCS: raster
/// attributes, the palette, then each six-pixel band as one
/// run-length-encoded row per colour it has.
fn sixel(size: usize, width: usize, height: usize, colours: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut frame = 0usize;
    while out.len() < size {
        let _ = write!(out, "\x1b[H\x1bP0;1;0q\"1;1;{width};{height}");
        for c in 0..colours {
            let level = |k: usize| c.saturating_mul(k).checked_rem(101).unwrap_or(0);
            let _ = write!(out, "#{c};2;{};{};{}", level(7), level(13), level(29));
        }
        let colour = |x: usize, y: usize| {
            x.saturating_add(y)
                .saturating_add(frame.saturating_mul(8))
                .checked_div(25)
                .and_then(|band| band.checked_rem(colours))
                .unwrap_or(0)
        };
        // Each band's sixels by colour, made in one pass over its pixels;
        // a colour no pixel of the band has is left out.
        let mut bits = vec![0u8; colours.saturating_mul(width)];
        for band in 0..height.div_ceil(6) {
            let top = band.saturating_mul(6);
            bits.fill(0);
            for x in 0..width {
                for dy in 0..6u8 {
                    let y = top.saturating_add(usize::from(dy));
                    let at = colour(x, y).saturating_mul(width).saturating_add(x);
                    if let (true, Some(b)) = (y < height, bits.get_mut(at)) {
                        *b |= 1u8.checked_shl(dy.into()).unwrap_or(0);
                    }
                }
            }
            for c in 0..colours {
                let row = bits
                    .get(c.saturating_mul(width)..c.saturating_add(1).saturating_mul(width))
                    .unwrap_or_default();
                if row.iter().all(|&b| b == 0) {
                    continue;
                }
                let _ = write!(out, "#{c}");
                let mut run: Option<(u8, usize)> = None;
                for &b in row {
                    let glyph = b'?'.saturating_add(b);
                    run = match run {
                        Some((g, n)) if g == glyph => Some((g, n.saturating_add(1))),
                        Some(done) => {
                            sixel_run(&mut out, done);
                            Some((glyph, 1))
                        }
                        None => Some((glyph, 1)),
                    };
                }
                if let Some(done) = run {
                    sixel_run(&mut out, done);
                }
                out.push(b'$');
            }
            out.push(b'-');
        }
        out.extend_from_slice(b"\x1b\\");
        frame = frame.saturating_add(1);
    }
    out
}

/// `n` sixels `glyph`: repeated by `!n` when that is shorter.
fn sixel_run(out: &mut Vec<u8>, (glyph, n): (u8, usize)) {
    if n > 3 {
        let _ = write!(out, "!{n}");
        out.push(glyph);
    } else {
        out.extend(std::iter::repeat_n(glyph, n));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn dir() -> std::path::PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        // bench/ or fux-vt/compare/.
        let root = if manifest.ends_with("compare") {
            manifest.join("../..")
        } else {
            manifest.join("..")
        };
        super::data(&root)
    }

    /// Every payload is made, to at least the size asked, and is the same
    /// each time.
    #[test]
    fn every_payload_is_made_alike() {
        for p in super::PAYLOADS {
            let made = (p.make)(&dir(), 1 << 16);
            let again = (p.make)(&dir(), 1 << 16);
            assert!(
                made.as_ref().is_ok_and(|b| b.len() >= 1 << 16),
                "{}: {:?}",
                p.name,
                made.as_ref().err()
            );
            assert!(made == again, "{}", p.name);
        }
    }

    /// How often `needle` is in `hay`.
    fn count(hay: &[u8], needle: &[u8]) -> usize {
        (0..hay.len())
            .filter(|&i| hay.get(i..).is_some_and(|rest| rest.starts_with(needle)))
            .count()
    }

    /// The shapes the census describes, checked by hand-counted bytes.
    #[test]
    fn the_payloads_have_their_shapes() {
        // vtebench scrolling: a reset, 100,001 setup lines, then `y` lines,
        // every newline sent as CR LF.
        let scrolling = super::make("vtebench:scrolling", &dir(), 1 << 20);
        let scrolling = scrolling.and_then(Result::ok).unwrap_or_default();
        assert!(scrolling.starts_with(b"\x1bcy\r\ny\r\n"));
        assert_eq!(count(&scrolling, b"\n"), count(&scrolling, b"\r\n"));
        // cursor_motion visits each of the 10,000 cells once per letter.
        assert_eq!(count(&super::cursor_motion(), b"\x1b["), 26 * 10_000);
        // Unique combining cells: 256 Ki of `a`, each with three marks.
        let combining = super::kitty_combining();
        assert_eq!(count(&combining, b"a"), 256 << 10);
        assert_eq!(combining.len(), (256 << 10) * 7);
        // The image: 4 MiB as base64 (5,592,408 bytes) in 1,366 chunks,
        // and its delete.
        let images = super::kitty_images();
        assert_eq!(count(&images, b"\x1b_G"), 1366 + 1);
        assert_eq!(count(&images, b"m=0;"), 1);
    }
}
