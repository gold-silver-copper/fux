//! The colours a program sets, queries and resets, with
//! [`crate::Options::palette`]: the 256-colour palette (OSC 4, OSC 104),
//! xterm's special colours (OSC 5, OSC 105, and OSC 4 past the palette) and
//! the dynamic colours (OSC 10 to 19, OSC 110 to 119), as xterm's ctlseqs
//! give them ("Operating System Commands") and xterm 411 reads them.
//!
//! A colour is kept as xterm keeps it, 8 bits a channel (what its 24-bit
//! visual holds), and reported as xterm reports it, each channel's byte
//! twice: `rgb:cdcd/0000/0000`. A specification is read as X11's
//! XParseColor reads it, in the two forms ctlseqs names: `rgb:R/G/B`, each
//! channel one to four hex digits scaled to 16 bits (`f` is `ffff`), and
//! `#RGB`, `#RRGGBB`, `#RRRGGGBBB` and `#RRRRGGGGBBBB`, the digits the
//! channel's high bits (`#fff` is `f000`); of the 16 bits the high byte is
//! kept, so `#fff` reports `f0f0`, as xterm does. Colour names and the
//! device-independent spaces (`rgbi:`, `CIEXYZ:` and the rest) are not
//! read: a specification fux-vt cannot read changes nothing.
//!
//! What fux-vt keeps is what the program set, and what the host says its
//! terminal shows for palette entries 0 to 15 ([`crate::Screen::set_host_color`]):
//! the colours themselves are the terminal's. A palette entry the program
//! has not set is answered with the host's colour for it, if the host gave
//! one, else with xterm's default, which is what `TERM=xterm-256color`
//! means by it (see `DEFAULT`); a dynamic colour it has not set is asked of the host
//! ([`crate::Event::ColorQuery`], as without the palette), which knows its
//! terminal's; a special colour it has not set is not answered, xterm's
//! default being the terminal's foreground, which fux-vt does not know.
use crate::parser::{Event, Sink};

/// A colour: red, green and blue, 8 bits each.
pub(crate) type Rgb = [u8; 3];

/// How many special colours xterm has (ctlseqs, OSC 5): bold, underline,
/// blink, reverse and italic.
const SPECIALS: u16 = 5;
/// The first OSC 4 colour number past the palette: the special colours.
const PAST_PALETTE: u16 = 256;

/// xterm's colours 0 to 15 (its resources `color0` to `color15`, compiled
/// in: black, red3, green3, yellow3, blue2, magenta3, cyan3, gray90,
/// gray50, red, green, yellow, rgb:5c/5c/ff, magenta, cyan and white), as
/// xterm 411 reports them.
const SIXTEEN: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// The default of palette entry `index`, as xterm 411 reports it: the 16
/// colours above; 16 to 231 the 6×6×6 cube, each channel 0 or 55 + 40 n;
/// 232 to 255 the grey ramp, 8 + 10 n. Ghostty's cube and ramp are the
/// same; its 16 are its own theme's.
pub(crate) fn default(index: u8) -> Rgb {
    let level = |n: u8| {
        if n == 0 {
            0
        } else {
            n.saturating_mul(40).saturating_add(55)
        }
    };
    match index {
        0..=15 => SIXTEEN.get(usize::from(index)).copied().unwrap_or_default(),
        16..=231 => {
            let n = index.saturating_sub(16);
            let channel = |weight: u8| {
                level(
                    n.checked_div(weight)
                        .unwrap_or(0)
                        .checked_rem(6)
                        .unwrap_or(0),
                )
            };
            [channel(36), channel(6), channel(1)]
        }
        _ => {
            let grey = index
                .saturating_sub(232)
                .saturating_mul(10)
                .saturating_add(8);
            [grey, grey, grey]
        }
    }
}

/// How many palette entries the host can give its terminal's colour for:
/// 0 to 15, the ones themes change.
pub(crate) const HOST_ENTRIES: usize = 16;

