//! Direct DEC ANSI transition parser, based on Paul Williams's transition
//! model (reference and attribution in the crate README), with UTF-8 ground decoding.
//! Ignored control strings retain no payload. No parser dependency is used.

use crate::{Error, Reply, Screen, screen::Dispatch};

#[cfg(test)]
#[path = "../tests/corpus/mod.rs"]
pub(crate) mod test_corpus;

#[derive(Clone, Debug)]
pub(crate) struct Parameters {
    values: [u16; 32],
    /// Bit `i` set when value `i` continues the previous colon-delimited
    /// group: none until a `:` is read, and each value a group of its own.
    sub: u32,
    len: usize,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            values: [0; 32],
            sub: 0,
            len: 1,
        }
    }
}
impl Parameters {
    fn digit(&mut self, byte: u8) {
        // A value too large for a u16 stays at its largest.
        if let Some(n) = self.len.checked_sub(1).and_then(|i| self.values.get_mut(i))
            && let Some(digit) = byte.checked_sub(b'0')
        {
            *n = n.saturating_mul(10).saturating_add(u16::from(digit));
        }
    }
    fn separator(&mut self, colon: bool) -> bool {
        let Some(len) = self.len.checked_add(1).filter(|n| *n <= self.values.len()) else {
            return false;
        };
        // The new parameter starts empty: `clear` left what a sequence
        // before put there.
        if let Some(value) = self.values.get_mut(self.len) {
            *value = 0;
        }
        self.continue_group(self.len, colon);
        self.len = len;
        true
    }
    /// Marks value `index` as continuing the previous group if `colon`.
    #[inline]
    fn continue_group(&mut self, index: usize, colon: bool) {
        if colon && let Ok(index) = u32::try_from(index) {
            self.sub |= 1u32.wrapping_shl(index);
        }
    }
    /// Whether value `index` continues the previous group.
    fn continues(&self, index: usize) -> bool {
        u32::try_from(index)
            .ok()
            .and_then(|i| self.sub.checked_shr(i))
            .is_some_and(|bits| bits & 1 != 0)
    }
    /// `separator` for a reader that keeps the parameter being read and
    /// how many there are itself (`Parser::sequence`): the parameter
    /// `len` ends with `value`, and the next begins, continuing its group
    /// if `colon`. The new count, or `None`, with nothing changed, where
    /// `separator` refuses one past the last slot.
    #[inline]
    fn next(&mut self, len: usize, value: u16, colon: bool) -> Option<usize> {
        let more = len.checked_add(1).filter(|n| *n <= self.values.len())?;
        self.end(len, value);
        self.continue_group(len, colon);
        Some(more)
    }
    /// The parameters as such a reader leaves them: `len` of them, the
    /// last `value`.
    #[inline]
    fn end(&mut self, len: usize, value: u16) {
        if let Some(slot) = len.checked_sub(1).and_then(|i| self.values.get_mut(i)) {
            *slot = value;
        }
        self.len = len;
    }
    /// Back to one empty parameter, touching only what `separator` and
    /// `digit` read: a CSI or DCS is begun often, so this is kept to
    /// three stores rather than rewriting all the parameters.
    fn clear(&mut self) {
        self.len = 1;
        self.sub = 0;
        if let Some(value) = self.values.first_mut() {
            *value = 0;
        }
    }
    pub fn groups(&self) -> impl Iterator<Item = &[u16]> + use<'_> {
        let mut start = 0;
        std::iter::from_fn(move || {
            if start >= self.len {
                return None;
            }
            let mut end = start.checked_add(1)?;
            while self.sub != 0 && end < self.len && self.continues(end) {
                end = end.checked_add(1)?;
            }
            let result = self.values.get(start..end);
            start = end;
            result
        })
    }
    /// Whether no parameter was given (or only 0, which reads the same).
    fn is_empty(&self) -> bool {
        self.len == 1 && self.values.first() == Some(&0)
    }
    /// The first value of the group `index`, or `default` if it is 0 or
    /// there is no such group. Without colons the group is the value: a
    /// load, inlined into every caller, where a sequence with colons asks
    /// `grouped` out of line.
    #[inline(always)]
    pub fn first(&self, index: usize, default: u16) -> u16 {
        let value = if self.sub != 0 {
            self.grouped(index)
        } else {
            self.values
                .get(..self.len)
                .and_then(|v| v.get(index))
                .copied()
        };
        match value {
            Some(0) | None => default,
            Some(value) => value,
        }
    }
    /// The first value of the group `index`, if there is one.
    #[inline(never)]
    fn grouped(&self, index: usize) -> Option<u16> {
        self.groups().nth(index).and_then(|g| g.first()).copied()
    }
}

/// The non-ASCII character a valid UTF-8 sequence at the start of `bytes`
/// encodes, and its length; `None` for ASCII, and for a sequence invalid or
/// cut short, which the byte-by-byte path reads. The checks are the ones
/// `Parser::ground` makes: no overlong forms, surrogates or code points
/// past U+10FFFF.
fn decode(bytes: &[u8]) -> Option<(char, usize)> {
    let &first = bytes.first()?;
    let (length, mut code) = match first {
        0xc2..=0xdf => (2, u32::from(first & 0x1f)),
        0xe0..=0xef => (3, u32::from(first & 0x0f)),
        0xf0..=0xf4 => (4, u32::from(first & 0x07)),
        _ => return None,
    };
    let (low, high) = match first {
        0xe0 => (0xa0, 0xbf),
        0xed => (0x80, 0x9f),
        0xf0 => (0x90, 0xbf),
        0xf4 => (0x80, 0x8f),
        _ => (0x80, 0xbf),
    };
    let continuation = bytes.get(1..length)?;
    for (i, &byte) in continuation.iter().enumerate() {
        let (low, high) = if i == 0 { (low, high) } else { (0x80, 0xbf) };
        if !(low..=high).contains(&byte) {
            return None;
        }
        code = code.checked_shl(6)? | u32::from(byte & 0x3f);
    }
    Some((char::from_u32(code)?, length))
}

