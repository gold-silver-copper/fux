use crate::compact::{Compact, PROTECTED};
use crate::link::{Held, Pen};
use crate::mode::{Kind, Mode, Modes, Switch};
use crate::unicode::Cluster;
use crate::{
    Attributes, Blink, CellRef, Color, Error, Feature, Hyperlink, Mark, Options, Reply, Row, RowId,
    Rows, UnderlineStyle, Window,
    geometry::{Size, Span},
    grid::{Cursor, Grid, Scroll},
    parser::Parameters,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The blank style (`Screen::blank_style`) not asked for since the pen
/// changed. The pen's own style is found at once (`pen_changed`).
const UNKNOWN: u32 = u32::MAX;
/// A pen style that is no number of its own: the grid's table has it.
const TABLED: u32 = u32::MAX - 1;

/// The mouse reporting a program asked for, the latest set winning
/// (`CSI ? 9 / 1000 / 1002 / 1003 h`). fux-vt sends no report of its own;
/// the host encodes one with `Screen::encode_mouse`, which reads this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
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
/// How mouse reports are to be encoded (`CSI ? 1005 / 1006 h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
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
/// DEC STD 070, ch. 3; the VT520 manual's SCS): G0 and G1, each ASCII or
/// DEC Special Graphics, and which of them is in GL, G1 after SO and G0
/// after SI. G2, G3 and the national sets are not kept: a set other than
/// Special Graphics is ASCII, as xterm reads one in UTF-8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Charsets {
    g0_graphics: bool,
    g1_graphics: bool,
    shifted: bool,
}

impl Charsets {
    /// Whether printable ASCII prints as DEC Special Graphics.
    fn graphics(self) -> bool {
        if self.shifted {
            self.g1_graphics
        } else {
            self.g0_graphics
        }
    }
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
    /// The last stop before `col`, or the first column.
    fn previous(&self, col: u16) -> u16 {
        let mut at = col;
        while at > 0 {
            at = at.saturating_sub(1);
            if self.is_stop(at) {
                return at;
            }
        }
        0
    }
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

/// Which protected glyphs erasing leaves (xterm's `protected_mode`): set
/// by the last DECSCA (DEC's) or SPA (ISO's) for the whole terminal, as the
/// glyphs printed after either are protected; none after a reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Protection {
    /// No erase leaves any glyph.
    #[default]
    Off,
    /// DECSED and DECSEL leave protected glyphs (DEC STD 070, 5.11.1.2,
    /// Selectively Erasable Character Attribute); ED, EL and ECH erase
    /// them, as on a VT220.
    Dec,
    /// Every erase, ED, EL, ECH, DECSED and DECSEL, leaves them (ECMA-48,
    /// SPA and ERM reset; xterm, where DECSED and DECSEL do too).
    Iso,
}

/// The most flag sets a kitty keyboard stack holds; a push onto a full stack
/// drops the oldest, as kitty does, so a runaway program cannot grow it.
const KEYBOARD_STACK_LIMIT: usize = 32;

/// One screen's kitty keyboard protocol flag stack. The primary and
/// alternate screens each keep one, so a program that pushes flags on the
/// alternate screen and exits without popping them leaves the shell's alone.
#[derive(Clone, Copy, Debug, Default)]
struct KeyboardStack {
    flags: [u8; KEYBOARD_STACK_LIMIT],
    len: usize,
}

impl KeyboardStack {
    fn top(&self) -> u8 {
        self.len
            .checked_sub(1)
            .and_then(|i| self.flags.get(i))
            .copied()
            .unwrap_or(0)
    }
    fn push(&mut self, flags: u8) {
        if self.len >= KEYBOARD_STACK_LIMIT {
            // A turn by one of the whole, non-empty array.
            self.flags.rotate_left(1);
            self.len = KEYBOARD_STACK_LIMIT.saturating_sub(1);
        }
        if let Some(slot) = self.flags.get_mut(self.len)
            && let Some(len) = self.len.checked_add(1)
        {
            *slot = flags;
            self.len = len;
        }
    }
    fn pop(&mut self, count: u16) {
        self.len = self.len.saturating_sub(usize::from(count));
    }
    /// `CSI = flags ; mode u`: mode 1 (the default) replaces the top's
    /// flags, 2 adds to them and 3 removes from them.
    fn set(&mut self, flags: u8, mode: u16) {
        if self.len == 0 {
            self.push(0);
        }
        if let Some(top) = self.len.checked_sub(1).and_then(|i| self.flags.get_mut(i)) {
            *top = match mode {
                2 => *top | flags,
                3 => *top & !flags,
                _ => flags,
            };
        }
    }
}

/// Terminal state. Reading a window never changes where subsequent output lands.
#[derive(Clone, Debug)]
pub struct Screen {
    primary: Grid,
    alternate: Grid,
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
    /// nothing up.
    pen_style: u32,
    blank_style: u32,
    /// The number each of those has in the grid's table, if it is
    /// `TABLED` and its number was found since it changed: with whether
    /// the grid was the alternate screen's, and the grid's styles' epoch
    /// then (`Grid::epoch`). The number holds while both are still so
    /// (`still`).
    pen_table: Option<(u32, bool, u64)>,
    blank_table: Option<(u32, bool, u64)>,
    saved_attributes: Attributes,
    charsets: Charsets,
    /// The character sets DECSC saved, which DECRC restores.
    saved_charsets: Charsets,
    /// The modes kept as bits (`Kind::Flag`, `Margins`, `Frame`,
    /// `SizeReport`) that are set.
    modes: Modes,
    /// The modes XTSAVE saved set. One never saved is reset, as in xterm,
    /// and RIS and DECSTR keep them, as xterm 411 does.
    saved_modes: Modes,
    /// [`PROTECTED`] while the glyphs printed are protected from selective
    /// erase (DECSCA 1, SPA), else 0: or-ed into the style they are
    /// written in. DECSC saves it, as xterm does.
    protect: u32,
    saved_protect: u32,
    /// Which erases leave protected glyphs.
    protection: Protection,
    tabs: TabStops,
    /// How many times synchronized output has been set, for
    /// `Parser::process_until_frame`.
    frames_begun: u64,
    cursor_shape: u16,
    mouse: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    primary_keyboard: KeyboardStack,
    alternate_keyboard: KeyboardStack,
    modify_other_keys: Option<u8>,
    /// The cell the last glyph was printed in, and the state of its
    /// grapheme cluster, while the cursor has not moved nor the row been
    /// edited since: a character that continues the cluster joins its cell
    /// rather than taking one of its own.
    last_print: Option<Printed>,
    /// The character REP (`CSI b`) repeats: the last one printed that took
    /// a cell of its own, as long as nothing but printing came after it.
    repeat: Option<char>,
    /// The hyperlink the program opened (OSC 8), which glyphs printed take.
    link: Option<Pen>,
    /// Whether a link was opened since the screen was made or reset: until
    /// one is, no row has links, and printing, which asks this alone,
    /// leaves the links be.
    links_seen: bool,
    /// The key the next link opened takes (`Hyperlink::key`).
    next_link: u64,
    /// The colours the program set, with `Feature::Palette`: made when
    /// the first is set, so a screen whose program sets none keeps none.
    colours: Option<Box<crate::palette::Colours>>,
}

#[cfg(test)]
mod tests;