/// The colours a program set, each `None` while it is the terminal's own,
/// and the host's colours for palette entries 0 to 15.
#[derive(Clone, Debug)]
pub(crate) struct Colours {
    palette: [Option<Rgb>; 256],
    special: [Option<Rgb>; 5],
    /// OSC 10 to 19.
    dynamic: [Option<Rgb>; 10],
    /// What the host's terminal shows for palette entries 0 to 15: what a
    /// query of an entry the program has not set is answered with. No
    /// reset of the program's colours touches them.
    host: [Option<Rgb>; HOST_ENTRIES],
}

impl Default for Colours {
    fn default() -> Self {
        Self {
            palette: [None; 256],
            special: [None; 5],
            dynamic: [None; 10],
            host: [None; HOST_ENTRIES],
        }
    }
}

impl Colours {
    /// Palette entry `index`, if the program set it.
    pub(crate) fn palette(&self, index: u8) -> Option<Rgb> {
        self.palette.get(usize::from(index)).copied().flatten()
    }
    /// Dynamic colour `number` (10 to 19), if the program set it.
    pub(crate) fn dynamic(&self, number: u8) -> Option<Rgb> {
        let i = usize::from(number.checked_sub(10)?);
        self.dynamic.get(i).copied().flatten()
    }
    /// The host's colour for palette entry `index`, if it gave one.
    pub(crate) fn host(&self, index: u8) -> Option<Rgb> {
        self.host.get(usize::from(index)).copied().flatten()
    }
    /// Sets the host's colour for palette entry `index` (0 to 15), or
    /// clears it; whether `index` is one the host can give.
    pub(crate) fn set_host(&mut self, index: u8, colour: Option<Rgb>) -> bool {
        match self.host.get_mut(usize::from(index)) {
            Some(slot) => {
                *slot = colour;
                true
            }
            None => false,
        }
    }
    /// Whether the program set a palette entry or a dynamic colour: what
    /// a host that draws the screen needs to know of.
    pub(crate) fn changed(&self) -> bool {
        self.palette
            .iter()
            .chain(&self.dynamic)
            .any(Option::is_some)
    }
    /// The palette back to the terminal's own (OSC 104 alone, DECSTR and
    /// RIS, as xterm resets it).
    pub(crate) fn reset_palette(&mut self) {
        self.palette = [None; 256];
    }
    /// The slot of OSC 4's colour `number`: a palette entry, or a special
    /// colour past them.
    fn slot(&mut self, number: u16) -> Option<&mut Option<Rgb>> {
        match number.checked_sub(PAST_PALETTE) {
            None => self.palette.get_mut(usize::from(number)),
            Some(special) => self.special.get_mut(usize::from(special)),
        }
    }
}

/// `text` as C's `atoi` reads it, as xterm reads OSC 4's colour numbers:
/// leading white space, a sign, and the digits that follow, 0 if none; a
/// number too large for an `i32` is as large as one.
fn atoi(text: &[u8]) -> i32 {
    let mut i = 0usize;
    while text.get(i).is_some_and(|b| b" \t\n\x0b\x0c\r".contains(b)) {
        i = i.saturating_add(1);
    }
    let negative = text.get(i) == Some(&b'-');
    if matches!(text.get(i), Some(b'-' | b'+')) {
        i = i.saturating_add(1);
    }
    let mut magnitude = 0i32;
    while let Some(d) = text.get(i).filter(|b| b.is_ascii_digit()) {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i32::from(d.saturating_sub(b'0')));
        i = i.saturating_add(1);
    }
    if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    }
}

/// A channel of `digits` hex digits (at most four) read as XParseColor
/// reads `rgb:`, scaled to 16 bits.
fn scaled(digits: &[u8]) -> Option<u16> {
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
    let bits = u32::try_from(digits.len()).ok()?.checked_mul(4)?;
    let largest = 1u32.checked_shl(bits)?.checked_sub(1)?;
    u16::try_from(value.checked_mul(0xffff)?.checked_div(largest)?).ok()
}