/// How many bytes `bytes` begins with are printable ASCII, 0x20 to 0x7e,
/// its first being one: a run of one, as a character between sequences
/// is, by the second byte alone; else eight at a time, as a word, then one
/// at a time. In a word, a byte below 0x20 borrows when 0x20 is taken from
/// it, setting its top bit where the byte's own is clear; one from 0x7f up
/// has its top bit set once 1 is added to it, or before. A borrow or carry
/// runs only into the bytes after the one it came from, so the first byte
/// flagged is the first that is not printable.
fn printable(bytes: &[u8]) -> usize {
    const ONES: u64 = u64::from_le_bytes([0x01; 8]);
    const TOPS: u64 = u64::from_le_bytes([0x80; 8]);
    debug_assert!(bytes.first().is_some_and(|b| (0x20..=0x7e).contains(b)));
    if !bytes.get(1).is_some_and(|b| (0x20..=0x7e).contains(b)) {
        return 1;
    }
    let (words, _) = bytes.as_chunks::<8>();
    let mut length = 0usize;
    for word in words {
        let x = u64::from_le_bytes(*word);
        let below = x.wrapping_sub(ONES.wrapping_mul(0x20)) & !x;
        let above = x.wrapping_add(ONES) | x;
        let flagged = (below | above) & TOPS;
        if flagged != 0 {
            let first = usize::try_from(flagged.trailing_zeros() / 8).unwrap_or(0);
            return length.saturating_add(first);
        }
        length = length.saturating_add(8);
    }
    let tail = bytes.get(length..).unwrap_or_default();
    length.saturating_add(
        tail.iter()
            .take_while(|b| (0x20..=0x7e).contains(*b))
            .count(),
    )
}

/// Where the parser is in what it reads, and what it keeps of that: each
/// state holds exactly what is read in it, and nothing outlives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// A character's first bytes, its lead byte first.
    Utf8(Bytes<4>),
    /// An escape sequence and its intermediates: none yet, at its ESC.
    Escape(Intermediates),
    /// An escape sequence with more intermediates than are kept: read to
    /// its final byte, and not carried out.
    EscapeIgnore,
    /// A CSI; its parameters are the parser's.
    Csi(Phase),
    /// A CSI that will not be carried out, read to its final byte.
    CsiIgnore,
    /// A DCS before its final byte; its parameters are the parser's.
    Dcs(Phase),
    /// A string read to its end and dropped: a DCS fux-vt does not
    /// answer, SOS, PM or APC.
    IgnoreString,
    /// A DECRQSS (`DCS $ q Pt ST`) with [`Options::setting_reports`]: its
    /// request so far, `None` once it ran past any setting fux-vt knows.
    Request(Option<Bytes<REQUEST_LIMIT>>),
    /// An OSC string, its payload the parser's: `whole` until it ran past
    /// what is kept of one.
    Osc { whole: bool },
}

/// Up to `N` bytes, as many as were read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bytes<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

/// The intermediates (and private marker) a sequence keeps: xterm's
/// longest are two (`?$`).
type Intermediates = Bytes<2>;

impl<const N: usize> Default for Bytes<N> {
    fn default() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }
}

impl<const N: usize> Bytes<N> {
    /// `byte` alone.
    fn of(byte: u8) -> Self {
        Self::default().push(byte).unwrap_or_default()
    }
    /// These bytes and `byte`, or `None` if there is no room for it.
    fn push(mut self, byte: u8) -> Option<Self> {
        *self.bytes.get_mut(usize::from(self.len))? = byte;
        self.len = self.len.checked_add(1)?;
        Some(self)
    }
    fn as_slice(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }
}

/// How far a CSI or DCS has come, and its intermediates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing read after its introducer.
    Entry,
    /// Reading parameters, after a private marker if there is one.
    Param(Intermediates),
    /// Reading intermediates.
    Intermediate(Intermediates),
}

/// What a byte makes of a CSI or DCS in a [`Phase`].
enum Next {
    /// It goes on, in this phase.
    Phase(Phase),
    /// It is malformed or past the bounds kept, and is read to its end
    /// without being carried out.
    Ignore,
    /// The byte is a C0 control, executed within a CSI.
    Control,
    /// The byte is its final byte, these its intermediates.
    Final(Intermediates),
}

impl Phase {
    /// What `byte` makes of a CSI or DCS in this phase, its digits and
    /// separators read into `params`.
    fn next(self, byte: u8, params: &mut Parameters) -> Next {
        let read = match self {
            Phase::Entry => Intermediates::default(),
            Phase::Param(read) | Phase::Intermediate(read) => read,
        };
        match byte {
            0x00..=0x1f => Next::Control,
            0x20..=0x2f => read
                .push(byte)
                .map_or(Next::Ignore, |read| Next::Phase(Phase::Intermediate(read))),
            0x30..=0x3f if matches!(self, Phase::Intermediate(_)) => Next::Ignore,
            b'0'..=b'9' => {
                params.digit(byte);
                Next::Phase(Phase::Param(read))
            }
            b';' | b':' => {
                if params.separator(byte == b':') {
                    Next::Phase(Phase::Param(read))
                } else {
                    Next::Ignore
                }
            }
            0x3c..=0x3f if self == Phase::Entry => {
                Next::Phase(Phase::Param(Intermediates::of(byte)))
            }
            0x3c..=0x3f => Next::Ignore,
            0x40..=0x7e => Next::Final(read),
            0x7f..=0xff => Next::Phase(self),
        }
    }
}

/// The most OSC payload bytes retained for [`Event`] delivery, hyperlinks
/// and the palette. A longer OSC string is consumed without an event or a link;
/// nothing beyond this is ever buffered.
pub const OSC_PAYLOAD_LIMIT: usize = 64 * 1024;

/// The most OSC payload bytes retained with [`Options::prompt_marks`] alone:
/// enough to tell a prompt mark, `133;A`, or a prompt's kind, `133;P;k=i`.
const OSC_PREFIX: usize = 16;

/// The most bytes of a DECRQSS request kept: xterm's longest are two
/// (` q`, `$|`, `"p`). A longer request is no setting fux-vt knows.
const REQUEST_LIMIT: usize = 4;

