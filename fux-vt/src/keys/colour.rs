//! The colours a terminal reports of itself: its foreground and background
//! (`OSC 10`, `OSC 11`) and its colour scheme (`CSI ? 997 n`), and the
//! answers a program asking for them is given.

/// A colour as xterm reports it: 16 bits a channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    /// Red, 16 bits.
    pub r: u16,
    /// Green, 16 bits.
    pub g: u16,
    /// Blue, 16 bits.
    pub b: u16,
}

impl Rgb {
    /// An `rgb:R/G/B` specification, each channel one to four hex digits,
    /// read as XParseColor reads it: scaled to 16 bits, so `f`, `ff` and
    /// `ffff` are all full. Anything else is `None`.
    pub fn parse(spec: &[u8]) -> Option<Rgb> {
        let rest = spec.strip_prefix(b"rgb:")?;
        let mut channels = rest.split(|b| *b == b'/').map(|digits| {
            let text = std::str::from_utf8(digits).ok()?;
            if text.is_empty() || text.len() > 4 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let value = u32::from_str_radix(text, 16).ok()?;
            // At most four digits: the largest is 0xffff.
            let bits = u32::try_from(text.len()).ok()?.checked_mul(4)?;
            let largest = 1u32.checked_shl(bits)?.checked_sub(1)?;
            u16::try_from(value.checked_mul(0xffff)?.checked_div(largest)?).ok()
        });
        let (r, g, b) = (channels.next()??, channels.next()??, channels.next()??);
        channels.next().is_none().then_some(Rgb { r, g, b })
    }

    /// xterm's answer to `OSC number ; ?`: `OSC number ; rgb:RRRR/GGGG/BBBB`,
    /// ended with BEL or ST as the query was (ctlseqs, "Operating System
    /// Commands").
    pub fn answer(self, number: u8, bel: bool) -> Vec<u8> {
        let end = if bel { "\x07" } else { "\x1b\\" };
        let Rgb { r, g, b } = self;
        format!("\x1b]{number};rgb:{r:04x}/{g:04x}/{b:04x}{end}").into_bytes()
    }
}

/// The terminal's colour scheme, as `CSI ? 997 ; 1|2 n` reports it
/// (`references/modern/mode_2031_color_scheme_updates.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// A dark scheme: light text on a dark background.
    Dark,
    /// A light scheme: dark text on a light background.
    Light,
}

impl Scheme {
    /// The report: `CSI ? 997 ; 1 n` dark, `CSI ? 997 ; 2 n` light.
    pub fn report(self) -> &'static [u8] {
        match self {
            Scheme::Dark => b"\x1b[?997;1n",
            Scheme::Light => b"\x1b[?997;2n",
        }
    }
}

/// What a terminal said of its colours, as far as it said it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colours {
    /// The foreground colour (`OSC 10`), if the terminal said.
    pub foreground: Option<Rgb>,
    /// The background colour (`OSC 11`), if the terminal said.
    pub background: Option<Rgb>,
    /// The colour scheme (`CSI ? 997 n`), if the terminal said.
    pub scheme: Option<Scheme>,
}

impl Colours {
    /// Whether the terminal said anything.
    pub fn known(&self) -> bool {
        *self != Colours::default()
    }

    /// The answer to a program's `OSC number ; ?`, if fux knows that colour.
    pub fn answer(&self, number: u8, bel: bool) -> Option<Vec<u8>> {
        let colour = match number {
            10 => self.foreground,
            11 => self.background,
            _ => None,
        };
        colour.map(|c| c.answer(number, bel))
    }
}