/// A colour specification, as xterm reads it (see the module's
/// documentation); `None` for one fux-vt does not read.
pub(crate) fn parse(spec: &[u8]) -> Option<Rgb> {
    let sixteen: [u16; 3] = if let Some(hex) = spec.strip_prefix(b"#") {
        // Three channels of 1 to 4 digits each, the digits the high bits.
        let n = hex.len().checked_div(3)?;
        if n == 0 || hex.len().checked_rem(3) != Some(0) || n > 4 {
            return None;
        }
        let shift = u32::try_from(n).ok()?.checked_mul(4)?;
        let channel = |i: usize| -> Option<u16> {
            let digits = hex.get(i.checked_mul(n)?..i.checked_add(1)?.checked_mul(n)?)?;
            let value = u16::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
            value.checked_shl(16u32.checked_sub(shift)?)
        };
        [channel(0)?, channel(1)?, channel(2)?]
    } else {
        // `rgb:`, its prefix in either case.
        let prefix = spec.get(..4)?;
        if !prefix.eq_ignore_ascii_case(b"rgb:") {
            return None;
        }
        let mut channels = spec.get(4..)?.split(|b| *b == b'/');
        let colour = [
            scaled(channels.next()?)?,
            scaled(channels.next()?)?,
            scaled(channels.next()?)?,
        ];
        if channels.next().is_some() {
            return None;
        }
        colour
    };
    Some(sixteen.map(|c| {
        let [high, _] = c.to_be_bytes();
        high
    }))
}

/// xterm's answer: `OSC command ; index ; rgb:RRRR/GGGG/BBBB` (no index for
/// a dynamic colour), each channel's byte twice, ended as the query was.
/// Built byte by byte rather than formatted: zellij asks all 256 entries
/// as it starts, and formatting cost more than the rest of the work.
fn report(sink: &mut impl Sink, command: u16, index: Option<u16>, colour: Rgb, bel: bool) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Answer::default();
    out.push(b"\x1b]");
    out.decimal(command);
    if let Some(index) = index {
        out.byte(b';');
        out.decimal(index);
    }
    out.push(b";rgb:");
    for (i, channel) in colour.into_iter().enumerate() {
        if i > 0 {
            out.byte(b'/');
        }
        let high = HEX.get(usize::from(channel >> 4)).copied().unwrap_or(b'0');
        let low = HEX.get(usize::from(channel & 0xf)).copied().unwrap_or(b'0');
        out.push(&[high, low, high, low]);
    }
    out.push(if bel { b"\x07" } else { b"\x1b\\" });
    sink.reply(out.bytes.get(..out.len).unwrap_or_default());
}

/// An answer being built: the longest, `OSC 4 ; 260 ; rgb:`, three
/// channels and ST, is 31 bytes.
#[derive(Default)]
struct Answer {
    bytes: [u8; 32],
    len: usize,
}

impl Answer {
    fn byte(&mut self, b: u8) {
        if let Some(slot) = self.bytes.get_mut(self.len) {
            *slot = b;
            self.len = self.len.saturating_add(1);
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }
    /// `n` in decimal: at most three digits, as every colour number is.
    fn decimal(&mut self, n: u16) {
        let digit = |d: u16| {
            let d = u8::try_from(d.checked_rem(10).unwrap_or(0)).unwrap_or(0);
            b'0'.saturating_add(d)
        };
        if n >= 100 {
            self.byte(digit(n.checked_div(100).unwrap_or(0)));
        }
        if n >= 10 {
            self.byte(digit(n.checked_div(10).unwrap_or(0)));
        }
        self.byte(digit(n));
    }
}

/// The parameters of an OSC string, after its command.
fn parameters(rest: &[u8]) -> impl Iterator<Item = &[u8]> {
    rest.split(|b| *b == b';')
}

/// Carries out the colour OSC `command` with `rest` after it, if it is
/// one, ended by BEL if `bel`; whether it was. `colours` is the screen's,
/// made when a colour is first set; `events` says whether a dynamic colour
/// the program has not set may be asked of the host.
pub(crate) fn osc(
    colours: &mut Option<Box<Colours>>,
    command: &[u8],
    rest: &[u8],
    bel: bool,
    events: bool,
    sink: &mut impl Sink,
) -> bool {
    match command {
        b"4" => pairs(colours, rest, 0, bel, sink),
        b"5" => pairs(colours, rest, PAST_PALETTE, bel, sink),
        b"104" => reset(colours, rest, 0),
        b"105" => reset(colours, rest, PAST_PALETTE),
        b"10" | b"11" | b"12" | b"13" | b"14" | b"15" | b"16" | b"17" | b"18" | b"19" => {
            dynamic(colours, command, rest, bel, events, sink);
        }
        b"110" | b"111" | b"112" | b"113" | b"114" | b"115" | b"116" | b"117" | b"118" | b"119" => {
            // With a parameter, nothing, as in xterm and Ghostty.
            if rest.is_empty()
                && let Some(c) = colours
                && let Some(number) = command.get(1..)
                && let Some(slot) = std::str::from_utf8(number)
                    .ok()
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| n.checked_sub(10))
                    .and_then(|i| c.dynamic.get_mut(i))
            {
                *slot = None;
            }
        }
        _ => return false,
    }
    true
}

