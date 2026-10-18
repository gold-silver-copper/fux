use crate::compact::{Compact, PROTECTED};
use crate::keys::KITTY_FLAGS_KEPT;
use crate::keys::encode::OtherKeys;
use crate::link::Held;
use crate::mode::{Kind, Mode, ModeStatus, Modes, RESET_FOR_GOOD, SET_FOR_GOOD, Switch};
use crate::unicode::Cluster;
use crate::{
    Attributes, Blink, CellRef, Color, Error, Feature, Hyperlink, Identity, LineRendition, Mark,
    Options, Prints, Reply, Rgb, Row, RowId, Rows, SgrForm, UnderlineStyle, Window,
    geometry::{Size, Span},
    grid::{Cursor, Erase, Grid, Scroll, Written},
    palette::Colours,
    parser::Parameters,
};
use std::{collections::VecDeque, mem, ops::Range};

/// The blank style (`Screen::blank_style`) not asked for since the pen
/// changed. The pen's own style is found at once (`pen_changed`).
const UNKNOWN: u32 = u32::MAX;
/// A pen style that is no number of its own: the grid's table has it.
const TABLED: u32 = u32::MAX - 1;

/// The mouse reporting a program asked for, the latest set winning
/// (`CSI ? 9 / 1000 / 1002 / 1003 h`). fux-vt sends no report of its own;
/// the host encodes one with `Screen::encode_mouse`, which reads this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocolMode {
    /// No reporting.
    #[default]
    None,
    /// Mode 9, X10: button presses.
    Press,
    /// Mode 1000: presses and releases.
    PressRelease,
    /// Mode 1002: also motion while a button is down.
    ButtonMotion,
    /// Mode 1003: also motion with no button down.
    AnyMotion,
}
closed! {
    /// The cursor's style, as DECSCUSR sets it (`CSI Ps SP q`; ctlseqs,
    /// "Set cursor style", VT520): Ps 0 and 1 are both a blinking block, 7
    /// xterm's initial resources. A Ps it lists no style for is ignored.
    /// Ps 0 is kept apart from 1, as many terminals read it as their own
    /// shape: a host passes on the program's 0 as 0.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub enum CursorShape {
        /// Its Ps.
        pub fn number(self) -> u16;
        /// The terminal's own, which fux-vt does not know: before any
        /// DECSCUSR, after RIS, and as DECSCUSR 7 restores it.
        #[default]
        Initial => 7,
        /// A blinking block, as Ps 0 (or none) names it.
        Zero => 0,
        /// A blinking block (Ps 1).
        BlinkingBlock => 1,
        /// A steady block.
        SteadyBlock => 2,
        /// A blinking underline.
        BlinkingUnderline => 3,
        /// A steady underline.
        SteadyUnderline => 4,
        /// A blinking bar.
        BlinkingBar => 5,
        /// A steady bar.
        SteadyBar => 6,
    }
}

/// How mouse reports are to be encoded (`CSI ? 1005 / 1006 h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    /// The legacy encoding: coordinates as single bytes.
    #[default]
    Default,
    /// Mode 1005: coordinates in UTF-8.
    Utf8,
    /// Mode 1006, SGR: `CSI < b ; x ; y M`, or `m` for a release.
    Sgr,
}

/// The cell printed last, the state of its grapheme cluster, and whether
/// the cluster is full: once a character of it is dropped, so is every one
/// after, so a cell always holds a start of its cluster.
#[derive(Clone, Copy, Debug)]
struct Printed {
    at: (u16, u16),
    cluster: Cluster,
    full: bool,
}

impl Printed {
    fn new(at: (u16, u16), c: char) -> Self {
        Self {
            at,
            cluster: Cluster::start(c),
            full: false,
        }
    }
}

/// The character sets a program designates and shifts between (ECMA-35;
/// DEC STD 070, ch. 3; the VT520 manual's SCS): G0 to G3, each ASCII or
/// DEC Special Graphics, the one in GL, and the one a single shift invokes
/// for the next character. The national sets are not kept: a set other
/// than Special Graphics is ASCII, as xterm reads one in UTF-8. Four
/// bytes, aligned as a `u32`, which `ascii` reads once a run: three bytes
/// among the screen's other bytes made the printing paths count up to
/// 0.3% more instructions as those changed (bench-commit).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, align(4))]
pub(crate) struct Charsets {
    /// Which sets are DEC Special Graphics, a bit each (`Set::bit`).
    graphics: u8,
    /// The set in GL: G0 after SI, G1 after SO, G2 after LS2, G3 after LS3.
    gl: Set,
    /// G2 after SS2, G3 after SS3: the set of the next graphic character.
    single: Option<Set>,
}

/// One of the four sets a program designates (ECMA-35, 6.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Set {
    #[default]
    G0,
    G1,
    G2,
    G3,
}

impl Set {
    fn bit(self) -> u8 {
        1 << self as u8
    }
}

impl Charsets {
    /// Whether text takes more than printing as it is: a single shift
    /// waits, or the set in GL is DEC Special Graphics. `ascii` asks once
    /// a run.
    fn graphics(self) -> bool {
        self.single.is_some() || self.graphics & self.gl.bit() != 0
    }
    /// The character `raw` prints as, in the set a single shift invokes,
    /// which it spends, or else the set in GL.
    fn map(&mut self, raw: char) -> char {
        let set = self.single.take().unwrap_or(self.gl);
        if self.graphics & set.bit() != 0 {
            special_graphics(raw)
        } else {
            raw
        }
    }
    /// SCS: `set` designated DEC Special Graphics, or else ASCII.
    fn designate(&mut self, set: Set, graphics: bool) {
        self.graphics = (self.graphics & !set.bit()) | if graphics { set.bit() } else { 0 };
    }
}

/// What DECSC saves and DECRC restores: the cursor, with its pending wrap
/// and origin mode (DEC STD 070, Appendix D.6.1), the pen, the character
/// sets and DECSCA's selective erase attribute (as the VT420 manual lists
/// it; SPA's guarded area is no state of the cursor's). Each grid keeps its
/// own, as each of xterm's screens does.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Saved {
    pub(crate) cursor: Cursor,
    pub(crate) attributes: Attributes,
    pub(crate) charsets: Charsets,
    pub(crate) protect: u32,
}

/// The DEC Special Graphics character for `c`, as xterm draws it: 0x5f to
/// 0x7e are a blank, a diamond, a checkerboard, control pictures, degree
/// and plus-minus, line drawing, scan lines, comparisons, pi, not-equal,
/// pound and a middle dot (VT520 manual, Special Graphics set; xterm,
/// `fux-vt-compare replay --engines xterm` of `ESC ( 0` and the bytes 0x5f to 0x7e).
fn special_graphics(c: char) -> char {
    const GRAPHICS: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻',
        '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    u32::from(c)
        .checked_sub(0x5f)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| GRAPHICS.get(i))
        .copied()
        .unwrap_or(c)
}

/// What a cell adds to a DECRQCRA checksum (`Screen::rectangle_checksum`),
/// as xterm counts it by default (`xtermCheckRect`, `xtermCharSetDec`):
/// its character, a DEC Special Graphics glyph as the code it was drawn
/// with (0x60 to 0x7e), any other past Latin-1 or below a space as ESC; its
/// combining marks as they are; and its VT100 attributes.
fn checksum_of(cell: &CellRef<'_>) -> u16 {
    const ESC: u16 = 0x1b;
    let mut chars = cell.contents().chars();
    let base = match chars.next() {
        None if cell.is_wide_continuation() => ESC,
        None => 0x20,
        Some(c) => match u16::try_from(u32::from(c)) {
            Ok(code @ 0x20..=0xff) => code,
            _ => (0x60u16..=0x7e)
                .find(|&code| char::from_u32(u32::from(code)).map(special_graphics) == Some(c))
                .unwrap_or(ESC),
        },
    };
    let marks = chars.fold(0u16, |sum, c| {
        sum.wrapping_add(u16::try_from(u32::from(c) & 0xffff).unwrap_or(0))
    });
    let a = cell.attributes();
    let rendition = [
        (a.hidden(), 0x08),
        (a.underline(), 0x10),
        (a.inverse(), 0x20),
        (a.blink() != Blink::None, 0x40),
        (a.bold(), 0x80),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0u16, |sum, (_, bit)| sum | bit);
    base.wrapping_add(marks).wrapping_add(rendition)
}

/// The tab stops (ECMA-48: HTS sets one, TBC clears; HT, CHT and CBT move
/// to them), one set for both screens, as in xterm: one every eight
/// columns at first and after RIS. Kept for every column the terminal has
/// had, so a resize keeps them, as xterm keeps them; past those, a column
/// is a stop if it is a multiple of eight and no TBC 3 came since the last
/// reset.
#[derive(Clone, Debug)]
struct TabStops {
    stops: Vec<bool>,
    /// Whether the columns past `stops` have the stops of a reset.
    beyond: bool,
}

impl Default for TabStops {
    fn default() -> Self {
        Self {
            stops: Vec::new(),
            beyond: true,
        }
    }
}

impl TabStops {
    fn is_stop(&self, col: u16) -> bool {
        match self.stops.get(usize::from(col)) {
            Some(stop) => *stop,
            None => self.beyond && col.is_multiple_of(8),
        }
    }
    fn set(&mut self, col: u16, stop: bool) {
        let col = usize::from(col);
        while self.stops.len() <= col {
            let at = self.stops.len();
            self.stops.push(self.beyond && at.is_multiple_of(8));
        }
        if let Some(slot) = self.stops.get_mut(col) {
            *slot = stop;
        }
    }
    /// TBC 3: no stops anywhere.
    fn clear(&mut self) {
        self.stops.clear();
        self.beyond = false;
    }
    /// The first stop after `col`, or `last`, the last column, if none
    /// comes before it.
    fn next(&self, col: u16, last: u16) -> u16 {
        if self.stops.is_empty() && self.beyond {
            // A stop every eight columns.
            return (col / 8).saturating_add(1).saturating_mul(8).min(last);
        }
        let mut at = col;
        while at < last {
            at = at.saturating_add(1);
            if self.is_stop(at) {
                return at;
            }
        }
        last
    }
    /// The last stop before `col` from `first` on, or `first`, the first
    /// column the cursor may go back to.
    fn previous(&self, col: u16, first: u16) -> u16 {
        let mut at = col;
        while at > first {
            at = at.saturating_sub(1);
            if self.is_stop(at) {
                return at;
            }
        }
        first
    }
}

/// How DECRQSS answers a request (`Screen::setting_report`).
pub(crate) enum Setting {
    /// The setting, written.
    Written,
    /// A request fux-vt does not know, which xterm answers as invalid.
    Unknown,
    /// No answer: the terminal's own cursor shape (`CursorShape::Initial`),
    /// which fux-vt does not know. xterm and Ghostty always know
    /// theirs, and vim, the program that asks, takes an invalid answer for
    /// keys.
    Unanswered,
}

/// What a CSI sequence came to.
pub(crate) enum Dispatch {
    /// Carried out, with nothing to answer.
    Done,
    /// A query, and its answer.
    Reply(Reply),
    /// Not a sequence the screen implements.
    Unhandled,
}

/// What DSR 75 reports of the data integrity flag (DEC STD 070, DSR, 775):
/// power-up (73) until it has been reported once after the screen is made
/// or RIS, then no error since the last report (70). Four bytes, as a
/// `u32`: a byte among the screen's flags made the printing paths count
/// up to 0.3% more instructions (bench-commit), by the loads of its
/// neighbours the compiler joins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Integrity {
    #[default]
    PowerUp,
    Reported,
}

impl Integrity {
    /// The report's number.
    pub(crate) fn report(self) -> u8 {
        match self {
            Self::PowerUp => 73,
            Self::Reported => 70,
        }
    }
}

/// The cold state, which RIS starts again as it was made: one owner of
/// both, so a field of it cannot be left out of either. The screen's
/// other fields (`protected`, `modes`, `alternate_active`, `pictures`)
/// are set in both by hand.
#[derive(Clone, Debug, Default)]
struct Terminal {
    /// Left by 1047 with this pen: the alternate screen is cleared in its
    /// colours when shown again, and keeps what the program drew till then
    /// (`alternate_rows`).
    clear_on_show: Option<Attributes>,
    tabs: TabStops,
    cursor_shape: CursorShape,
    mouse: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    modify_other_keys: Option<OtherKeys>,
    /// What DSR 75 reports of the data integrity flag.
    integrity: Integrity,
}

/// Terminal state. Reading a window never changes where subsequent output lands.
#[derive(Clone, Debug)]
pub struct Screen {
    primary: Grid,
    alternate: Grid,
    /// Pictures placed on either grid (`pictures`), the last one's number,
    /// and what the host's terminal draws.
    pictures: VecDeque<pictures::Placement>,
    picture_ids: u64,
    graphics: Graphics,
    /// The most bytes the pictures may hold (`PICTURE_BYTES`).
    picture_budget: usize,
    /// The shell's command marks (`shell`).
    commands: Commands,
    next_id: u64,
    version: u64,
    structural: u64,
    alternate_active: bool,
    attributes: Attributes,
    /// The pen's style, and the style of its colours alone (what erasing
    /// fills with), if each is a style that is its own number (`style.rs`),
    /// the same in either grid; else `TABLED`. The pen's is found when the
    /// pen changes (`pen_changed`); the colours' is `UNKNOWN` until it is
    /// first asked for after that. So printing and erasing in the pen look
    /// nothing up but a `TABLED` style, which the grid finds among those it
    /// found lately (`Grid::table_style`).
    pen_style: u32,
    blank_style: u32,
    charsets: Charsets,
    /// The modes kept as bits (`Kind::Flag`, `Margins`, `Frame`,
    /// `SizeReport`) that are set.
    modes: Modes,
    /// The modes XTSAVE saved set. One never saved is reset, as in xterm,
    /// and RIS and DECSTR keep them, as xterm 411 does.
    saved_modes: Modes,
    /// [`PROTECTED`] while DECSCA 1 protects the glyphs printed from
    /// DECSED and DECSEL (DEC STD 070, 5.11.1.2), else 0: or-ed into the
    /// style they are written in. DECSC saves it, as xterm does.
    protect: u32,
    /// `TABLED` while the glyphs printed are in SPA's guarded area
    /// (ECMA-48 8.3.140), which ED, EL and ECH leave, else 0: or-ed into
    /// the pen's style (`pen_changed`), it makes that the table's, where
    /// the style keeps `Attributes::GUARDED` (`pen_style`). Folded so, the
    /// guard costs `pen_changed`, which every SGR calls, no branch.
    guard: u32,
    /// Whether a glyph either protects may be on a grid: erases look for
    /// them only then. Set at DECSCA 1 and SPA; RIS clears it.
    protected: bool,
    /// How many times synchronized output has been set, for
    /// `Parser::process_until_stop`.
    frames_begun: u64,
    /// What RIS starts again.
    terminal: Terminal,
    /// The cell the last glyph was printed in, and the state of its
    /// grapheme cluster, while the cursor has not moved nor the row been
    /// edited since: a character that continues the cluster joins its cell
    /// rather than taking one of its own.
    last_print: Option<Printed>,
    /// The character REP (`CSI b`) repeats: the last one printed that took
    /// a cell of its own, as long as nothing but printing came after it.
    repeat: Option<char>,
    /// The hyperlink the program opened (OSC 8), which glyphs printed take.
    link: Option<Hyperlink>,
    /// Whether a link was opened since the screen was made or reset: until
    /// one is, no row has links, and printing, which asks this alone,
    /// leaves the links be.
    links_seen: bool,
    /// The key the next link opened takes (`Hyperlink::key`).
    next_link: u64,
    /// The colours the program set, with `Feature::Palette`: made when
    /// the first is set, so a screen whose program sets none keeps none.
    colours: Option<Box<Colours>>,
    /// What was printed since the host last took it, while it records
    /// (`record_prints`): text drawn and drawn over before it reads the
    /// screen is there still.
    prints: Option<Prints>,
    /// The nonce a shell mark must carry to count (`require_mark_nonce`):
    /// one without it is a program's output, not its shell's.
    mark_nonce: Option<Box<[u8]>>,
    /// The nonce a mark of a program the shell runs carries to count, while
    /// the command that started it runs (`require_mark_nonce`).
    nested_nonce: Option<Box<[u8]>>,
    /// The shell's running command, set aside while marks of a program it
    /// runs say what runs: its output's start, and whether that began on
    /// the alternate screen.
    outer: Option<(CommandMark, bool)>,
    /// Where the cursor was as the host last said a line was submitted
    /// (`note_submit`): a late `C` starts the output after that line.
    submitted: Option<CommandMark>,
}