/// The palette colour an SGR parameter names: 30–37 and 40–47 are colours
/// 0–7, and 90–97 and 100–107 their bright forms 8–15.
fn palette(n: u16) -> Option<Color> {
    let (base, bright) = match n {
        30..=37 => (30, 0),
        40..=47 => (40, 0),
        90..=97 => (90, 8),
        100..=107 => (100, 8),
        _ => return None,
    };
    let index = u8::try_from(n.checked_sub(base)?).ok()?;
    Some(Color::Idx(index.checked_add(bright)?))
}

/// The colour of 38, 48 or 58 in their colon form, `rest` being what
/// follows the selector: `5:index`, `2:r:g:b`, or ITU-T T.416's
/// `2:space:r:g:b`, its colour space ignored and empty, as xterm reads it,
/// with anything after the blue ignored. `None` if the colour is out of
/// range or of another kind.
fn colour_of(rest: &[u16]) -> Option<Color> {
    match rest {
        [5, index, ..] => u8::try_from(*index).ok().map(Color::Idx),
        [2, r, g, b] | [2, _, r, g, b, ..] => rgb(*r, *g, *b),
        _ => None,
    }
}

fn rgb(r: u16, g: u16, b: u16) -> Option<Color> {
    Some(Color::Rgb(
        u8::try_from(r).ok()?,
        u8::try_from(g).ok()?,
        u8::try_from(b).ok()?,
    ))
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
            .is_some_and(|c| c.same(&Compact::continuation()))
}