/// OSC 4 (`offset` 0) and OSC 5 (`offset` 256): pairs of a colour number
/// and a specification, `?` asking for the colour, as xterm's
/// `ChangeAnsiColorRequest` reads them: the number as `atoi` reads it, and
/// the first pair without a specification, with a number out of range or
/// a specification fux-vt cannot read ends them.
fn pairs(
    colours: &mut Option<Box<Colours>>,
    rest: &[u8],
    offset: u16,
    bel: bool,
    sink: &mut impl Sink,
) {
    let limit = PAST_PALETTE.saturating_add(SPECIALS);
    let mut params = parameters(rest);
    while let (Some(number), Some(spec)) = (params.next(), params.next()) {
        let Some(n) = u16::try_from(atoi(number))
            .ok()
            .and_then(|n| n.checked_add(offset))
            .filter(|n| *n < limit)
        else {
            return;
        };
        if spec == b"?" {
            let set = colours.as_mut().and_then(|c| *c.slot(n)?);
            let colour = match (set, u8::try_from(n)) {
                (Some(colour), _) => colour,
                // The host's terminal's colour, if it said; else xterm's.
                (None, Ok(index)) => colours
                    .as_ref()
                    .and_then(|c| c.host(index))
                    .unwrap_or_else(|| default(index)),
                // A special colour the program has not set: fux-vt does
                // not know the terminal's foreground, xterm's default.
                (None, Err(_)) => continue,
            };
            // Answered in the form it was asked: OSC 5 by its own number.
            let asked = n.saturating_sub(offset);
            let command = if offset == 0 { 4 } else { 5 };
            report(sink, command, Some(asked), colour, bel);
            continue;
        }
        let Some(colour) = parse(spec) else {
            return;
        };
        if let Some(slot) = colours.get_or_insert_default().slot(n) {
            *slot = Some(colour);
        }
    }
}

/// OSC 104 (`offset` 0) and OSC 105 (`offset` 256): the colours listed back
/// to the terminal's own, read as xterm's `ResetAnsiColorRequest` reads
/// them, up to the first that is no number or is followed by anything but
/// `;`. With none listed, OSC 104 resets every palette entry, as ctlseqs
/// says; OSC 105 resets nothing, as in xterm 411, where ctlseqs says every
/// special colour (the README's "Departures from the references").
fn reset(colours: &mut Option<Box<Colours>>, rest: &[u8], offset: u16) {
    let Some(c) = colours else {
        return;
    };
    if rest.is_empty() {
        if offset == 0 {
            c.reset_palette();
        }
        return;
    }
    for item in parameters(rest) {
        // `strtol`: white space, a sign and digits, and nothing after.
        let digits = item.trim_ascii_start();
        let digits = digits.strip_prefix(b"+").unwrap_or(digits);
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return;
        }
        let n = atoi(digits);
        if let Some(slot) = u16::try_from(n)
            .ok()
            .and_then(|n| n.checked_add(offset))
            .filter(|n| *n < PAST_PALETTE.saturating_add(SPECIALS))
            .and_then(|n| c.slot(n))
        {
            *slot = None;
        }
    }
}