/// Opt-in behaviour that needs the host's cooperation. The default
/// (everything off) is fux's policy: child output causes no title, bell or
/// clipboard side effects, OSC payloads are never retained, only DSR 5n/6n
/// and primary DA are answered, keyboard protocol requests are ignored,
/// hyperlinks and prompt marks are ignored, and a resize does not reflow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Deliver OSC 0/1/2 (icon name / window title), OSC 52 (clipboard),
    /// BEL, and the dynamic colours' queries (OSC 10 to 19, as
    /// [`Event::ColorQuery`]) as [`Event`]s. OSC payloads are buffered up
    /// to [`OSC_PAYLOAD_LIMIT`].
    pub events: bool,
    /// Also answer DECRQM (`CSI ? Ps $ p` and `CSI Ps $ p`), DECXCPR
    /// (`CSI ? 6 n`) and secondary device attributes (`CSI > c`).
    pub extended_replies: bool,
    /// Answer DECRQM (`CSI ? Ps $ p` and `CSI Ps $ p`) alone, as
    /// [`Options::extended_replies`] does with the rest: how programs learn
    /// which modes the terminal knows, synchronized output (2026) among
    /// them, without what DA2 and DECXCPR say about it.
    pub mode_reports: bool,
    /// Mode 2048, in-band resize (`references/modern/mode_2048_in_band_resize.md`):
    /// track it, report the size as `CSI 48 ; rows ; cols ; 0 ; 0 t` when a
    /// program sets it, and give the host the report to send after a
    /// resize ([`Parser::resize_report`]). Pixel sizes are reported as 0,
    /// which the spec allows a terminal that does not know them. Off, the
    /// mode is not recognized, and DECRQM says so.
    pub in_band_resize: bool,
    /// Answer xterm's text-area size query (`CSI 18 t`, ctlseqs' window
    /// manipulation) with `CSI 8 ; rows ; cols t`, the screen's size in
    /// characters. The pixel query (`CSI 14 t`) stays unanswered: fux-vt
    /// knows no pixels.
    pub size_reports: bool,
    /// Mode 2031, colour-scheme change reports
    /// (`references/modern/mode_2031_color_scheme_updates.md`): track it,
    /// read with [`Screen::color_scheme_updates`], and report it to DECRQM.
    /// State only: the host knows the colour scheme, and sends the reports
    /// (`CSI ? 997 ; 1 n` dark, `CSI ? 997 ; 2 n` light) and answers
    /// `CSI ? 996 n`, which stays unhandled. Off, the mode is not
    /// recognized, and DECRQM says so.
    pub color_scheme_updates: bool,
    /// Track the kitty keyboard protocol's flag stacks (`CSI > u`, `CSI < u`,
    /// `CSI = u`) and xterm's modifyOtherKeys (`CSI > 4 ; Pv m`), and answer
    /// the flag query `CSI ? u`. State only: the host encodes keys, reading
    /// [`Screen::kitty_keyboard_flags`] and [`Screen::modify_other_keys`].
    /// A host that cannot encode keys that way must leave this off, or
    /// programs will believe it can.
    pub kitty_keyboard: bool,
    /// Re-wrap the primary screen and its history at the new width on
    /// resize, keeping the cursor on its character. The alternate screen is
    /// resized without reflow, as its programs redraw anyway.
    pub reflow: bool,
    /// Keep hyperlinks (`OSC 8 ; params ; URI ST`): each cell printed while
    /// one is open has it, read with [`crate::Row::link`]. The URIs are
    /// kept, so OSC 8 payloads are buffered, up to [`OSC_PAYLOAD_LIMIT`],
    /// and each screen holds up to 4 MiB of links. Off, OSC 8 is ignored.
    pub hyperlinks: bool,
    /// Keep prompt marks (`OSC 133 ; A`, semantic prompts): the row a prompt
    /// starts on is marked, read with [`crate::Row::starts_prompt`], and `A`
    /// and `L` start a fresh line. An OSC string's first bytes are kept to
    /// tell one. Off, OSC 133 is ignored.
    pub prompt_marks: bool,
    /// Answer DECRQCRA, a checksum of a rectangle of the screen, with
    /// DECCKSR, as xterm and the VT520 sum it (`Screen::rectangle_checksum`).
    /// It lets a program read what its screen shows, so xterm refuses it by
    /// default, and fux's panes leave it off; esctest reads the screen so.
    pub rectangle_checksums: bool,
    /// Answer DECRQSS (`DCS $ q Pt ST`, xterm's ctlseqs; DECRPSS in the
    /// VT510 manual) for the pen (`m`, SGR), the cursor shape (` q`,
    /// DECSCUSR) and the margins (`r`, DECSTBM), as xterm answers: `DCS 1 $
    /// r Pt ST`, `Pt` the sequence that sets it, such as `0;1;4:3m`.
    /// Another request is answered as invalid, `DCS 0 $ r ST`, and a cursor
    /// shape left to the terminal (DECSCUSR 0, the default) not at all. A
    /// request's first bytes are kept to tell it, nothing more. Programs
    /// learn from the pen what the terminal draws: neovim sets a curly
    /// underline and asks, and draws its diagnostics curly only if `4:3`
    /// comes back. Off, DECRQSS is ignored, as every other DCS.
    pub setting_reports: bool,
    /// Keep the colours a program sets and answer its queries of them, as
    /// xterm does: the palette (OSC 4, 104), the special colours (OSC 5,
    /// 105) and the dynamic colours (OSC 10 to 19, 110 to 119); `palette.rs`
    /// says how. Read with [`Screen::palette_color`] and
    /// [`Screen::dynamic_color`]. OSC payloads are buffered, up to
    /// [`OSC_PAYLOAD_LIMIT`]. Off, these OSCs are ignored, and OSC 10 to 19
    /// queries are events, with [`Options::events`].
    pub palette: bool,
    /// Answer as this terminal rather than as a bare VT100: see [`Identity`].
    pub identity: Option<Identity>,
}

impl Options {
    /// Everything off, as [`Options::default`]; the `with_` methods turn
    /// each on, in a `const` too.
    pub const fn new() -> Self {
        Self {
            events: false,
            extended_replies: false,
            mode_reports: false,
            in_band_resize: false,
            size_reports: false,
            color_scheme_updates: false,
            kitty_keyboard: false,
            reflow: false,
            hyperlinks: false,
            prompt_marks: false,
            rectangle_checksums: false,
            setting_reports: false,
            palette: false,
            identity: None,
        }
    }
    /// These options with [`Options::events`] as `on` says.
    pub const fn with_events(mut self, on: bool) -> Self {
        self.events = on;
        self
    }
    /// These options with [`Options::extended_replies`] as `on` says.
    pub const fn with_extended_replies(mut self, on: bool) -> Self {
        self.extended_replies = on;
        self
    }
    /// These options with [`Options::mode_reports`] as `on` says.
    pub const fn with_mode_reports(mut self, on: bool) -> Self {
        self.mode_reports = on;
        self
    }
    /// These options with [`Options::in_band_resize`] as `on` says.
    pub const fn with_in_band_resize(mut self, on: bool) -> Self {
        self.in_band_resize = on;
        self
    }
    /// These options with [`Options::size_reports`] as `on` says.
    pub const fn with_size_reports(mut self, on: bool) -> Self {
        self.size_reports = on;
        self
    }
    /// These options with [`Options::color_scheme_updates`] as `on` says.
    pub const fn with_color_scheme_updates(mut self, on: bool) -> Self {
        self.color_scheme_updates = on;
        self
    }
    /// These options with [`Options::kitty_keyboard`] as `on` says.
    pub const fn with_kitty_keyboard(mut self, on: bool) -> Self {
        self.kitty_keyboard = on;
        self
    }
    /// These options with [`Options::reflow`] as `on` says.
    pub const fn with_reflow(mut self, on: bool) -> Self {
        self.reflow = on;
        self
    }
    /// These options with [`Options::prompt_marks`] as `on` says.
    pub const fn with_prompt_marks(mut self, on: bool) -> Self {
        self.prompt_marks = on;
        self
    }
    /// These options with [`Options::hyperlinks`] as `on` says.
    pub const fn with_hyperlinks(mut self, on: bool) -> Self {
        self.hyperlinks = on;
        self
    }
    /// These options with [`Options::rectangle_checksums`] as `on` says.
    pub const fn with_rectangle_checksums(mut self, on: bool) -> Self {
        self.rectangle_checksums = on;
        self
    }
    /// These options with [`Options::setting_reports`] as `on` says.
    pub const fn with_setting_reports(mut self, on: bool) -> Self {
        self.setting_reports = on;
        self
    }
    /// These options with [`Options::palette`] as `on` says.
    pub const fn with_palette(mut self, on: bool) -> Self {
        self.palette = on;
        self
    }
    /// These options answering as `identity`, or as a bare VT100 if `None`.
    pub const fn with_identity(mut self, identity: Option<Identity>) -> Self {
        self.identity = identity;
        self
    }
}