/// What a mark joined to a cell prints: the mark, after the space a blank
/// cell takes for it; in `buf`.
fn joined_text(blank: bool, c: char, buf: &mut [u8; 8]) -> &str {
    let (space, rest) = buf
        .split_at_mut_checked(usize::from(blank))
        .unwrap_or_default();
    if let Some(space) = space.first_mut() {
        *space = b' ';
    }
    let len = c.encode_utf8(rest).len().saturating_add(usize::from(blank));
    buf.get(..len)
        .and_then(|b| str::from_utf8(b).ok())
        .unwrap_or_default()
}

/// The screen's part of the invariant checker (threats.md section 6).
#[cfg(feature = "checks")]
impl Screen {
    /// Checks every invariant of the screen that its state alone shows:
    /// both grids, their history, styles and links, the row identities,
    /// the pictures and `Prints`; and which checks ran, and the faults
    /// found ([`crate::Checked`]). The work counters do not see it.
    pub fn check(&self) -> crate::Checked {
        crate::check::unseen(|| {
            let mut report = crate::Checked::default();
            for (which, grid) in [("primary", &self.primary), ("alternate", &self.alternate)] {
                grid.check(which, self.version, &mut report);
            }
            report.record("screen.ids", "", self.check_ids());
            report.record("screen.structural", "", self.check_structural());
            report.record("screen.tabs", "", self.check_tabs());
            report.record("screen.nest", "", self.check_nest());
            report.record("pictures.count", "", self.check_picture_count());
            report.record("pictures.bytes", "", self.check_picture_bytes(0));
            report.record("pictures.shown", "", self.check_pictures_shown());
            if let Some(prints) = &self.prints {
                report.record("prints.runs", "", prints.check());
                report.record("prints.cap", "", prints.check_cap());
            }
            report.wait("grid.pending_wrap", "P11: the tag of which setter set it");
            report.wait("styles.memo", "P22: the DECCARA memo");
            report.wait("grid.uniform", "P19: uniform rows");
            report
        })
    }

    /// 6a: a shell command is set aside for a nested program's marks only
    /// while both nonces are required: the shell's, and the nested one its
    /// programs mark with (threats.md 1.2, REB-4).
    fn check_nest(&self) -> Result<(), String> {
        crate::check::ensure(
            self.outer.is_none() || (self.mark_nonce.is_some() && self.nested_nonce.is_some()),
            || "a command set aside without both nonces".into(),
        )
    }

    /// 20a: no row identity is two rows', in history or on either screen,
    /// and each of the shown screen's rows, and history's oldest and
    /// newest, is found by its identity where it is.
    fn check_ids(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for grid in [&self.primary, &self.alternate] {
            for row in Rows::of(grid) {
                crate::check::ensure(seen.insert(row.id()), || {
                    format!("row {:?} is held twice", row.id())
                })?;
            }
        }
        let grid = self.grid();
        let history = grid.history_len();
        let mut places: Vec<usize> = (history..grid.retained_len()).collect();
        places.extend(
            [0, history.saturating_sub(1)]
                .into_iter()
                .filter(|i| *i < history),
        );
        for index in places {
            let Some(row) = grid.row_at(index) else {
                return Err(format!("no row {index}"));
            };
            crate::check::ensure(grid.index_of(row.id()) == Some(index), || {
                format!(
                    "row {:?} at {index} is found at {:?}",
                    row.id(),
                    grid.index_of(row.id())
                )
            })?;
        }
        Ok(())
    }

    /// 20: a structural change is no later than the screen's version.
    fn check_structural(&self) -> Result<(), String> {
        crate::check::ensure(self.structural <= self.version, || {
            format!(
                "structural {} past version {}",
                self.structural, self.version
            )
        })
    }

    /// 36: tab stops are kept for no more columns than a grid has.
    fn check_tabs(&self) -> Result<(), String> {
        let kept = self.terminal.tabs.stops.len();
        crate::check::ensure(kept <= usize::from(u16::MAX).saturating_add(1), || {
            format!("tab stops kept for {kept} columns")
        })
    }
}

mod pictures;
mod shell;
pub use pictures::{Graphics, PICTURE_BYTES, PLACEHOLDER, Shown};
pub use shell::{Commands, Mark as CommandMark, Report, Reported};
#[cfg(test)]
mod tests;

/// Palette colour `n - from`, for an SGR parameter `n` of a range that
/// starts at colour `from`'s.
fn indexed(n: u16, from: u16) -> Color {
    Color::Idx(u8::try_from(n.saturating_sub(from)).unwrap_or(0))
}

/// The colour of 38, 48 or 58, `rest` being what follows the selector:
/// `5:index`, `2:r:g:b`, or ITU-T T.416's `2:space:r:g:b`, its colour
/// space ignored and empty, as xterm reads it, with anything after the
/// blue ignored. `None` if the colour is out of range or of another kind.
fn colour_of(rest: &[u16]) -> Option<Color> {
    match rest {
        [5, index, ..] => u8::try_from(*index).ok().map(Color::Idx),
        [2, r, g, b] | [2, _, r, g, b, ..] => Some(Color::Rgb(Rgb {
            r: u8::try_from(*r).ok()?,
            g: u8::try_from(*g).ok()?,
            b: u8::try_from(*b).ok()?,
        })),
        _ => None,
    }
}

/// Sets a mouse mode or encoding `value` in `slot`, the latest set
/// winning; reset, it is the default again if it is the one set.
fn latest<T: Copy + Default + PartialEq>(slot: &mut T, value: T, on: bool) {
    if on {
        *slot = value;
    } else if *slot == value {
        *slot = T::default();
    }
}

/// Whether a glyph `width` wide at `i` has its second half after it, if it
/// needs one. Kept out of line, as `unchanged` is (grid.rs), so that the
/// print that usually follows compiles as if it were not there.
#[inline(never)]
fn whole(cells: &[Compact], i: usize, width: u16) -> bool {
    width != 2
        || i.checked_add(1)
            .and_then(|j| cells.get(j))
            .is_some_and(|c| *c == Compact::continuation())
}

impl Screen {
    pub(crate) fn new(size: Size, history: usize) -> Result<Self, Error> {
        let mut next_id = 1;
        Ok(Self {
            primary: Grid::new(size, history, &mut next_id, 1)?,
            // Its cells are made when a program first shows it.
            alternate: Grid::unmade(size, &mut next_id, 1)?,
            pictures: VecDeque::new(),
            picture_ids: 0,
            graphics: Graphics::default(),
            picture_budget: PICTURE_BYTES,
            commands: Commands::default(),
            next_id,
            version: 1,
            structural: 1,
            alternate_active: false,
            attributes: Attributes::default(),
            pen_style: 0,
            blank_style: 0,
            charsets: Charsets::default(),
            modes: Modes::DEFAULT,
            saved_modes: Modes::default(),
            protect: 0,
            guard: 0,
            protected: false,
            frames_begun: 0,
            terminal: Terminal::default(),
            last_print: None,
            repeat: None,
            link: None,
            links_seen: false,
            next_link: 1,
            colours: None,
            prints: None,
            mark_nonce: None,
            nested_nonce: None,
            outer: None,
            submitted: None,
        })
    }
    /// The primary screen's grid, as a test starts from it.
    #[cfg(test)]
    pub(crate) fn primary_grid(&self) -> &Grid {
        &self.primary
    }
    /// Makes the alternate screen's cells now, as a test compares a screen
    /// that made them at once with one that waited.
    #[cfg(test)]
    pub(crate) fn make_alternate(&mut self) -> Result<(), Error> {
        self.alternate.make()
    }
    fn grid(&self) -> &Grid {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }
    /// The scrollback, dropped; the screen stays. It is the main screen's,
    /// on the alternate screen too, which keeps none (ctlseqs: ED 3 "Erase
    /// Saved Lines").
    pub(crate) fn clear_history(&mut self) {
        self.primary.clear_history();
        self.structural = self.version;
    }