impl Screen {
    pub(crate) fn new(size: Size, history: usize) -> Result<Self, Error> {
        let mut next_id = 1;
        Ok(Self {
            primary: Grid::new(size, history, &mut next_id, 1)?,
            // Its cells are made when a program first shows it.
            alternate: Grid::unmade(size, &mut next_id, 1)?,
            next_id,
            version: 1,
            structural: 1,
            alternate_active: false,
            attributes: Attributes::default(),
            pen_style: 0,
            blank_style: 0,
            pen_table: None,
            blank_table: None,
            saved_attributes: Attributes::default(),
            charsets: Charsets::default(),
            saved_charsets: Charsets::default(),
            modes: Modes::DEFAULT,
            saved_modes: Modes::default(),
            protect: 0,
            saved_protect: 0,
            protection: Protection::Off,
            tabs: TabStops::default(),
            frames_begun: 0,
            cursor_shape: 0,
            mouse: MouseProtocolMode::None,
            encoding: MouseProtocolEncoding::Default,
            primary_keyboard: KeyboardStack::default(),
            alternate_keyboard: KeyboardStack::default(),
            modify_other_keys: None,
            last_print: None,
            repeat: None,
            link: None,
            links_seen: false,
            next_link: 1,
            colours: None,
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
        self.pen_style = self.attributes.inline_style().unwrap_or(TABLED);
        self.blank_style = UNKNOWN;
        self.pen_table = None;
        self.blank_table = None;
    }
    /// The number `table`, of the table of the grid shown, if it is still
    /// that grid's and its epoch's.
    #[inline]
    fn still(&self, table: Option<(u32, bool, u64)>) -> Option<u32> {
        let (id, alternate, epoch) = table?;
        (alternate == self.alternate_active && epoch == self.grid().epoch()).then_some(id)
    }
    /// The number of the pen's style in the grid shown.
    #[inline]
    fn pen_style(&mut self) -> u32 {
        let style = self.pen_style;
        debug_assert!(style >= TABLED || Some(style) == self.attributes.inline_style());
        if style < TABLED {
            return style;
        }
        if let Some(id) = self.still(self.pen_table) {
            return id;
        }
        self.find_pen_style(false)
    }
    /// The number, in the grid shown, of the style of the pen's colours
    /// alone, which erasing, scrolling and inserting fill cells with (`bce`).
    #[inline]
    fn blank_style(&mut self) -> u32 {
        let style = self.blank_style;
        debug_assert!(style >= TABLED || Some(style) == self.attributes.erased().inline_style());
        if style < TABLED {
            return style;
        }
        if let Some(id) = self.still(self.blank_table) {
            return id;
        }
        self.find_pen_style(true)
    }
    /// `pen_style`, or `blank_style` if `blank`, when it is not known to
    /// be a number of its own: found, and kept if it is one.
    #[inline(never)]
    fn find_pen_style(&mut self, blank: bool) -> u32 {
        let (attributes, known) = if blank {
            (self.attributes.erased(), &mut self.blank_style)
        } else {
            (self.attributes, &mut self.pen_style)
        };
        if *known == UNKNOWN {
            *known = attributes.inline_style().unwrap_or(TABLED);
        }
        if *known < TABLED {
            return *known;
        }
        let id = self.grid_mut().table_style(attributes);
        // After the search, which may have swept the styles.
        let found = Some((id, self.alternate_active, self.grid().epoch()));
        if blank {
            self.blank_table = found;
        } else {
            self.pen_table = found;
        }
        id
    }
    pub(crate) fn begin(&mut self) -> Result<(), Error> {
        self.version = self
            .version
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        Ok(())
    }
    /// The screen's rows and columns.
    pub fn size(&self) -> Size {
        self.grid().size()
    }
    /// The cursor's row and column, always on the screen. A glyph printed
    /// in the last column (or at the right margin) leaves the cursor on
    /// it, with [`pending_wrap`](Self::pending_wrap) set, as xterm does.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.grid().cursor.at()
    }
    /// DEC STD 070's Last Column Flag: a glyph went into the last column,
    /// or the right margin's ([`left_right_margins`](Self::left_right_margins)),
    /// and the next one, with autowrap on, first moves to the start of the
    /// next line, wherever the cursor waits (a margin reset since leaves
    /// it where the margin was). Cursor movements, line feeds and edits
    /// end it; DECSC and SCOSC save it with the cursor.
    pub fn pending_wrap(&self) -> bool {
        self.grid().cursor.pending_wrap
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
            Kind::SaveCursor | Kind::Reset => false,
            Kind::Mouse(mouse) => self.mouse == mouse,
            Kind::Encoding(encoding) => self.encoding == encoding,
        }
    }
    /// The in-band resize report of the current size, pixels unknown.
    pub(crate) fn size_report(&self) -> Reply {
        let (rows, cols) = self.size().into();
        Reply::of(format_args!("\x1b[48;{rows};{cols};0;0t"))
    }
    pub(crate) fn frames_begun(&self) -> u64 {
        self.frames_begun
    }
    /// The cursor shape last set with DECSCUSR (`CSI Ps SP q`); 0, the
    /// default, is the terminal's own. State only: fux-vt draws no cursor.
    pub fn cursor_shape(&self) -> u16 {
        self.cursor_shape
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
        self.mouse
    }
    /// How the program asked for mouse reports to be encoded.
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.encoding
    }
    /// The kitty keyboard protocol flags in force: the top of the current
    /// screen's stack, 0 (legacy key reporting) when it is empty. Always 0
    /// without [`Feature::KittyKeyboard`].
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.keyboard().top()
    }
    /// The xterm modifyOtherKeys level set by `CSI > 4 ; Pv m`, `None` when
    /// it is off (`Pv` 0 or absent). Always `None` without
    /// [`Feature::KittyKeyboard`].
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }
    fn keyboard(&self) -> &KeyboardStack {
        if self.alternate_active {
            &self.alternate_keyboard
        } else {
            &self.primary_keyboard
        }
    }
    fn keyboard_mut(&mut self) -> &mut KeyboardStack {
        if self.alternate_active {
            &mut self.alternate_keyboard
        } else {
            &mut self.primary_keyboard
        }
    }
    /// The one-based cursor position a DSR 6n or DECXCPR reports. The line
    /// is counted as CUP addresses it: from the top margin with DECOM set
    /// (DEC STD 070, CPR and DECXCPR, pages 5-53 to 5-56; xterm's
    /// `CASE_CPR`), and a cursor above the margin, as DECRC can leave one,
    /// on the first line. A cursor waiting to wrap is reported one past
    /// the last column, as the vt100 crate did; with an identity it is
    /// reported at the last column, as xterm does.
    pub(crate) fn reported_cursor(&self, options: &Options) -> (u32, u32) {
        let g = self.grid();
        let col = if options.identity().is_some() {
            g.cursor.col()
        } else {
            g.next_column()
        };
        // In origin mode the column counts from the left margin, as xterm
        // reports it.
        let col = col.saturating_sub(g.addressed().1.first());
        (u32::from(g.cursor_line()) + 1, u32::from(col) + 1)
    }
    /// The pen: the colours and rendition of the next glyph printed.
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }
    /// The pen's background colour.
    pub fn bgcolor(&self) -> Color {
        self.attributes.background()
    }
    /// Whether the pen is inverse.
    pub fn inverse(&self) -> bool {
        self.attributes.inverse()
    }
    /// The cell at `row`, `col` of the screen.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
        self.grid().cell(row, col)
    }
    /// The hyperlink of the cell at `row`, `col` of the screen (see
    /// [`Row::link`]). Always `None` without [`Feature::Hyperlinks`].
    pub fn link(&self, row: u16, col: u16) -> Option<Hyperlink<'_>> {
        self.grid().live_row(row)?.link(usize::from(col))
    }
    /// The hyperlink the program has open (`OSC 8 ; params ; URI ST`), which
    /// glyphs printed now take: its URI and `id`.
    pub fn hyperlink(&self) -> Option<(&str, Option<&str>)> {
        self.link.as_ref().map(|pen| (&*pen.uri, pen.id.as_deref()))
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
    pub fn palette_color(&self, index: u8) -> Option<(u8, u8, u8)> {
        let [r, g, b] = self.colours.as_ref()?.palette(index)?;
        Some((r, g, b))
    }
    /// Sets the host's colour for palette entry `index` ([`crate::Parser::set_host_color`]).
    pub(crate) fn set_host_color(&mut self, index: u8, rgb: Option<(u8, u8, u8)>) -> bool {
        let colour = rgb.map(|(r, g, b)| [r, g, b]);
        if usize::from(index) >= crate::palette::HOST_ENTRIES {
            return false;
        }
        if colour.is_none() && self.colours.is_none() {
            return true;
        }
        self.colours.get_or_insert_default().set_host(index, colour)
    }
    /// The colour dynamic colour `number` shows if the program set it (OSC
    /// 10 to 19, with `Feature::Palette`): 10 the text foreground and 11
    /// the background, which a cell of `Color::Default` shows, 12 the
    /// cursor, and the others xterm's pointer, Tektronix and highlight
    /// colours. `None` while it is the terminal's own, and after OSC 110 to
    /// 119 reset it.
    pub fn dynamic_color(&self, number: u8) -> Option<(u8, u8, u8)> {
        let [r, g, b] = self.colours.as_ref()?.dynamic(number)?;
        Some((r, g, b))
    }
    /// Whether the program changed a palette entry or a dynamic colour, so
    /// that a host drawing the screen knows to ask `palette_color` and
    /// `dynamic_color`.
    pub fn colors_changed(&self) -> bool {
        self.colours.as_ref().is_some_and(|c| c.changed())
    }
    /// The colours the program set, for the parser's OSC handling.
    pub(crate) fn colours_mut(&mut self) -> &mut Option<Box<crate::palette::Colours>> {
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
            Some(Pen {
                uri: uri.into(),
                id: id.map(Into::into),
                key,
                held: [Held::Pending; 2],
            })
        });
        self.links_seen |= self.link.is_some();
    }

    /// Gives the cells `span` of row `row` of the grid shown the open link,
    /// if `open`, or none: what printing glyphs there does to their links,
    /// once a link has been opened (`links_seen`). Out of line, so that
    /// printing, which never needs it until then, carries none of it.
    #[inline(never)]
    fn link_cells(&mut self, row: u16, span: std::ops::Range<usize>, open: bool) {
        let link = if open { self.pen_link() } else { 0 };
        let version = self.version;
        self.grid_mut().set_link(row, span, link, version);
    }

    /// The number the open link has in the grid shown, held there the first
    /// time a glyph is printed with it; 0 if no link is open, or the grid has
    /// no room for it.
    fn pen_link(&mut self) -> u16 {
        let Some(pen) = &mut self.link else {
            return 0;
        };
        let (grid, held) = if self.alternate_active {
            (&mut self.alternate, &mut pen.held[1])
        } else {
            (&mut self.primary, &mut pen.held[0])
        };
        match *held {
            Held::At(n) => n,
            Held::Refused => 0,
            Held::Pending => {
                let n = grid.intern(&pen.uri, pen.id.as_ref(), pen.key, self.version);
                *held = n.map_or(Held::Refused, Held::At);
                n.unwrap_or(0)
            }
        }
    }

    /// OSC 133, semantic prompts (`references/modern/osc133_*`): `A`
    /// marks the row a prompt starts on, after a fresh line, and `L` is the
    /// fresh line alone: a new line unless the cursor is in the first
    /// column, as the semantic prompt proposal and Ghostty have it. The
    /// other commands (`B`, `C`, `D`, `P` and the rest) change nothing.
    pub(crate) fn prompt_osc(&mut self, payload: &[u8]) -> Result<(), Error> {
        let mut parts = payload.split(|b| *b == b';');
        let command = parts.next().unwrap_or_default();
        // P (explicit start of prompt) of the primary kind, `k=i` or none,
        // marks where it is without a fresh line: shells send it in A's
        // place (Ghostty's bash integration under ble.sh does). Right-side
        // and continuation prompts (`k=r`, `k=c`, `k=s`) start none.
        if command == b"P" {
            let kind = parts.find_map(|option| option.strip_prefix(b"k="));
            if kind.is_none_or(|k| k == b"i") {
                let row = self.grid().cursor.row();
                self.grid_mut().mark_prompt(row);
            }
            return Ok(());
        }
        // N is A that may first end the previous command, which fux-vt
        // keeps no record of.
        if !matches!(command, b"A" | b"N" | b"L") {
            return Ok(());
        }
        // CR, then IND, as Ghostty does it.
        if self.grid().cursor.col() != 0 {
            self.control(b'\r')?;
            self.linefeed()?;
        }
        if command != b"L" {
            let row = self.grid().cursor.row();
            self.grid_mut().mark_prompt(row);
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
        let grid = self.grid();
        Rows {
            grid,
            range: 0..grid.retained_len(),
        }
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
        // Ghostty, any resize ends synchronized output.
        self.modes.set(Mode::SynchronizedOutput, false);
        if self.size() == size {
            return Ok(());
        }
        self.last_print = None;
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let mut next = self.next_id;
        let mut primary = if reflow {
            self.primary.reflowed(size, &mut next, version)?
        } else {
            self.primary.resized(size, &mut next, version)?
        };
        let mut alternate = self.alternate.resized(size, &mut next, version)?;
        // The cells' links keep their numbers, so the links go along.
        primary.adopt_links(std::mem::take(&mut self.primary.links));
        alternate.adopt_links(std::mem::take(&mut self.alternate.links));
        self.primary = primary;
        self.alternate = alternate;
        self.next_id = next;
        self.version = version;
        self.structural = version;
        Ok(())
    }

    fn scroll(&mut self, region: Span, count: u16, up: bool, history: bool) -> Result<(), Error> {
        if self.grid().lr() {
            self.scroll_columns(region, count, up);
            return Ok(());
        }
        self.scroll_rows(region, count, up, history)
    }
    /// `scroll` without left and right margins: the rows move.
    fn scroll_rows(
        &mut self,
        region: Span,
        count: u16,
        up: bool,
        history: bool,
    ) -> Result<(), Error> {
        // A bounded multi-row scroll can fail after earlier rows have moved
        // (allocation/identity exhaustion). Even that partial result must
        // invalidate every reader's window, not just its newly blank rows.
        self.structural = self.version;
        // The rows brought in take the pen's colours (`bce`), as in xterm.
        let blank = self.blank_style();
        self.with_grid(|g, next, version| {
            let direction = if up {
                Scroll::Up { history }
            } else {
                Scroll::Down
            };
            g.scroll(region, count, direction, blank, next, version)
        })
    }
    /// A scroll inside left and right margins (`Grid::scroll_columns`),
    /// its blanks in the pen's colours. Out of line: without margins it is
    /// never reached.
    #[inline(never)]
    fn scroll_columns(&mut self, region: Span, count: u16, up: bool) {
        let blank = self.blank_style();
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
                    self.scroll_columns(g.lines, 1, true);
                }
                return Ok(());
            }
            self.scroll_rows(g.lines, 1, true, true)?;
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
                self.scroll(g.lines, 1, false, false)?;
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
    /// last column it fits in with autowrap off.
    ///
    /// The end of the cursor's line after it (`Grid::line_end`). Whether
    /// the glyph fits is asked inline, on every glyph's way; the wrap is
    /// out of line.
    #[inline]
    fn wrap_for(&mut self, width: u16) -> Result<u16, Error> {
        let g = self.grid();
        let end = g.line_end();
        // The last column a glyph this wide can start in, before the right
        // margin while the cursor is not past it; a wider glyph is never
        // printed.
        let Some(room) = end.checked_sub(width) else {
            return Ok(end);
        };
        // A pending wrap is carried out wherever it waits: at the line's
        // end, or where the right margin was before DECLRMM was reset (DEC
        // STD 070's Last Column Flag is no column; xterm and Ghostty wrap
        // there too).
        if !g.cursor.pending_wrap && g.cursor.col() <= room {
            return Ok(end);
        }
        self.wrap(end, room)
    }
    /// `wrap_for` where the glyph does not fit: the cursor goes to the
    /// next line, or back to `room`, the last column the glyph fits in,
    /// on a line ending at `end`.
    #[inline(never)]
    fn wrap(&mut self, end: u16, room: u16) -> Result<u16, Error> {
        let g = self.grid();
        let wrap = self.mode(Mode::Autowrap);
        if !wrap {
            // Back to the last column the glyph fits in; a wrap left
            // pending where a right margin was overwrites there, as xterm
            // does.
            let g = self.grid_mut();
            g.set_col(g.cursor.col().min(room));
            g.cursor.pending_wrap = false;
            return Ok(end);
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
        if self.grid().lr() {
            self.new_line_in_margins()?;
            return Ok(self.grid().line_end());
        }
        self.grid_mut().set_col(0);
        self.linefeed()?;
        Ok(end)
    }

    pub(crate) fn print(&mut self, raw: char) -> Result<(), Error> {
        let c = if self.charsets.graphics() {
            special_graphics(raw)
        } else {
            raw
        };
        if ('\u{80}'..'\u{a0}').contains(&c) {
            return Ok(());
        }
        let width = c.width();
        if width.is_none() && u32::from(c) < 256 {
            return Ok(());
        }
        // Too wide for the grid, whatever the number, so not printed.
        let Ok(width) = u16::try_from(width.unwrap_or(1)) else {
            return Ok(());
        };
        if width > self.grid().size().cols() {
            return Ok(());
        }
        if self.extend_cluster(c) {
            return Ok(());
        }
        if width == 0 {
            let g = self.grid();
            let (row, col) = (g.cursor.row(), g.next_column());
            let above = row.checked_sub(1);
            let previous = if let Some(left) = col.checked_sub(1) {
                Some((row, left))
            } else if let Some(above) = above
                && g.live_wrapped(above)
            {
                Some((above, g.size().columns().last()))
            } else {
                None
            };
            if let Some((row, mut col)) = previous {
                if g.stored(row, col)
                    .is_some_and(Compact::is_wide_continuation)
                {
                    col = col.saturating_sub(1);
                }
                // A blank cell takes a space for the mark to follow, and
                // with it the open link, as a glyph printed there would.
                let blank = g.stored(row, col).is_some_and(|c| !c.has_contents());
                let end = col.saturating_add(1);
                // The mark joins the cell's cluster; a cell already holding
                // all it can takes no more (`append`).
                self.with_grid(|g, _, v| {
                    g.mutate_line(row, v, end, |line| line.append(usize::from(col), c))
                });
                // A mark dropped leaves the cell blank, its link unread.
                if blank && self.links_seen {
                    let at = usize::from(col);
                    self.link_cells(row, at..at.saturating_add(1), true);
                }
            }
            return Ok(());
        }
        // What REP repeats: the last glyph printed, not a mark.
        self.repeat = Some(raw);
        let end = self.wrap_for(width)?;
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
                if current.same(&glyph) && whole(cells, i, width) {
                    return false;
                }
                // An overwrite at either half removes the other half too.
                if cells.get(i).is_some_and(Compact::is_wide_continuation)
                    && let Some(other) = i.checked_sub(1).and_then(|j| cells.get_mut(j))
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
        Ok(())
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
            if c.width() != Some(0) {
                return None;
            }
            let mut left = col.checked_sub(1)?;
            if g.stored(row, left)?.is_wide_continuation() {
                left = left.checked_sub(1)?;
            }
            // Its cluster's state, from its text.
            let cluster = Cluster::of(g.cell(row, left)?.contents());
            Some(Printed {
                at: (row, left),
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
        // Within the right margin, while the cursor is not past it.
        let fits = col < g.line_end();
        let mut widened = false;
        let mut kept = !full;
        self.with_grid(|g, _, v| {
            g.mutate_line(row, v, col.saturating_add(1), |line| {
                kept = kept && line.append(at, c);
                if !kept {
                    return false;
                }
                // Width is kept once wide; the last column has no room to widen.
                if narrow && fits && line.text(at).width() >= 2 {
                    if let Some(cell) = line.cells.get_mut(at) {
                        cell.widen();
                    }
                    // The cell under the cursor becomes the second half; if
                    // it led a wide glyph, that glyph's half is left blank.
                    if line.cells.get(cursor).is_some_and(Compact::is_wide)
                        && let Some(orphan) =
                            cursor.checked_add(1).and_then(|i| line.cells.get_mut(i))
                    {
                        *orphan = Compact::default();
                    }
                    if let Some(cell) = line.cells.get_mut(cursor) {
                        *cell = Compact::continuation();
                    }
                    widened = true;
                }
                true
            });
            if widened {
                g.advance_to(col.saturating_add(1));
            }
        });
        self.last_print = Some(Printed {
            at: (anchor_row, anchor_col),
            cluster,
            full: !kept,
        });
        true
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
        let width = c.width().unwrap_or(1).max(1);
        // Copies to a row, between the margins once the copies wrap: a wide
        // glyph leaves an odd last column blank.
        let line = g.columns().len();
        let Some(per_row) = usize::from(line).checked_div(width).filter(|n| *n > 0) else {
            return Ok(());
        };
        let lines = usize::from(g.size().rows())
            .saturating_add(g.history_limit)
            .saturating_add(1);
        let full = lines.saturating_mul(per_row);
        let mut count = usize::from(count);
        if let Some(beyond) = count.checked_sub(full) {
            count = full.saturating_add(beyond.checked_rem(per_row).unwrap_or(0));
        }
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
        while let Some((&first, tail)) = bytes.split_first() {
            let line = self.wrap_for(1)?;
            let (row, col) = self.grid().cursor.at();
            // `wrap_for` left room for at least one cell, before the right
            // margin or the screen's edge; without it, the run could not
            // advance.
            let Some(room) = line.checked_sub(col).filter(|r| *r > 0) else {
                return Ok(());
            };
            // A run too long for a u16 still stops at the margin.
            let count = u16::try_from(bytes.len()).map_or(room, |n| n.min(room));
            let Some(end) = col.checked_add(count) else {
                return Ok(());
            };
            let style = self.pen_style() | self.protect;
            let run = bytes.get(..usize::from(count)).unwrap_or_default();
            // Written whole unless the run meets half of a wide glyph,
            // which the general path repairs.
            if !self.with_grid(|g, _, v| g.write_ascii(row, col, run, style, v)) {
                self.print(char::from(first))?;
                bytes = tail;
                continue;
            }
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
            bytes = bytes.get(usize::from(count)..).unwrap_or_default();
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
        // D.6.1). HT does not: it leaves a cursor in the last column where
        // it is, still waiting to wrap, as xterm does.
        if matches!(byte, 8 | 13) {
            g.cursor.pending_wrap = false;
        }
        match byte {
            // Back a column, stopping at the left margin unless the cursor
            // is already left of it (xterm's `CursorBack`).
            8 => g.back(),
            9 => self.tab(1, true),
            // LNM: a new line, the carriage returned too.
            10..=12 if new_line => self.new_line()?,
            10..=12 => self.linefeed()?,
            13 => g.carriage_return(),
            // SO puts G1 in GL, SI G0.
            14 => self.charsets.shifted = true,
            15 => self.charsets.shifted = false,
            _ => {}
        }
        Ok(())
    }
    /// Moves the cursor `count` tab stops forward, stopping at the last
    /// column, or back, stopping at the first.
    /// With DECLRMM set, forward stops at the right margin, wherever the
    /// cursor is, as xterm's `TabToNextStop` has it (DEC STD 070's HT goes
    /// on to the last column from right of the margin, or outside the
    /// scrolling region: see the README's departures); back, in origin
    /// mode, at the left margin (`TabToPrevStop`). Without margins the
    /// right margin is the last column.
    fn tab(&mut self, count: u16, forward: bool) {
        let g = self.grid();
        let (mut col, last) = (g.cursor.col(), g.columns().last());
        let first = g.addressed().1.first();
        for _ in 0..count {
            col = if forward {
                self.tabs.next(col, last)
            } else {
                self.tabs.previous(col).max(first)
            };
        }
        self.grid_mut().set_col(col);
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

    /// DECSC: the cursor, with its pending wrap (DEC STD 070, Appendix
    /// D.6.1), origin mode, the drawing attributes and whether glyphs are
    /// protected (DECSCA's selective erase attribute, as the VT420 manual
    /// lists it and xterm saves it).
    fn save(&mut self) {
        let g = self.grid_mut();
        g.saved = g.cursor;
        self.saved_attributes = self.attributes;
        self.saved_charsets = self.charsets;
        self.saved_protect = self.protect;
    }
    /// DECRC. In origin mode the cursor comes back no further right than
    /// the right margin, as xterm's `CursorRestore` places it.
    fn restore(&mut self) {
        let g = self.grid_mut();
        g.cursor = g.saved;
        g.set_col(g.cursor.col().min(g.addressed().1.last()));
        self.attributes = self.saved_attributes;
        self.pen_changed();
        self.charsets = self.saved_charsets;
        self.protect = self.saved_protect;
    }
    /// Carries out an escape sequence; whether fux-vt implements it.
    pub(crate) fn escape(&mut self, intermediates: &[u8], byte: u8) -> Result<bool, Error> {
        // SCS: ESC ( F designates G0 and ESC ) F G1; F `0` is DEC Special
        // Graphics, and any other set is ASCII here.
        match intermediates {
            b"(" => self.charsets.g0_graphics = byte == b'0',
            b")" => self.charsets.g1_graphics = byte == b'0',
            b"#" if byte == b'8' => {
                self.alignment();
                return Ok(true);
            }
            [] => {}
            _ => return Ok(false),
        }
        if !intermediates.is_empty() {
            return Ok(true);
        }
        if matches!(byte, b'7' | b'8' | b'D' | b'E' | b'M' | b'c') {
            self.break_cluster();
        }
        match byte {
            b'7' => self.save(),
            b'8' => self.restore(),
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
            // between them are protected, and every erase leaves them (ISO
            // protection, xterm's `CASE_SPA`); EPA keeps the kind of
            // protection, as xterm does.
            b'V' => {
                self.protection = Protection::Iso;
                self.protect = PROTECTED;
            }
            b'W' => self.protect = 0,
            // HTS (ECMA-48 8.3.62): a tab stop at the cursor's column.
            b'H' => {
                let col = self.grid().cursor.col();
                self.tabs.set(col, true);
            }
            b'M' => self.reverse_index()?,
            b'c' => {
                let size = self.size();
                let mut next = self.next_id;
                // Both grids start again: in the storage they have, if both
                // hold only their live rows at this size, else afresh. Either
                // way nothing changes unless both can.
                let recycle = [&self.primary, &self.alternate]
                    .iter()
                    .all(|g| g.recyclable() && g.size() == size);
                if recycle {
                    let needed = u64::from(size.rows()).saturating_mul(2);
                    next.checked_add(needed).ok_or(Error::IdentityExhausted)?;
                    self.primary.clear(&mut next, self.version)?;
                    self.alternate.clear(&mut next, self.version)?;
                } else {
                    let history = self.primary.history_limit;
                    let primary = Grid::new(size, history, &mut next, self.version)?;
                    let alternate = Grid::unmade(size, &mut next, self.version)?;
                    self.primary = primary;
                    self.alternate = alternate;
                }
                self.next_id = next;
                // No cell has a link now, and none is open.
                self.link = None;
                self.links_seen = false;
                self.primary.reset_links();
                self.alternate.reset_links();
                self.alternate_active = false;
                self.attributes = Attributes::default();
                self.pen_changed();
                self.saved_attributes = Attributes::default();
                self.charsets = Charsets::default();
                self.saved_charsets = Charsets::default();
                self.modes = Modes::DEFAULT;
                self.protect = 0;
                self.saved_protect = 0;
                self.protection = Protection::Off;
                self.tabs = TabStops::default();
                self.cursor_shape = 0;
                self.mouse = MouseProtocolMode::None;
                self.encoding = MouseProtocolEncoding::Default;
                self.primary_keyboard = KeyboardStack::default();
                self.alternate_keyboard = KeyboardStack::default();
                self.modify_other_keys = None;
                // The palette, as xterm resets it; the dynamic and special
                // colours stay, as in xterm.
                if let Some(colours) = &mut self.colours {
                    colours.reset_palette();
                }
                self.structural = self.version;
            }
            // ST (ECMA-48 8.3.143) is done too: it ended the string before
            // it, which the parser has dispatched, or ends nothing.
            _ => return Ok(byte == b'\\'),
        }
        Ok(true)
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
        let (col, lines) = (g.cursor.col(), g.lines);
        let blank = self.blank_style();
        self.with_grid(|g, _, version| {
            for y in lines.first()..=lines.last() {
                g.edit_row(y, col, count, insert, blank, version);
            }
        });
    }

    /// DECSLRM (`CSI Pl ; Pr s`, DEC STD 070, 5-27), while DECLRMM is set:
    /// margins with the left one left of the right are set, and the cursor
    /// goes home, obeying DECOM; others are ignored. A right margin past
    /// the screen is its last column, as xterm reads it, as it reads
    /// DECSTBM (where DEC STD 070 ignores it). Out of line, as `csi` is on
    /// every CSI's way.
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

    /// ECH under ISO protection: the columns `start` to `end` of row `row`
    /// erased but for their protected glyphs. Out of line, as protection is
    /// rare and `csi` is on every CSI's way.
    #[inline(never)]
    fn erase_kept(&mut self, row: u16, start: u16, end: u16) {
        let blank = self.blank_style();
        self.with_grid(|g, _, v| g.erase_unprotected(row, start, end, blank, v));
    }

    /// ED, EL, DECSED or DECSEL while glyphs may be protected: DECSED and
    /// DECSEL (`private`) leave them with any protection, ED and EL with
    /// ISO's alone (xterm's `do_erase_display`). Whether it erased: ED and
    /// EL under DEC protection erase every cell, as `csi` does without. Out
    /// of line: inlined, the tmux recordings count up to 0.35% more
    /// instructions (fux-bench).
    #[inline(never)]
    fn erase_protected(&mut self, private: bool, display: bool, mode: u16) -> bool {
        if private || self.protection == Protection::Iso {
            self.selective_erase(display, mode);
            return true;
        }
        false
    }

    /// ED or EL (`display`) in `mode` leaving protected glyphs: DECSED and
    /// DECSEL with any protection, ED and EL with ISO's (xterm's
    /// `do_erase_display` and `do_erase_line`). The rest is as for ED and
    /// EL. As in xterm, an ED of the whole screen (2, or 0 from the first
    /// cell, or 1 from the last) that finds no protected glyph ends the
    /// protection: until the next DECSCA or SPA, erases leave nothing. Out
    /// of line, as protection is rare and `csi` is on every CSI's way.
    #[inline(never)]
    fn selective_erase(&mut self, display: bool, mode: u16) {
        let blank = self.blank_style();
        let version = self.version;
        let g = self.grid_mut();
        let ((row, col), size) = (g.cursor.at(), g.size());
        let found = g.erase_in(display, mode, |g, y, start, end| {
            g.erase_unprotected(y, start, end, blank, version)
        });
        let whole = mode == 2
            || (mode == 0 && (row, col) == (0, 0))
            || (mode == 1 && (row, col) == (size.lines().last(), size.columns().last()));
        if display && whole && !found {
            self.protection = Protection::Off;
        }
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
            g.clear_prompt(y);
        }
    }

    /// DECSTR (`CSI ! p`), as xterm does it: the modes a program sets go
    /// back to their defaults, as the VT520 manual's table (p. 5-150) and
    /// DEC STD 070's Soft Terminal Reset (p. 4-37) list them: the cursor
    /// shown, DECOM, DECCKM and DECKPAM off, the scroll region the whole
    /// screen, the pen and the saved cursor's attributes normal, the
    /// character sets ASCII with G0 in GL, and the saved cursor home
    /// (`Modes::SOFT_RESET` has the modes). DECAWM goes back to its
    /// default, which both leave to the terminal's setting (xterm's: on).
    /// The screen, the
    /// cursor, a pending wrap, the alternate screen, bracketed paste, focus
    /// reporting, mouse modes and kitty keyboard flags stay as they are,
    /// as in xterm.
    fn soft_reset(&mut self) {
        self.modes.reset(Modes::SOFT_RESET);
        self.attributes = Attributes::default();
        self.pen_changed();
        self.saved_attributes = Attributes::default();
        self.charsets = Charsets::default();
        self.saved_charsets = Charsets::default();
        // The left and right margins, and the selective erase attribute,
        // both in the VT520 manual's table, and what erasing leaves, as
        // xterm resets it.
        self.protect = 0;
        self.saved_protect = 0;
        self.protection = Protection::Off;
        for g in [&mut self.primary, &mut self.alternate] {
            g.cursor.origin = false;
            g.reset_margins();
        }
        self.grid_mut().saved = Cursor::default();
        // The palette, as xterm 411 resets it (in neither table); the
        // dynamic and special colours stay.
        if let Some(colours) = &mut self.colours {
            colours.reset_palette();
        }
    }

    /// Clears the alternate screen, its blanks in the pen's colours
    /// (`bce`), as xterm clears it for 1047 and 1049. The cursor, origin
    /// mode, the margins and the screen's saved cursor stay, and a pending
    /// wrap ends, as ED's does (DEC STD 070, Appendix D.6.1).
    fn clear_alternate(&mut self) -> Result<(), Error> {
        let g = &self.alternate;
        let (cursor, saved, lines, columns) = (g.cursor, g.saved, g.lines, g.columns());
        self.alternate.clear(&mut self.next_id, self.version)?;
        let blank = self.attributes.erased();
        let blank = match blank.inline_style() {
            Some(style) => style,
            None => self.alternate.style(blank),
        };
        let g = &mut self.alternate;
        (g.cursor, g.saved, g.lines) = (cursor, saved, lines);
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
        let (lines, columns) = self.grid().addressed();
        let at = |span: Span, index: usize, default: u16| match p.first(index, 0) {
            0 => default,
            n => span.nth(n.saturating_sub(1)),
        };
        let (top, left) = (at(lines, 2, lines.first()), at(columns, 3, columns.first()));
        let (bottom, right) = (at(lines, 4, lines.last()), at(columns, 5, columns.last()));
        let g = self.grid();
        let mut sum = 0u16;
        for y in top..=bottom {
            for x in left..=right {
                if let Some(cell) = g.cell(y, x) {
                    sum = sum.wrapping_add(checksum_of(&cell));
                }
            }
        }
        let id = p.first(0, 0);
        let checksum = sum.wrapping_neg();
        Reply::of(format_args!("\x1bP{id}!~{checksum:04X}\x1b\\"))
    }

    /// The primary device attributes (DA1, `CSI c`; and DECID, `ESC Z`):
    /// a VT220 with ANSI colour with an identity, else a VT100 with
    /// advanced video, as the vt100 crate answered.
    pub(crate) fn primary_attributes(options: &Options) -> Reply {
        if options.identity().is_some() {
            Reply::of(format_args!("\x1b[?62;22c"))
        } else {
            Reply::of(format_args!("\x1b[?1;2c"))
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
                None if mode.savable() => self.saved_modes.contains(mode),
                None => continue,
            };
            // A mode that is a bit and no more is set here, without a call.
            if mode.kind() == Kind::Flag {
                self.modes.set(mode, on);
            } else {
                report |= self.set_mode(mode, on)?;
            }
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
                && let Some(mode) = Mode::of(*n, true, options).filter(|m| m.savable())
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
            Kind::Flag => self.modes.set(mode, on),
            // DECLRMM: reset, the margins go back to the screen's edges (DEC
            // STD 070, DECLRMM; xterm's `set_left_right_margin_mode`).
            Kind::Margins => {
                self.modes.set(mode, on);
                if !on {
                    for g in [&mut self.primary, &mut self.alternate] {
                        g.set_columns(g.size().columns());
                    }
                }
            }
            Kind::Frame => {
                self.modes.set(mode, on);
                if on {
                    self.frames_begun = self.frames_begun.wrapping_add(1);
                }
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
                    Switch::ClearedOnLeaving => {
                        if !on && self.alternate_active {
                            self.clear_alternate()?;
                        }
                        self.switch_screen(on);
                    }
                    Switch::SavingCursor if on => {
                        self.save();
                        self.switch_screen(true);
                        self.clear_alternate()?;
                    }
                    Switch::SavingCursor => {
                        self.switch_screen(false);
                        self.restore();
                    }
                }
            }
            Kind::SaveCursor if on => self.save(),
            Kind::SaveCursor => self.restore(),
            Kind::Reset => {}
            Kind::Mouse(mouse) => latest(&mut self.mouse, mouse, on),
            Kind::Encoding(encoding) => latest(&mut self.encoding, encoding, on),
        }
        Ok(false)
    }

    /// DECRQSS (`DCS $ q Pt ST`): the setting `request` names, written to
    /// `out` as the control function that sets it, in the form xterm
    /// answers (`do_dcs` and `xtermFormatSGR`, xterm 411 misc.c). `Some(true)`
    /// for a setting written, `Some(false)` for a request fux-vt does not
    /// know, which xterm answers as invalid; `None` for a cursor shape
    /// left to the terminal (DECSCUSR 0 or never set), which fux-vt does
    /// not know: xterm and Ghostty always know theirs, and vim, the program
    /// that asks, takes an invalid answer for keys.
    pub(crate) fn setting_report(&self, request: &[u8], out: &mut String) -> Option<bool> {
        use std::fmt::Write;
        match request {
            b"m" => {
                self.sgr_report(out);
                out.push('m');
            }
            b" q" => {
                let shape = self.cursor_shape;
                if !(1..=6).contains(&shape) {
                    return None;
                }
                let _ = write!(out, "{shape} q");
            }
            b"r" => {
                let (top, bottom) = self.scroll_region();
                let (top, bottom) = (u32::from(top), u32::from(bottom));
                let _ = write!(
                    out,
                    "{};{}r",
                    top.saturating_add(1),
                    bottom.saturating_add(1)
                );
            }
            _ => return Some(false),
        }
        Some(true)
    }

    /// The pen as xterm reports it to DECRQSS (`xtermFormatSGR`): 0, then
    /// bold, underline, blink, inverse, hidden, faint, italic, strikeout and
    /// double underline, in that order, then the foreground and background,
    /// 16 colours in their short forms and the rest as `38:5:n` or
    /// `38:2::r:g:b`. What xterm does not have follows its forms: rapid
    /// blink is 6 in blink's place, an underline style `4:n` in the
    /// underline's, and the underline colour `58:5:n` or `58:2::r:g:b`
    /// last. So a curly underline alone is `0;4:3`, which is what neovim
    /// looks for to learn the terminal draws styles.
    fn sgr_report(&self, out: &mut String) {
        use std::fmt::Write;
        let a = self.attributes;
        out.push('0');
        if a.bold() {
            out.push_str(";1");
        }
        match a.underline_style() {
            UnderlineStyle::None | UnderlineStyle::Double => {}
            UnderlineStyle::Single => out.push_str(";4"),
            style @ (UnderlineStyle::Curly | UnderlineStyle::Dotted | UnderlineStyle::Dashed) => {
                let _ = write!(out, ";4:{}", style.number());
            }
        }
        match a.blink() {
            Blink::Slow => out.push_str(";5"),
            Blink::Rapid => out.push_str(";6"),
            Blink::None => {}
        }
        for (on, code) in [
            (a.inverse(), ";7"),
            (a.hidden(), ";8"),
            (a.dim(), ";2"),
            (a.italic(), ";3"),
            (a.strikeout(), ";9"),
            (a.underline_style() == UnderlineStyle::Double, ";21"),
        ] {
            if on {
                out.push_str(code);
            }
        }
        let colour = |out: &mut String, colour: Color, short: Option<(u8, u8)>, long: u8| {
            let _ = match (colour, short) {
                // 30 to 37 and 90 to 97, 40 to 47 and 100 to 107: no sum
                // comes near 255.
                (Color::Idx(n @ 0..=7), Some((base, _))) => {
                    write!(out, ";{}", base.saturating_add(n))
                }
                (Color::Idx(n @ 8..=15), Some((_, bright))) => {
                    write!(out, ";{}", bright.saturating_add(n.saturating_sub(8)))
                }
                (Color::Idx(n), _) => write!(out, ";{long}:5:{n}"),
                (Color::Rgb(r, g, b), _) => write!(out, ";{long}:2::{r}:{g}:{b}"),
                (Color::Default, _) => Ok(()),
            };
        };
        colour(out, a.foreground(), Some((30, 90)), 38);
        colour(out, a.background(), Some((40, 100)), 48);
        colour(out, a.underline_color(), None, 58);
    }

    /// The kitty keyboard protocol and modifyOtherKeys sequences, with
    /// [`Feature::KittyKeyboard`]; `None` for any other sequence.
    fn keyboard_protocol(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
    ) -> Option<Dispatch> {
        // Flags are a bit set below 32, but kitty tolerates larger values:
        // they saturate rather than being refused.
        let flags = |n: u16| u8::try_from(n).unwrap_or(u8::MAX);
        match (intermediates, byte) {
            (b">", b'u') => self.keyboard_mut().push(flags(p.first(0, 0))),
            (b"<", b'u') => self.keyboard_mut().pop(p.first(0, 1)),
            (b"=", b'u') => {
                let (set, mode) = (flags(p.first(0, 0)), p.first(1, 1));
                self.keyboard_mut().set(set, mode);
            }
            (b"?", b'u') => {
                let flags = self.kitty_keyboard_flags();
                return Some(Dispatch::Reply(Reply::of(format_args!("\x1b[?{flags}u"))));
            }
            // Only resource 4 is modifyOtherKeys; other resources are unhandled.
            (b">", b'm') if p.groups().next() == Some(&[4][..]) => {
                self.modify_other_keys = match p.first(1, 0) {
                    0 => None,
                    level => Some(flags(level)),
                };
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
            self.cursor_shape = p.first(0, 0);
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
        // DL and DECSTBM below, once they are carried out. SU and SD are not
        // among them: the cursor stays waiting to wrap, as in xterm.
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
            // and the cursor goes home, obeying DECOM; others are ignored.
            // A right margin past the screen is its last column, as xterm
            // reads it, as it reads DECSTBM (where DEC STD 070 ignores it).
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
            // Under ISO protection (SPA) the protected glyphs stay.
            b'X' => {
                let a = self.blank_style();
                let end = col.saturating_add(n);
                if self.protection == Protection::Iso {
                    self.erase_kept(row, col, end);
                } else {
                    self.with_grid(|g, _, v| g.erase(row, col, end, a, v));
                }
            }
            b'J' | b'K' => {
                let mode = p.first(0, 0);
                if mode > 2 {
                    return Ok(Dispatch::Unhandled);
                }
                // DECSED and DECSEL (`CSI ? Ps J`, `CSI ? Ps K`) leave
                // protected glyphs; ED and EL only ISO's (SPA), as xterm's
                // `do_erase_display` has it.
                if self.protection != Protection::Off
                    && self.erase_protected(private, byte == b'J', mode)
                {
                    return Ok(Dispatch::Done);
                }
                let a = self.blank_style();
                self.with_grid(|g, _, v| {
                    g.erase_in(byte == b'J', mode, |g, y, start, end| {
                        g.erase(y, start, end, a, v);
                        false
                    })
                });
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
                    self.scroll(region, n, byte == b'M', false)?;
                }
            }
            b'S' | b'T' => {
                let lines = self.grid().lines;
                self.scroll(lines, n, byte == b'S', true)?;
            }
            // DECSTBM (DEC STD 070, 5-25): margins with the top above the
            // bottom are set, and the cursor goes home, obeying DECOM;
            // others are ignored. A bottom past the screen is the last
            // line, as xterm reads it, where DEC STD 070 ignores it.
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
            // CHT (ECMA-48 8.3.10): HT n times. Like HT, it leaves a
            // pending wrap waiting in the last column.
            b'I' => self.tab(n, true),
            // CBT (ECMA-48 8.3.7): back n tab stops, or to the first
            // column. With a wrap pending it does nothing, as in xterm.
            b'Z' => {
                if !self.grid().cursor.pending_wrap {
                    self.tab(n, false);
                }
            }
            // TBC (ECMA-48 8.3.154): 0 clears the stop at the cursor, 3
            // every stop; xterm ignores the others, and so does fux-vt.
            b'g' => match p.first(0, 0) {
                0 => {
                    let col = self.grid().cursor.col();
                    self.tabs.set(col, false);
                }
                3 => self.tabs.clear(),
                _ => {}
            },
            b'n' => match p.first(0, 0) {
                5 => return Ok(Dispatch::Reply(Reply::of(format_args!("\x1b[0n")))),
                6 => {
                    let (row, col) = self.reported_cursor(options);
                    let reply = Reply::of(format_args!("\x1b[{row};{col}R"));
                    return Ok(Dispatch::Reply(reply));
                }
                _ => return Ok(Dispatch::Unhandled),
            },
            b'c' if p.first(0, 0) == 0 => {
                return Ok(Dispatch::Reply(Self::primary_attributes(options)));
            }
            _ => return Ok(Dispatch::Unhandled),
        }
        Ok(Dispatch::Done)
    }

    /// The CSI sequences with an intermediate the screen carries out
    /// (DECSCUSR and DECSTR aside): DECSCA, DECIC and DECDC. `csi` leaves
    /// them unhandled, and the parser asks here then (`csi_dispatch`):
    /// carried out in `csi`, even out of line, they cost every CSI a
    /// percent or two of its instructions.
    #[inline(never)]
    pub(crate) fn intermediate_csi(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
    ) -> Dispatch {
        match (intermediates, byte) {
            // DECSCA (`CSI Ps " q`; DEC STD 070, 5.11.1.2): 1 protects the
            // glyphs printed next from DECSED and DECSEL, 0 and 2 end it,
            // as xterm reads it, and any of them makes the protection
            // DEC's.
            (b"\"", b'q') => {
                self.protection = Protection::Dec;
                match p.first(0, 0) {
                    0 | 2 => self.protect = 0,
                    1 => self.protect = PROTECTED,
                    _ => {}
                }
            }
            // DECIC and DECDC (`CSI Pn ' }`, `CSI Pn ' ~`; DEC STD 070,
            // 5.4.3): columns inserted or deleted at the cursor's in every
            // line of the scrolling region.
            (b"'", b'}' | b'~') => {
                self.break_cluster();
                self.edit_columns(p.first(0, 1), byte == b'}');
            }
            _ => return Dispatch::Unhandled,
        }
        Dispatch::Done
    }

    fn sgr(&mut self, p: &Parameters) {
        let mut groups = p.groups().peekable();
        while let Some(group) = groups.next() {
            match group {
                [0] => self.attributes = Attributes::default(),
                // Bold and dim are kept apart, and both can be on, as in
                // xterm; 22 ends both (ECMA-48 8.3.117).
                [1] => self.attributes.flags |= Attributes::BOLD,
                [2] => self.attributes.flags |= Attributes::DIM,
                [3] => self.attributes.flags |= Attributes::ITALIC,
                [4] => self.underline(UnderlineStyle::Single),
                // Doubly underlined (ECMA-48 8.3.117, xterm's ctlseqs).
                [21] => self.underline(UnderlineStyle::Double),
                // Slow and rapid blink replace one another.
                [5] => self.attributes = self.attributes.with_blink(Blink::Slow),
                [6] => self.attributes = self.attributes.with_blink(Blink::Rapid),
                [7] => self.attributes.flags |= Attributes::INVERSE,
                [8] => self.attributes.flags |= Attributes::HIDDEN,
                [9] => self.attributes.flags |= Attributes::STRIKEOUT,
                [22] => self.attributes.flags &= !(Attributes::BOLD | Attributes::DIM),
                [23] => self.attributes.flags &= !Attributes::ITALIC,
                [24] => self.underline(UnderlineStyle::None),
                [25] => self.attributes.flags &= !Attributes::BLINK,
                [27] => self.attributes.flags &= !Attributes::INVERSE,
                [28] => self.attributes.flags &= !Attributes::HIDDEN,
                [29] => self.attributes.flags &= !Attributes::STRIKEOUT,
                [39] => self.attributes = self.attributes.with_foreground(Color::Default),
                [49] => self.attributes = self.attributes.with_background(Color::Default),
                [59] => self.attributes = self.attributes.with_underline_color(Color::Default),
                [n @ (30..=37 | 90..=97)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes = self.attributes.with_foreground(color);
                    }
                }
                [n @ (40..=47 | 100..=107)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes = self.attributes.with_background(color);
                    }
                }
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
        let mut values = [0u16; 3];
        for value in values.iter_mut().take(count) {
            let Some([n]) = groups.peek() else {
                break;
            };
            *value = *n;
            groups.next();
        }
        let [a, b, c] = values;
        if kind == 5 {
            u8::try_from(a).ok().map(Color::Idx)
        } else {
            rgb(a, b, c)
        }
    }
}