/// Who the terminal says it is. With [`Options::identity`] set, primary DA
/// (`CSI c`) answers `CSI ? 62 ; 22 c` (VT220 class, ANSI colour), secondary
/// DA (`CSI > c`) answers `CSI > 1 ; Pv ; 0 c` with `version` encoded as
/// `major * 10000 + minor * 100 + patch`, XTVERSION (`CSI > q`) answers
/// `DCS > | name version ST`, and cursor reports (DSR 6n, DECXCPR) give a
/// cursor waiting to wrap at the last column, as xterm does, rather than
/// one past it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The terminal's name, as XTVERSION reports it.
    pub name: &'static str,
    /// Its `major.minor.patch` version; a pre-release suffix is ignored in
    /// DA2. XTVERSION goes unanswered if name and version exceed
    /// [`Identity::MAX_LEN`] bytes together.
    pub version: &'static str,
}

impl Identity {
    /// The most bytes of name and version XTVERSION reports.
    pub const MAX_LEN: usize = 48;

    /// The version as DA2's firmware field: its parts, at most three, as
    /// digits in base 100, anything past a `-` or `+` dropped: `0.5.0` is
    /// 500, `1.2` is 102 (a test pins the short forms).
    fn encoded_version(&self) -> u32 {
        let release = self.version.split(['-', '+']).next().unwrap_or_default();
        release.split('.').take(3).fold(0u32, |sum, part| {
            let part = part.parse::<u32>().unwrap_or(0).min(99);
            sum.saturating_mul(100).saturating_add(part)
        })
    }
}

/// A side effect requested by child output. Only delivered with
/// [`Options::events`]; payloads are raw bytes, bounded by [`OSC_PAYLOAD_LIMIT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// OSC 0 or OSC 2.
    Title(&'a [u8]),
    /// OSC 0 or OSC 1.
    IconName(&'a [u8]),
    /// BEL executed outside a control string.
    Bell,
    /// OSC 52 set request; a `?` query is not an event.
    Clipboard {
        /// The selection parameter, `Pc`: `c`, `p`, `s` and so on, or empty.
        selection: &'a [u8],
        /// The data, `Pd`, as sent (normally base64).
        data: &'a [u8],
    },
    /// A query of one of xterm's dynamic colours, `OSC Ps ; ?` with `Ps`
    /// from 10 to 19: 10 the text foreground, 11 the background, 12 the
    /// cursor (ctlseqs, "Operating System Commands"). One OSC can ask for
    /// several, each `?` the next colour (`OSC 10 ; ? ; ?` asks 10 and 11),
    /// an event each, in order. xterm answers `OSC Ps ; rgb:RRRR/GGGG/BBBB`,
    /// ended as the query was; the host knows the colours, so the answer
    /// is its to make. A request to set a colour is no event; with
    /// [`Options::palette`], neither is a query of a colour the program
    /// set, which fux-vt answers itself.
    ColorQuery {
        /// The colour asked for, 10 to 19.
        number: u8,
        /// Whether the query ended with BEL rather than ST.
        bel: bool,
    },
}

/// The colour queries of an OSC 10 to 19, `command` its number and `rest`
/// what follows it: each parameter is the next colour, and each `?` among
/// them asks for that one (ctlseqs, "Operating System Commands").
fn color_queries(command: &[u8], rest: &[u8], bel: bool, sink: &mut impl Sink) {
    let Some(first) = std::str::from_utf8(command)
        .ok()
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (10..=19).contains(n))
    else {
        return;
    };
    for (number, parameter) in (first..=19).zip(rest.split(|b| *b == b';')) {
        if parameter == b"?" {
            sink.event(Event::ColorQuery { number, bel });
        }
    }
}

/// A complete sequence fux-vt parsed but does not implement, so a host can
/// log or answer it. Sequences cut short by their bounds (too many
/// parameters or intermediates) are dropped without being reported.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Unhandled<'a> {
    /// `CSI`, its private marker and intermediates, parameters and final byte.
    Csi {
        /// Its parameters.
        params: Params<'a>,
        /// Its private marker and intermediate bytes.
        intermediates: &'a [u8],
        /// Its final byte.
        action: u8,
    },
    /// `ESC`, its intermediates and final byte.
    Escape {
        /// Its intermediate bytes.
        intermediates: &'a [u8],
        /// Its final byte.
        action: u8,
    },
}

/// A CSI sequence's parameters.
#[derive(Clone, Copy, Debug)]
pub struct Params<'a>(&'a Parameters);

impl<'a> Params<'a> {
    /// Each parameter with its colon-separated subparameters; an empty
    /// parameter is 0.
    pub fn groups(&self) -> impl Iterator<Item = &'a [u16]> + use<'a> {
        self.0.groups()
    }
}