    fn grid_mut(&mut self) -> &mut Grid {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }
    fn with_grid<T>(&mut self, f: impl FnOnce(&mut Grid, &mut u64, u64) -> T) -> T {
        let grid = if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        f(grid, &mut self.next_id, self.version)
    }
    /// Finds the pen's style again, and forgets the style of its colours
    /// (`pen_style`, `blank_style`), to find it when next asked for: every
    /// change of `attributes` is followed by this.
    fn pen_changed(&mut self) {
        self.pen_style = self.attributes.inline_style().unwrap_or(TABLED) | self.guard;
        self.blank_style = UNKNOWN;
    }
    /// The number of the pen's style in the grid shown.
    #[inline]
    fn pen_style(&mut self) -> u32 {
        let style = self.pen_style;
        if style < TABLED {
            return style;
        }
        let attributes = self.attributes.with_guard(self.guard != 0);
        self.grid_mut().table_style(attributes)
    }
    /// The number, in the grid shown, of the style of the pen's colours
    /// alone, which erasing, scrolling and inserting fill cells with (`bce`).
    #[inline]
    fn blank_style(&mut self) -> u32 {
        let attributes = self.attributes.erased();
        if self.blank_style == UNKNOWN {
            self.blank_style = attributes.inline_style().unwrap_or(TABLED);
        }
        let style = self.blank_style;
        if style < TABLED {
            return style;
        }
        self.grid_mut().table_style(attributes)
    }
    /// A new version, for what is processed next: like identities, 64 bits
    /// counted up by one do not run out.
    pub(crate) fn begin(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
    /// The screen's rows and columns.
    pub fn size(&self) -> Size {
        self.grid().size()
    }
    /// The cursor's row and column, always on the screen. A glyph printed
    /// in the last column (or at the right margin) leaves the cursor on
    /// it, with [`pending_wrap`](Self::pending_wrap) set while autowrap is
    /// on.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.grid().cursor.at()
    }
    /// DEC STD 070's Last Column Flag: a glyph went into the last column,
    /// or the right margin's (DECSLRM, with DECLRMM set), with autowrap
    /// on, and the next one first moves to the start of the next line,
    /// wherever the cursor waits (a margin reset since leaves it where the
    /// margin was). Cursor movements, line feeds, edits and resetting
    /// autowrap end it; DECSC and SCOSC save it with the cursor.
    pub fn pending_wrap(&self) -> bool {
        self.grid().cursor.pending_wrap && self.mode(Mode::Autowrap)
    }
    /// The Last Column Flag, autowrap on or off: a glyph's, set with it on,
    /// or one DECRC restored.
    fn last_column_flag(&self) -> bool {
        let cursor = self.grid().cursor;
        cursor.pending_wrap && (cursor.restored || self.mode(Mode::Autowrap))
    }
    /// Whether `mode` is set, as DECRQM reports it. The alternate screen's
    /// three modes are set together, while it is shown; DECARM and 1048
    /// never are.
    #[inline]
    pub fn mode(&self, mode: Mode) -> bool {
        match mode.kind() {
            Kind::Flag | Kind::Margins | Kind::Frame | Kind::SizeReport => {
                self.modes.contains(mode)
            }
            Kind::Origin => self.grid().cursor.origin,
            Kind::Screen(_) => self.alternate_active,
            Kind::SaveCursor => false,
            Kind::Permanent(set) => set,
            Kind::Mouse(mouse) => self.terminal.mouse == mouse,
            Kind::Encoding(encoding) => self.terminal.encoding == encoding,
        }
    }
    /// The in-band resize report of the current size, pixels unknown.
    pub(crate) fn size_report(&self) -> Reply {
        let (rows, cols) = self.size().into();
        reply!("\x1b[48;{rows};{cols};0;0t")
    }
    /// Whether XTSAVE holds `mode` set.
    pub(crate) fn saved_mode(&self, mode: Mode) -> bool {
        self.saved_modes.contains(mode)
    }
    pub(crate) fn frames_begun(&self) -> u64 {
        self.frames_begun
    }
    /// The cursor shape last set with DECSCUSR (`CSI Ps SP q`). State
    /// only: fux-vt draws no cursor.
    pub fn cursor_shape(&self) -> CursorShape {
        self.terminal.cursor_shape
    }
    /// The top and bottom margins (DECSTBM), zero-based and inclusive.
    pub fn scroll_region(&self) -> (u16, u16) {
        let lines = self.grid().lines;
        (lines.first(), lines.last())
    }
    /// The left and right margins (DECSLRM, with DECLRMM set), zero-based
    /// and inclusive: the first and last columns unless a program set
    /// them.
    pub fn left_right_margins(&self) -> (u16, u16) {
        let columns = self.grid().columns();
        (columns.first(), columns.last())
    }
    /// The mouse reporting the program asked for.
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.terminal.mouse
    }
    /// How the program asked for mouse reports to be encoded.
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.terminal.encoding
    }
    /// The kitty keyboard protocol flags in force: the top of the current
    /// screen's stack, 0 (legacy key reporting) when it is empty. Always 0
    /// without [`Feature::KittyKeyboard`].
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.grid().keyboard.top()
    }
    /// The xterm modifyOtherKeys level set by `CSI > 4 ; Pv m`, `None` when
    /// it is off (`Pv` 0 or absent, or past 3, which ctlseqs gives no keys).
    /// Always `None` without [`Feature::KittyKeyboard`].
    pub fn modify_other_keys(&self) -> Option<OtherKeys> {
        self.terminal.modify_other_keys
    }
    /// The one-based cursor position a DSR 6n or DECXCPR reports. The line
    /// is counted as CUP addresses it: from the top margin with DECOM set
    /// (DEC STD 070, CPR and DECXCPR, pages 5-53 to 5-56; xterm's
    /// `CASE_CPR`), and a cursor above the margin, should one be, on the
    /// first line. A cursor waiting to wrap is reported at its column, the
    /// last: the Last Column Flag is a flag (DEC STD 070, D.6.1), and CPR
    /// reports the active position (ECMA-48 8.3.14; DEC STD 070, CPR).
    pub(crate) fn reported_cursor(&self) -> (u32, u32) {
        let g = self.grid();
        let col = g.cursor.col();
        // In origin mode the column counts from the left margin, as xterm
        // reports it.
        let col = col.saturating_sub(g.addressed().1.first());
        (u32::from(g.cursor_line()) + 1, u32::from(col) + 1)
    }
    /// The pen: the colours and rendition of the next glyph printed.
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }
    /// The cell at `row`, `col` of the screen.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
        self.grid().cell(row, col)
    }
    /// The hyperlink of the cell at `row`, `col` of the screen (see
    /// [`Row::link`]). Always `None` without [`Feature::Hyperlinks`].
    pub fn link(&self, row: u16, col: u16) -> Option<&Hyperlink> {
        self.grid().live_row(row)?.link(usize::from(col))
    }
    /// The hyperlink the program has open (`OSC 8 ; params ; URI ST`), which
    /// glyphs printed now take: its URI and `id`.
    pub fn hyperlink(&self) -> Option<(&str, Option<&str>)> {
        self.link.as_ref().map(|link| (link.uri(), link.id()))
    }
    /// Whether row `row` of the screen is where a prompt starts (see
    /// [`Row::starts_prompt`]).
    pub fn starts_prompt(&self, row: u16) -> bool {
        self.grid().live_row(row).is_some_and(|r| r.starts_prompt())
    }

    /// The colour palette entry `index` shows if the program changed it
    /// (OSC 4, with `Feature::Palette`), as red, green and blue; `None`
    /// while it is the terminal's own, and after OSC 104, DECSTR or RIS
    /// reset it. A cell of `Color::Idx(index)` shows this colour.
    pub fn palette_color(&self, index: u8) -> Option<Rgb> {
        self.colours.as_ref()?.palette(index)
    }
    /// Sets the host's colours for palette entries 0 to 15 ([`crate::Parser::set_host_palette`]).
    pub(crate) fn set_host_palette(&mut self, palette: [Option<Rgb>; 16]) {
        if palette != [None; 16] || self.colours.is_some() {
            self.colours.get_or_insert_default().host = palette;
        }
    }
    /// The colour dynamic colour `number` shows if the program set it (OSC
    /// 10 to 19, with `Feature::Palette`): 10 the text foreground and 11
    /// the background, which a cell of `Color::Default` shows, 12 the
    /// cursor, and the others xterm's pointer, Tektronix and highlight
    /// colours. `None` while it is the terminal's own, and after OSC 110 to
    /// 119 reset it.
    pub fn dynamic_color(&self, number: u8) -> Option<Rgb> {
        self.colours.as_ref()?.dynamic(number)
    }
    /// Whether the program changed a palette entry or a dynamic colour, so
    /// that a host drawing the screen knows to ask `palette_color` and
    /// `dynamic_color`.
    pub fn colors_changed(&self) -> bool {
        self.colours.as_ref().is_some_and(|c| c.changed())
    }
    /// The colours the program set, for the parser's OSC handling.
    pub(crate) fn colours_mut(&mut self) -> &mut Option<Box<Colours>> {
        &mut self.colours
    }

    /// OSC 8: opens the link `payload` names, or closes the open one
    /// (`link::parse`). A link without an `id` is a link of its own each
    /// time it is opened, as VTE makes it (the spec's "Hover underlining and
    /// the `id` parameter").
    pub(crate) fn hyperlink_osc(&mut self, payload: &[u8]) {
        self.link = crate::link::parse(payload).and_then(|(uri, id)| {
            let key = self.next_link;
            // Keys never run out in practice: one a link opened.
            self.next_link = key.checked_add(1)?;
            Some(Hyperlink {
                uri: uri.into(),
                id: id.map(Into::into),
                key,
            })
        });
        // Neither grid has made a link of the one open now.
        self.primary.links.open = Held::Pending;
        self.alternate.links.open = Held::Pending;
        self.links_seen |= self.link.is_some();
    }

    /// Gives the cells `span` of row `row` of the grid shown the open link,
    /// if `open`, or none: what printing glyphs there does to their links,
    /// once a link has been opened (`links_seen`). Out of line, so that
    /// printing, which never needs it until then, carries none of it.
    #[inline(never)]
    fn link_cells(&mut self, row: u16, span: Range<usize>, open: bool) {
        let version = self.version;
        let pen = self.link.as_ref().filter(|_| open);
        let grid = if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        grid.print_link(row, span, pen, version);
    }

    /// OSC 133, semantic prompts (`references/modern/osc133_*`): `A`
    /// marks the row a prompt starts on, after a fresh line, and `L` is the
    /// fresh line alone: a new line unless the cursor is in the first
    /// column, as the semantic prompt proposal and Ghostty have it. `N` is
    /// `A` that first ends the command running, as a `D` with no status.
    /// `C` and `D` mark a command (`command_mark`), `P` a prompt where it
    /// is; the rest (`B` and others) change nothing. `S` reports a state
    /// (`report`). Every mark that counts is counted (`Commands::marks`).
    pub(crate) fn prompt_osc(&mut self, payload: &[u8]) -> Result<(), Error> {
        let mut parts = payload.split(|b| *b == b';');
        let carries = |nonce: &[u8]| {
            (payload.split(|b| *b == b';')).any(|o| o.strip_prefix(b"nonce=") == Some(nonce))
        };
        let shell = self.mark_nonce.as_deref().is_none_or(carries);
        let nested = !shell && self.nested_nonce.as_deref().is_some_and(carries);
        // A state report counts as the marks around it do, and changes
        // none of what they say: the shell's, or the nested nonce's while
        // a command the shell started runs.
        if let Some(options) = payload.strip_prefix(b"S") {
            let within = self.outer.is_some() || self.commands.running();
            if (options.is_empty() || options.starts_with(b";")) && (shell || (nested && within)) {
                self.report(options);
            }
            return Ok(());
        }
        if shell {
            self.unnest();
        } else if !(nested && self.nest()) {
            // Printed by a program, not its shell nor one given the
            // nested nonce while its command runs: no mark.
            return Ok(());
        }
        let command = parts.next().unwrap_or_default();
        self.commands.marks = self.commands.marks.wrapping_add(1);
        if matches!(command, b"C" | b"D") {
            self.command_mark(command, parts.next());
            return Ok(());
        }
        // P (explicit start of prompt) of the primary kind, `k=i` or none,
        // marks where it is without a fresh line: shells send it in A's
        // place (Ghostty's bash integration under ble.sh does). Right-side
        // and continuation prompts (`k=r`, `k=c`, `k=s`) start none.
        if command == b"P" {
            let kind = parts.find_map(|option| option.strip_prefix(b"k="));
            if kind.is_none_or(|k| k == b"i") {
                self.prompted();
            }
            return Ok(());
        }
        if !matches!(command, b"A" | b"N" | b"L") {
            return Ok(());
        }
        // N "may first implicitly terminate a previous command": the one
        // running, the only one fux-vt keeps (it keeps no `aid`).
        if command == b"N" {
            self.command_mark(b"D", None);
        }
        // CR, then IND, as Ghostty does it.
        if self.grid().cursor.col() != 0 {
            self.control(b'\r')?;
            self.linefeed()?;
        }
        if command != b"L" {
            self.prompted();
        }
        Ok(())
    }
    /// Whether row `row` of the screen is soft-wrapped: its line goes on in
    /// the next row.
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid().live_wrapped(row)
    }
    /// How many rows of history the screen keeps now.
    pub fn history_len(&self) -> usize {
        self.grid().history_len()
    }
    /// The retained row with identity `id`, if it is still kept.
    pub fn row_by_id(&self, id: RowId) -> Option<Row<'_>> {
        self.grid().row_by_id(id)
    }
    /// The rows retained, history's oldest first, then the screen's.
    pub fn rows(&self) -> Rows<'_> {
        Rows::of(self.grid())
    }
    /// The primary screen's rows, its history first, whichever screen is in
    /// use: what a program printed before it took the alternate screen.
    pub fn main_rows(&self) -> Rows<'_> {
        Rows::of(&self.primary)
    }
    /// The alternate screen's rows, whichever screen is in use: what a
    /// full-screen program drew last, kept after it leaves (1049, 1047, 47)
    /// until the alternate screen is shown again. None if it never was.
    pub fn alternate_rows(&self) -> Rows<'_> {
        Rows::of(&self.alternate)
    }
    /// Where the retained row `id` is, from the oldest, if it is kept.
    pub fn row_index(&self, id: RowId) -> Option<usize> {
        self.grid().index_of(id)
    }
    /// The screen's rows: the window no rows up into history.
    pub fn window(&self) -> Window<'_> {
        let grid = self.grid();
        Window {
            grid,
            start: grid.history_len(),
        }
    }
    /// A mark of the screen as it is now, to ask later what changed.
    pub fn mark(&self) -> Mark {
        Mark(self.version)
    }
    /// The number of the last processing (or `Parser::tick`): each row
    /// keeps the number of the one that last changed it (`Row::version`).
    pub fn version(&self) -> u64 {
        self.version
    }
    /// Records what is printed from now on, or stops and forgets it.
    pub(crate) fn record_prints(&mut self, on: bool) {
        let cells = self.size().cells();
        self.prints = on.then(|| Prints::new(cells));
    }
    /// What was printed since the last call, while recording.
    pub(crate) fn take_prints(&mut self) -> Prints {
        let cells = self.size().cells();
        (self.prints.as_mut())
            .map(|prints| mem::replace(prints, Prints::new(cells)))
            .unwrap_or_default()
    }
    /// The number of the last change to a row of the primary screen's
    /// history that has since been let go past its budget: 0 if none was.
    pub fn let_go(&self) -> u64 {
        self.primary.let_go()
    }
    /// Notes that `text` was printed at live row `row`, column `col`, if
    /// recording.
    fn printed(&mut self, row: u16, cols: Range<u16>, text: &str) {
        let Some(id) = self.grid().live_row(row).map(|r| r.id()) else {
            return;
        };
        if let Some(prints) = &mut self.prints {
            prints.push(id, cols, text);
        }
    }
    /// Notes that `c` joined the cell at live row `row`, column `col`,
    /// after a space if it was `blank`, over the columns the cell takes now:
    /// out of line, as only a host that records prints asks for it.
    #[cold]
    #[inline(never)]
    fn mark_joined(&mut self, row: u16, col: u16, blank: bool, c: char) {
        let g = self.grid();
        let Some(id) = g.live_row(row).map(|r| r.id()) else {
            return;
        };
        let wide = g.stored(row, col).is_some_and(Compact::is_wide);
        let cols = col..col.saturating_add(if wide { 2 } else { 1 });
        let mut text = [0; 8];
        let text = joined_text(blank, c, &mut text);
        if let Some(prints) = &mut self.prints {
            prints.join(id, cols, text);
        }
    }
    /// Makes shell marks count only with `nonce=NONCE` among their
    /// options, or with none (`None`) every one; and a nested program's
    /// with `nonce=NESTED`, while a command the shell started runs.
    pub(crate) fn require_mark_nonce(&mut self, nonce: Option<&[u8]>, nested: Option<&[u8]>) {
        self.mark_nonce = nonce.map(Box::from);
        self.nested_nonce = nested.map(Box::from);
        self.outer = None;
    }
    /// Notes that a line was submitted with the cursor where it is.
    pub(crate) fn note_submit(&mut self) {
        self.submitted = self.cursor_mark();
    }
    /// Whether the screen may have changed since `mark`: true after any
    /// processing at all, as the README says; false, nothing changed.
    pub fn changed_since(&self, mark: Mark) -> bool {
        mark.0 != self.version
    }
    /// Whether a reader of `mark` must read every row again: something
    /// structural changed since (a scroll, resize, reset, switch of screens,
    /// or rows leaving history).
    pub fn full_refresh_since(&self, mark: Mark) -> bool {
        mark.0 < self.structural || mark.0 > self.version
    }
    /// Independent readers can use the same mark; consuming this iterator does
    /// not acknowledge or clear changes for anyone else.
    pub fn dirty_rows_since(&self, mark: Mark) -> impl Iterator<Item = Row<'_>> {
        let full = self.full_refresh_since(mark);
        self.rows().filter(move |r| full || r.version > mark.0)
    }
    /// The live rows changed since `mark`, each with its place on the screen
    /// (0 at the top), top to bottom; every live row after a full refresh.
    /// Only the screen's rows are read, however much history there is: the
    /// live rows `dirty_rows_since` yields, without walking the history.
    pub fn dirty_live_rows_since(&self, mark: Mark) -> impl Iterator<Item = (u16, Row<'_>)> {
        let full = self.full_refresh_since(mark);
        let grid = self.grid();
        (0..grid.size().rows())
            .filter_map(move |y| grid.live_row(y).map(|row| (y, row)))
            .filter(move |(_, row)| full || row.version > mark.0)
    }
    /// Retained allocation in cells, for capacity/plateau diagnostics.
    pub fn storage_cells(&self) -> usize {
        // Each is a Vec's capacity, far below the limit of a usize.
        self.primary
            .storage_cells()
            .saturating_add(self.alternate.storage_cells())
    }

    pub(crate) fn resize(&mut self, size: Size, reflow: bool) -> Result<(), Error> {
        // A frame drawn for the old size is no frame for the new one; as in
        // Ghostty, any resize ends synchronized output: one taken, or to
        // the same size; a rejected one changes nothing.
        if self.size() == size {
            self.modes.set(Mode::SynchronizedOutput, false);
            return Ok(());
        }
        self.last_print = None;
        let version = self.version.wrapping_add(1);
        let mut next = self.next_id;
        let mut primary = if reflow {
            self.primary.reflowed(size, &mut next, version)?
        } else {
            self.primary.resized(size, &mut next, version)?
        };
        let mut alternate = self.alternate.resized(size, &mut next, version)?;
        // The links' tables go along, as the cells keep their links, and
        // the keyboard stacks.
        primary.links = mem::take(&mut self.primary.links);
        alternate.links = mem::take(&mut self.alternate.links);
        primary.keyboard = mem::take(&mut self.primary.keyboard);
        alternate.keyboard = mem::take(&mut self.alternate.keyboard);
        self.primary = primary;
        self.alternate = alternate;
        self.modes.set(Mode::SynchronizedOutput, false);
        self.next_id = next;
        self.version = version;
        self.structural = version;
        // History is made anew: the pictures are found in it again.
        self.retrack_pictures();
        // What was printed before it is on rows and columns that changed.
        if let Some(prints) = self.prints.as_mut().filter(|p| !p.is_empty()) {
            prints.lose();
        }
        Ok(())
    }

    pub(crate) fn set_history_bytes(&mut self, bytes: usize) {
        if self.primary.history_bytes() == bytes {
            return;
        }
        self.primary.set_history_bytes(bytes);
        // Rows may have left history: every reader reads again.
        self.version = self.version.wrapping_add(1);
        self.structural = self.version;
    }

    fn scroll(&mut self, region: Span, count: u16, direction: Scroll) -> Result<(), Error> {
        if self.grid().lr() {
            self.scroll_columns(region, count, direction);
            return Ok(());
        }
        self.scroll_rows(region, count, direction)
    }
    /// `scroll` without left and right margins: the rows move.
    fn scroll_rows(&mut self, region: Span, count: u16, direction: Scroll) -> Result<(), Error> {
        // A bounded multi-row scroll can fail after earlier rows have moved
        // (allocation failure). Even that partial result must
        // invalidate every reader's window, not just its newly blank rows.
        self.structural = self.version;
        // The rows brought in take the pen's colours (`bce`), as in xterm.
        let blank = self.blank_style();
        self.with_grid(|g, next, version| {
            g.scroll(region, count, direction, blank, next, version)
        })?;
        // Rows moved within the screen (lines deleted or put in above
        // them, a region scrolled) are recorded, their text unchanged: a
        // whole screen scrolled into history moves none of its lines.
        if self.prints.is_some()
            && !(direction == Scroll::Up { history: true } && region == self.grid().size().lines())
            && region.len() > count
        {
            let first = match direction {
                Scroll::Up { .. } => region.first(),
                Scroll::Down => region.first().saturating_add(count),
            };
            self.printed(first, 0..0, "");
        }
        Ok(())
    }
    /// A scroll inside left and right margins (`Grid::scroll_columns`),
    /// its blanks in the pen's colours. Out of line: without margins it is
    /// never reached.
    #[inline(never)]
    fn scroll_columns(&mut self, region: Span, count: u16, direction: Scroll) {
        let blank = self.blank_style();
        let up = direction != Scroll::Down;
        self.with_grid(|g, _, version| g.scroll_columns(region, count, up, blank, version));
    }
    /// IND, and LF, VT and FF: down a line, scrolling at the bottom margin;
    /// with left and right margins only while the cursor is between them,
    /// and outside them the cursor stays at the margin, as DEC STD 070
    /// (5.4.3) and xterm's `xtermIndex` have it.
    fn linefeed(&mut self) -> Result<(), Error> {
        self.grid_mut().cursor.pending_wrap = false;
        let g = self.grid();
        if g.cursor.row() == g.lines.last() {
            if g.lr() {
                if g.in_columns() {
                    self.scroll_columns(g.lines, 1, Scroll::Up { history: true });
                }
                return Ok(());
            }
            self.scroll_rows(g.lines, 1, Scroll::Up { history: true })?;
        } else {
            // Down a row, stopping at the last.
            let g = self.grid_mut();
            g.set_row(g.cursor.row().saturating_add(1));
        }
        Ok(())
    }
    /// A line feed and a carriage return, NEL's and LNM's: the line feed
    /// first, from the column the cursor is in, which with left and right
    /// margins says whether it scrolls, then the return (xterm's
    /// `CASE_NEL` and `CASE_VMOT`). Without margins the order does not
    /// matter, as the line feed does not look at the column, and the return
    /// comes first.
    fn new_line(&mut self) -> Result<(), Error> {
        let g = self.grid_mut();
        if g.lr() {
            return self.new_line_in_margins();
        }
        g.set_col(0);
        self.linefeed()
    }
    /// `new_line` with left and right margins: the line feed, then the
    /// carriage to the left margin. Out of line, as margins are rare and
    /// `new_line` is on every new line's way.
    #[inline(never)]
    fn new_line_in_margins(&mut self) -> Result<(), Error> {
        self.linefeed()?;
        self.grid_mut().carriage_return();
        Ok(())
    }
    /// RI: up a line, scrolling at the top margin, as `linefeed` goes down.
    fn reverse_index(&mut self) -> Result<(), Error> {
        self.grid_mut().cursor.pending_wrap = false;
        let g = self.grid();
        if g.cursor.row() == g.lines.first() {
            if !g.lr() || g.in_columns() {
                self.scroll(g.lines, 1, Scroll::Down)?;
            }
        } else {
            let g = self.grid_mut();
            g.set_row(g.cursor.row().saturating_sub(1));
        }
        Ok(())
    }
    /// Makes room for a glyph `width` wide at the cursor, which is left
    /// where it goes, with no wrap pending: a pending wrap, or a glyph too
    /// wide for what is left of the row, moves it to the start of the next
    /// line with autowrap on (DEC STD 070, Appendix D.6.1), and back to the
    /// last column it fits in with autowrap off, but for a wide glyph,
    /// which is not printed then, as xterm drops it.
    ///
    /// The end of the cursor's line after it (`Grid::line_end`), or none
    /// for a glyph not printed. Whether the glyph fits is asked inline, on
    /// every glyph's way; the wrap is out of line.
    #[inline]
    fn wrap_for(&mut self, width: u16) -> Result<Option<u16>, Error> {
        let g = self.grid();
        let end = g.line_end();
        // The last column a glyph this wide can start in, before the right
        // margin while the cursor is not past it; a wider glyph is never
        // printed.
        let Some(room) = end.checked_sub(width) else {
            return Ok(Some(end));
        };
        // A pending wrap is carried out wherever it waits: at the line's
        // end, or where the right margin was before DECLRMM was reset (DEC
        // STD 070's Last Column Flag is no column; xterm and Ghostty wrap
        // there too).
        if !g.cursor.pending_wrap && g.cursor.col() <= room {
            return Ok(Some(end));
        }
        self.wrap(end, room, width)
    }
    /// `wrap_for` where the glyph, `width` wide, does not fit: the cursor
    /// goes to the next line, or back to `room`, the last column the glyph
    /// fits in, on a line ending at `end`.
    #[inline(never)]
    fn wrap(&mut self, end: u16, room: u16, width: u16) -> Result<Option<u16>, Error> {
        let g = self.grid();
        let wrap = self.mode(Mode::Autowrap);
        // xterm writes a wide glyph over none before it (`replay --engines
        // xterm --size 2x4 '\e[?7labc\u{754c}'` keeps `abc`), unless it
        // fits: the flag a glyph left at an old right margin is no wrap.
        if !wrap && width > 1 && g.cursor.col() > room {
            return Ok(None);
        }
        if !wrap {
            // Back to the last column the glyph fits in; a wrap left
            // pending where a right margin was overwrites there, as xterm
            // does.
            let g = self.grid_mut();
            g.set_col(g.cursor.col().min(room));
            g.cursor.pending_wrap = false;
            return Ok(Some(end));
        }
        let row = g.cursor.row();
        // The glyph goes on to the next line, so this row is soft-wrapped,
        // whatever its last column holds: blank when a wide glyph did not
        // fit, or after an erase the wrap outlived; at the right margin
        // too, as xterm marks it. Only where the line feed leaves the
        // cursor on its row (the last row, below the scroll region; the
        // bottom margin, right of the right margin) does the glyph stay on
        // the same row.
        let wrapped = if g.lr() && row == g.lines.last() {
            g.in_columns()
        } else {
            row < g.size().lines().last() || row == g.lines.last()
        };
        // Set before scrolling so a departing row carries its soft-wrap into history.
        self.with_grid(|g, _, v| g.wrap(row, wrapped, v));
        // With margins, the line feed goes from the column the glyph did
        // not fit in, which says whether it scrolls, then to the left
        // margin (xterm's `WrapLine`).
        self.new_line()?;
        let g = self.grid();
        Ok(Some(if g.narrow() { g.line_end() } else { end }))
    }

    /// SUB (DEC STD 070, p. 5-132): the error character, a reversed
    /// question mark (U+2426), placed at the active position as a glyph is,
    /// in no character set: a single shift waits on for the next graphic
    /// character.
    #[cold]
    #[inline(never)]
    pub(crate) fn substitute(&mut self) -> Result<(), Error> {
        let single = self.charsets.single;
        self.print('\u{2426}')?;
        self.charsets.single = single;
        Ok(())
    }

    pub(crate) fn print(&mut self, raw: char) -> Result<(), Error> {
        let c = if self.charsets.graphics() {
            self.charsets.map(raw)
        } else {
            raw
        };
        // A control character (C0, DEL or C1, the only ones of no width)
        // is not printed.
        let Some(width) = crate::char_width(c).and_then(|w| u16::try_from(w).ok()) else {
            return Ok(());
        };
        // Nor where it fits nowhere, but for a pending wrap, which xterm
        // still carries out (`replay --engines xterm --size 3x1 'a\u{754c}'`).
        // It is still the preceding graphic character, which REP repeats.
        if width > self.grid().size().cols() {
            self.repeat = Some(c);
            if self.grid().cursor.pending_wrap && self.mode(Mode::Autowrap) {
                let end = self.grid().line_end();
                self.wrap(end, 0, width)?;
            }
            return Ok(());
        }
        if self.extend_cluster(c) {
            // What joined is printed, over its cell as it is now (PAR-13,
            // TXT-6), unless the cluster was full.
            if self.prints.is_some()
                && let Some(Printed {
                    at, full: false, ..
                }) = self.last_print
            {
                self.mark_joined(at.0, at.1, false, c);
            }
            return Ok(());
        }
        if width == 0 {
            if let Some((row, col)) = self.before_cursor() {
                let g = self.grid();
                // A blank cell takes a space for the mark to follow, and
                // with it the open link, as a glyph printed there would.
                let blank = g.stored(row, col).is_some_and(|c| !c.has_contents());
                let end = col.saturating_add(1);
                // The mark joins the cell's cluster; a cell already holding
                // all it can takes no more (`append`).
                let mut joined = false;
                self.with_grid(|g, _, v| {
                    g.mutate_line(row, v, end, |line| {
                        joined = line.append(usize::from(col), c);
                        joined
                    })
                });
                // A mark dropped leaves the cell blank, its link unread.
                if blank && self.links_seen {
                    let at = usize::from(col);
                    self.link_cells(row, at..at.saturating_add(1), true);
                }
                // The mark is printed, over its cell (PAR-13, TXT-6).
                if joined && self.prints.is_some() {
                    self.mark_joined(row, col, blank, c);
                }
                // While text is being printed, what follows is segmented
                // after the mark, as UAX #29 segments the cell's text.
                if joined && self.last_print.is_some() {
                    self.last_print = Some(Printed::new((row, col), c));
                }
            }
            return Ok(());
        }
        // What REP repeats: the last glyph printed, not a mark.
        self.repeat = Some(c);
        let Some(end) = self.wrap_for(width)? else {
            return Ok(());
        };
        work!(cells, width);
        if self.mode(Mode::Insert) {
            // Room for the glyph, what was there moving right (ICH).
            let blank = self.blank_style();
            self.with_grid(|g, _, v| g.edit_cells(width, true, blank, v));
        }
        let (row, col) = self.grid().cursor.at();
        let style = self.pen_style();
        // The glyph is protected while the pen protects; what it clears
        // around it is not.
        let protect = self.protect;
        // A narrow glyph over a wide one's first half leaves a space in its
        // second, which no link printed.
        let split = self.links_seen
            && width == 1
            && self.grid().stored(row, col).is_some_and(Compact::is_wide);
        let mut changed = false;
        self.with_grid(|g, _, version| {
            g.mutate_row(row, version, col.saturating_add(width), |cells| {
                let i = usize::from(col);
                let glyph = Compact::glyph(c, usize::from(width), style | protect);
                // Already this glyph, whole, as a redraw finds it: the row is
                // as it was. Otherwise what follows writes a cell that
                // differs, the glyph or its second half.
                let Some(current) = cells.get(i) else {
                    return false;
                };
                if *current == glyph && whole(cells, i, width) {
                    return false;
                }
                // An overwrite at either half removes the other half too.
                if cells.get(i).is_some_and(Compact::is_wide_continuation)
                    && let Some(other) = cells.get_mut(i.wrapping_sub(1))
                {
                    *other = Compact::blank(style);
                }
                if cells.get(i).is_some_and(Compact::is_wide)
                    && let Some(other) = cells.get_mut(i + 1)
                {
                    *other = Compact::glyph(' ', 1, style);
                }
                if width == 2
                    && cells.get(i + 1).is_some_and(Compact::is_wide)
                    && let Some(other) = cells.get_mut(i + 2)
                {
                    *other = Compact::blank(style);
                }
                if let Some(cell) = cells.get_mut(i) {
                    *cell = glyph;
                }
                if width == 2
                    && let Some(cell) = cells.get_mut(i + 1)
                {
                    *cell = Compact::continuation();
                }
                changed = true;
                true
            });
            // Past the glyph; in the last column it waits there to wrap.
            g.advance_within(col.saturating_add(width), end);
        });
        // The cells' links, after the cells, as `ascii` does them.
        if self.links_seen {
            let at = usize::from(col);
            self.link_cells(row, at..at.saturating_add(usize::from(width)), true);
            if split {
                let next = at.saturating_add(1);
                self.link_cells(row, next..next.saturating_add(1), false);
            }
        }
        self.last_print = Some(Printed::new((row, col), c));
        if changed && self.prints.is_some() {
            self.printed(
                row,
                col..col.saturating_add(width),
                c.encode_utf8(&mut [0; 4]),
            );
        }
        Ok(())
    }

    /// The cell before the cursor, which a mark joins: the one left of it,
    /// or else the last of the soft-wrapped row above; of a wide glyph, its
    /// first half.
    fn before_cursor(&self) -> Option<(u16, u16)> {
        let g = self.grid();
        let (row, col) = (g.cursor.row(), g.next_column());
        let (row, col) = match col.checked_sub(1) {
            Some(left) => (row, left),
            None => {
                let above = row.checked_sub(1).filter(|&above| g.live_wrapped(above))?;
                (above, g.size().columns().last())
            }
        };
        let second = g.stored(row, col)?.is_wide_continuation();
        Some((row, if second { col.saturating_sub(1) } else { col }))
    }

    /// Joins `c` to the cell printed last, if the cursor is just past it and
    /// `c` continues its grapheme cluster (UAX #29, see `unicode.rs`): a
    /// spacing vowel sign, a variation selector, a ZWJ sequence, a flag's
    /// second regional indicator. Programs laid out with unicode-width give a
    /// cluster one cell of its string width, so a narrow cell whose cluster
    /// becomes two columns wide is widened, the cell under the cursor
    /// becoming its second half, as kitty and Ghostty (mode 2027) do. A
    /// zero-width mark joins the cell before the cursor, even after a
    /// cursor move. Whether `c` was taken: joined, or dropped
    /// because the cluster is full, which never splits it.
    fn extend_cluster(&mut self, c: char) -> bool {
        let g = self.grid();
        let (row, col) = (g.cursor.row(), g.next_column());
        let anchor = self.last_print.or_else(|| {
            if crate::char_width(c) != Some(0) {
                return None;
            }
            // The cell before the cursor, on the cursor's row: on the row
            // above, it fails the row check below.
            let at = self.before_cursor()?;
            // Its cluster's state, from its text.
            let cluster = Cluster::of(g.cell(at.0, at.1)?.contents());
            Some(Printed {
                at,
                cluster,
                full: false,
            })
        });
        let Some(Printed {
            at: (anchor_row, anchor_col),
            mut cluster,
            full,
        }) = anchor
        else {
            return false;
        };
        let Some(cell) = g.stored(anchor_row, anchor_col) else {
            return false;
        };
        let narrow = !cell.is_wide();
        let next = anchor_col.checked_add(if narrow { 1 } else { 2 });
        if anchor_row != row
            || next != Some(col)
            || !cell.has_contents()
            || cell.is_wide_continuation()
            || !cluster.push(c)
        {
            return false;
        }
        let at = usize::from(anchor_col);
        let cursor = usize::from(col);
        work!(cells, 2);
        // Within the right margin, while the cursor is not past it.
        let fits = col < g.line_end();
        let mut widen = false;
        let mut kept = !full;
        self.with_grid(|g, _, v| {
            g.mutate_line(row, v, col.saturating_add(1), |line| {
                kept = kept && line.append(at, c);
                // Width is kept once wide; the last column has no room to widen.
                widen = kept && narrow && fits && crate::str_width(line.text(at)) >= 2;
                kept
            });
        });
        if widen {
            self.widen(row, at, cursor);
        }
        self.last_print = Some(Printed {
            at: (anchor_row, anchor_col),
            cluster,
            full: !kept,
        });
        true
    }

    /// Widens cell `at` of `row`, its cluster grown to two columns, over
    /// the cell under the cursor, `cursor`, which becomes its second half
    /// as a glyph's would: under IRM what is there moves right first
    /// (ECMA-48 7.2.10); a wide glyph whose first half it was keeps its
    /// second as the pen's blank, as `print` repairs it; and the new half
    /// takes the open link.
    #[cold]
    #[inline(never)]
    fn widen(&mut self, row: u16, at: usize, cursor: usize) {
        if self.mode(Mode::Insert) {
            let blank = self.blank_style();
            self.with_grid(|g, _, v| g.edit_cells(1, true, blank, v));
        }
        let style = self.pen_style();
        let end = u16::try_from(cursor).unwrap_or(u16::MAX).saturating_add(1);
        self.with_grid(|g, _, v| {
            g.mutate_line(row, v, end, |line| {
                if let Some(cell) = line.cells.get_mut(at) {
                    cell.widen();
                }
                // The cell under the cursor becomes the second half; if it
                // led a wide glyph, that glyph's half is left blank.
                if line.cells.get(cursor).is_some_and(Compact::is_wide)
                    && let Some(orphan) = cursor.checked_add(1).and_then(|i| line.cells.get_mut(i))
                {
                    *orphan = Compact::blank(style);
                }
                if let Some(cell) = line.cells.get_mut(cursor) {
                    *cell = Compact::continuation();
                }
                true
            });
            g.advance_to(end);
        });
        if self.links_seen {
            self.link_cells(row, cursor..cursor.saturating_add(1), true);
        }
    }

    /// Ends the cluster being printed: the next character starts a cell of
    /// its own. Anything that moves the cursor or edits a row does.
    pub(crate) fn break_cluster(&mut self) {
        self.last_print = None;
    }

    /// Forgets the character REP repeats. The parser calls it for every
    /// control in ground state and every sequence or string it ends, as
    /// xterm forgets its last character whenever its parser returns to the
    /// ground state without printing.
    pub(crate) fn forget_repeat(&mut self) {
        self.repeat = None;
    }

    /// REP (ECMA-48 8.3.103): the preceding graphic character printed
    /// `count` more times, as if it had been sent again. Nothing if anything
    /// but printing came after that character, where ECMA-48 leaves REP
    /// undefined, as in xterm.
    ///
    /// Once the copies have filled the screen and every row of history the
    /// grid keeps, each further row's worth leaves all of it as it was, so
    /// whole rows' worth past that are skipped: no more than a screen and
    /// its history's worth is ever printed, and what shows is exactly what
    /// printing them all would show. An ASCII character is printed in runs,
    /// as text is.
    fn repeat(&mut self, count: u16) -> Result<(), Error> {
        let Some(c) = self.repeat else {
            return Ok(());
        };
        let g = self.grid();
        let width = crate::char_width(c).unwrap_or(1).max(1);
        // Copies to a row, between the margins once the copies wrap: a wide
        // glyph leaves an odd last column blank.
        let line = g.columns().len();
        let Some(per_row) = usize::from(line).checked_div(width).filter(|n| *n > 0) else {
            return Ok(());
        };
        // The rows of copies history can keep: each is 24 bytes and 8 a cell,
        // so at least 32, and no more than a grid may have (threats.md
        // B33: history's bytes read as rows never clamped).
        let kept = (g.history_bytes() / 32).min(crate::grid::MAX_ROWS);
        let lines = usize::from(g.size().rows())
            .saturating_add(kept)
            .saturating_add(1);
        let full = lines.saturating_mul(per_row);
        let asked = usize::from(count);
        let mut count = asked;
        if let Some(beyond) = count.checked_sub(full) {
            count = full.saturating_add(beyond.checked_rem(per_row).unwrap_or(0));
        }
        // The copies skipped cost nothing (threats.md B33); printing counts
        // the ones made. Every copy is spent from the call's budget here.
        crate::budget::spend(crate::budget::units(asked), crate::budget::weight::cells);
        if let Ok(byte) = u8::try_from(c)
            && (0x20..=0x7e).contains(&byte)
        {
            const RUN: usize = 128;
            let run = [byte; RUN];
            while count > 0 {
                let n = count.min(RUN);
                self.ascii(run.get(..n).unwrap_or_default())?;
                count = count.saturating_sub(n);
            }
        } else {
            for _ in 0..count {
                self.print(c)?;
                // A glyph that joins its own cluster, once the cluster is
                // full, drops every copy after: they change nothing, and are
                // skipped (threats.md B34a).
                if self.last_print.is_some_and(|p| p.full) {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Prints a run of ASCII straight into cells, wrapping at the right
    /// margin itself (`wrap_for`); a cell that needs the general glyph path
    /// (a wide glyph's half there, a cluster to extend) takes `print`.
    /// Never enters parser dispatch.
    pub(crate) fn ascii(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        // DEC Special Graphics print other characters, and insert mode
        // moves what is there, one glyph at a time.
        if self.charsets.graphics() || self.mode(Mode::Insert) {
            for &byte in bytes {
                self.print(char::from(byte))?;
            }
            return Ok(());
        }
        // The first joins a Prepend character printed last (UAX #29, GB9b),
        // as the general path joins it.
        if self.last_print.is_some_and(|p| p.cluster.prepended())
            && let Some((&first, tail)) = bytes.split_first()
        {
            self.print(char::from(first))?;
            bytes = tail;
        }
        while let Some((&first, tail)) = bytes.split_first() {
            let Some(line) = self.wrap_for(1)? else {
                return Ok(());
            };
            let (row, col) = self.grid().cursor.at();
            // `wrap_for` left room for at least one cell, before the right
            // margin or the screen's edge; without it, the run could not
            // advance.
            let Some(room) = line.checked_sub(col).filter(|r| *r > 0) else {
                return Ok(());
            };
            // A run too long for a u16 still stops at the margin.
            let count = u16::try_from(bytes.len()).map_or(room, |n| n.min(room));
            let style = self.pen_style() | self.protect;
            let mut run = bytes.get(..usize::from(count)).unwrap_or_default();
            // A run that meets half of a wide glyph, which the general path
            // repairs, is written up to it, and the glyph there printed.
            let written = loop {
                match self.with_grid(|g, _, v| g.write_ascii(row, col, run, style, v)) {
                    Written::All => {
                        if self.prints.is_some() {
                            let end = col.saturating_add(u16::try_from(run.len()).unwrap_or(0));
                            let text = str::from_utf8(run).unwrap_or_default();
                            self.printed(row, col..end, text);
                        }
                        break true;
                    }
                    Written::Same => break true,
                    Written::Before(n) => match run.get(..n) {
                        Some(before) if !before.is_empty() && n < run.len() => run = before,
                        Some(_) | None => break false,
                    },
                }
            };
            let end = u16::try_from(run.len())
                .ok()
                .and_then(|n| col.checked_add(n));
            let Some(end) = end.filter(|_| written) else {
                self.print(char::from(first))?;
                bytes = tail;
                continue;
            };
            self.grid_mut().advance_within(end, line);
            // The cells' links, after the cells: before, the call would make
            // the write above load again what it had in hand.
            if self.links_seen {
                self.link_cells(row, usize::from(col)..usize::from(end), true);
            }
            // The run's last glyph, which a mark or selector may join, and
            // REP repeats.
            self.last_print = end
                .checked_sub(1)
                .zip(run.last())
                .map(|(last, &byte)| Printed::new((row, last), char::from(byte)));
            if let Some(&last) = run.last() {
                self.repeat = Some(char::from(last));
            }
            bytes = bytes.get(run.len()..).unwrap_or_default();
        }
        Ok(())
    }

    pub(crate) fn control(&mut self, byte: u8) -> Result<(), Error> {
        if (8..=13).contains(&byte) {
            self.break_cluster();
        }
        if byte == 8 && self.reverse_wraps() {
            let pending = self.grid().cursor.pending_wrap;
            self.cursor_back(1, pending);
            return Ok(());
        }
        let new_line = self.mode(Mode::NewLine);
        let g = self.grid_mut();
        // BS, LF, VT, FF and CR end a pending wrap (DEC STD 070, Appendix
        // D.6.1); HT too, in `tab`.
        if matches!(byte, 8 | 13) {
            g.cursor.pending_wrap = false;
        }
        match byte {
            // Back a column, stopping at the left margin unless the cursor
            // is already left of it (xterm's `CursorBack`).
            8 => g.backward(1),
            9 => self.tab(1, true),
            // LNM: a new line, the carriage returned too.
            10..=12 if new_line => self.new_line()?,
            10..=12 => self.linefeed()?,
            13 => g.carriage_return(),
            // SO puts G1 in GL, SI G0.
            14 => self.charsets.gl = Set::G1,
            15 => self.charsets.gl = Set::G0,
            // SUB: its error character (DEC STD 070, p. 5-132).
            0x1a => self.substitute()?,
            _ => {}
        }
        Ok(())
    }
    /// Moves the cursor `count` tab stops forward, stopping at the last
    /// column, or back, stopping at the first.
    /// Forward stops at the right margin, but from right of it or from a
    /// line outside the scrolling region goes on to the last column (DEC
    /// STD 070, HT, note 1; xterm's `TabToNextStop` stops at the margin
    /// wherever the cursor is). Back, in origin mode, stops at the left
    /// margin (`TabToPrevStop`). Without margins the right margin is the
    /// last column.
    ///
    /// Each stop is found from the last, never past the column it stops
    /// at, and once the cursor stops moving the rest of the count is
    /// spent: the work is the line's width at most, whatever the count.
    ///
    /// It ends a pending wrap: DEC STD 070 (Appendix D.6.1) lists HT, CHT
    /// is HT that many times, and CBT moves back from the last column
    /// (xterm keeps the wrap through all three).
    fn tab(&mut self, count: u16, forward: bool) {
        let g = self.grid();
        let (mut col, mut last) = (g.cursor.col(), g.columns().last());
        if col > last || !g.lines.contains(g.cursor.row()) {
            last = g.size().columns().last();
        }
        let first = g.addressed().1.first();
        for _ in 0..count {
            let to = if forward {
                self.terminal.tabs.next(col, last)
            } else {
                self.terminal.tabs.previous(col, first)
            };
            if to == col {
                break;
            }
            col = to;
        }
        let g = self.grid_mut();
        g.set_col(col);
        g.cursor.pending_wrap = false;
    }
    /// Whether BS and CUB wrap back: reverse wraparound, either kind, with
    /// DECAWM, as xterm has it.
    fn reverse_wraps(&self) -> bool {
        self.mode(Mode::Autowrap)
            && (self.mode(Mode::ReverseWrap) || self.mode(Mode::ExtendedReverseWrap))
    }

    /// BS and CUB with reverse wraparound, as xterm 411 moves (`CursorBack`;
    /// ctlseqs, `CSI ? 45 h` and `CSI ? 1045 h`): back `count` columns, and
    /// at the first column on from the last column of the line before,
    /// which takes one of the count. With 45 the line before is only a
    /// soft-wrapped row above, so a line's text is gone back over and no
    /// further (since xterm patch 383); with 1045 it is any row above, and
    /// from the top margin the bottom margin. A cursor waiting to wrap
    /// counts as one past the last column, so the first column back is
    /// the one it waits in. Where xterm 411 crashes or leaves the screen
    /// (45 past a soft-wrapped first row; 1045 above the top margin), the
    /// cursor stops at the first row, as Ghostty's does. With left and
    /// right margins the line runs from the left margin, unless the cursor
    /// starts left of it, to the right margin, as in xterm.
    fn cursor_back(&mut self, count: u16, pending: bool) {
        let extended = self.mode(Mode::ExtendedReverseWrap);
        let g = self.grid_mut();
        g.cursor.pending_wrap = false;
        let mut count = if pending {
            count.saturating_sub(1)
        } else {
            count
        };
        let (lines, columns) = (g.lines, g.columns());
        let left = columns.first();
        let first = if g.cursor.col() < left { 0 } else { left };
        while count > 0 {
            let moved = g.cursor.col().saturating_sub(first).min(count);
            g.set_col(g.cursor.col().saturating_sub(moved));
            count = count.saturating_sub(moved);
            if count == 0 {
                break;
            }
            let row = g.cursor.row();
            let before = if extended {
                if row == lines.first() {
                    Some(lines.last())
                } else {
                    row.checked_sub(1)
                }
            } else {
                row.checked_sub(1).filter(|above| g.live_wrapped(*above))
            };
            let Some(before) = before else {
                break;
            };
            g.set_row(before);
            g.set_col(columns.last());
            count = count.saturating_sub(1);
        }
    }

    /// DECSC, into the grid shown (`Saved`).
    fn save(&mut self) {
        let (attributes, charsets, protect) = (self.attributes, self.charsets, self.protect);
        let pending_wrap = self.last_column_flag();
        let g = self.grid_mut();
        let mut cursor = g.cursor;
        cursor.pending_wrap = pending_wrap;
        g.saved = Saved {
            cursor,
            attributes,
            charsets,
            protect,
        };
    }
    /// DECRC. With origin mode restored, the cursor comes back inside the
    /// margins, all four (DEC STD 070, DECRC, note 3 and its algorithm).
    fn restore(&mut self) {
        let g = self.grid_mut();
        let saved = g.saved;
        g.cursor = saved.cursor;
        g.cursor.restored = saved.cursor.pending_wrap;
        let (lines, columns) = g.addressed();
        g.set_row(g.cursor.row().clamp(lines.first(), lines.last()));
        g.set_col(g.cursor.col().clamp(columns.first(), columns.last()));
        self.attributes = saved.attributes;
        self.pen_changed();
        // A single shift is no state DECSC saves (DEC STD 070, DECSC and
        // DECRC; ECMA-35, 9.4): the one pending stays, spent or not.
        self.charsets = Charsets {
            single: self.charsets.single,
            ..saved.charsets
        };
        self.protect = saved.protect;
    }
    /// Carries out an escape sequence; whether fux-vt implements it.
    pub(crate) fn escape(&mut self, intermediates: &[u8], byte: u8) -> Result<bool, Error> {
        // SCS: ESC ( F designates G0, ESC ) F G1, ESC * F G2 and ESC + F
        // G3; F `0` is DEC Special Graphics, and any other set is ASCII
        // here.
        match intermediates {
            b"(" => self.charsets.designate(Set::G0, byte == b'0'),
            b")" => self.charsets.designate(Set::G1, byte == b'0'),
            b"*" => self.charsets.designate(Set::G2, byte == b'0'),
            b"+" => self.charsets.designate(Set::G3, byte == b'0'),
            b"#" if byte == b'8' => {
                self.alignment();
                return Ok(true);
            }
            b"#" => return Ok(self.line_rendition(byte)),
            [] => {}
            _ => return Ok(false),
        }
        if !intermediates.is_empty() {
            return Ok(true);
        }
        if matches!(byte, b'6' | b'7' | b'8' | b'9' | b'D' | b'E' | b'M' | b'c') {
            self.break_cluster();
        }
        match byte {
            b'6' | b'9' => self.index_column(byte == b'9'),
            b'7' => self.save(),
            b'8' => self.restore(),
            // LS2 and LS3 (ECMA-35, 9.3.1): G2 or G3 into GL; SS2 and SS3
            // (9.4): either for the next graphic character alone.
            b'n' => self.charsets.gl = Set::G2,
            b'o' => self.charsets.gl = Set::G3,
            b'N' => self.charsets.single = Some(Set::G2),
            b'O' => self.charsets.single = Some(Set::G3),
            b'=' => self.modes.set(Mode::ApplicationKeypad, true),
            b'>' => self.modes.set(Mode::ApplicationKeypad, false),
            // IND (DEC STD 070; xterm's ctlseqs): a line feed, scrolling
            // at the bottom margin.
            b'D' => self.linefeed()?,
            // NEL (ECMA-48 8.3.86): a line feed, whose column says whether
            // it scrolls, then the carriage back as CR takes it, to the
            // left margin (xterm's `CASE_NEL`).
            b'E' => self.new_line()?,
            // SPA and EPA (ECMA-48 8.3.140, 8.3.49): the glyphs printed
            // between them are a guarded area, which ED, EL and ECH leave
            // (ISO protection).
            b'V' | b'W' => {
                let guarded = byte == b'V';
                self.guard = if guarded { TABLED } else { 0 };
                self.protected |= guarded;
                self.pen_changed();
            }
            // HTS (ECMA-48 8.3.62): a tab stop at the cursor's column.
            b'H' => {
                let col = self.grid().cursor.col();
                self.terminal.tabs.set(col, true);
            }
            b'M' => self.reverse_index()?,
            // RIS: it resets synchronized output too, which
            // `parser::FrameScan::ends` restates.
            b'c' => {
                let size = self.size();
                let mut next = self.next_id;
                // Both grids start again: in the storage they have, if both
                // hold only their live rows at this size, else afresh. With
                // no room for them afresh, each is cleared, which blanks it
                // in place if it must (`Grid::clear`, SCR-10).
                let recycle = [&self.primary, &self.alternate]
                    .iter()
                    .all(|g| g.recyclable() && g.size() == size);
                let history = self.primary.history_bytes();
                let fresh = (!recycle)
                    .then(|| {
                        let primary = Grid::new(size, history, &mut next, self.version).ok()?;
                        Some((primary, Grid::unmade(size, &mut next, self.version).ok()?))
                    })
                    .flatten();
                if let Some((primary, alternate)) = fresh {
                    self.primary = primary;
                    self.alternate = alternate;
                } else {
                    self.primary.clear(&mut next, self.version)?;
                    self.alternate.clear(&mut next, self.version)?;
                }
                self.next_id = next;
                // No cell has a link now, and none is open.
                self.link = None;
                self.links_seen = false;
                self.primary.reset_links();
                self.alternate.reset_links();
                // Nor a picture.
                self.pictures.clear();
                self.alternate_active = false;
                // The pen, the character sets, protection and the palette,
                // as DECSTR resets them; the grids' cursors, saved cursors
                // and margins are new already.
                self.soft_reset();
                self.modes = Modes::DEFAULT;
                self.terminal = Terminal::default();
                self.protected = false;
                self.structural = self.version;
            }
            // ST (ECMA-48 8.3.143) is done too: it ended the string before
            // it, which the parser has dispatched, or ends nothing.
            _ => return Ok(byte == b'\\'),
        }
        Ok(true)
    }

    /// DECSWL (`ESC # 5`), DECDWL (`ESC # 6`) and DECDHL (`ESC # 3`, `ESC #
    /// 4`): the cursor's line takes the rendition (`Grid::set_rendition`),
    /// and a pending wrap ends (DEC STD 070, Appendix D.6.1). DECDWL and
    /// DECDHL do nothing while DECLRMM is set (DECDWL, note 3; DECDHL, note
    /// 4). Whether the sequence is one of the four.
    fn line_rendition(&mut self, byte: u8) -> bool {
        let rendition = match byte {
            b'3' => LineRendition::DoubleHeightTop,
            b'4' => LineRendition::DoubleHeightBottom,
            b'5' => LineRendition::Single,
            b'6' => LineRendition::DoubleWidth,
            _ => return false,
        };
        if rendition.is_double() && self.mode(Mode::LeftRightMargins) {
            return true;
        }
        self.break_cluster();
        let version = self.version;
        let g = self.grid_mut();
        g.cursor.pending_wrap = false;
        let row = g.cursor.row();
        g.set_rendition(row, rendition, version);
        true
    }

    /// DECIC (`insert`) and DECDC: `count` columns inserted or deleted at
    /// the cursor's column in every line of the scrolling region, between
    /// the left and right margins, as ICH and DCH insert and delete cells
    /// in one (DEC STD 070, 5.4.3; xterm's `xtermColScroll`). Nothing
    /// outside the margins. The cursor stays, and so does a pending wrap,
    /// as in xterm; DECDC ends each line's soft wrap, as DCH does. Out of
    /// line: inlined, the claude recordings count 0.01% more instructions
    /// (fux-bench).
    #[inline(never)]
    fn edit_columns(&mut self, count: u16, insert: bool) {
        let g = self.grid();
        if !g.lines.contains(g.cursor.row()) || !g.in_columns() {
            return;
        }
        self.shift_columns(g.cursor.col(), count, insert);
    }

    /// DECBI (`ESC 6`) and DECFI (`forward`, `ESC 9`; DEC STD 070, Back
    /// Index and Forward Index): the cursor a column back or forward,
    /// ending a pending wrap. From the left margin back, or the right
    /// margin forward, the columns between the margins of every line of
    /// the scrolling region shift the other way instead, a blank one coming
    /// in at that margin, and the cursor stays. Elsewhere the cursor moves,
    /// never past the page's edge. Out of line, as `escape` is on every
    /// sequence's way.
    #[inline(never)]
    fn index_column(&mut self, forward: bool) {
        let g = self.grid_mut();
        g.cursor.pending_wrap = false;
        let (col, columns) = (g.cursor.col(), g.columns());
        let margin = if forward {
            columns.last()
        } else {
            columns.first()
        };
        if col == margin {
            self.shift_columns(columns.first(), 1, !forward);
        } else if forward {
            g.set_col(col.saturating_add(1));
        } else {
            g.set_col(col.saturating_sub(1));
        }
    }

    /// The columns from `from` to the right margin of every line of the
    /// scrolling region moved `count` right, or left, blanks coming in at
    /// the side they leave; the cursor stays. SL and SR, and DECBI and
    /// DECFI at a margin, from the left margin; DECIC and DECDC from the
    /// cursor's column.
    fn shift_columns(&mut self, from: u16, count: u16, right: bool) {
        let blank = self.blank_style();
        self.with_grid(|g, _, version| {
            // Every line of the region, a row each (`budget`): ICH and DCH,
            // a row's edit, spend nothing.
            let cells = usize::from(g.lines.len()).saturating_mul(usize::from(g.size().cols()));
            crate::budget::spend(crate::budget::units(cells), crate::budget::weight::cells);
            for y in g.lines.first()..=g.lines.last() {
                g.edit_row(y, from, count, right, blank, version);
            }
        });
    }

    /// DECSLRM (`CSI Pl ; Pr s`, DEC STD 070, 5-27), while DECLRMM is set:
    /// margins with the left one left of the right are set, and the cursor
    /// goes home, obeying DECOM; others are ignored, a right margin past
    /// the screen too (notes 2 and 3; xterm takes it as the last column).
    /// Out of line, as `csi` is on every CSI's way.
    #[inline(never)]
    fn set_left_right_margins(&mut self, p: &Parameters) {
        let cols = self.grid().size().cols();
        let right = p.first(1, cols).saturating_sub(1);
        let left = p.first(0, 1).saturating_sub(1);
        if let Some(columns) = self.grid().size().columns().margins(left, right) {
            let g = self.grid_mut();
            g.set_columns(columns);
            g.position(0, 0);
        }
    }

    /// ECH while glyphs may be protected: the columns `start` to `end` of
    /// row `row` erased but for their guarded glyphs (ISO's). Out of line,
    /// as protection is rare and `csi` is on every CSI's way.
    #[inline(never)]
    fn erase_kept(&mut self, row: u16, start: u16, end: u16) {
        let blank = self.blank_style();
        self.with_grid(|g, _, v| g.erase_unprotected(row, start, end, blank, v));
    }

    /// ED or EL (`display`) while glyphs may be protected, or DECSED or
    /// DECSEL (`private`), in `mode`. DECSED and DECSEL make each glyph
    /// DECSCA does not protect a space, its rendition and attributes as
    /// they were, and leave empty positions empty (DEC STD 070, DECSEL and
    /// DECSED; 5.11.1.2), as DECSERA does; one reaching the last column
    /// ends the row's soft wrap, as ED's and xterm's do. ED and EL leave the
    /// guarded glyphs (ECMA-48, SPA with ERM reset), the rest erased as
    /// for ED and EL. An erase that finds no protected glyph leaves the
    /// protection as it is (DEC STD 070, 5.11.1.2; ECMA-48 8.3.140), where
    /// xterm ends it after an ED of the whole screen. Out of line: inlined,
    /// the tmux recordings count up to 0.35% more instructions (fux-bench).
    #[inline(never)]
    fn erase_protected(&mut self, private: bool, display: bool, part: Erase) {
        let (blank, version, cols) = (self.blank_style(), self.version, self.size().cols());
        self.grid_mut()
            .erase_in(display, part, version, |g, y, start, end| {
                if private {
                    g.erase_to_spaces(y, (start, end), None, version);
                    if end >= cols {
                        g.wrap(y, false, version);
                    }
                } else {
                    g.erase_unprotected(y, start, end, blank, version);
                }
            });
    }

    /// DECALN (`ESC # 8`; DEC STD 070, Appendix D, Screen Alignment; VT520
    /// manual, 5-17): the screen filled with E, the margins the whole
    /// screen, origin mode off, the cursor home with no wrap pending, and
    /// the pen's rendition off, as DEC STD 070's algorithm has it. As in
    /// xterm 411, which DEC leaves the rest to: the pen keeps its colours
    /// and loses every attribute, and the E's have neither. Each row's soft
    /// wrap ends, as in Ghostty, alacritty and wezterm (xterm keeps it: a
    /// screen of E's is no line going on), and its prompt mark goes, as an
    /// ED erasing it whole takes it. A row already all plain E's is left
    /// as it is, its version too.
    fn alignment(&mut self) {
        self.break_cluster();
        self.attributes = self.attributes.erased();
        self.pen_changed();
        let version = self.version;
        let g = self.grid_mut();
        // Home, with origin mode off and no wrap pending, and the left and
        // right margins too, as xterm resets them; DECLRMM stays as it is.
        g.cursor = Cursor::default();
        g.reset_margins();
        // The default attributes, style 0 (`style.rs`).
        let plain = 0;
        let cols = g.size().cols();
        for y in 0..g.size().rows() {
            let filled = !g.live_row(y).is_some_and(|r| r.has_links())
                && g.live_cells(y).iter().all(|c| c.is_ascii(b'E', plain));
            spend!(cells, cols);
            if !filled {
                // Erased whole first: its long clusters' text and its links
                // go, as an erase takes them.
                g.erase(y, 0, cols, plain, version);
                g.mutate_row(y, version, cols, |cells| {
                    cells.fill(Compact::ascii(b'E', plain));
                    true
                });
            }
            g.wrap(y, false, version);
            g.set_prompt(y, false, version);
        }
        // Every line single width (DEC STD 070, DECALN, its algorithm).
        g.single_width(version);
    }

    /// DECSTR (`CSI ! p`), as xterm does it: the modes a program sets go
    /// back to their defaults, as the VT520 manual's table (p. 5-150) and
    /// DEC STD 070's Soft Terminal Reset (p. 4-37) list them: the cursor
    /// shown, DECOM, DECCKM and DECKPAM off, the scroll region the whole
    /// screen, the pen normal, the character sets ASCII with G0 in GL,
    /// and the saved cursor home with them (`Modes::SOFT_RESET` has the
    /// modes); the saved cursor is the shown screen's, as xterm resets it.
    /// DECAWM goes back to its default, which both leave to the terminal's
    /// setting (xterm's: on). The screen, the cursor, a pending wrap (the
    /// Last Column Flag), the alternate screen, bracketed paste, focus
    /// reporting, mouse modes and kitty keyboard flags stay as they are,
    /// as in xterm. It resets synchronized output (2026), which
    /// `parser::FrameScan::ends` restates: the two change together.
    fn soft_reset(&mut self) {
        // DECAWM comes back on with no wrap pending if it was off, but
        // the Last Column Flag DECRC restored.
        if !self.last_column_flag() {
            self.grid_mut().cursor.pending_wrap = false;
        }
        self.modes.reset(Modes::SOFT_RESET);
        // The selective erase attribute, in the VT520 manual's table, and
        // the guarded area, as xterm ends it.
        self.protect = 0;
        self.guard = 0;
        self.attributes = Attributes::default();
        self.pen_changed();
        self.charsets = Charsets::default();
        // The left and right margins, in the VT520 manual's table too.
        for g in [&mut self.primary, &mut self.alternate] {
            g.cursor.origin = false;
            g.reset_margins();
        }
        self.grid_mut().saved = Saved::default();
        // The palette, as xterm 411 resets it (in neither table); the
        // dynamic and special colours stay.
        if let Some(colours) = &mut self.colours {
            colours.reset_palette();
        }
    }

    /// Clears the alternate screen, its blanks in `pen`'s colours (`bce`),
    /// as xterm clears it for 1047 and 1049. The cursor, origin mode, the
    /// margins and the screen's saved cursor stay, and a pending wrap ends,
    /// as ED's does (DEC STD 070, Appendix D.6.1).
    fn clear_alternate(&mut self, pen: Attributes) -> Result<(), Error> {
        let g = &mut self.alternate;
        let (cursor, saved, lines, columns) = (g.cursor, g.saved, g.lines, g.columns());
        let keyboard = mem::take(&mut g.keyboard);
        self.alternate.clear(&mut self.next_id, self.version)?;
        let blank = self.alternate.style(pen.erased());
        let g = &mut self.alternate;
        (g.cursor, g.saved, g.lines, g.keyboard) = (cursor, saved, lines, keyboard);
        g.set_columns(columns);
        g.cursor.pending_wrap = false;
        if blank != 0 {
            let cols = g.size().cols();
            for y in 0..g.size().rows() {
                g.erase(y, 0, cols, blank, self.version);
            }
        }
        Ok(())
    }

    /// Shows the alternate screen, or the primary. The cursor, with its
    /// pending wrap, origin mode and the margins go along, as in xterm,
    /// where they are the terminal's rather than either screen's: a
    /// program that switches finds the cursor where it left it. Each
    /// screen keeps its own saved cursor, as each of xterm's does.
    fn switch_screen(&mut self, alternate: bool) {
        if self.alternate_active != alternate {
            let (from, to) = if alternate {
                (&self.primary, &mut self.alternate)
            } else {
                (&self.alternate, &mut self.primary)
            };
            (to.cursor, to.lines) = (from.cursor, from.lines);
            to.set_columns(from.columns());
        }
        self.alternate_active = alternate;
        self.structural = self.version;
    }

    /// DECRQCRA (`CSI Pi ; Pp ; Pt ; Pl ; Pb ; Pr * y`, VT420 and up), with
    /// [`Feature::RectangleChecksums`]: the checksum of the rectangle's
    /// cells, as DECCKSR (`DCS Pi ! ~ xxxx ST`, four upper-case hex
    /// digits). DEC's manuals leave the sum to the terminal; this is
    /// xterm's default, which matches the VT520's (`xtermCheckRect`, ctlseqs'
    /// XTCHECKSUM): each cell's character, plus 0x08 hidden, 0x10
    /// underlined, 0x20 inverse, 0x40 blinking and 0x80 bold, summed in 16
    /// bits and negated. A character past Latin-1 or below a space, and a
    /// wide glyph's second half, count as ESC (0x1b), and a cluster's
    /// combining marks are added as they are, as xterm counts them. An
    /// empty cell counts as a space: on DEC's terminals an erased cell holds
    /// one; xterm skips cells nothing was ever written to, a difference
    /// esctest allows. The page `Pp` is ignored, as xterm ignores it (DEC
    /// has 0 mean all pages): there is one. The rectangle's coordinates are
    /// one-based and relative to the margins in origin mode, clamped to the
    /// screen (to the margins in origin mode), with absent or 0 meaning
    /// its whole extent; one whose top is below its bottom, or left past
    /// its right, sums nothing.
    pub(crate) fn rectangle_checksum(&self, p: &Parameters) -> Reply {
        let g = self.grid();
        let mut sum = 0u16;
        if let Some((lines, columns)) = self.rectangle(p, 2) {
            spend!(
                cells,
                usize::from(lines.len()).saturating_mul(usize::from(columns.len()))
            );
            for y in lines.each() {
                for x in columns.each() {
                    if let Some(cell) = g.cell(y, x) {
                        sum = sum.wrapping_add(checksum_of(&cell));
                    }
                }
            }
        }
        let id = p.first(0, 0);
        let checksum = sum.wrapping_neg();
        reply!("\x1bP{id}!~{checksum:04X}\x1b\\")
    }

    /// The rectangle `Pt ; Pl ; Pb ; Pr` of the parameters from `at` on, as
    /// the rectangular area operations and DECRQCRA take it (DEC STD 070,
    /// 5.12.1, 5.12.2): one-based, from the margins in origin mode and
    /// clamped to them, as the cursor is, else to the screen; absent or 0
    /// its whole extent. `None` if its top is below its bottom or its left
    /// past its right, which has the function ignored.
    fn rectangle(&self, p: &Parameters, at: usize) -> Option<(Span, Span)> {
        let (lines, columns) = self.grid().addressed();
        let edge = |span: Span, i: usize, default: u16| {
            span.nth(p.first(at.saturating_add(i), default).saturating_sub(1))
        };
        let (top, bottom) = (edge(lines, 0, 1), edge(lines, 2, u16::MAX));
        let (left, right) = (edge(columns, 1, 1), edge(columns, 3, u16::MAX));
        Some((lines.part(top, bottom)?, columns.part(left, right)?))
    }

    /// DEC STD 070's rectangular area operations (5.12.1), by final byte:
    /// DECFRA (`x`), DECERA (`z`), DECSERA (`{`) and DECCRA (`v`), on the
    /// rectangle their parameters name (`rectangle`), which none of the
    /// margins bounds otherwise, the cursor staying where it is. A page
    /// parameter is ignored: there is one. Out of line, as they are rare.
    #[inline(never)]
    fn rectangle_area(&mut self, p: &Parameters, byte: u8) {
        // DECFRA's rectangle follows its character.
        let Some((lines, columns)) = self.rectangle(p, usize::from(byte == b'x')) else {
            return;
        };
        self.break_cluster();
        let span = (columns.first(), columns.end());
        // A fill or an erase of each row's span, its wide glyphs' halves
        // erased first: twice the rectangle's cells (`budget`). DECCRA's
        // copy spends as it goes.
        if byte != b'v' {
            let cells = usize::from(lines.len()).saturating_mul(usize::from(columns.len()));
            let cells = cells.saturating_mul(2);
            crate::budget::spend(crate::budget::units(cells), crate::budget::weight::cells);
        }
        match byte {
            // DECFRA: Pch from 32 to 126 is that character in GL's set,
            // 160 to 255 in GR's, Latin-1; another is ignored. The cells
            // take the pen, as printing gives it.
            b'x' => {
                let c = match u8::try_from(p.first(0, 32)) {
                    Ok(byte @ 32..=126) => {
                        let mut gl = self.charsets;
                        gl.single = None;
                        gl.map(char::from(byte))
                    }
                    Ok(byte @ 160..=255) => char::from(byte),
                    _ => return,
                };
                let style = self.pen_style() | self.protect;
                self.each_row(lines, |g, y, v| g.fill(y, span, c, style, v));
            }
            // DECERA: every character a space in the pen's colours, as ED's
            // blanks are.
            b'z' => {
                let blank = self.blank_style();
                self.each_row(lines, |g, y, v| g.erase_to_spaces(y, span, Some(blank), v));
            }
            // DECSERA: the glyphs DECSCA does not protect, SPA's guard being
            // no DECSCA, made spaces in their own attributes.
            b'{' => self.each_row(lines, |g, y, v| g.erase_to_spaces(y, span, None, v)),
            // DECCRA: to `Ptd ; Pld`, addressed as the rectangle is.
            _ => {
                let (to_lines, to_columns) = self.grid().addressed();
                let row = to_lines.nth(p.first(5, 1).saturating_sub(1));
                let col = to_columns.nth(p.first(6, 1).saturating_sub(1));
                self.with_grid(|g, _, v| g.copy_rectangle(lines, columns, (row, col), v));
            }
        }
    }

    /// `f` with the grid shown, each of the rows `lines` and the version,
    /// each row keeping its soft wrap: a rectangular area operation changes
    /// no line's (DEC STD 070, 5.12.1), as xterm keeps them.
    fn each_row(&mut self, lines: Span, mut f: impl FnMut(&mut Grid, u16, u64)) {
        self.with_grid(|g, _, v| {
            for y in lines.each() {
                let wrapped = g.live_wrapped(y);
                f(g, y, v);
                g.wrap(y, wrapped, v);
            }
        });
    }

    /// The primary device attributes (DA1, `CSI c`; and DECID, `ESC Z`):
    /// a VT220 with ANSI colour with an identity, else a VT100 with
    /// advanced video, as the vt100 crate answered.
    pub(crate) fn primary_attributes(&self, options: &Options) -> Reply {
        // 4: sixel graphics, when the host's terminal shows them.
        let sixel = if self.graphics.sixel { ";4" } else { "" };
        if options.identity().is_some() {
            // Level 2 (DEC STD 070, DA): fux-vt lacks level 3's terminal
            // state reports and level 4's text macros. Its extensions
            // (Table 1): selectively erasable characters (6: DECSCA,
            // DECSED, DECSEL), horizontal scrolling (21: DECSLRM, DECIC,
            // DECDC, DECBI, DECFI), colour text (22) and rectangular
            // editing (28: DECFRA and the rest).
            reply!("\x1b[?62{sixel};6;21;22;28c")
        } else {
            reply!("\x1b[?1;2{sixel}c")
        }
    }

    /// SM and RM, DECSET and DECRST (`on` for `h`), and XTRESTORE
    /// (`on` `None`: each mode as XTSAVE saved it, but 1048 and DECARM,
    /// which it does not save). Of the ANSI modes, a sequence that names
    /// none fux-vt keeps is unhandled.
    #[inline(always)]
    fn set_modes(
        &mut self,
        p: &Parameters,
        private: bool,
        on: Option<bool>,
        options: &Options,
    ) -> Result<Dispatch, Error> {
        let (mut handled, mut report) = (private, false);
        for group in p.groups() {
            let [n] = group else { continue };
            let Some(mode) = Mode::of(*n, private, options) else {
                continue;
            };
            handled = true;
            let on = match on {
                Some(on) => on,
                None if mode.fixed().is_none() => self.saved_modes.contains(mode),
                None => continue,
            };
            report |= self.set_mode(mode, on)?;
        }
        Ok(match (handled, report) {
            (_, true) => Dispatch::Reply(self.size_report()),
            (true, false) => Dispatch::Done,
            (false, false) => Dispatch::Unhandled,
        })
    }

    /// XTSAVE (`CSI ? Pm s`, with `save`): each mode saved as it is; and
    /// XTRESTORE (`CSI ? Pm r`): each set as it was saved.
    #[inline(never)]
    fn save_or_restore_modes(
        &mut self,
        p: &Parameters,
        save: bool,
        options: &Options,
    ) -> Result<Dispatch, Error> {
        if !save {
            return self.set_modes(p, true, None, options);
        }
        for group in p.groups() {
            if let [n] = group
                && let Some(mode) = Mode::of(*n, true, options).filter(|m| m.fixed().is_none())
            {
                self.saved_modes.set(mode, self.mode(mode));
            }
        }
        Ok(Dispatch::Done)
    }

    /// Sets or resets `mode`; whether a size report is due (setting
    /// in-band resize reports at once, however often it is set).
    #[inline(never)]
    fn set_mode(&mut self, mode: Mode, on: bool) -> Result<bool, Error> {
        match mode.kind() {
            Kind::Flag => {
                // Resetting DECAWM ends a pending wrap (DEC STD 070,
                // Appendix D.6.1), and setting it again starts with none
                // but the Last Column Flag DECRC restored.
                if mode == Mode::Autowrap && !(on && self.last_column_flag()) {
                    self.grid_mut().cursor.pending_wrap = false;
                }
                self.modes.set(mode, on);
            }
            // DECLRMM: reset, the margins go back to the screen's edges (DEC
            // STD 070, DECLRMM; xterm's `set_left_right_margin_mode`).
            // Set, every line is single width (DECLRMM, 5-29).
            Kind::Margins => {
                self.modes.set(mode, on);
                let version = self.version;
                for g in [&mut self.primary, &mut self.alternate] {
                    if on {
                        g.single_width(version);
                    } else {
                        g.set_columns(g.size().columns());
                    }
                }
            }
            // Which sequences reset it, `parser::FrameScan::ends` restates:
            // it changes with this arm, DECSTR, RIS and XTRESTORE.
            Kind::Frame => {
                // A frame begins when the mode is set from reset: set again
                // while set, the frame being drawn goes on.
                if on && !self.modes.contains(mode) {
                    self.frames_begun = self.frames_begun.wrapping_add(1);
                }
                self.modes.set(mode, on);
            }
            Kind::SizeReport => {
                self.modes.set(mode, on);
                return Ok(on);
            }
            Kind::Origin => {
                let g = self.grid_mut();
                g.cursor.origin = on;
                g.position(0, 0);
            }
            // A switch of screens leaves the printed cell behind. The
            // alternate screen is made when first shown, before anything
            // changes, so that failing to make it changes nothing.
            Kind::Screen(switch) => {
                if on {
                    self.alternate.make()?;
                }
                self.break_cluster();
                match switch {
                    Switch::Plain => self.switch_screen(on),
                    // Cleared on leaving, as xterm does it, but only once
                    // shown again: till then, what the program drew is
                    // kept to read. The clear's end of a pending wrap is
                    // now.
                    Switch::ClearedOnLeaving => {
                        if !on && self.alternate_active {
                            self.terminal.clear_on_show = Some(self.attributes);
                            self.alternate.cursor.pending_wrap = false;
                        }
                        self.switch_screen(on);
                    }
                    Switch::SavingCursor if on => {
                        self.save();
                        self.switch_screen(true);
                        self.terminal.clear_on_show = None;
                        self.clear_alternate(self.attributes)?;
                    }
                    Switch::SavingCursor => {
                        self.switch_screen(false);
                        self.restore();
                    }
                }
                if let Some(pen) = self.terminal.clear_on_show.take_if(|_| on) {
                    self.clear_alternate(pen)?;
                }
            }
            Kind::SaveCursor if on => self.save(),
            Kind::SaveCursor => self.restore(),
            Kind::Permanent(_) => {}
            Kind::Mouse(mouse) => latest(&mut self.terminal.mouse, mouse, on),
            Kind::Encoding(encoding) => latest(&mut self.terminal.encoding, encoding, on),
        }
        Ok(false)
    }

    /// DECRQSS (`DCS $ q Pt ST`): the setting `request` names, written to
    /// `out` as the control function that sets it, in the form xterm
    /// answers (`do_dcs` and `xtermFormatSGR`, xterm 411 misc.c).
    pub(crate) fn setting_report(&self, request: &[u8], out: &mut String) -> Setting {
        use std::fmt::Write;
        match request {
            b"m" => {
                let mut sgr = b"0".to_vec();
                self.attributes.write_sgr(&mut sgr, SgrForm::Report);
                out.push_str(&String::from_utf8_lossy(&sgr));
                out.push('m');
            }
            b" q" => {
                let shape = self.terminal.cursor_shape;
                if shape == CursorShape::Initial {
                    return Setting::Unanswered;
                }
                // 0 as the blinking block it is, as xterm answers it.
                let ps = shape.number().max(1);
                let _ = write!(out, "{ps} q");
            }
            b"r" => {
                // Exact: the margins are rows of a screen a u16 counts.
                let (top, bottom) = self.scroll_region();
                let (top, bottom) = (top.saturating_add(1), bottom.saturating_add(1));
                let _ = write!(out, "{top};{bottom}r");
            }
            // DECSLRM's margins, the screen's edges without DECLRMM.
            b"s" => {
                let (left, right) = self.left_right_margins();
                let (left, right) = (left.saturating_add(1), right.saturating_add(1));
                let _ = write!(out, "{left};{right}s");
            }
            // DECSCA's selective erase attribute, which SPA is not.
            b"\"q" => {
                let on = self.protect != 0;
                let _ = write!(out, "{}\"q", u8::from(on));
            }
            _ => return Setting::Unknown,
        }
        Setting::Written
    }

    /// The kitty keyboard protocol and modifyOtherKeys sequences, with
    /// [`Feature::KittyKeyboard`]; `None` for any other sequence.
    fn keyboard_protocol(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
    ) -> Option<Dispatch> {
        // The flags, a bit set of those fux-vt honours; other bits are
        // dropped.
        let flags = |n: u16| u8::try_from(n & u16::from(KITTY_FLAGS_KEPT)).unwrap_or(0);
        match (intermediates, byte) {
            (b">", b'u') => self.grid_mut().keyboard.push(flags(p.first(0, 0))),
            (b"<", b'u') => self.grid_mut().keyboard.pop(p.first(0, 1)),
            (b"=", b'u') => {
                let (set, mode) = (flags(p.first(0, 0)), p.first(1, 1));
                self.grid_mut().keyboard.set(set, mode);
            }
            (b"?", b'u') => {
                let flags = self.kitty_keyboard_flags();
                return Some(Dispatch::Reply(reply!("\x1b[?{flags}u")));
            }
            // XTMODKEYS with no parameters resets every resource,
            // modifyOtherKeys too (ctlseqs); 0 alone is modifyKeyboard's.
            (b">", b'm') if !p.given => self.terminal.modify_other_keys = None,
            // Only resource 4 is modifyOtherKeys; other resources are unhandled.
            (b">", b'm') if p.groups().next() == Some(&[4][..]) => {
                self.terminal.modify_other_keys = match p.first(1, 0) {
                    1 => Some(OtherKeys::Usual),
                    2 => Some(OtherKeys::All),
                    3 => Some(OtherKeys::Unmodified),
                    // 0 or none is off; ctlseqs knows no level past 3.
                    _ => None,
                };
            }
            // XTQMODKEYS (ctlseqs, `CSI ? Pp m`): "XTerm's response ... is
            // formatted as an XTMODKEYS control, i.e., CSI > Pp m", its
            // value 0 when off. Only modifyOtherKeys (4) is kept, so only
            // it is answered.
            (b"?", b'm') if p.groups().next() == Some(&[4][..]) => {
                let level = match self.terminal.modify_other_keys {
                    None => 0,
                    Some(OtherKeys::Usual) => 1,
                    Some(OtherKeys::All) => 2,
                    Some(OtherKeys::Unmodified) => 3,
                };
                return Some(Dispatch::Reply(reply!("\x1b[>4;{level}m")));
            }
            // Disables it (ctlseqs, `CSI > Ps n`: "a resource value of -1",
            // which keys read as off).
            (b">", b'n') if p.groups().next() == Some(&[4][..]) => {
                self.terminal.modify_other_keys = None;
            }
            _ => return None,
        }
        Some(Dispatch::Done)
    }

    pub(crate) fn csi(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
        options: &Options,
    ) -> Result<Dispatch, Error> {
        let private = intermediates == b"?";
        // DECSCUSR: its intermediate is a space.
        if intermediates == b" " && byte == b'q' {
            // 0, the default, is a blinking block, as 1 is, kept apart.
            let ps = p.first(0, 0);
            if let Some(&shape) = CursorShape::ALL.iter().find(|s| s.number() == ps) {
                self.terminal.cursor_shape = shape;
            }
            return Ok(Dispatch::Done);
        }
        if options.has(Feature::KittyKeyboard)
            && let Some(dispatch) = self.keyboard_protocol(p, intermediates, byte)
        {
            return Ok(dispatch);
        }
        if intermediates == b"!" && byte == b'p' {
            self.soft_reset();
            return Ok(Dispatch::Done);
        }
        if !intermediates.is_empty() && !private {
            return Ok(Dispatch::Unhandled);
        }
        if matches!(byte, b'h' | b'l') {
            return self.set_modes(p, private, Some(byte == b'h'), options);
        }
        // Every sequence that moves the cursor or edits a row; not SGR,
        // modes or queries.
        if matches!(
            byte,
            b'A'..=b'H'
                | b'J'
                | b'K'
                | b'L'
                | b'M'
                | b'P'
                | b'S'
                | b'T'
                | b'X'
                | b'@'
                | b'I'
                | b'Z'
                | b'`'
                | b'a'
                | b'd'
                | b'e'
                | b'f'
                | b'r'
                | b's'
                | b'u'
        ) {
            self.break_cluster();
        }
        // XTSAVE and XTRESTORE (ctlseqs). Their private marker tells them
        // from DECSLRM (`CSI Pl ; Pr s`) and SCOSC (`CSI s`), and from
        // DECSTBM (`CSI Pt ; Pb r`); `CSI s` is DECSLRM below while DECLRMM
        // is set, and SCOSC otherwise.
        if private && matches!(byte, b's' | b'r') {
            return self.save_or_restore_modes(p, byte == b's', options);
        }
        if private && !matches!(byte, b'J' | b'K') {
            return Ok(Dispatch::Unhandled);
        }
        let n = p.first(0, 1);
        let (row, col) = self.grid().cursor.at();
        let pending = self.grid().cursor.pending_wrap;
        // Every cursor movement, erase and edit ends a pending wrap (DEC STD
        // 070, Appendix D.6.1, which lists them), as xterm does; ED, EL, IL,
        // DL and DECSTBM below, once they are carried out, and CHT and CBT
        // in `tab`. SU and SD are not among them: the cursor stays waiting
        // to wrap, as in xterm.
        if matches!(byte, b'A'..=b'H' | b'X' | b'`' | b'a' | b'd' | b'e' | b'f') {
            self.grid_mut().cursor.pending_wrap = false;
        }
        match byte {
            // CUU, CUD, CNL and CPL, each stopping at the margin it comes
            // to: up at the top margin from at or below it, down at the
            // bottom margin from at or above it, else at the screen's edge
            // (xterm's `CursorUp` and `CursorDown`). CNL and CPL go to the
            // column CR goes to, the left margin (xterm's `CursorNextLine`).
            b'A' | b'B' | b'E' | b'F' => {
                let g = self.grid_mut();
                let lines = g.lines;
                g.set_row(if matches!(byte, b'A' | b'F') {
                    let stop = if row >= lines.first() {
                        lines.first()
                    } else {
                        0
                    };
                    row.saturating_sub(n).max(stop)
                } else if row <= lines.last() {
                    row.saturating_add(n).min(lines.last())
                } else {
                    row.saturating_add(n)
                });
                if matches!(byte, b'E' | b'F') {
                    g.carriage_return();
                }
            }
            // CUF: stops at the right margin, unless the cursor is past it
            // already, then at the last column (xterm's `CursorForward`;
            // DEC STD 070, 5.4.3).
            b'C' => self.grid_mut().forward(n, false),
            // HPR: a position, as CUP sets it (VT520 manual, HPR), so it
            // passes the right margin outside origin mode, unlike CUF, and
            // stops there in it (xterm's `CASE_HPR`).
            b'a' => self.grid_mut().forward(n, true),
            // CUB; with reverse wraparound, back over line ends too.
            b'D' if self.reverse_wraps() => self.cursor_back(n, pending),
            // CUB stops at the left margin, unless the cursor is left of it
            // already (xterm's `CursorBack`).
            b'D' => self.grid_mut().backward(n),
            // CHA, and HPA: the column as CUP addresses it, from the left
            // margin in origin mode. Coordinates are one-based, and 0
            // means 1.
            b'G' | b'`' => self.grid_mut().column(n.saturating_sub(1)),
            // CUP, and HVP, which is CUP with another final byte.
            b'H' | b'f' => self
                .grid_mut()
                .position(n.saturating_sub(1), p.first(1, 1).saturating_sub(1)),
            // DECSLRM (`CSI Pl ; Pr s`, DEC STD 070, 5-27) while DECLRMM is
            // set: margins with the left one left of the right are set,
            // and the cursor goes home, obeying DECOM; others are ignored,
            // a right margin past the screen too (notes 2 and 3).
            b's' if self.mode(Mode::LeftRightMargins) => self.set_left_right_margins(p),
            // SCOSC and SCORC share DECSC's slot and, like DECSC, save and
            // restore the attributes with the position.
            b's' => self.save(),
            b'u' => self.restore(),
            // VPA and VPR address lines as CUP does, from the top margin in
            // origin mode, within the margins there and the screen
            // otherwise (DEC STD 070, DECOM: in displaced mode the active
            // position cannot leave the margins), as xterm does. VPR
            // counts from the cursor's line, so unlike CUD it passes the
            // bottom margin with DECOM reset.
            b'd' | b'e' => {
                let g = self.grid_mut();
                let line = if byte == b'd' {
                    n.saturating_sub(1)
                } else {
                    g.cursor_line().saturating_add(n)
                };
                g.position_line(line);
            }
            b'@' | b'P' => {
                let blank = self.blank_style();
                self.with_grid(|g, _, v| g.edit_cells(n, byte == b'@', blank, v));
            }
            // Erased cells take the pen's colours alone, as xterm's do.
            // Guarded glyphs (SPA's) stay.
            b'X' => {
                let a = self.blank_style();
                let end = col.saturating_add(n);
                if self.protected {
                    self.erase_kept(row, col, end);
                } else {
                    self.with_grid(|g, _, v| g.erase(row, col, end, a, v));
                }
            }
            // ED and EL, DECSED and DECSEL carry out each parameter in
            // turn, as that many sequences of one would (DEC STD 070, their
            // algorithms, chapter 5's revision note 12); unhandled if none
            // is known.
            b'J' | b'K' => {
                let mut known = false;
                for group in p.groups() {
                    let mode = group.first().copied().unwrap_or(0);
                    // ED 3 (`CSI 3 J`, xterm's): the scrollback goes, as
                    // `clear` asks; the screen stays.
                    if byte == b'J' && mode == 3 && !private {
                        self.clear_history();
                        known = true;
                        continue;
                    }
                    let Some(part) = Erase::ALL.into_iter().find(|e| e.number() == mode) else {
                        continue;
                    };
                    known = true;
                    // DECSED and DECSEL (`CSI ? Ps J`, `CSI ? Ps K`) leave
                    // DECSCA's glyphs and every rendition, ED and EL SPA's.
                    if private || self.protected {
                        self.erase_protected(private, byte == b'J', part);
                    } else {
                        let a = self.blank_style();
                        self.with_grid(|g, _, v| {
                            g.erase_in(byte == b'J', part, v, |g, y, start, end| {
                                g.erase(y, start, end, a, v);
                            });
                        });
                    }
                    if byte == b'J' && part == Erase::All {
                        self.screen_erased();
                    }
                }
                if !known {
                    return Ok(Dispatch::Unhandled);
                }
            }
            // IL and DL, ignored outside the margins, leave the cursor in
            // the first column (DEC STD 070, IL and DL, note 2).
            // With left and right margins, only between them, and the
            // cursor goes to the left margin (xterm's `InsertLine`).
            b'L' | b'M' => {
                let g = self.grid_mut();
                if let Some(region) = g.lines.starting_at(row)
                    && g.in_columns()
                {
                    g.cursor.pending_wrap = false;
                    g.set_col(g.columns().first());
                    let direction = match byte {
                        b'M' => Scroll::Up { history: false },
                        _ => Scroll::Down,
                    };
                    spend_scroll(region, n, self.size());
                    self.scroll(region, n, direction)?;
                }
            }
            b'S' | b'T' => {
                let lines = self.grid().lines;
                let direction = match byte {
                    b'S' => Scroll::Up { history: true },
                    _ => Scroll::Down,
                };
                spend_scroll(lines, n, self.size());
                self.scroll(lines, n, direction)?;
            }
            // DECSTBM (DEC STD 070, 5-25): margins with the top above the
            // bottom are set, and the cursor goes home, obeying DECOM;
            // others are ignored, a bottom past the screen too (notes 2
            // and 3; xterm takes it as the last line).
            b'r' => {
                let lines = self.grid().size().lines();
                let bottom = p.first(1, lines.end()).saturating_sub(1);
                if let Some(lines) = lines.margins(n.saturating_sub(1), bottom) {
                    let g = self.grid_mut();
                    g.lines = lines;
                    g.position(0, 0);
                }
            }
            b'm' => {
                self.sgr(p);
                self.pen_changed();
            }
            b'b' => self.repeat(n)?,
            // CHT (ECMA-48 8.3.10): HT n times.
            b'I' => self.tab(n, true),
            // CBT (ECMA-48 8.3.7): back n tab stops, or to the first
            // column.
            b'Z' => self.tab(n, false),
            // TBC (ECMA-48 8.3.154; DEC STD 070, TBC, p. 5-66): each
            // parameter in turn, 0 clearing the stop at the cursor and 3
            // every stop, the two DEC STD 070 defines; the others, for
            // stops kept line by line, do nothing.
            b'g' => {
                for group in p.groups() {
                    match group.first() {
                        Some(0) => {
                            let col = self.grid().cursor.col();
                            self.terminal.tabs.set(col, false);
                        }
                        Some(3) => self.terminal.tabs.clear(),
                        _ => {}
                    }
                }
            }
            b'n' => match p.first(0, 0) {
                5 => return Ok(Dispatch::Reply(reply!("\x1b[0n"))),
                6 => {
                    let (row, col) = self.reported_cursor();
                    let reply = reply!("\x1b[{row};{col}R");
                    return Ok(Dispatch::Reply(reply));
                }
                _ => return Ok(Dispatch::Unhandled),
            },
            b'c' if p.first(0, 0) == 0 => {
                return Ok(Dispatch::Reply(self.primary_attributes(options)));
            }
            _ => return Ok(Dispatch::Unhandled),
        }
        Ok(Dispatch::Done)
    }

    /// The CSI sequences `csi` leaves unhandled, the parser asks here then
    /// (`Parser::csi_dispatch`): DECSCA, DECIC, DECDC, SL and SR and the
    /// rectangular area operations, carried out, and the
    /// queries' replies, some enabled by [`Feature::ExtendedReplies`] and
    /// [`Options::identity`]; `None` for the rest, unhandled. Carried out
    /// in `csi`, even out of line, they cost every CSI a percent or two of
    /// its instructions.
    #[inline(never)]
    pub(crate) fn other_csi(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
        options: &Options,
    ) -> Option<Dispatch> {
        let n = p.first(0, 0);
        let extended = options.has(Feature::ExtendedReplies);
        let modes = extended || options.has(Feature::ModeReports);
        let identity = options.identity();
        let reply = match (intermediates, byte) {
            // DECSCA (`CSI Ps " q`; DEC STD 070, 5.11.1.2): 1 protects the
            // glyphs printed next from DECSED and DECSEL, 0 and 2 end it,
            // as xterm reads it.
            (b"\"", b'q') => {
                match n {
                    0 | 2 => self.protect = 0,
                    1 => {
                        self.protect = PROTECTED;
                        self.protected = true;
                    }
                    _ => {}
                }
                return Some(Dispatch::Done);
            }
            // DECIC and DECDC (`CSI Pn ' }`, `CSI Pn ' ~`; DEC STD 070,
            // 5.4.3): columns inserted or deleted at the cursor's in every
            // line of the scrolling region.
            (b"'", b'}' | b'~') => {
                self.break_cluster();
                self.edit_columns(p.first(0, 1), byte == b'}');
                return Some(Dispatch::Done);
            }
            // SL and SR (`CSI Pn SP @`, `CSI Pn SP A`; ECMA-48 8.3.121,
            // 8.3.135): the data moved Pn columns left or right, within the
            // margins, and with the cursor between them, as xterm bounds
            // them.
            (b" ", b'@' | b'A') => {
                self.break_cluster();
                let g = self.grid();
                if g.lines.contains(g.cursor.row()) && g.in_columns() {
                    self.shift_columns(g.columns().first(), p.first(0, 1), byte == b'A');
                }
                return Some(Dispatch::Done);
            }
            (b"?", b'n') if extended => match n {
                6 => {
                    let (row, col) = self.reported_cursor();
                    Some(reply!("\x1b[?{row};{col}R"))
                }
                // The DSRs of DEC's other devices, which fux-vt has none
                // of (DEC STD 070, DSR, and its extensions'): no printer
                // (-7), no UDK extension (-11), a keyboard of unknown
                // language (as a VT220 reports it, which DA2 says fux-vt
                // is: the VT520 manual sends its status and type at level
                // 4 only), no locator (-13), of unknown type, no macro
                // space and so DECCKSR's checksum of no macros, labelled as
                // asked.
                15 => Some(reply!("\x1b[?13n")),
                25 => Some(reply!("\x1b[?23n")),
                26 => Some(reply!("\x1b[?27;0n")),
                55 => Some(reply!("\x1b[?53n")),
                56 => Some(reply!("\x1b[?57;0n")),
                62 => Some(reply!("\x1b[0*{{")),
                63 => {
                    let label = p.first(1, 0);
                    Some(reply!("\x1bP{label}!~0000\x1b\\"))
                }
                // The data integrity flag: power-up (73), until reported,
                // then no errors since the last report (70); RIS is a
                // power-up again.
                75 => {
                    let integrity = &mut self.terminal.integrity;
                    let status = mem::replace(integrity, Integrity::Reported).report();
                    Some(reply!("\x1b[?{status}n"))
                }
                // Not configured for multiple sessions.
                85 => Some(reply!("\x1b[?83n")),
                _ => None,
            },
            (b">", b'c') if n == 0 => match identity {
                Some(identity) => {
                    let version = identity.encoded_version();
                    Some(reply!("\x1b[>1;{version};0c"))
                }
                None if extended => Some(reply!("\x1b[>1;10;0c")),
                None => None,
            },
            (b">", b'q') if n == 0 => {
                let identity = identity?;
                let length = identity.name.len().checked_add(identity.version.len())?;
                if length > Identity::MAX_LEN {
                    return None;
                }
                let (name, version) = (identity.name, identity.version);
                Some(reply!("\x1bP>|{name} {version}\x1b\\"))
            }
            (b"", b't') if n == 18 && options.has(Feature::SizeReports) => {
                let (rows, cols) = self.size().into();
                Some(reply!("\x1b[8;{rows};{cols}t"))
            }
            // The cell's and the text area's size in pixels, from the host's
            // terminal: how a program sizes the pictures it draws.
            (b"", b't') if n == 16 => {
                let (width, height) = self.graphics.cell?;
                Some(reply!("\x1b[6;{height};{width}t"))
            }
            (b"", b't') if n == 14 => {
                let (height, width) = self.pixel_size()?;
                Some(reply!("\x1b[4;{height};{width}t"))
            }
            // XTSMGRAPHICS (ctlseqs, `CSI ? Pi ; Pa ; Pv S`), answered as
            // xterm 411 answers it, to a request of three parameters (or of
            // two, Pv left out, which xterm leaves unanswered): `CSI ?
            // Pi ; Ps [; Pv] S`, Ps 0 for success with the values, 1 for an
            // item it does not know, 2 for an action, 3 for a failure. Read,
            // read the maximum or reset: 256 colour registers, and pictures
            // as large as the screen, while the host shows sixel; setting
            // either fails, as fux-vt keeps them fixed, and so does every
            // request without sixel shown, and ReGIS's (3).
            (b"?", b'S') if matches!(p.groups().count(), 2 | 3) => {
                let action = p.first(1, 0);
                let status = match (n, action) {
                    (1..=3, 1..=4) => 3,
                    (1..=3, _) => 2,
                    _ => 1,
                };
                let read = self.graphics.sixel && matches!(action, 1 | 2 | 4);
                match n {
                    1 if read => Some(reply!("\x1b[?1;0;256S")),
                    2 if read && let Some((height, width)) = self.pixel_size() => {
                        Some(reply!("\x1b[?2;0;{width};{height}S"))
                    }
                    _ => Some(reply!("\x1b[?{n};{status}S")),
                }
            }
            (b"$", b'x' | b'z' | b'{' | b'v') => {
                self.rectangle_area(p, byte);
                return Some(Dispatch::Done);
            }
            (b"*", b'y') if options.has(Feature::RectangleChecksums) => {
                Some(self.rectangle_checksum(p))
            }
            // DECRQM.
            (b"?$" | b"$", b'p') if modes => {
                let private = intermediates == b"?$";
                let mode = Mode::of(n, private, options);
                let status = match mode {
                    None if !private && RESET_FOR_GOOD.contains(&n) => ModeStatus::PermanentlyReset,
                    None if !private && SET_FOR_GOOD.contains(&n) => ModeStatus::PermanentlySet,
                    None => ModeStatus::NotRecognized,
                    Some(mode) => mode.fixed().unwrap_or(match self.mode(mode) {
                        true => ModeStatus::Set,
                        false => ModeStatus::Reset,
                    }),
                };
                let (marker, status) = (if private { "?" } else { "" }, status.number());
                Some(reply!("\x1b[{marker}{n};{status}$y"))
            }
            _ => None,
        };
        reply.map(Dispatch::Reply)
    }

    fn sgr(&mut self, p: &Parameters) {
        let mut groups = p.groups().peekable();
        while let Some(group) = groups.next() {
            match group {
                [0] => self.attributes = Attributes::default(),
                // The flags' rows (`Attributes::FLAGS`), as literal arms the
                // hot path jumps to; a test holds them to the rows. Bold and
                // dim are kept apart, and both can be on, as in xterm; 22
                // ends both (ECMA-48 8.3.117).
                [1] => self.attributes.flags |= Attributes::BOLD,
                [2] => self.attributes.flags |= Attributes::DIM,
                [3] => self.attributes.flags |= Attributes::ITALIC,
                [7] => self.attributes.flags |= Attributes::INVERSE,
                [8] => self.attributes.flags |= Attributes::HIDDEN,
                [9] => self.attributes.flags |= Attributes::STRIKEOUT,
                [22] => self.attributes.flags &= !(Attributes::BOLD | Attributes::DIM),
                [23] => self.attributes.flags &= !Attributes::ITALIC,
                [27] => self.attributes.flags &= !Attributes::INVERSE,
                [28] => self.attributes.flags &= !Attributes::HIDDEN,
                [29] => self.attributes.flags &= !Attributes::STRIKEOUT,
                [4] => self.underline(UnderlineStyle::Single),
                // Doubly underlined (ECMA-48 8.3.117, xterm's ctlseqs).
                [21] => self.underline(UnderlineStyle::Double),
                // Slow and rapid blink replace one another.
                [5] => self.attributes = self.attributes.with_blink(Blink::Slow),
                [6] => self.attributes = self.attributes.with_blink(Blink::Rapid),
                [24] => self.underline(UnderlineStyle::None),
                [25] => self.attributes.flags &= !Attributes::BLINK,
                [39] => self.attributes = self.attributes.with_foreground(Color::Default),
                [49] => self.attributes = self.attributes.with_background(Color::Default),
                [59] => self.attributes = self.attributes.with_underline_color(Color::Default),
                // 30–37 and 40–47 are colours 0–7 of the foreground (38)
                // and the background (48), and 90–97 and 100–107 their
                // bright forms 8–15.
                [n @ 30..=37] => self.set_colour(38, indexed(*n, 30)),
                [n @ 40..=47] => self.set_colour(48, indexed(*n, 40)),
                [n @ 90..=97] => self.set_colour(38, indexed(*n, 90 - 8)),
                [n @ 100..=107] => self.set_colour(48, indexed(*n, 100 - 8)),
                // Underline styles (`references/modern/kitty_underlines.html`):
                // `4:0` ends the underline, and 1 to 5 are single, double,
                // curly, dotted and dashed. Another number changes nothing.
                [4, n, ..] => {
                    if let Some(style) = UnderlineStyle::from_number(*n) {
                        self.underline(style);
                    }
                }
                // Foreground, background and underline colour share their
                // forms (ITU-T T.416, 13.1.8, and xterm's ctlseqs). An
                // invalid colour is skipped, and the rest of the SGR goes on.
                [selector @ (38 | 48 | 58)] => {
                    if let Some(colour) = Self::colour_after(&mut groups) {
                        self.set_colour(*selector, colour);
                    }
                }
                [selector @ (38 | 48 | 58), rest @ ..] => {
                    if let Some(colour) = colour_of(rest) {
                        self.set_colour(*selector, colour);
                    }
                }

                _ => {}
            }
        }
    }

    fn underline(&mut self, style: UnderlineStyle) {
        self.attributes = self.attributes.with_underline_style(style);
    }

    fn set_colour(&mut self, selector: u16, colour: Color) {
        self.attributes = match selector {
            38 => self.attributes.with_foreground(colour),
            48 => self.attributes.with_background(colour),
            _ => self.attributes.with_underline_color(colour),
        };
    }

    /// The colour of 38, 48 or 58 in their semicolon form, taken from the
    /// parameters after it, as xterm reads it: `5;index` or `2;r;g;b`, a
    /// value the list ends before being 0. Another kind takes only itself.
    /// `None`, with what it named taken all the same, if the colour is out
    /// of range.
    fn colour_after<'a>(
        groups: &mut std::iter::Peekable<impl Iterator<Item = &'a [u16]>>,
    ) -> Option<Color> {
        let kind = match groups.peek() {
            Some([kind]) => *kind,
            _ => return None,
        };
        groups.next();
        let count = match kind {
            2 => 3,
            5 => 1,
            _ => return None,
        };
        // Read as the colon form is: `5:index` or `2:r:g:b`.
        let mut values = [kind, 0, 0, 0];
        for value in values.iter_mut().skip(1).take(count) {
            let Some([n]) = groups.peek() else {
                break;
            };
            *value = *n;
            groups.next();
        }
        colour_of(&values)
    }
}

/// The screen's checks, each shown a screen with its one fault (HAR-26).
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;

    fn screen() -> Result<Screen, Error> {
        let size = Size::new(3, 6).map_err(|_| Error::Capacity)?;
        let mut screen = Screen::new(size, 1 << 12)?;
        screen.record_prints(true);
        screen.ascii(b"hello")?;
        Ok(screen)
    }

    fn shows(check: &str, fault: impl FnOnce(&mut Screen)) -> Result<(), Error> {
        let mut screen = screen()?;
        assert!(screen.check().is_clean(), "{}", screen.check());
        fault(&mut screen);
        let report = screen.check();
        assert!(
            report.faults_of(check).next().is_some(),
            "{check}: {report}"
        );
        Ok(())
    }

    #[test]
    fn each_check_names_its_fault() -> Result<(), Error> {
        shows("screen.ids", |s| {
            let id = s.primary.live_row(0).map(|r| r.id());
            if let Some(id) = id {
                s.alternate.give_id(0, id);
            }
        })?;
        shows("screen.structural", |s| {
            s.structural = s.version.saturating_add(1)
        })?;
        shows("screen.tabs", |s| {
            s.terminal.tabs.stops = vec![false; 70_000]
        })?;
        shows("prints.runs", |s| {
            if let Some(prints) = &mut s.prints {
                prints.text.push('x');
            }
        })?;
        shows("prints.cap", |s| {
            if let Some(prints) = &mut s.prints {
                prints.text.reserve(prints.cap().saturating_add(1));
            }
        })?;
        Ok(())
    }
}

/// What IL, DL, SU or SD of `count` lines of `region` spends (`budget`):
/// each row it brings in, its cells blanked and coloured, and the slots it
/// turns: the region's once, whatever the count (`Order::rotate`), and the
/// screen's once more if the region crosses the ring's end. A line feed
/// spends nothing: a byte each, it does at most a row's work.
fn spend_scroll(region: Span, count: u16, size: Size) {
    let rows = usize::from(count.min(region.len()));
    let cells = rows
        .saturating_mul(usize::from(size.cols()))
        .saturating_mul(2);
    let turned = usize::from(region.len()).saturating_add(usize::from(size.rows()));
    let slots = rows.min(1).saturating_mul(turned);
    crate::budget::spend(crate::budget::units(cells), crate::budget::weight::cells);
    crate::budget::spend(crate::budget::units(slots), crate::budget::weight::slots);
}