/// OSC 10 to 19: each parameter the next dynamic colour from `command`'s,
/// `?` asking for it, as xterm's `ChangeColorsRequest` reads them; a
/// specification fux-vt cannot read leaves its colour and goes on to the
/// next, as in xterm. A colour the program set is answered here; one it has
/// not is asked of the host, with `events`.
fn dynamic(
    colours: &mut Option<Box<Colours>>,
    command: &[u8],
    rest: &[u8],
    bel: bool,
    events: bool,
    sink: &mut impl Sink,
) {
    let Some(first) = std::str::from_utf8(command)
        .ok()
        .and_then(|n| n.parse::<u8>().ok())
    else {
        return;
    };
    for (number, parameter) in (first..=19).zip(parameters(rest)) {
        if parameter == b"?" {
            match colours.as_ref().and_then(|c| c.dynamic(number)) {
                Some(colour) => report(sink, u16::from(number), None, colour, bel),
                None if events => sink.event(Event::ColorQuery { number, bel }),
                None => {}
            }
        } else if let Some(colour) = parse(parameter)
            && let Some(slot) = number.checked_sub(10).and_then(|i| {
                colours
                    .get_or_insert_default()
                    .dynamic
                    .get_mut(usize::from(i))
            })
        {
            *slot = Some(colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{atoi, default, parse};

    #[test]
    fn specifications_are_read_as_xterm_reads_them() {
        // XParseColor's rgb: scales each channel to 16 bits; # forms give
        // the high bits; the high byte is kept (xterm 411, `OSC 4 ; n ; ?`
        // after each).
        assert_eq!(parse(b"rgb:f0f0/f0f0/f0f0"), Some([0xf0, 0xf0, 0xf0]));
        assert_eq!(parse(b"rgb:ff00/8/80"), Some([0xff, 0x88, 0x80]));
        assert_eq!(parse(b"rgb:1/22/333"), Some([0x11, 0x22, 0x33]));
        assert_eq!(parse(b"RGB:ff/00/00"), Some([0xff, 0, 0]));
        assert_eq!(parse(b"#fff"), Some([0xf0, 0xf0, 0xf0]));
        assert_eq!(parse(b"#888"), Some([0x80, 0x80, 0x80]));
        assert_eq!(parse(b"#f0f0f0"), Some([0xf0, 0xf0, 0xf0]));
        assert_eq!(parse(b"#f00f00f00"), Some([0xf0, 0xf0, 0xf0]));
        assert_eq!(parse(b"#aaaabbbbcccc"), Some([0xaa, 0xbb, 0xcc]));
        for bad in [
            &b""[..],
            b"#",
            b"#f",
            b"#ffff",
            b"#fffffffffffffff",
            b"#ggg",
            b"  #102030",
            b"rgb:",
            b"rgb:1/2",
            b"rgb:1/2/3/4",
            b"rgb:12345/0/0",
            b"rgb:/0/0",
            b"rgbi:1/1/1",
            b"red",
            b"?",
        ] {
            assert_eq!(parse(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn the_default_palette_is_xterms() {
        // xterm 411's answers to `OSC 4 ; n ; ?`.
        let expected = [
            (0, [0, 0, 0]),
            (1, [0xcd, 0, 0]),
            (4, [0, 0, 0xee]),
            (7, [0xe5, 0xe5, 0xe5]),
            (8, [0x7f, 0x7f, 0x7f]),
            (12, [0x5c, 0x5c, 0xff]),
            (15, [0xff, 0xff, 0xff]),
            (16, [0, 0, 0]),
            (17, [0, 0, 0x5f]),
            (231, [0xff, 0xff, 0xff]),
            (232, [8, 8, 8]),
            (255, [0xee, 0xee, 0xee]),
        ];
        for (index, rgb) in expected {
            assert_eq!(default(index), rgb, "{index}");
        }
        // The cube's levels.
        assert_eq!(default(16 + 36 + 2 * 6 + 5), [0x5f, 0x87, 0xff]);
        assert_eq!(default(16 + 4 * 36 + 3 * 6), [0xd7, 0xaf, 0]);
    }

    #[test]
    fn colour_numbers_are_read_as_atoi_reads_them() {
        assert_eq!(atoi(b"12"), 12);
        assert_eq!(atoi(b"1x"), 1);
        assert_eq!(atoi(b"x"), 0);
        assert_eq!(atoi(b" 3"), 3);
        assert_eq!(atoi(b"+5"), 5);
        assert_eq!(atoi(b"-1"), -1);
        assert_eq!(atoi(b"99999999999"), i32::MAX);
    }
}