/// Receives terminal query replies, with [`Options::events`] events, and
/// sequences fux-vt does not implement. All default to discarding, so an
/// implementation handles only what it needs.
pub trait Sink {
    /// A reply to a query, to send back to the program.
    fn reply(&mut self, _bytes: &[u8]) {}
    /// An event, with [`Options::events`].
    fn event(&mut self, _event: Event<'_>) {}
    /// A complete sequence fux-vt does not implement.
    fn unhandled(&mut self, _sequence: Unhandled<'_>) {}
}

struct Replies<F>(F);
impl<F: FnMut(&[u8])> Sink for Replies<F> {
    fn reply(&mut self, bytes: &[u8]) {
        (self.0)(bytes);
    }
}

/// Incremental byte parser and its terminal state. Replies and events are
/// delivered synchronously to a caller-supplied sink; no unbounded queue exists.
#[derive(Clone, Debug)]
pub struct Parser {
    screen: Screen,
    options: Options,
    /// The OSC string's payload, at most `osc_limit` bytes: read only in
    /// `State::Osc`, emptied as one begins, its allocation kept.
    osc: Vec<u8>,
    /// `OSC_PAYLOAD_LIMIT` with `options.events`, `options.hyperlinks` or
    /// `options.palette`, else `OSC_PREFIX` with `options.prompt_marks`,
    /// else 0: set once, as it is asked for every byte of an OSC.
    osc_limit: usize,
    state: State,
    /// The CSI's or DCS's parameters: read only in `State::Csi` and
    /// `State::Dcs`, cleared as one begins.
    params: Parameters,
}
#[cfg(test)]
mod tests;

impl Parser {
    /// A parser with a `rows` by `cols` screen keeping up to `history_lines`
    /// rows of history, and [`Options::default`]: see [`Parser::with_options`].
    pub fn new(rows: u16, cols: u16, history_lines: usize) -> Result<Self, Error> {
        Self::with_options(rows, cols, history_lines, Options::default())
    }
    /// A parser with a `rows` by `cols` screen keeping up to `history_lines`
    /// rows of history (none on the alternate screen), with `options`.
    /// Zero rows or columns, or more cells than a grid may hold, are refused.
    pub fn with_options(
        rows: u16,
        cols: u16,
        history_lines: usize,
        options: Options,
    ) -> Result<Self, Error> {
        Ok(Self {
            screen: Screen::new(rows, cols, history_lines)?,
            options,
            osc: Vec::new(),
            osc_limit: if options.events || options.hyperlinks || options.palette {
                OSC_PAYLOAD_LIMIT
            } else if options.prompt_marks {
                OSC_PREFIX
            } else {
                0
            },
            state: State::Ground,
            params: Parameters::default(),
        })
    }
    /// The screen, for a test to reach into.
    #[cfg(test)]
    pub(crate) fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }
    /// The terminal's state: what the screen shows, the cursor and the modes.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }
    /// Tells the parser what the host's terminal shows for palette entry
    /// `index` (`None`: the host does not know), with `Options::palette`: a
    /// program's query of an entry it has not set (`OSC 4 ; index ; ?`) is
    /// answered with it rather than with xterm's default. A colour the
    /// program set still wins, and no reset of the program's colours (OSC
    /// 104, DECSTR, RIS) clears the host's. It changes no cell and no
    /// colour drawn: [`Screen::palette_color`] stays the program's. Entries
    /// 0 to 15 can be given, the ones themes change; returns whether `index`
    /// is one.
    pub fn set_host_color(&mut self, index: u8, rgb: Option<(u8, u8, u8)>) -> bool {
        self.screen.set_host_color(index, rgb)
    }
    /// Resizes the terminal, reflowing the primary screen with
    /// [`Options::reflow`].
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
        self.screen.resize(rows, cols, self.options.reflow)
    }
    /// The size report a program that set in-band resize (mode 2048) is to
    /// be sent after the terminal's size changed: `CSI 48 ; rows ; cols ; 0
    /// ; 0 t`, or `None` if it did not set it or
    /// [`Options::in_band_resize`] is off. The host sends it once the
    /// program's terminal has the new size, as the spec requires.
    pub fn resize_report(&self) -> Option<Vec<u8>> {
        (self.options.in_band_resize && self.screen.in_band_resize())
            .then(|| self.screen.size_report().as_bytes().to_vec())
    }
    /// The options the parser was made with.
    pub fn options(&self) -> Options {
        self.options
    }
    /// Process output while deliberately discarding terminal query replies.
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_with_replies(bytes, |_| {})
    }
    /// Process output, delivering query replies to `reply`.
    pub fn process_with_replies(
        &mut self,
        bytes: &[u8],
        reply: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
        self.process_with(bytes, &mut Replies(reply))
    }
    /// Process output, delivering replies and (with [`Options::events`]) events.
    pub fn process_with(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(), Error> {
        self.run::<false>(bytes, sink).map(|_| ())
    }
    /// Process output as [`Parser::process_with`] does, but stop right after
    /// a sequence that sets synchronized output (`CSI ? 2026 h`, BSU), and
    /// say how many bytes that took; `None` if no sequence did, and every
    /// byte was processed. A host that holds a program's frames
    /// ([`crate::Screen::synchronized_output`]) holds the rest from there, found
    /// exactly as the parser reads it: split across calls, or set beside
    /// other modes in one sequence.
    pub fn process_until_frame(
        &mut self,
        bytes: &[u8],
        sink: &mut impl Sink,
    ) -> Result<Option<usize>, Error> {
        self.run::<true>(bytes, sink)
    }
    /// Processes `bytes`, stopping after a BSU if `UNTIL_FRAME`; how many
    /// bytes it took if it stopped. A constant, so that `process_with`,
    /// which does not stop, compiles without the check for one.
    fn run<const UNTIL_FRAME: bool>(
        &mut self,
        bytes: &[u8],
        sink: &mut impl Sink,
    ) -> Result<Option<usize>, Error> {
        if bytes.is_empty() {
            return Ok(None);
        }
        self.screen.begin()?;
        let mut remaining = bytes;
        while let Some((&byte, tail)) = remaining.split_first() {
            let ground = matches!(self.state, State::Ground);
            let framed = if ground && (0x20..=0x7e).contains(&byte) {
                let length = printable(remaining);
                self.screen
                    .ascii(remaining.get(..length).unwrap_or_default())?;
                remaining = remaining.get(length..).unwrap_or_default();
                false
            } else if ground && byte < 0x20 && byte != 0x1b {
                // A C0 control, as `ground` carries it out.
                self.control(byte, sink)?;
                self.screen.forget_repeat();
                remaining = tail;
                false
            } else if ground
                && byte >= 0x80
                && let Some(length) = self.text(remaining)?
            {
                remaining = remaining.get(length..).unwrap_or_default();
                false
            } else if ground && byte == 0x1b {
                let (length, framed) = self.sequence(remaining, sink)?;
                remaining = remaining.get(length..).unwrap_or_default();
                framed
            } else {
                remaining = tail;
                self.byte(byte, sink)?
            };
            // A CSI that began a frame (BSU, or XTRESTORE of the mode)
            // ends whatever was taken: stop right after it.
            if UNTIL_FRAME && framed {
                return Ok(Some(bytes.len().saturating_sub(remaining.len())));
            }
        }
        Ok(None)
    }
    /// Prints the run of valid non-ASCII UTF-8 `bytes` begins with, decoded
    /// at once rather than byte by byte, and says how many
    /// bytes it took; `None`, with nothing done, if `bytes` begins with
    /// invalid or incomplete UTF-8, which the byte-by-byte path handles.
    fn text(&mut self, bytes: &[u8]) -> Result<Option<usize>, Error> {
        let mut taken = 0usize;
        while let Some((c, length)) = bytes.get(taken..).and_then(decode) {
            self.screen.print(c)?;
            taken = taken.saturating_add(length);
        }
        Ok((taken > 0).then_some(taken))
    }
    /// Reads the escape sequence `bytes` begins with (its ESC read in
    /// ground state) as far as it is one programs send most: an escape
    /// sequence of intermediates and a final byte, a CSI of parameters
    /// alone (a private marker, digits, `;` and `:`, and a final byte), or
    /// an OSC string up to what ends it. The bytes are taken in a loop of
    /// their own rather than each through `byte`, but each is done exactly
    /// as `byte` does it. It stops before any other byte, or at the end,
    /// with the parser in the state `byte` would have left it in, for
    /// `byte` to go on from. Says how many bytes it took, at least the ESC,
    /// and whether the sequence it carried out began a frame.
    fn sequence(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(usize, bool), Error> {
        if bytes.get(1) == Some(&b'[') {
            return self.csi_sequence(bytes, sink);
        }
        // The intermediates, kept here rather than in `state` until the
        // sequence ends or this does.
        let mut read = Intermediates::default();
        let mut at = 1usize;
        while let Some(&byte) = bytes.get(at) {
            let next = at.wrapping_add(1);
            let first = read.as_slice().is_empty();
            match byte {
                0x20..=0x2f => match read.push(byte) {
                    Some(more) => read = more,
                    // One too many, which `byte` reads.
                    None => break,
                },
                b']' if first => {
                    let taken = self.osc_string(bytes.get(next..).unwrap_or_default());
                    return Ok((next.saturating_add(taken), false));
                }
                // The other strings, which `byte` begins.
                b'P' | b'X' | b'^' | b'_' if first => break,
                0x30..=0x7e => {
                    self.state = State::Ground;
                    self.escape_dispatch(read.as_slice(), byte, sink)?;
                    self.screen.forget_repeat();
                    return Ok((next, false));
                }
                _ => break,
            }
            at = next;
        }
        self.state = State::Escape(read);
        Ok((at, false))
    }
    /// `sequence` for a CSI, `bytes` beginning with `ESC [`.
    fn csi_sequence(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(usize, bool), Error> {
        self.params.clear();
        // The parameter being read and how many there are, kept here
        // rather than in `params` until the sequence ends or this does: one
        // empty one, as `clear` left them. The value is read as
        // `Parameters::digit` reads it, staying at u16::MAX once past it:
        // `(v * 10).min(MAX) + d` saturated is `(v * 10 + d).min(MAX)`.
        let (mut len, mut value) = (1usize, 0u32);
        let max = u32::from(u16::MAX);
        let mut marker = Intermediates::default();
        let mut at = 2usize;
        while let Some(&byte) = bytes.get(at) {
            let next = at.wrapping_add(1);
            match byte {
                b'0'..=b'9' => {
                    let digit = u32::from(byte.wrapping_sub(b'0'));
                    value = value.wrapping_mul(10).wrapping_add(digit).min(max);
                }
                b';' | b':' => {
                    let ended = u16::try_from(value).unwrap_or(u16::MAX);
                    match self.params.next(len, ended, byte == b':') {
                        Some(more) => (len, value) = (more, 0),
                        None => {
                            self.params.end(len, ended);
                            self.state = State::CsiIgnore;
                            return Ok((next, false));
                        }
                    }
                }
                0x3c..=0x3f if at == 2 => marker = Intermediates::of(byte),
                0x40..=0x7e => {
                    self.params
                        .end(len, u16::try_from(value).unwrap_or(u16::MAX));
                    self.state = State::Ground;
                    let framed = self.csi_dispatch(marker.as_slice(), byte, sink)?;
                    self.screen.forget_repeat();
                    return Ok((next, framed));
                }
                _ => break,
            }
            at = next;
        }
        self.params
            .end(len, u16::try_from(value).unwrap_or(u16::MAX));
        self.state = State::Csi(if at == 2 {
            Phase::Entry
        } else {
            Phase::Param(marker)
        });
        Ok((at, false))
    }
    /// Begins an OSC string and takes its bytes `bytes` begins with, up to
    /// the BEL, CAN, SUB or ESC that ends it, at once rather than each
    /// through `byte`, keeping what `byte` would keep of them; how many it
    /// took. Out of line: inlined into `sequence`, it cost each OSC more.
    #[inline(never)]
    fn osc_string(&mut self, bytes: &[u8]) -> usize {
        let length = bytes
            .iter()
            .position(|b| matches!(b, 0x07 | 0x18 | 0x1a | 0x1b))
            .unwrap_or(bytes.len());
        self.osc.clear();
        let kept = bytes.get(..length.min(self.osc_limit)).unwrap_or_default();
        self.osc.extend_from_slice(kept);
        // The byte past the limit marks the string overflowed.
        self.state = State::Osc {
            whole: length <= self.osc_limit,
        };
        length
    }

    fn ground(&mut self, byte: u8, sink: &mut impl Sink) -> Result<(), Error> {
        match byte {
            0x1b => self.state = State::Escape(Intermediates::default()),
            0x00..=0x1f | 0x7f => {
                self.control(byte, sink)?;
                self.screen.forget_repeat();
            }
            0x20..=0x7e => self.screen.print(char::from(byte))?,
            0xc2..=0xf4 => self.state = State::Utf8(Bytes::of(byte)),
            // A continuation byte alone is read as Latin-1, as xterm reads
            // it: a raw C1 control, 0x80 to 0x9f, is ignored, and 0xa0 to
            // 0xbf print U+00A0 to U+00BF.
            0x80..=0xbf => self.screen.print(char::from(byte))?,
            // A byte that starts no UTF-8 sequence: U+FFFD.
            _ => self.screen.print(char::REPLACEMENT_CHARACTER)?,
        }
        Ok(())
    }

    /// Reads `byte` after `read`, a character's first bytes.
    fn continuation(
        &mut self,
        read: Bytes<4>,
        byte: u8,
        sink: &mut impl Sink,
    ) -> Result<(), Error> {
        let [lead, ..] = read.bytes;
        let valid = (0x80..=0xbf).contains(&byte)
            && (read.len != 1
                || match lead {
                    0xe0 => byte >= 0xa0,
                    0xed => byte < 0xa0,
                    0xf0 => byte >= 0x90,
                    0xf4 => byte < 0x90,
                    _ => true,
                });
        let need = match lead {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        self.state = State::Ground;
        match read.push(byte).filter(|_| valid) {
            Some(read) if usize::from(read.len) < need => self.state = State::Utf8(read),
            Some(read) => {
                if let Some((c, _)) = decode(read.as_slice()) {
                    self.screen.print(c)?;
                }
            }
            // An invalid sequence: what came of it is one U+FFFD (Unicode
            // 17, 3.9, "U+FFFD Substitution of Maximal Subparts"), as in
            // xterm. Then this byte is read again; it can be ESC.
            None => {
                self.screen.print(char::REPLACEMENT_CHARACTER)?;
                self.ground(byte, sink)?;
            }
        }
        Ok(())
    }

    /// Execute a C0 control. BEL becomes an event when events are enabled.
    fn control(&mut self, byte: u8, sink: &mut impl Sink) -> Result<(), Error> {
        if byte == 7 && self.options.events {
            sink.event(Event::Bell);
        }
        self.screen.control(byte)
    }

    /// Reads one byte; whether it ended a CSI that began a frame.
    fn byte(&mut self, byte: u8, sink: &mut impl Sink) -> Result<bool, Error> {
        let state = self.state;
        if matches!(state, State::Ground) {
            return self.ground(byte, sink).map(|()| false);
        }
        if let State::Utf8(read) = state {
            return self.continuation(read, byte, sink).map(|()| false);
        }
        // Anywhere transitions: CAN/SUB cancel, ESC starts a new sequence. ESC
        // also begins ST, so it completes a pending OSC string or DECRQSS.
        if matches!(byte, 0x18 | 0x1a) {
            self.state = State::Ground;
            self.screen.forget_repeat();
            return Ok(false);
        }
        if byte == 0x1b {
            match state {
                State::Osc { whole } => self.dispatch_osc(whole, false, sink)?,
                State::Request(request) => self.dispatch_request(request, sink),
                State::Ground
                | State::Utf8(_)
                | State::Escape(_)
                | State::EscapeIgnore
                | State::Csi(_)
                | State::CsiIgnore
                | State::Dcs(_)
                | State::IgnoreString => {}
            }
            self.state = State::Escape(Intermediates::default());
            return Ok(false);
        }
        let mut framed = false;
        match state {
            // Returned above; named for the match to be whole.
            State::Ground | State::Utf8(_) => {}
            State::Osc { whole } => match byte {
                0x07 => {
                    self.dispatch_osc(whole, true, sink)?;
                    self.state = State::Ground;
                }
                _ if !whole => {}
                _ if self.osc.len() < self.osc_limit => self.osc.push(byte),
                // What came first stays: a prompt mark, and an OSC 8 too
                // long to keep, which closes the link, are told by it.
                _ => self.state = State::Osc { whole: false },
            },
            // In UTF-8 a string ends only at ESC (ST is ESC \, ECMA-48
            // 8.3.143): the byte 0x9c, 8-bit ST, is part of a character
            // there, as in `\u{271c}` (e2 9c 9c).
            State::Request(request) => {
                self.state = State::Request(request.and_then(|r| r.push(byte)));
            }
            State::IgnoreString => {}
            State::Escape(read) => match byte {
                0x00..=0x1f => self.control(byte, sink)?,
                0x20..=0x2f => {
                    self.state = read.push(byte).map_or(State::EscapeIgnore, State::Escape);
                }
                0x30..=0x7e => match (read.as_slice(), byte) {
                    ([], b'[') => {
                        self.params.clear();
                        self.state = State::Csi(Phase::Entry);
                    }
                    ([], b'P') => {
                        self.params.clear();
                        self.state = State::Dcs(Phase::Entry);
                    }
                    ([], b']') => {
                        self.osc.clear();
                        self.state = State::Osc { whole: true };
                    }
                    ([], b'X' | b'^' | b'_') => self.state = State::IgnoreString,
                    (intermediates, _) => {
                        self.state = State::Ground;
                        self.escape_dispatch(intermediates, byte, sink)?;
                    }
                },
                _ => {}
            },
            State::EscapeIgnore => match byte {
                0x00..=0x1f => self.control(byte, sink)?,
                0x30..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::Csi(phase) => match phase.next(byte, &mut self.params) {
                Next::Phase(phase) => self.state = State::Csi(phase),
                Next::Ignore => self.state = State::CsiIgnore,
                Next::Control => self.control(byte, sink)?,
                Next::Final(read) => {
                    self.state = State::Ground;
                    framed = self.csi_dispatch(read.as_slice(), byte, sink)?;
                }
            },
            State::CsiIgnore => match byte {
                0x00..=0x1f => self.control(byte, sink)?,
                0x40..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::Dcs(phase) => match phase.next(byte, &mut self.params) {
                Next::Phase(phase) => self.state = State::Dcs(phase),
                Next::Ignore => self.state = State::IgnoreString,
                Next::Control => {}
                Next::Final(read) => {
                    // DECRQSS: `$ q` with no parameters.
                    let decrqss = self.options.setting_reports
                        && byte == b'q'
                        && read.as_slice() == b"$"
                        && self.params.is_empty();
                    self.state = if decrqss {
                        State::Request(Some(Bytes::default()))
                    } else {
                        State::IgnoreString
                    };
                }
            },
        }
        // A sequence or string ended, REP's included: there is no character
        // for REP to repeat until one is printed.
        if matches!(self.state, State::Ground) {
            self.screen.forget_repeat();
        }
        Ok(framed)
    }

    /// Carries out the escape sequence whose final byte is `byte`, its
    /// intermediates collected.
    fn escape_dispatch(
        &mut self,
        intermediates: &[u8],
        byte: u8,
        sink: &mut impl Sink,
    ) -> Result<(), Error> {
        // DECID, the VT100's request for its identity, which the VT220
        // replaced by DA (ctlseqs: "Obsolete form of CSI c"): answered as
        // DA1 is.
        if intermediates.is_empty() && byte == b'Z' {
            let reply = Screen::primary_attributes(&self.options);
            sink.reply(reply.as_bytes());
        } else if !self.screen.escape(intermediates, byte)? {
            sink.unhandled(Unhandled::Escape {
                intermediates,
                action: byte,
            });
        }
        Ok(())
    }

    /// Carries out the CSI sequence whose final byte is `byte`, its
    /// parameters collected; whether it began a frame (set synchronized
    /// output), for `Parser::process_until_frame` to stop after.
    fn csi_dispatch(
        &mut self,
        intermediates: &[u8],
        byte: u8,
        sink: &mut impl Sink,
    ) -> Result<bool, Error> {
        let begun = self.screen.frames_begun();
        let dispatch = self
            .screen
            .csi(&self.params, intermediates, byte, &self.options)?;
        let framed = self.screen.frames_begun() != begun;
        match dispatch {
            Dispatch::Done => {}
            Dispatch::Reply(reply) => sink.reply(reply.as_bytes()),
            Dispatch::Unhandled => {
                if !intermediates.is_empty()
                    && matches!(
                        self.screen
                            .intermediate_csi(&self.params, intermediates, byte),
                        Dispatch::Done
                    )
                {
                    return Ok(framed);
                }
                match self.query_reply(intermediates, byte) {
                    Some(reply) => sink.reply(reply.as_bytes()),
                    None => sink.unhandled(Unhandled::Csi {
                        params: Params(&self.params),
                        intermediates,
                        action: byte,
                    }),
                }
            }
        }
        Ok(framed)
    }

    /// Carries out the completed OSC string, cut short by what is kept of
    /// one unless `whole`, ended by BEL if `bel`, else
    /// by ESC (ST): 133 (prompt marks), from its first bytes, with
    /// [`Options::prompt_marks`]; 8 (hyperlinks) with [`Options::hyperlinks`];
    /// the colours (4, 5, 10 to 19, 104, 105, 110 to 119) with
    /// [`Options::palette`]; 0, 1, 2, 52 and colour queries (10 to 19)
    /// delivered as events with [`Options::events`]. An overflowed string is no event, and closes any
    /// link; others are dropped.
    ///
    /// Kept out of line: inlined into `byte`, with the screen's OSC
    /// handling, it made `byte` too large to inline into the parser's loop,
    /// and escape-heavy output slower.
    #[inline(never)]
    fn dispatch_osc(&mut self, whole: bool, bel: bool, sink: &mut impl Sink) -> Result<(), Error> {
        let payload = std::mem::take(&mut self.osc);
        let result = self.osc_command(&payload, whole, bel, sink);
        // Reuse the allocation for the next OSC string.
        self.osc = payload;
        self.osc.clear();
        result
    }

    /// Answers the DECRQSS whose string just ended (see
    /// [`Options::setting_reports`]). Out of line, as `dispatch_osc` is.
    #[inline(never)]
    fn dispatch_request(&mut self, request: Option<Bytes<REQUEST_LIMIT>>, sink: &mut impl Sink) {
        let mut setting = String::new();
        let valid = match request {
            Some(request) => self.screen.setting_report(request.as_slice(), &mut setting),
            None => Some(false),
        };
        let Some(valid) = valid else {
            return;
        };
        let status = if valid { '1' } else { '0' };
        sink.reply(format!("\x1bP{status}$r{setting}\x1b\\").as_bytes());
    }

    fn osc_command(
        &mut self,
        payload: &[u8],
        whole: bool,
        bel: bool,
        sink: &mut impl Sink,
    ) -> Result<(), Error> {
        let (command, rest) = match payload.iter().position(|b| *b == b';') {
            Some(i) => (
                payload.get(..i).unwrap_or_default(),
                payload
                    .get(i..)
                    .and_then(|r| r.get(1..))
                    .unwrap_or_default(),
            ),
            None => (payload, &[][..]),
        };
        match command {
            b"133" if self.options.prompt_marks => return self.screen.prompt_osc(rest),
            // A link too long to keep is no link: what follows is printed
            // without one.
            b"8" if self.options.hyperlinks => {
                self.screen.hyperlink_osc(if whole { rest } else { &[] });
                return Ok(());
            }
            _ => {}
        }
        if self.options.palette
            && whole
            && crate::palette::osc(
                self.screen.colours_mut(),
                command,
                rest,
                bel,
                self.options.events,
                sink,
            )
        {
            return Ok(());
        }
        if !self.options.events || !whole {
            return Ok(());
        }
        match command {
            b"0" => {
                sink.event(Event::IconName(rest));
                sink.event(Event::Title(rest));
            }
            b"1" => sink.event(Event::IconName(rest)),
            b"2" => sink.event(Event::Title(rest)),
            b"52" => {
                if let Some(i) = rest.iter().position(|b| *b == b';') {
                    let selection = rest.get(..i).unwrap_or_default();
                    let data = rest.get(i..).and_then(|r| r.get(1..)).unwrap_or_default();
                    if data != b"?" {
                        sink.event(Event::Clipboard { selection, data });
                    }
                }
            }
            _ => color_queries(command, rest, bel, sink),
        }
        Ok(())
    }

    /// Replies enabled by [`Options::extended_replies`] and
    /// [`Options::identity`] for CSI sequences the screen does not answer
    /// itself.
    fn query_reply(&self, intermediates: &[u8], byte: u8) -> Option<Reply> {
        let n = self.params.first(0, 0);
        let extended = self.options.extended_replies;
        let modes = extended || self.options.mode_reports;
        let identity = self.options.identity;
        match (intermediates, byte) {
            (b"?", b'n') if n == 6 && extended => {
                let (row, col) = self.screen.reported_cursor(&self.options);
                Some(Reply::of(format_args!("\x1b[?{row};{col}R")))
            }
            (b">", b'c') if n == 0 => match identity {
                Some(identity) => {
                    let version = identity.encoded_version();
                    Some(Reply::of(format_args!("\x1b[>1;{version};0c")))
                }
                None if extended => Some(Reply::of(format_args!("\x1b[>1;10;0c"))),
                None => None,
            },
            (b">", b'q') if n == 0 => {
                let identity = identity?;
                let length = identity.name.len().checked_add(identity.version.len())?;
                if length > Identity::MAX_LEN {
                    return None;
                }
                let (name, version) = (identity.name, identity.version);
                Some(Reply::of(format_args!("\x1bP>|{name} {version}\x1b\\")))
            }
            (b"", b't') if n == 18 && self.options.size_reports => {
                let (rows, cols) = self.screen.size();
                Some(Reply::of(format_args!("\x1b[8;{rows};{cols}t")))
            }
            (b"*", b'y') if self.options.rectangle_checksums => {
                Some(self.screen.rectangle_checksum(&self.params))
            }
            (b"?$", b'p') if modes => {
                let status = match n {
                    2048 if !self.options.in_band_resize => 0,
                    2031 if !self.options.color_scheme_updates => 0,
                    _ => self.screen.private_mode_status(n),
                };
                Some(Reply::of(format_args!("\x1b[?{n};{status}$y")))
            }
            (b"$", b'p') if modes => {
                // IRM and LNM alone of the ANSI modes are known.
                let status = match n {
                    4 if self.screen.insert_mode() => 1,
                    20 if self.screen.new_line_mode() => 1,
                    4 | 20 => 2,
                    _ => 0,
                };
                Some(Reply::of(format_args!("\x1b[{n};{status}$y")))
            }
            _ => None,
        }
    }
}
