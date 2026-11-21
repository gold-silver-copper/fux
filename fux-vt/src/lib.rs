//! The terminal emulator: bytes in, a grid of cells out. It answers the
//! queries programs wait on, keeps the lines that scroll off the top, and
//! takes sixel pictures whole. It knows nothing of files or processes.
#![forbid(unsafe_code)]

pub mod sixel;
#[cfg(test)]
mod tests;
pub mod width;

use crate::width::{Joiner, plain, spread, width};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The most bytes one picture may have; a longer one is dropped and noted.
pub const PICTURE_MAX: usize = 4 << 20;
/// The most pictures a screen keeps; a new one past it drops the oldest.
const PICTURES_MAX: usize = 8;
/// The most columns and the most rows a screen may have.
pub const SIZE_MAX: usize = 500;
/// The most history kept between two looks by the holder; lines past it are dropped and noted.
const HISTORY_PENDING_MAX: usize = 4 << 20;
const HISTORY_FULL: &str = "lines were not kept in history: 4 MiB of it were waiting to be written";
/// (Room for an answer to every question about a colour that a viewer asks at its start.)
const REPLY_MAX: usize = 16 << 10;
const DROPPED: &str = "an answer to the program was dropped: 16 KiB of answers were waiting";
/// The keyboard flags fux passes on (all of the kitty protocol's: escape codes told apart,
/// presses and releases, alternate keys, every key as an escape code, the text a key gives),
/// and how many sets of flags a program can push aside on a screen.
pub const KEYS: u8 = 31;
const KEYS_KEPT: usize = 16;
/// What a program is told when it asks for a capability by its terminfo name (XTGETTCAP): what
/// this emulator does, and no more. A name that is not here is answered "not valid", each by
/// itself. (The README has the same table; a test holds the two together.)
const CAPS: &[(&str, &str)] = &[
    ("TN", "xterm-256color"),
    ("name", "xterm-256color"),
    ("Co", "256"),
    ("colors", "256"),
    ("RGB", "8/8/8"),
    ("Tc", ""),
    ("setrgbf", "\x1b[38;2;%p1%d;%p2%d;%p3%dm"),
    ("setrgbb", "\x1b[48;2;%p1%d;%p2%d;%p3%dm"),
    ("Smulx", "\x1b[4:%p1%dm"),
    ("Setulc", "\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm"),
    ("Ss", "\x1b[%p1%d q"),
    ("Se", "\x1b[0 q"),
];
/// How many links' addresses are kept at a time.
const LINKS_MAX: usize = 1000;
/// How long the list of the links on the screen may be: what a reader reads of it.
pub const LINKS_LIST_MAX: usize = 256 << 10;
/// The most cells of rows that a resize sent to history are kept to come back at the next one.
const ABOVE_MAX: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u16 = 1;
pub const DIM: u16 = 2;
const ITALIC: u16 = 4;
const UNDERLINE: u16 = 8;
const BLINK: u16 = 16;
const REVERSE: u16 = 32;
const HIDDEN: u16 = 64;
const STRIKE: u16 = 128;
/// The kind of an underline that is not one plain line (with `UNDERLINE`).
const DOUBLE: u16 = 1024;
const CURLY: u16 = 2048;
const DOTTED: u16 = 4096;
const DASHED: u16 = 8192;
const KIND: [u16; 4] = [DOUBLE, CURLY, DOTTED, DASHED];
const KINDS: u16 = DOUBLE | CURLY | DOTTED | DASHED;
/// The looks that are one bit each: the SGR code that sets it, the bit, the code that takes it away.
const LOOKS: [(u16, u16, u16); 8] = [
    (1, BOLD, 22),
    (2, DIM, 22),
    (3, ITALIC, 23),
    (4, UNDERLINE, 24),
    (5, BLINK, 25),
    (7, REVERSE, 27),
    (8, HIDDEN, 28),
    (9, STRIKE, 29),
];
/// Not looks, but marks. `CONT` on a row's first cell: the row goes on from
/// the one above (the text wrapped into it). `PAD` on a row's last cell: the
/// cell is no text (a wide character had no room in it and went to the next
/// row whole). Erasing a cell takes its mark with it.
const CONT: u16 = 256;
const PAD: u16 = 512;
const MARKS: u16 = CONT | PAD;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// The underline's colour, if it has one of its own.
    pub ul: Color,
    pub flags: u16,
}

impl Style {
    /// Which of the kinds of underline that are not one plain line it has: 2 to 5, as SGR `4:N` counts them.
    fn kind(self) -> Option<usize> {
        KIND.iter().position(|kind| self.flags & kind != 0).map(|kind| kind + 2)
    }

    /// The SGR codes of a colour, each after a `;`: `base` is 30 for the text's, 40 for the
    /// background's, 50 for the underline's (which has the long form only).
    fn colour(out: &mut String, base: u16, color: Color, colons: bool) {
        let _ = match color {
            Color::Default => Ok(()),
            Color::Indexed(n @ 0..8) if base != 50 => write!(out, ";{}", base + u16::from(n)),
            Color::Indexed(n @ 8..16) if base != 50 => write!(out, ";{}", base + 52 + u16::from(n)),
            Color::Indexed(n) if colons => write!(out, ";{}:5:{n}", base + 8),
            Color::Indexed(n) => write!(out, ";{};5;{n}", base + 8),
            Color::Rgb(r, g, b) if colons => write!(out, ";{}:2::{r}:{g}:{b}", base + 8),
            Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
        };
    }

    /// The escape code that sets exactly this style.
    pub fn sgr(self, out: &mut String) {
        out.push_str("\x1b[0");
        for (code, _, _) in LOOKS.iter().filter(|look| self.flags & look.1 != 0) {
            let _ = write!(out, ";{code}");
        }
        Style::colour(out, 30, self.fg, false);
        Style::colour(out, 40, self.bg, false);
        out.push('m');
        // An underline's kind and colour come in a code of their own, in the form with colons:
        // a terminal that does not know that form passes the whole code over, and has the
        // plain underline and the colours from the one before.
        let mut own = self.kind().map_or(String::new(), |kind| format!(";4:{kind}"));
        Style::colour(&mut own, 50, self.ul, true);
        if let Some(own) = own.strip_prefix(';') {
            let _ = write!(out, "\x1b[{own}m");
        }
    }
}

/// A colour as a program or a terminal gives one, its first eight bits of each part: `#` and
/// 3, 6, 9 or 12 hex digits (the high bits, as X reads them), or `rgb:R/G/B` with one to four
/// hex digits each (a part of the whole). No other form is one: no name, no `rgbi:`.
pub fn colour(text: &str) -> Option<[u8; 3]> {
    let ([r, g, b], scaled) = match (text.strip_prefix("rgb:"), text.strip_prefix('#')) {
        // (A fourth part stays in the third, which is then no number.)
        (Some(parts), _) => {
            let mut parts = parts.splitn(3, '/');
            ([parts.next()?, parts.next()?, parts.next()?], true)
        }
        (_, Some(hex)) if hex.is_ascii() && hex.len().is_multiple_of(3) => {
            let each = hex.len() / 3;
            ([hex.get(..each)?, hex.get(each..2 * each)?, hex.get(2 * each..)?], false)
        }
        _ => return None,
    };
    let value = |part: &str| {
        let bits = 4 * part.len() as u32;
        let hex = (1..=4).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_hexdigit());
        let n = u32::from_str_radix(part, 16).ok().filter(|_| hex)?;
        u8::try_from(if scaled { n * 0xffff / ((1 << bits) - 1) } else { n << (16 - bits) } >> 8).ok()
    };
    Some([value(r)?, value(g)?, value(b)?])
}

/// Where a pane keeps a colour its program set that is no palette number's (those are 0 to
/// 255): the two it draws with by default, and the cursor's (OSC 10, 11, 12).
const FOREGROUND: u16 = 256;
const BACKGROUND: u16 = 257;
const CURSOR: u16 = 258;
type Own = BTreeMap<u16, [u8; 3]>;

/// A character can have this many parts after its first; more are dropped.
pub const MORE: usize = 9;

/// One character cell. `ch` is NUL in the second half of a wide character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    /// The rest of the character (the cluster): marks, joiners and what they join.
    pub more: [char; MORE],
    pub style: Style,
    /// The link the character is part of (OSC 8): its number in whoever keeps the cell's table
    /// of addresses, 0 for none.
    pub link: u16,
}

impl Cell {
    pub fn blank(style: Style) -> Cell {
        Cell { ch: ' ', more: ['\0'; MORE], style: Style { bg: style.bg, ..Style::default() }, link: 0 }
    }
    pub fn wide(&self) -> bool {
        width(self.ch, &self.more) == 2
    }
    /// Whether every terminal gives this character the columns it has here.
    pub fn plain(&self) -> bool {
        plain(self.ch, &self.more)
    }
    /// How many columns more than its own another terminal may give this character.
    pub fn spill(&self) -> usize {
        spread(self.ch, &self.more).saturating_sub(width(self.ch, &self.more))
    }
    /// The text this cell contributes. Its wide continuation and unused slots contribute nothing.
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.ch).chain(self.more.iter().copied()).filter(|&c| self.ch != '\0' && c != '\0')
    }
    fn bytes(&self) -> usize {
        self.chars().map(char::len_utf8).sum()
    }
}

/// A picture the program drew: where what the screen has of it is, in cells, and how many of
/// its rows have gone off the screen's top (`row` is 0 then); and its bytes, until they are taken.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub id: u32,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub off: usize,
    bytes: Option<Vec<u8>>,
}

/// Where a mark is: on the main screen, a row and a column, for as long as its row is there
/// (what is done to the screen's rows is done to it); a line of the pane's text and how many
/// bytes into it, once the row has gone into history.
#[derive(Clone, Copy, PartialEq)]
enum At {
    Screen(usize, usize),
    Text(usize, usize),
}

/// A mark a shell put at its prompt: where, its kind, a command's exit code.
type Mark = (At, &'static str, Option<u8>);

/// Where history's text ends: how many whole lines it has (the number of the line the screen's
/// first row is of); if its last row was not ended (its line goes on in the screen's first row),
/// how many bytes of that line it has; and what its last character is to the next (in
/// `history.styled`, two that would read as one are kept apart).
#[derive(Clone, Copy, Default)]
struct Tail {
    lines: usize,
    open: Option<usize>,
    joiner: Joiner,
}

impl Tail {
    /// The line is ended, in both texts.
    fn end(&mut self, out: &mut String, styled: &mut String) {
        out.push('\n');
        styled.push('\n');
        (self.lines, self.open) = (self.lines + 1, None);
    }
}

/// The rows a resize sent to history, while they are still its last lines: a later resize that
/// has room takes them back. With them, how many bytes (plain and styled) they are there, and
/// where history ended before them.
struct Lent {
    rows: Vec<Vec<Cell>>,
    bytes: usize,
    styled: usize,
    before: Tail,
}

/// What the emulator produces besides the screen, for the holder to take: history when enough waits, or all at once.
#[derive(Default)]
pub struct Out {
    /// Answers to the program's queries, to be written to it.
    pub reply: Vec<u8>,
    /// Lines that scrolled off the top of the main screen, as plain text; and before them,
    /// how many bytes to take off the end of what history has: a resize took rows back.
    pub history: String,
    pub unhistory: usize,
    /// The same lines with their looks (text and SGR codes only, a row as `styled` gives it),
    /// and how many bytes to take off the end of what was given of those.
    pub styled: String,
    pub unstyled: usize,
    /// What was dropped for being over a bound, or refused, for the log (each kind once).
    pub refused: Vec<&'static str>,
    /// What the program put on the clipboard last.
    pub clipboard: Option<Vec<u8>>,
    /// The directory the program said it is in last (OSC 7): the host it named, and the path.
    pub cwd: Option<(String, String)>,
    /// What the program wanted noticed last (empty for the bell).
    pub notice: Option<String>,
}

/// The character printed last: where the cursor was left, and the character's place, its
/// cell as it was left and what was in that cell before (no place, if it found no room).
#[derive(Clone, Copy)]
struct Last {
    at: Option<(usize, usize, Cell, Cell)>,
    cursor: (usize, usize, bool),
    joiner: Joiner,
}

/// The cursor: where it is, and what a program saves and brings back with it (DECSC, DECRC).
#[derive(Clone, Copy, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    style: Style,
    origin: bool,
    graphics: [bool; 2],
    shift: bool,
    /// It waits at the row's end to wrap.
    wrap: bool,
}

impl Cursor {
    /// Its row and column, one that waits to wrap being one column past its row's end.
    fn place(self) -> (usize, usize) {
        (self.row, self.col + usize::from(self.wrap))
    }
}

/// A control sequence (CSI) as far as it has been read: each one starts as a new one of these.
#[derive(Default)]
struct Csi {
    /// Its numbers, each with whether a `:` came before it; the number being read, and whether a `:` came before that.
    params: Vec<(u16, bool)>,
    param: Option<u16>,
    sub: bool,
    private: u8,
    inter: u8,
    /// The sequence's first number was left out.
    bare: bool,
    /// One of the sequence's numbers was too large to keep.
    huge: bool,
    /// It is malformed or has more parameters than are kept: it will be ignored.
    too_long: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// With the byte that came after ESC.
    EscapeInter(u8),
    Csi,
    /// A string (OSC, DCS, APC, SOS, PM), kept in `string` up to its ST (or, for
    /// OSC alone, a BEL): a title, a clipboard or a sixel picture if it says so.
    Str,
    /// ESC seen inside a string: `\` ends it, anything else starts anew.
    StrEscape,
}

/// What is shown: the grid, the cursor and the modes. Whoever keeps a terminal reads its own
/// by reference; a reader of a pane's files has one that `read` made of them.
#[derive(Default)]
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    grid: Vec<Vec<Cell>>,
    alt: bool,
    /// The kitty keyboard protocol: the flags a program asked for on the main screen and on
    /// the alternate one, with the flags it pushed aside on each.
    keys: [(u8, Vec<u8>); 2],
    pub cursor: Cursor,
    pub cursor_shown: bool,
    pub cursor_shape: u8,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub paste: bool,
    /// The program asked to be told when its pane gains and loses the keys (mode 1004).
    pub focus: bool,
    /// The program is in the middle of drawing (mode 2026): what it has drawn so far is not to be shown yet.
    pub sync: bool,
    /// The mouse reports the program asked for: 0 for none, else 9, 1000, 1002 or 1003; and whether as SGR (1006).
    pub mouse: u16,
    pub mouse_sgr: bool,
    pub pictures: Vec<Picture>,
}

/// What whoever keeps a terminal tells it of the world outside it.
#[derive(Default)]
pub struct Told {
    /// What this terminal is called and its version, for a program that asks (XTVERSION).
    pub name: String,
    /// Which keyboard flags the terminal that sends the keys takes (none, until told).
    pub keyboard: u8,
    /// The size of a cell in pixels, to know how many cells a picture covers.
    pub cell: (usize, usize),
    /// What the colours are: lines of a name (`foreground`, `background`, a palette number) and
    /// `#rrggbb`. A program that asks for a colour is answered from here, and not at all if its
    /// line is missing.
    pub colours: String,
}

pub struct Term {
    screen: Screen,
    told: Told,
    other: Vec<Vec<Cell>>,
    top: usize,
    bottom: usize,
    autowrap: bool,
    insert: bool,
    tabs: Vec<bool>,
    saved: [Cursor; 2],
    last: char,
    /// The character printed last, and what the next one needs to know to join it.
    cluster: Option<Last>,
    /// The addresses of the links on the two screens (OSC 8), by a cell's `link` less one; and
    /// the link that what is printed now is part of.
    links: Vec<String>,
    link: u16,
    /// The marks a shell put at its prompts (OSC 133), the last thousand.
    marks: Vec<Mark>,
    /// Where history ends.
    tail: Tail,
    /// The rows the last resize sent to history, while they are still its last lines.
    lent: Option<Lent>,
    state: State,
    csi: Csi,
    /// The bytes read so far of a character of more than one.
    utf8: Vec<u8>,
    /// The string being read, after the byte that began it (`]` for OSC, `P` for DCS…).
    string: Vec<u8>,
    /// (A control byte came inside it and was dropped: what is left is not what was sent.)
    string_holed: bool,
    out: Out,
    /// The pictures of the screen not shown (the main one while the alternate is).
    other_pictures: Vec<Picture>,
    next_picture: u32,
    /// The title now, and the titles a program put aside to bring back (`CSI 22 t`, `CSI 23 t`), at most 8.
    title: String,
    titles: Vec<String>,
    /// The colours this pane's program set (OSC 4, 10, 11, 12), by palette number, `FOREGROUND`,
    /// `BACKGROUND` or `CURSOR`: 259 at most, and one set for both screens. They are the pane's:
    /// its cells are shown in them and a question is answered with them, until the program
    /// gives them back (OSC 104, 110 to 112, a full reset). The cells stay as they were written.
    own: Own,
}

impl Screen {
    pub fn cell_at(&self, row: usize, col: usize) -> Option<&Cell> {
        self.grid.get(row)?.get(col)
    }

    /// The screen a `styled` text says, `cols` wide and of as many rows as it has lines (a reader's
    /// side of `Term::styled`): cells and looks, and none of what `set_words` and `set_link` say.
    pub fn read(styled: &str, cols: usize) -> Screen {
        // (One column more than the pane is said to have, where there is room for one: there a
        // line that is longer ends, and is cut off.)
        let mut term = Term::new((cols + 1).min(SIZE_MAX), styled.lines().count());
        term.feed(b"\x1b[?7l");
        for (row, line) in styled.lines().enumerate().take(SIZE_MAX) {
            // The rows are text and colours, as the holder writes them: anything else in a
            // file of someone's own making is shown as `?`.
            let mut text = String::with_capacity(line.len());
            let mut rest = line;
            while let Some(at) = rest.find(|c: char| c.is_control()) {
                let (before, after) = rest.split_at(at);
                let colour = after.strip_prefix("\x1b[").and_then(|sgr| {
                    let end = sgr.find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))?;
                    sgr.get(end..)?.starts_with('m').then_some(2 + end + 1)
                });
                text.push_str(before);
                text.push_str(colour.and_then(|length| after.get(..length)).unwrap_or("?"));
                let length = colour.or(after.chars().next().map(char::len_utf8)).unwrap_or(1);
                rest = after.get(length..).unwrap_or_default();
            }
            text.push_str(rest);
            term.feed(format!("\x1b[{};1H\x1b[m{text}", row + 1).as_bytes());
            if cols < term.screen.cols {
                term.feed(format!("\x1b[{};{}H\x1b[m\x1b[K", row + 1, cols + 1).as_bytes());
            }
        }
        term.screen
    }

    /// Says of a cell which link it is part of (for a reader that has the links from a file).
    pub fn set_link(&mut self, row: usize, col: usize, link: u16) {
        if let Some(cell) = self.grid.get_mut(row).and_then(|cells| cells.get_mut(col)) {
            cell.link = link;
        }
    }

    /// The modes the `modes` file lists by name, and where each is kept.
    fn modes(&mut self) -> [(&'static str, &mut bool); 4] {
        [
            ("appcursor", &mut self.app_cursor),
            ("keypad", &mut self.app_keypad),
            ("paste", &mut self.paste),
            ("focus", &mut self.focus),
        ]
    }

    /// The keyboard flags in force: what the program on the screen shown asked for, as far as
    /// the terminal that sends the keys took it when the program asked.
    pub fn keys(&self) -> u8 {
        self.keys.get(usize::from(self.alt)).map_or(0, |(flags, _)| *flags)
    }

    fn words(&mut self) -> String {
        // Which reports (`mouse1002`) and in what form (`mousesgr`) are two things a program sets apart.
        let mouse = (self.mouse != 0).then(|| format!("mouse{}", self.mouse));
        let sgr = self.mouse_sgr.then(|| "mousesgr".to_string());
        // And the keyboard flags a program asked for (the kitty protocol), as a number.
        let keys = (self.keys() != 0).then(|| format!("keys{}", self.keys()));
        let on = self.modes().into_iter().filter(|m| *m.1).map(|m| m.0.to_string());
        on.chain(mouse).chain(sgr).chain(keys).collect::<Vec<String>>().join(" ")
    }

    /// Takes the modes those words say (a reader's side of `words`).
    pub fn set_words(&mut self, words: &str) {
        let has = |mode: &str| words.split_whitespace().any(|m| m == mode);
        self.modes().into_iter().for_each(|(name, on)| *on = has(name));
        self.mouse = [9, 1000, 1002, 1003].into_iter().find(|m| has(&format!("mouse{m}"))).unwrap_or(0);
        self.mouse_sgr = has("mousesgr");
        // (Whichever screen is shown has the keyboard flags.)
        let keys = words.split_whitespace().find_map(|m| m.strip_prefix("keys")?.parse().ok()).unwrap_or(0);
        self.keys = [(keys, Vec::new()), (keys, Vec::new())];
    }
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let (cols, rows) = (cols.clamp(1, SIZE_MAX), rows.clamp(1, SIZE_MAX));
        let grid = vec![vec![Cell::blank(Style::default()); cols]; rows];
        Term {
            screen: Screen { cols, rows, grid: grid.clone(), cursor_shown: true, ..Screen::default() },
            told: Told { cell: (10, 20), ..Told::default() },
            other: grid,
            top: 0,
            bottom: rows - 1,
            autowrap: true,
            insert: false,
            tabs: (0..SIZE_MAX).map(|c| c % 8 == 0).collect(),
            saved: [Cursor::default(); 2],
            last: '\0',
            cluster: None,
            marks: Vec::new(),
            links: Vec::new(),
            link: 0,
            tail: Tail::default(),
            lent: None,
            state: State::Ground,
            csi: Csi::default(),
            utf8: Vec::new(),
            string: Vec::new(),
            string_holed: false,
            out: Out::default(),
            other_pictures: Vec::new(),
            next_picture: 1,
            title: String::new(),
            titles: Vec::new(),
            own: BTreeMap::new(),
        }
    }

    /// What is shown.
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// How input is to be sent to the program, as words: what the `modes` file says.
    pub fn words(&mut self) -> String {
        self.screen.words()
    }

    /// What the terminal is told of the world outside it: to be read, and to be said anew.
    pub fn told(&mut self) -> &mut Told {
        &mut self.told
    }

    /// The pictures on either screen whose bytes were not taken yet: the number of each and its bytes.
    pub fn new_pictures(&mut self) -> Vec<(u32, Vec<u8>)> {
        let pictures = self.screen.pictures.iter_mut().chain(&mut self.other_pictures);
        pictures.filter_map(|p| Some((p.id, p.bytes.take()?))).collect()
    }

    /// Whether either screen has the picture of this number.
    pub fn has_picture(&self, id: u32) -> bool {
        self.screen.pictures.iter().chain(&self.other_pictures).any(|p| p.id == id)
    }

    /// Of the pictures on both screens, those whose number `keep` says to keep stay.
    pub fn keep_pictures(&mut self, keep: impl Fn(u32) -> bool) {
        self.screen.pictures.retain(|p| keep(p.id));
        self.other_pictures.retain(|p| keep(p.id));
    }

    /// Whether history's last rows are a resize's, which the next one may take back.
    pub fn lent(&self) -> bool {
        self.lent.is_some()
    }

    /// The rows a resize sent to history stay there (it could not be written as it was given).
    pub fn keep_lent(&mut self) {
        self.lent = None;
    }

    /// The window title the program set last, without control characters (empty: none).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What was produced since the last time this was asked: the answers and refusals; the history
    /// once `over` bytes of it wait; and the rest (what the program said once) when `over` is 0.
    pub fn take(&mut self, over: usize) -> Out {
        let (reply, refused) = (std::mem::take(&mut self.out.reply), std::mem::take(&mut self.out.refused));
        let all = self.out.history.len().max(self.out.styled.len()) >= over;
        let mut out = Out { reply, refused, ..if all { std::mem::take(&mut self.out) } else { Out::default() } };
        if all && over > 0 {
            (self.out.clipboard, self.out.cwd, self.out.notice) =
                (out.clipboard.take(), out.cwd.take(), out.notice.take());
        }
        out
    }

    // ---- files -----------------------------------------------------------

    /// A wrapped row keeps its trailing spaces, except for a wide character's padding cell.
    fn row_text(row: &[Cell], goes_on: bool, out: &mut String) {
        let blank = |cell: &Cell| cell.ch == '\0' || (cell.ch == ' ' && cell.more.first() == Some(&'\0'));
        let used = if goes_on {
            text_cells(row).len()
        } else {
            row.iter().rposition(|cell| !blank(cell)).map_or(0, |last| last + 1)
        };
        out.extend(row.iter().take(used).flat_map(Cell::chars));
    }

    /// A row's text as history keeps it. A row that goes on in the next is not ended, so history
    /// holds whole lines however they were wrapped; `open` says the line before was left so, and
    /// a row that does not go on from it ends it first (what it went on in was erased).
    /// `styled` gets the same row with its looks, and a line's end wherever `out` gets one;
    /// `joiner` is what the open line's last character is to this row's first, `own` the colours
    /// the program set, in which the row is told as it is now: history is not told again later.
    /// With 4 MiB waiting to be written in `out` the row is not kept, which is what false says:
    /// it ends the line it would have gone on, in both texts.
    fn row_history(row: &[Cell], goes_on: bool, own: &Own, tail: &mut Tail, out: &mut Out) -> bool {
        let (out, styled) = (&mut out.history, &mut out.styled);
        let kept = out.len().max(styled.len()) < HISTORY_PENDING_MAX;
        if tail.open.is_some() && !(kept && continues(row)) {
            tail.end(out, styled);
        }
        if !kept {
            return false;
        }
        if tail.open.is_none() {
            tail.joiner = Joiner::default();
        }
        let start = out.len();
        Term::row_text(row, goes_on, out);
        tail.open = goes_on.then(|| tail.open.unwrap_or(0) + out.len() - start);
        // (Most rows have no looks and no character that could join another: their text is all of it.)
        let looks = |cell: &Cell| Style { flags: cell.style.flags & !MARKS, ..cell.style } != Style::default();
        let bare =
            |cell: &Cell| cell.ch.is_ascii() && cell.more.first() == Some(&'\0') && cell.link == 0 && !looks(cell);
        let text = out.get(start..).unwrap_or_default();
        let first = text.chars().next().is_some_and(|first| tail.joiner.clone().joins(first));
        // (With a default colour of the program's own no cell is without a look.)
        let plain = !own.contains_key(&FOREGROUND) && !own.contains_key(&BACKGROUND);
        if !first && plain && row.iter().all(bare) {
            styled.push_str(text);
            // (No ASCII character joins the one before it: the last one is all the next row needs.)
            tail.joiner = Joiner::default();
            text.chars().next_back().map(|last| tail.joiner.joins(last));
        } else {
            Term::row_styled(row, goes_on, false, own, &mut tail.joiner, styled);
        }
        if !goes_on {
            tail.end(out, styled);
        }
        true
    }

    /// The screen as plain text, one line per row, blanks at the end cut.
    pub fn plain(&self) -> String {
        Term::text(&self.screen.grid)
    }

    /// The main screen while the alternate screen covers it, as `plain`, `styled` and `wraps`
    /// would have it.
    pub fn covered(&self) -> Option<(String, String, String)> {
        self.screen.alt.then(|| (Term::text(&self.other), self.looks(&self.other), self.goes_on(&self.other, true)))
    }

    fn text(grid: &[Vec<Cell>]) -> String {
        let mut out = String::new();
        for (row, goes_on) in with_next(grid) {
            Term::row_text(row, goes_on, &mut out);
            out.push('\n');
        }
        out
    }

    /// The cursor's colour, if the program set one (OSC 12).
    pub fn cursor_colour(&self) -> Option<[u8; 3]> {
        self.own.get(&CURSOR).copied()
    }

    /// The screen with colours: text and SGR codes only, each row on a line
    /// that starts and ends in the default style. A colour the program set for a palette number
    /// or for the two it draws with by default is told as what it set (direct colour), wherever
    /// a cell has that number or that default: so with a background of its own every cell of
    /// the screen has it, the blank ones too, and every row is as long as the screen is wide.
    pub fn styled(&self) -> String {
        self.looks(&self.screen.grid)
    }

    fn looks(&self, grid: &[Vec<Cell>]) -> String {
        let mut out = String::new();
        for (row, goes_on) in with_next(grid) {
            Term::row_styled(row, goes_on, true, &self.own, &mut Joiner::default(), &mut out);
            out.push('\n');
        }
        out
    }

    /// A row with its looks, from the default style to the default style, and no line's end.
    /// `own` are the colours the program set: a cell that has such a palette number or default
    /// is told in that colour. `joiner` is what came before the row in its line, and then what
    /// the row leaves.
    /// `screen`: the cell a wide character left empty at the row's end is told too, if it has a
    /// look (it is no text: U+200B before it says so, and a history has none of it).
    fn row_styled(row: &[Cell], goes_on: bool, screen: bool, own: &Own, joiner: &mut Joiner, out: &mut String) {
        let plain = Style::default();
        let own = |color: Color, default: Option<u16>| {
            let slot = match color {
                Color::Default => default,
                Color::Indexed(n) => Some(u16::from(n)),
                Color::Rgb(..) => None,
            };
            slot.and_then(|slot| own.get(&slot)).map_or(color, |[r, g, b]| Color::Rgb(*r, *g, *b))
        };
        // (A blank at a row's end is not told for a foreground of its own alone: it shows none.)
        let blank = Cell { style: Style { fg: own(Color::Default, Some(FOREGROUND)), ..plain }, ..Cell::blank(plain) };
        // (The marks are no looks, and a link is none here: the file has no links. An underline
        // with no colour of its own has the text's.)
        let look = |cell: &Cell| {
            let Style { fg, bg, ul, flags } = cell.style;
            let (fg, bg, ul) = (own(fg, Some(FOREGROUND)), own(bg, Some(BACKGROUND)), own(ul, None));
            Cell { style: Style { fg, bg, ul, flags: flags & !MARKS }, link: 0, ..*cell }
        };
        // (A row that goes on in the next keeps the blanks at its end, as in `plain`.)
        let gap = row.last().filter(|cell| goes_on && cell.style.flags & PAD != 0);
        let used = match goes_on {
            true => row.len() - usize::from(gap.is_some()),
            false => row.iter().rposition(|c| look(c) != blank).map_or(0, |last| last + 1),
        };
        let gap = gap.filter(|cell| screen && look(cell) != blank);
        let mut style = plain;
        for cell in row.iter().take(used).chain(gap).filter(|cell| cell.ch != '\0') {
            if look(cell).style != style {
                style = look(cell).style;
                style.sgr(out);
            }
            if gap.is_some_and(|gap| std::ptr::eq(gap, cell)) {
                out.push('\u{200b}');
            }
            // Two characters that would read back as one are kept apart by U+200B, which
            // takes no room and is no part of either.
            if joiner.joins(cell.ch) {
                out.push('\u{200b}');
                *joiner = Joiner::default();
                joiner.joins(cell.ch);
            }
            cell.more.iter().take_while(|&&c| c != '\0').for_each(|&c| _ = joiner.joins(c));
            out.extend(cell.chars());
        }
        if style != plain {
            out.push_str("\x1b[m");
        }
    }

    /// How many lines are before the main screen's first row: those gone into history whole
    /// (one that history has the start of is whole once what it went on in was erased: `settle`).
    pub fn top(&self) -> usize {
        self.tail.lines
    }

    /// The number of a link's address in the table, which keeps at most `LINKS_MAX`: when it is
    /// full, the addresses no cell uses any more make room; if every one is in use, no link.
    fn link_number(&mut self, address: &str) -> u16 {
        if let Some(at) = self.links.iter().position(|known| known == address) {
            return at as u16 + 1;
        }
        if self.links.len() >= LINKS_MAX {
            let mut used = vec![false; self.links.len()];
            // (Of both screens, of the rows a resize set aside to bring back, and of the cell
            // that may be put back under a character still being joined.)
            let under = self.cluster.and_then(|last| last.at).map(|(.., under)| under);
            let lent = self.lent.iter().flat_map(|lent| &lent.rows);
            let cells = self.screen.grid.iter().chain(&self.other).chain(lent).flatten().chain(&under);
            for cell in cells.filter(|cell| cell.link != 0) {
                used.get_mut(usize::from(cell.link) - 1).into_iter().for_each(|used| *used = true);
            }
            for (known, _) in self.links.iter_mut().zip(used).filter(|(_, used)| !used) {
                known.clear();
            }
        }
        let free =
            self.links.iter().position(String::is_empty).or((self.links.len() < LINKS_MAX).then_some(self.links.len()));
        let Some(at) = free else {
            self.refuse("a link was not kept: 1000 others are on the screen");
            return 0;
        };
        self.links.resize(self.links.len().max(at + 1), String::new());
        self.links.get_mut(at).into_iter().for_each(|slot| *slot = address.to_string());
        at as u16 + 1
    }

    /// The links on the screen, a line each: `LINE COL LEN ADDRESS`. LINE is the line's
    /// number as a mark has it; COL counts cells from the start of that line's first row on the
    /// screen, a whole row for each row before; LEN is how many cells, in reading order.
    pub fn links(&self) -> String {
        // (A list that would be longer than a reader reads ends before the run that makes it so.)
        let (mut out, mut full) = (String::new(), false);
        // (Of the screen shown: a program on the alternate screen has links too. Its lines are
        // numbered on from `top` as the main screen's are, though none of them goes to history.)
        for (line, rows) in (self.top()..).zip(self.screen.grid.chunk_by(|_, next| continues(next))) {
            // A run is a line's cells of one link, one after the other: no link after the line's
            // last cell ends the run that reaches it.
            let (mut start, mut open) = (0, 0);
            for (at, link) in rows.iter().flatten().map(|cell| cell.link).chain([0]).enumerate() {
                if link != open && open != 0 {
                    let address = self.links.get(usize::from(open) - 1);
                    let record = format!("{line} {start} {} {}\n", at - start, address.map_or("", String::as_str));
                    full |= out.len() + record.len() > LINKS_LIST_MAX;
                    if address.is_some_and(|address| !address.is_empty()) && !full {
                        out.push_str(&record);
                    }
                }
                (start, open) = if link == open { (start, open) } else { (at, link) };
            }
        }
        out
    }

    /// The main screen's rows, whichever screen is shown.
    fn main(&self) -> &[Vec<Cell>] {
        if self.screen.alt { &self.other } else { &self.screen.grid }
    }

    /// Where each mark is, those on rows `rows` of the main screen told as places in the pane's
    /// text: the line's number, and how many bytes of the line's text are before the mark (those
    /// history has of it among them). Each row's line and the bytes of the line before the row
    /// are counted once, and only as far down as a mark is: a thousand marks cost one walk over
    /// the screen, and a scroll with no mark on its rows costs none. `main` are the main screen's
    /// rows and `tail` where history ends before them.
    fn texts(main: &[Vec<Cell>], marks: &[Mark], tail: Tail, rows: std::ops::Range<usize>) -> Vec<At> {
        let text = |cells: &[Cell], upto: usize| text_cells(cells).iter().take(upto).map(Cell::bytes).sum::<usize>();
        let last = main.len().saturating_sub(1);
        let on = |at: At| match at {
            At::Screen(row, col) if rows.contains(&row) => Some((row.min(last), col)),
            _ => None,
        };
        let down = marks.iter().filter_map(|mark| on(mark.0)).map(|(row, _)| row + 1).max().unwrap_or(0);
        let (mut line, mut bytes, mut starts) = (tail.lines, 0, Vec::with_capacity(down));
        for (y, cells) in main.iter().enumerate().take(down) {
            match (y, continues(cells)) {
                (0, true) => bytes = tail.open.unwrap_or(0),
                (0, false) | (_, true) => {}
                (_, false) => (line, bytes) = (line + 1, 0),
            }
            starts.push((line, bytes));
            bytes += text(cells, cells.len());
        }
        let place = |(row, col): (usize, usize)| {
            let (line, before) = starts.get(row).copied().unwrap_or((tail.lines, 0));
            At::Text(line, before + main.get(row).map_or(0, |cells| text(cells, col)))
        };
        marks.iter().map(|mark| on(mark.0).map_or(mark.0, place)).collect()
    }

    /// The shell's marks, a line each: `LINE AT KIND`, and the command's exit code after `end`.
    pub fn marks(&self) -> String {
        let mut out = String::new();
        let texts = Term::texts(self.main(), &self.marks, self.tail, 0..usize::MAX);
        for ((_, kind, code), at) in self.marks.iter().zip(texts) {
            let At::Text(line, bytes) = at else { continue };
            let _ = write!(out, "{line} {bytes} {kind}");
            let _ = writeln!(out, "{}", code.map_or(String::new(), |code| format!(" {code}")));
        }
        out
    }

    /// What is done to the main screen's rows is done to the marks on them: `to` says where a
    /// mark at a row and column goes.
    fn move_marks(&mut self, to: impl Fn(usize, usize) -> (usize, usize)) {
        for mark in self.marks.iter_mut().filter(|_| !self.screen.alt) {
            if let At::Screen(row, col) = &mut mark.0 {
                (*row, *col) = to(*row, *col);
            }
        }
    }

    /// When a line that history has the start of no longer goes on in the screen's first row
    /// (that row was erased), history's line is ended there.
    fn settle(&mut self) {
        if self.tail.open.is_some() && !self.main().first().is_some_and(|row| continues(row)) {
            self.tail.end(&mut self.out.history, &mut self.out.styled);
            // (What comes after them in history makes the rows a resize sent there stay there.)
            self.lent = None;
        }
    }

    /// The rows of the screen that go on from the row above (the text wrapped into them), as
    /// numbers from 0: what `plain` cannot say. The first row may go on from history's last line.
    pub fn wraps(&self) -> String {
        self.goes_on(&self.screen.grid, !self.screen.alt)
    }

    fn goes_on(&self, grid: &[Vec<Cell>], main: bool) -> String {
        let open = self.tail.open.is_some() && main;
        let rows = grid.iter().enumerate().filter(|(y, row)| continues(row) && (*y > 0 || open));
        rows.map(|(y, _)| y.to_string()).collect::<Vec<String>>().join(" ")
    }

    // ---- the grid --------------------------------------------------------

    fn blank(&self) -> Cell {
        Cell::blank(self.screen.cursor.style)
    }

    /// Repairs a row after cells moved: half of a wide character becomes a blank.
    fn tidy(&mut self, row: usize) {
        let Some(cells) = self.screen.grid.get_mut(row) else {
            return;
        };
        let mut lead = false;
        for i in 0..cells.len() {
            let next_is_half = cells.get(i + 1).is_some_and(|c| c.ch == '\0');
            let Some(cell) = cells.get_mut(i) else { return };
            let wide = cell.wide();
            if (cell.ch == '\0' && !lead) || (wide && !next_is_half) {
                *cell = Cell { style: cell.style, ..Cell::blank(cell.style) };
            }
            lead = wide && next_is_half;
        }
    }

    /// Blanks a wide character that a boundary before column `at` would cut in two.
    fn split(&mut self, row: usize, at: usize) {
        let blank = self.blank();
        if let Some(cells) = self.screen.grid.get_mut(row).filter(|cells| cells.get(at).is_some_and(|c| c.ch == '\0'))
            && let Some(pair) = cells.get_mut(at.saturating_sub(1)..=at)
        {
            pair.fill(blank);
        }
    }

    /// Row `row` no longer goes on from the one above it: its neighbour is another now.
    fn parts(&mut self, row: usize) {
        if let Some(first) = self.screen.grid.get_mut(row).and_then(|cells| cells.first_mut()) {
            first.style.flags &= !CONT;
        }
    }

    /// Blanks columns `from..to` of a row.
    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let (blank, to) = (self.blank(), to.min(self.screen.cols));
        if from == 0 && to == self.screen.cols {
            // A row erased whole is an empty line: the next row does not go on from it.
            self.parts(row + 1);
        }
        self.split(row, from);
        self.split(row, to);
        if let Some(cells) = self.screen.grid.get_mut(row).and_then(|r| r.get_mut(from..to)) {
            cells.fill(blank);
        }
        self.screen.cursor.wrap = false;
        self.uncover(row, from, to);
        // (Its marks are where the erased part begins; the place right after it is its end too.)
        self.move_marks(|r, c| (r, if r == row && (from + 1..=to).contains(&c) { from } else { c }));
    }

    /// The pictures that lie on cells `from` to `to` of a row are written over: they go.
    /// (Asked first, here and in `scroll_pictures`: a flood prints and scrolls millions of times over none.)
    fn uncover(&mut self, row: usize, from: usize, to: usize) {
        let on = |p: &Picture| (p.row..p.row + p.rows).contains(&row) && p.col < to && from < p.col + p.cols;
        let Some(_) = self.screen.pictures.iter().position(on) else { return };
        self.screen.pictures.retain(|p| !on(p));
    }

    /// Pictures move with rows `from..=bottom` when those scroll by `n`; one
    /// that would leave those rows, or lies across their edge, goes. Off the
    /// screen's top a picture goes row by row: it stays while it has one left.
    /// Past the screen's last row it is only not shown.
    fn scroll_pictures(&mut self, from: usize, n: usize, up: bool) {
        let Some(_) = self.screen.pictures.first() else { return };
        let bottom = self.bottom;
        let low = if bottom + 1 == self.screen.rows { usize::MAX } else { bottom + 1 };
        self.screen.pictures.retain_mut(|p| {
            let end = p.row + p.rows;
            if end <= from || p.row > bottom {
                return true;
            }
            let inside = p.row >= from && end <= low;
            if up {
                let gone = if from == 0 { (n - n.min(p.row)).min(p.rows) } else { 0 };
                let stays = inside && p.row + gone >= from + n && gone < p.rows;
                (p.row, p.rows, p.off) = (p.row.saturating_sub(n), p.rows - gone, p.off + gone);
                stays
            } else {
                p.row += n;
                inside && p.off == 0 && end.saturating_add(n) <= low && p.row <= bottom
            }
        });
    }

    /// Moves rows `from..=bottom` up by `n`, blank rows coming in below.
    fn scroll_up(&mut self, from: usize, n: usize, wrapping: bool) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = self.blank();
        let kept = from == 0 && !self.screen.alt;
        // A mark on a row that goes into history is from then on a place in the text; on a row
        // that is dropped, it is where the rows that stay begin; on a row that moves, it moves.
        let bottom = self.bottom;
        if kept {
            self.settle();
            // (Asked first: a flood scrolls two million times past no mark.)
            if !self.marks.is_empty() {
                let texts = Term::texts(self.main(), &self.marks, self.tail, 0..n);
                self.marks.iter_mut().zip(texts).for_each(|(mark, at)| mark.0 = at);
            }
            // What comes after them in history makes the rows a resize sent there stay there.
            self.lent = None;
        }
        self.move_marks(|row, col| match row {
            row if row < from || row > bottom => (row, col),
            row if row >= from + n => (row - n, col),
            _ => (from, 0),
        });
        let Some(region) = self.screen.grid.get_mut(from..=self.bottom) else {
            return;
        };
        let mut full = false;
        // (Blanks that have a colour are a row's looks to its end: also the background's, if the
        // program set one of its own.)
        let cut = blank == Cell::blank(Style::default()) && !self.own.contains_key(&BACKGROUND);
        for i in 0..n {
            // (The row the text is just wrapping out of, which `wrapping` says, has no next row yet.)
            let goes_on = region.get(i + 1).map_or(wrapping, |next| continues(next));
            let Some(row) = region.get_mut(i) else { break };
            // (Most of a row is the blanks at its end, as they will be again: one look finds
            // where they start, and they are neither read for history, which would cut them,
            // nor written again.)
            let used = row.iter().rposition(|cell| *cell != blank).map_or(0, |last| last + 1);
            if kept {
                let text = row.get(..used.max(1)).filter(|_| cut && !goes_on).unwrap_or(row);
                full |= !Term::row_history(text, goes_on, &self.own, &mut self.tail, &mut self.out);
            }
            row.iter_mut().take(used).for_each(|cell| *cell = blank);
        }
        region.rotate_left(n);
        self.scroll_pictures(from, n, true);
        // The rows have new neighbours at both ends of what moved. (The row now at the top of
        // the main screen may go on from the line that history has the start of.)
        if !kept {
            self.parts(from);
        }
        self.parts(self.bottom + 1);
        if full {
            self.refuse(HISTORY_FULL);
        }
    }

    /// Moves rows `from..=bottom` down by `n`, blank rows coming in at `from`.
    fn scroll_down(&mut self, from: usize, n: usize) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = self.blank();
        let bottom = self.bottom;
        self.move_marks(|row, col| match row {
            row if row < from || row > bottom => (row, col),
            row if row + n > bottom => (bottom, usize::MAX),
            row => (row + n, col),
        });
        let Some(region) = self.screen.grid.get_mut(from..=self.bottom) else {
            return;
        };
        region.rotate_right(n);
        region.iter_mut().take(n).for_each(|row| row.fill(blank));
        self.scroll_pictures(from, n, false);
        // Rows have new neighbours at both ends of what moved.
        self.parts(from + n);
        self.parts(self.bottom + 1);
    }

    fn linefeed(&mut self, wrapping: bool) {
        self.screen.cursor.wrap = false;
        if self.screen.cursor.row == self.bottom {
            self.scroll_up(self.top, 1, wrapping);
        } else if self.screen.cursor.row + 1 < self.screen.rows {
            self.screen.cursor.row += 1;
        }
    }

    fn reverse_linefeed(&mut self) {
        self.screen.cursor.wrap = false;
        if self.screen.cursor.row == self.top {
            self.scroll_down(self.top, 1);
        } else {
            self.screen.cursor.row = self.screen.cursor.row.saturating_sub(1);
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        let (low, high) = if self.screen.cursor.origin { (self.top, self.bottom) } else { (0, self.screen.rows - 1) };
        self.screen.cursor.row = (low + row).min(high);
        self.screen.cursor.col = col.min(self.screen.cols - 1);
        self.screen.cursor.wrap = false;
    }

    /// The character printed last, while text goes on from it: the cursor is where that left it
    /// and its cell is as it left it. A change of style or a title between a letter and its
    /// accent moves nothing, a move or an erase does.
    fn going_on(&self) -> Option<&Last> {
        let here = (self.screen.cursor.row, self.screen.cursor.col, self.screen.cursor.wrap);
        self.cluster.as_ref().filter(|last| {
            last.cursor == here
                && last.at.as_ref().is_none_or(|(row, col, cell, _)| self.screen.cell_at(*row, *col) == Some(cell))
        })
    }

    fn print(&mut self, mut c: char) {
        if self.screen.cursor.graphics.get(usize::from(self.screen.cursor.shift)) == Some(&true) {
            c = line_drawing(c);
        }
        let here = (self.screen.cursor.row, self.screen.cursor.col, self.screen.cursor.wrap);
        let mut joiner = self.going_on().map_or(Joiner::default(), |last| last.joiner);
        if joiner.joins(c) {
            self.join(c);
        } else if width(c, &[]) == 0 {
            // A mark or a joiner with nothing to go on: it has no place on a screen.
            self.cluster = None;
        } else {
            // Until it is placed it has no cell: what joins a character that found no room goes with it.
            (self.last, self.cluster) = (c, Some(Last { at: None, cursor: here, joiner }));
            let new = Cell { ch: c, more: ['\0'; MORE], style: self.screen.cursor.style, link: self.link };
            self.place(new, width(c, &[]) == 2, self.insert);
        }
        // What the next character must find to go on from this one.
        let here = (self.screen.cursor.row, self.screen.cursor.col, self.screen.cursor.wrap);
        if let Some(last) = &mut self.cluster {
            (last.cursor, last.joiner) = (here, joiner);
            last.at =
                last.at.and_then(|(row, col, _, under)| Some((row, col, *self.screen.grid.get(row)?.get(col)?, under)));
        }
    }

    /// Adds `c` to the character printed last: it is the same cluster (UAX #29).
    fn join(&mut self, c: char) {
        let Some((row, col, _, under)) = self.cluster.and_then(|last| last.at) else { return };
        let insert = self.insert;
        let Some(cell) = self.screen.grid.get_mut(row).and_then(|r| r.get_mut(col)) else { return };
        let (whole, was_wide) = (*cell, cell.wide());
        let Some(slot) = cell.more.iter_mut().find(|m| **m == '\0') else {
            return self.refuse("a character lost its end: it had more than ten parts");
        };
        *slot = c;
        let grown = *cell;
        if grown.wide() && !was_wide {
            // U+FE0F asked an emoji for its picture, which takes a second column: the
            // character is put again, and at a row's end it wraps whole.
            if self.screen.cols < 2 || (col + 2 > self.screen.cols && !self.autowrap) {
                *cell = whole;
                return;
            }
            if col + 2 > self.screen.cols {
                // It leaves the row's last cell as it found it.
                *cell = under;
            } else if insert && let Some(rest) = self.screen.grid.get_mut(row).and_then(|r| r.get_mut(col + 1..)) {
                // In insert mode its second column is one more column put in.
                rest.rotate_right(1);
            }
            (self.screen.cursor.col, self.screen.cursor.wrap) = (col, false);
            self.place(grown, true, false);
            if insert {
                self.tidy(row);
            }
        }
    }

    /// Puts a character at the cursor, on the next row if this one has no room, and moves on.
    fn place(&mut self, mut new: Cell, wide: bool, insert: bool) {
        if wide && self.screen.cols < 2 {
            return;
        }
        if self.screen.cursor.wrap || (wide && self.screen.cursor.col + 2 > self.screen.cols) {
            if self.autowrap {
                // The text goes on in the next row, whose first cell will say so. A wide
                // character that had no room in this row's last cell leaves it as no text, if
                // nothing is in it.
                let waiting = self.screen.cursor.wrap;
                if let Some(last) = self.screen.grid.get_mut(self.screen.cursor.row).and_then(|row| row.last_mut())
                    && !waiting
                    && *last == Cell::blank(last.style)
                {
                    last.style.flags |= PAD;
                }
                self.screen.cursor.col = 0;
                self.linefeed(true);
                new.style.flags |= CONT;
            } else if wide {
                return; // No room and no wrapping: it is dropped, as xterm drops it.
            }
            self.screen.cursor.wrap = false;
        }
        let (row, col) = (self.screen.cursor.row, self.screen.cursor.col);
        let cells = 1 + usize::from(wide);
        let blank = self.blank();
        let mut under = blank;
        if let Some(line) = self.screen.grid.get_mut(row) {
            // What this overwrites or parts may be half of a wide character: blank the other half.
            if line.get(col).is_some_and(|c| c.ch == '\0')
                && let Some(lead) = col.checked_sub(1).and_then(|i| line.get_mut(i))
            {
                *lead = blank;
            }
            match line.get_mut(col..) {
                Some(rest) if insert => rest.rotate_right(cells.min(rest.len())),
                _ => under = line.get(col).copied().filter(|c| c.ch != '\0').unwrap_or(blank),
            }
            // Written over, a row's first cell still says that the row goes on from the one
            // above: a line editor draws a long line again row by row. Erased, it does not.
            new.style.flags |= under.style.flags & CONT;
            if let Some(half) = line.get_mut(col + cells).filter(|c| c.ch == '\0') {
                *half = blank;
            }
            if let Some(cell) = line.get_mut(col) {
                *cell = new;
            }
            if let Some(cell) = line.get_mut(col + 1).filter(|_| wide) {
                *cell = Cell { ch: '\0', more: ['\0'; MORE], style: new.style, link: new.link };
            }
        }
        if insert {
            self.tidy(row);
        }
        self.uncover(row, col, col + cells);
        if let Some(last) = &mut self.cluster {
            last.at = Some((row, col, new, under));
        }
        self.screen.cursor.col += cells;
        if self.screen.cursor.col >= self.screen.cols {
            self.screen.cursor.col = self.screen.cols - 1;
            // The wrap waits even while wrapping is off, as in xterm: turned on again, it is done.
            self.screen.cursor.wrap = true;
        }
    }

    fn save_cursor(&mut self) {
        if let Some(slot) = self.saved.get_mut(usize::from(self.screen.alt)) {
            *slot = self.screen.cursor;
        }
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved.get(usize::from(self.screen.alt)).copied().unwrap_or_default();
        let (row, col) = (saved.row.min(self.screen.rows - 1), saved.col.min(self.screen.cols - 1));
        self.screen.cursor = Cursor { row, col, wrap: saved.wrap && saved.col + 1 == self.screen.cols, ..saved };
    }

    fn set_alt(&mut self, alt: bool, clear: bool) {
        if alt != self.screen.alt {
            std::mem::swap(&mut self.screen.grid, &mut self.other);
            // (A character still being joined is of the screen it was printed on.)
            self.cluster = None;
            std::mem::swap(&mut self.screen.pictures, &mut self.other_pictures);
            self.screen.alt = alt;
        }
        if alt && clear {
            self.clear();
        }
    }

    fn clear(&mut self) {
        let blank = vec![Cell::blank(Style::default()); self.screen.cols];
        self.screen.grid.iter_mut().for_each(|row| row.clone_from(&blank));
        self.screen.pictures.clear();
    }

    /// Takes a new size (the nearest a terminal may have, as `new`). The main screen's
    /// text is wrapped anew: a line that was wrapped is one line again, and
    /// rows that no longer fit go to history from the top. The alternate
    /// screen is cut or padded where it is, and none of it goes to history.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.clamp(1, SIZE_MAX), rows.clamp(1, SIZE_MAX));
        if (cols, rows) == (self.screen.cols, self.screen.rows) {
            return;
        }
        // A command is being typed if the shell's last two marks are a prompt and where its
        // command begins, in that order and still on the screen.
        let pending = match self.marks.as_slice() {
            [.., (At::Screen(ar, ac), "prompt", _), (At::Screen(br, bc), "command", _)] if (br, bc) > (ar, ac) => {
                Some(((*ar, *ac), (*br, *bc)))
            }
            _ => None,
        };
        let blank = Cell::blank(Style::default());
        let last = self.going_on().and_then(|last| last.at).filter(|_| !self.screen.alt);
        let shown = (&mut self.screen.grid, &mut self.screen.pictures);
        let hidden = (&mut self.other, &mut self.other_pictures);
        let ((main, main_pictures), (cut, still)) = if self.screen.alt { (hidden, shown) } else { (shown, hidden) };
        // Places on the main screen that stay on their character, each with whether it waits to
        // wrap at its row's end. First the cursor, whose line is taken to reach as far as it
        // stands (behind the alternate screen, the saved cursor is the one that will come back);
        // then the saved cursor, the character printed last, two corners of each picture (the
        // upper one on the highest of its rows that is kept still), and the marks on the screen:
        // each in its row's text, and at a row's end rather than where the next row starts.
        let waits = |cursor: Cursor| (cursor.place().0, cursor.place().1, cursor.wrap);
        let (saved, cursor) = (waits(self.saved.first().copied().unwrap_or_default()), waits(self.screen.cursor));
        let mut places = vec![if self.screen.alt { saved } else { cursor }, saved];
        places.push(last.map_or(cursor, |(row, col, ..)| (row, col, false)));
        // (A picture's rows below the screen's last are counted on from the last.)
        let low = |p: &Picture| (p.row + p.rows - 1).min(main.len().saturating_sub(1));
        places.extend(main_pictures.iter().flat_map(|p| [(p.row, p.col, false), (low(p), p.col, false)]));
        let text = |row: usize| main.get(row).map_or(0, |cells| text_cells(cells).len());
        places.extend(self.marks.iter().filter_map(|mark| match mark.0 {
            At::Screen(row, col) => Some((row, col.min(text(row)), true)),
            At::Text(..) => None,
        }));
        // The rows the last resize sent to history are wrapped along: they come back if there is room.
        let lent = self.lent.take();
        let held = lent.as_ref().map_or(0, |lent| lent.rows.len());
        places.iter_mut().for_each(|place| place.0 += held);
        // (Of a picture's rows that are off the top, the last resize's are among them.)
        for (i, picture) in main_pictures.iter().enumerate() {
            places.get_mut(3 + 2 * i).into_iter().for_each(|place| place.0 -= picture.off.min(held));
        }
        let old = lent.iter().flat_map(|lent| &lent.rows).chain(main.iter());
        // While a shell reads a command, the line being typed is cut or padded at a new width,
        // not wrapped anew: the shell draws it again counting rows as it left them. Only where
        // that is sure to be the shell's own line: the cursor is at or after where the command
        // begins and on its line, and the prompt began that line or an earlier one (so nothing
        // before it on the line is finished output). Anything else is wrapped anew, whole.
        let typed = pending.filter(|_| !self.screen.alt && cols != self.screen.cols).and_then(|(prompt, command)| {
            let goes_on = |y: usize| main.get(y).is_some_and(|row| continues(row));
            let first = (1..=command.0).rev().find(|&y| !goes_on(y)).unwrap_or(0);
            let end = (command.0 + 1..main.len()).find(|&y| !goes_on(y)).unwrap_or(main.len());
            let ours = prompt.0 < first || prompt == (first, 0);
            let here = self.screen.cursor.place();
            (ours && here >= command && here.0 < end).then_some(first + held..end + held)
        });
        // (Its rows are one line still when each has been cut.)
        let mut wrapped = reflow(old, cols, &mut places, typed.unwrap_or(0..0));
        // (A cursor beside its line's text is kept inside the row; a picture keeps its column.)
        let place = |i: usize| places.get(i).copied().unwrap_or_default();
        let at = |i: usize| (place(i).0, if place(i).2 { place(i).1.min(cols - 1) } else { place(i).1 });
        // Blank rows below the cursor go first; a row with something on it is never dropped:
        // rows go to history from the top, and the cursor with them if text below it needs the room.
        while wrapped.len() > rows.max(at(0).0 + 1) && wrapped.last().is_some_and(|row| row.iter().all(|c| *c == blank))
        {
            wrapped.pop();
        }
        let up = wrapped.len().saturating_sub(rows);
        // The marks are where their text is now: on the screen, or with the rows above it a
        // place in the text, as history will have them.
        let mut moved = places.iter().skip(3 + 2 * main_pictures.len());
        for mark in &mut self.marks {
            if let At::Screen(row, col) = &mut mark.0
                && let Some(place) = moved.next()
            {
                (*row, *col) = (place.0, place.1.min(cols));
            }
        }
        // History is given the rows above the screen again. First what it had of them is taken
        // back (here what waits still, the rest by whoever took it) and it ends where it did before
        // them. The last of the rows, a screen's worth at most, are kept to come back another time.
        let (back, was) = lent.as_ref().map_or(([0, 0], self.tail), |lent| ([lent.bytes, lent.styled], lent.before));
        let given = [(&mut self.out.history, &mut self.out.unhistory), (&mut self.out.styled, &mut self.out.unstyled)];
        for ((given, taken), back) in given.into_iter().zip(back) {
            *taken += back.saturating_sub(given.len());
            given.truncate(given.floor_char_boundary(given.len().saturating_sub(back)));
        }
        self.tail = was;
        let texts = Term::texts(&wrapped, &self.marks, self.tail, 0..up);
        for (mark, at) in self.marks.iter_mut().zip(texts) {
            mark.0 = match at {
                At::Screen(row, col) => At::Screen(row.saturating_sub(up).min(rows - 1), col),
                text => text,
            };
        }
        let keep = up.saturating_sub(ABOVE_MAX / cols);
        let (mut before, mut full) = ((0, 0, self.tail), false);
        for (y, (row, goes_on)) in with_next(&wrapped).take(up).enumerate() {
            if y == keep {
                before = (self.out.history.len(), self.out.styled.len(), self.tail);
            }
            full |= !Term::row_history(row, goes_on, &self.own, &mut self.tail, &mut self.out);
        }
        let rows_lent: Vec<Vec<Cell>> = wrapped.drain(..up).skip(keep).collect();
        wrapped.resize(rows, vec![blank; cols]);
        // The other screen keeps its rows: one that loses half a wide character loses it whole.
        cut.resize_with(rows, Vec::new);
        for row in cut {
            if row.get(cols).is_some_and(|c| c.ch == '\0')
                && let Some(lead) = row.get_mut(cols - 1)
            {
                *lead = Cell::blank(lead.style);
            }
            row.resize(cols, blank);
        }
        // A picture stays with the cells it was over, if they are still one under the other and
        // one of its rows is on the screen: the rows above the screen are off it, and those the
        // last resize took are back with their text. Past the right edge and the last row it is
        // only not shown.
        let mut kept: Vec<Picture> = Vec::new();
        for (i, picture) in main_pictures.drain(..).enumerate() {
            let ((row, col, _), (below, under, _)) = (place(3 + 2 * i), place(4 + 2 * i));
            let below = below + picture.row + picture.rows - 1 - low(&picture);
            let back = picture.off.min(held);
            let (off, gone) = (picture.off - back, up.saturating_sub(row));
            let stacked = under == col && below == row + back + picture.rows - 1;
            // (One that has lost rows for good has its next row on the screen's first.)
            if stacked && below >= up && row < up + rows && (off == 0 || row <= up) {
                let rows = below + 1 - row - gone;
                kept.push(Picture { row: row.saturating_sub(up), col, rows, off: off + gone, ..picture });
            }
        }
        still.retain(|p| p.row < rows && p.col < cols);
        (*main, *main_pictures) = (wrapped, kept);
        let fit = |(row, col): (usize, usize)| (row.saturating_sub(up).min(rows - 1), col);
        let ((row, col), saved_at) = (fit(at(0)), fit(at(1)));
        let [main, _] = &mut self.saved;
        (main.row, main.col, main.wrap) = (saved_at.0, saved_at.1.min(cols - 1), saved_at.1 >= cols);
        self.interrupt();
        if self.screen.alt {
            self.cluster = None;
            (self.screen.cursor.row, self.screen.cursor.col, self.screen.cursor.wrap) =
                (self.screen.cursor.row.min(rows - 1), self.screen.cursor.col.min(cols - 1), false);
        } else {
            // A cursor past its row's last column is the one that waits to wrap there.
            let here = (row, col.min(cols - 1), col >= cols);
            (self.screen.cursor.row, self.screen.cursor.col, self.screen.cursor.wrap) = here;
            // The character printed last is where it was in the text: what comes next may join it.
            let (at_row, at_col) = fit(at(2));
            self.cluster = last.filter(|_| at(2).0 >= up).and_then(|(.., under)| {
                let at = Some((at_row, at_col, *self.screen.cell_at(at_row, at_col)?, under));
                Some(Last { at, cursor: here, ..self.cluster? })
            });
        }
        self.lent = (!rows_lent.is_empty()).then(|| Lent {
            rows: rows_lent,
            bytes: self.out.history.len() - before.0,
            styled: self.out.styled.len() - before.1,
            before: before.2,
        });
        if full {
            self.refuse(HISTORY_FULL);
        }
        (self.screen.cols, self.screen.rows) = (cols, rows);
        (self.top, self.bottom) = (0, rows - 1);
        self.settle();
    }

    /// Notes a refusal for the log, once until the holder has taken it.
    fn refuse(&mut self, why: &'static str) {
        if !self.out.refused.contains(&why) {
            self.out.refused.push(why);
        }
    }

    fn respond(&mut self, text: &str) {
        if self.out.reply.len() + text.len() > REPLY_MAX {
            self.refuse(DROPPED);
        } else {
            self.out.reply.extend_from_slice(text.as_bytes());
        }
    }

    // ---- the parser ------------------------------------------------------

    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
        self.settle();
    }

    fn byte(&mut self, byte: u8) {
        match self.state {
            State::Ground => self.ground(byte),
            State::Str => match byte {
                0x1b => self.state = State::StrEscape,
                0x07 if self.string.first() == Some(&b']') => self.end_string(Some("\x07")),
                0x18 | 0x1a => self.end_string(None),
                0x00..=0x1f | 0x7f => self.string_holed = true,
                _ if self.string.len() <= PICTURE_MAX => self.string.push(byte),
                // Past its bound a string is dropped, and said so at once: it may never end.
                // (The one byte too many is kept: the string's length says that it is dropped.)
                _ if self.string.len() == PICTURE_MAX + 1 => {
                    self.string.push(byte);
                    self.refuse("a control string over 4 MiB (a picture, a clipboard, a title) was dropped");
                }
                _ => {}
            },
            State::StrEscape => {
                self.end_string((byte == b'\\').then_some("\x1b\\"));
                if byte != b'\\' {
                    self.state = State::Escape;
                    self.byte(byte);
                }
            }
            _ => match byte {
                0x1b => self.state = State::Escape,
                0x18 | 0x1a => {
                    self.state = State::Ground;
                    self.interrupt();
                }
                0x00..=0x1f => self.control(byte),
                0x7f => {}
                _ => match self.state {
                    State::Escape => self.escape(byte),
                    State::EscapeInter(first) => self.escape_inter(first, byte),
                    _ => self.csi(byte),
                },
            },
        }
    }

    fn ground(&mut self, byte: u8) {
        if let Some(&first) = self.utf8.first() {
            if byte & 0xc0 == 0x80 {
                self.utf8.push(byte);
                if self.utf8.len() == first.leading_ones() as usize {
                    // A sequence of the right length for no character (written too long, a surrogate,
                    // past the last one) shows one mark, as in xterm.
                    let whole = std::str::from_utf8(&self.utf8).ok().and_then(|text| text.chars().next());
                    self.utf8.clear();
                    // A C1 control written as UTF-8 is no character and, as in xterm, no control: nothing.
                    if !matches!(whole, Some('\u{80}'..'\u{a0}')) {
                        self.print(whole.unwrap_or('\u{fffd}'));
                    }
                }
                return;
            }
            self.utf8.clear();
            self.print('\u{fffd}');
        }
        match byte {
            0x1b => self.state = State::Escape,
            0x00..=0x1f => self.control(byte),
            0x20..=0x7e => self.print(char::from(byte)),
            0xc2..=0xf4 => self.utf8.push(byte),
            0x7f => {}
            _ => self.print('\u{fffd}'),
        }
    }

    /// Something other than text came: REP has nothing to repeat.
    fn interrupt(&mut self) -> char {
        std::mem::take(&mut self.last)
    }

    fn control(&mut self, byte: u8) {
        self.interrupt();
        match byte {
            0x07 => self.notice(""),
            0x08 => self.move_to_col(self.screen.cursor.col.saturating_sub(1)),
            0x09 => self.tab(1, false),
            0x0a..=0x0c => self.linefeed(false),
            0x0d => self.move_to_col(0),
            0x0e => self.screen.cursor.shift = true,
            0x0f => self.screen.cursor.shift = false,
            _ => {}
        }
    }

    /// To the `n`th tab stop after the cursor, or before it (`back`): to the row's edge, if it has not as many.
    fn tab(&mut self, n: usize, back: bool) {
        let (col, stop) = (self.screen.cursor.col, |c: &usize| self.tabs.get(*c) == Some(&true));
        self.screen.cursor.col = match back {
            true => (0..col).rev().filter(stop).nth(n - 1).unwrap_or(0),
            false => (col + 1..self.screen.cols).filter(stop).nth(n - 1).unwrap_or(self.screen.cols - 1),
        };
        self.screen.cursor.wrap &= !back;
    }

    fn escape(&mut self, byte: u8) {
        self.state = State::Ground;
        if byte != b'[' {
            self.interrupt();
        }
        match byte {
            b'[' => {
                // (The room for its numbers is the last one's.)
                let mut params = std::mem::take(&mut self.csi.params);
                params.clear();
                (self.csi, self.state) = (Csi { params, ..Csi::default() }, State::Csi);
            }
            b']' | b'P' | b'X' | b'^' | b'_' => {
                (self.string, self.string_holed) = (vec![byte], false);
                self.state = State::Str;
            }
            0x20..=0x2f => self.state = State::EscapeInter(byte),
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(false),
            b'E' => {
                self.screen.cursor.col = 0;
                self.linefeed(false);
            }
            b'M' => self.reverse_linefeed(),
            b'H' => {
                if let Some(stop) = self.tabs.get_mut(self.screen.cursor.col) {
                    *stop = true;
                }
            }
            b'=' => self.screen.app_keypad = true,
            b'>' => self.screen.app_keypad = false,
            b'c' => {
                // A full reset. What was produced stays to be taken, and picture
                // numbers go on, so no file's name is used twice.
                let (told, out, title) =
                    (std::mem::take(&mut self.told), std::mem::take(&mut self.out), std::mem::take(&mut self.title));
                *self = Term {
                    told,
                    out,
                    title,
                    titles: std::mem::take(&mut self.titles),
                    next_picture: self.next_picture,
                    tail: self.tail,
                    marks: std::mem::take(&mut self.marks),
                    ..Term::new(self.screen.cols, self.screen.rows)
                };
                self.move_marks(|_, _| (0, 0));
            }
            _ => {}
        }
    }

    fn escape_inter(&mut self, first: u8, byte: u8) {
        if (0x20..=0x2f).contains(&byte) {
            return;
        }
        self.state = State::Ground;
        match (first, byte) {
            (b'(' | b')', _) => {
                if let Some(set) = self.screen.cursor.graphics.get_mut(usize::from(first == b')')) {
                    *set = byte == b'0';
                }
            }
            (b'#', b'8') => {
                let fill = Cell { ch: 'E', ..Cell::blank(Style::default()) };
                self.screen.grid.iter_mut().for_each(|row| row.fill(fill));
                self.screen.pictures.clear();
                // (As a screen that was erased whole: its marks, and what a resize took off its top.)
                self.move_marks(|_, _| (0, 0));
                self.lent = self.lent.take().filter(|_| self.screen.alt);
                // And, as in xterm, the whole screen scrolls again and the cursor goes home.
                (self.top, self.bottom, self.screen.cursor.origin) = (0, self.screen.rows - 1, false);
                self.move_to(0, 0);
            }
            _ => {}
        }
    }

    /// The end of a string: the BEL or ST it reached (an answer to it ends the same way), or none: it was broken off.
    fn end_string(&mut self, end: Option<&str>) {
        self.state = State::Ground;
        let string = std::mem::take(&mut self.string);
        if string.len() > PICTURE_MAX + 1 {
            return;
        }
        match string.split_first().zip(end) {
            Some(((b'P', data), _)) if data.starts_with(b"$q") && !self.string_holed => {
                self.report_setting(data.get(2..).unwrap_or_default());
            }
            Some(((b'P', data), _)) if data.starts_with(b"+q") && !self.string_holed => {
                self.report_caps(data.get(2..).unwrap_or_default());
            }
            Some(((b'P', data), _)) => {
                let header = data.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
                if data.get(header) == Some(&b'q') && data.iter().all(|b| (0x20..=0x7e).contains(b)) {
                    self.picture(data);
                }
            }
            Some(((b']', data), end)) => self.command(&String::from_utf8_lossy(data), end),
            _ => {}
        }
    }

    /// Report the selector's *current* setting. DECRPSS carries one CSI body.
    fn report_setting(&mut self, selector: &[u8]) {
        let body = match selector {
            b"m" => {
                let (style, mut body) = (self.screen.cursor.style, String::from("0"));
                // (In xterm's order: 1, 4, 5, 7, 8, 2, 3, 9.)
                let looks = [0, 3, 4, 5, 6, 1, 2, 7].iter().filter_map(|at| LOOKS.get(*at));
                for (code, bit, _) in looks.filter(|look| style.flags & look.1 != 0) {
                    let _ = match style.kind().filter(|_| *bit == UNDERLINE) {
                        Some(kind) => write!(body, ";4:{kind}"),
                        None => write!(body, ";{code}"),
                    };
                }
                for (base, color) in [(30, style.fg), (40, style.bg), (50, style.ul)] {
                    Style::colour(&mut body, base, color, true);
                }
                body + "m"
            }
            b"r" => format!("{};{}r", self.top + 1, self.bottom + 1),
            b" q" => format!("{} q", self.screen.cursor_shape),
            _ => return self.respond("\x1bP0$r\x1b\\"),
        };
        self.respond(&format!("\x1bP1$r{body}\x1b\\"));
    }

    /// One reply per name, including an unknown name in the middle of a list.
    fn report_caps(&mut self, request: &[u8]) {
        for name in request.split(|&b| b == b';') {
            let hex = (1..=64).contains(&name.len()) && name.len().is_multiple_of(2);
            let name = std::str::from_utf8(name).ok().filter(|name| hex && name.bytes().all(|b| b.is_ascii_hexdigit()));
            let name = name.unwrap_or_default();
            let pair = |at: usize| u8::from_str_radix(name.get(at..at + 2)?, 16).ok();
            let asked: Vec<u8> = (0..name.len()).step_by(2).filter_map(pair).collect();
            // (A name that is no name is "not valid" with nothing after; a capability that is
            // a yes has no value.)
            let known = CAPS.iter().find(|(cap, _)| cap.as_bytes() == asked);
            let value: String = known.iter().flat_map(|(_, value)| value.bytes()).map(|b| format!("{b:02X}")).collect();
            let (valid, is) = (u8::from(known.is_some()), if value.is_empty() { "" } else { "=" });
            self.respond(&format!("\x1bP{valid}+r{name}{is}{value}\x1b\\"));
        }
    }

    /// Something the program wants noticed: text without control characters, a kibibyte at most.
    pub fn notice(&mut self, text: &str) {
        let mut text: String = text.chars().filter(|c| !c.is_control()).collect();
        text.truncate(text.floor_char_boundary(1024));
        self.out.notice = Some(text);
    }

    /// An operating system command: a title (0, 2), the clipboard (52), or a question about a
    /// colour (4, 10, 11, 12 with `?`) or a colour set for this pane (the same with a value).
    fn command(&mut self, text: &str, end: &str) {
        let (number, rest) = text.split_once(';').unwrap_or((text, ""));
        match number {
            "0" | "2" => {
                let mut title = rest.chars().filter(|c| !c.is_control());
                self.title = title.by_ref().take(256).collect();
                if title.next().is_some() {
                    self.refuse("a title over 256 characters was cut");
                }
            }
            "4" | "10" | "11" | "12" => {
                // `4` takes pairs of a palette number and what to do with it; `10`, `11` and `12`
                // take one part each for the colours from theirs on (`10;?;?` asks for the
                // foreground and the background). A part that is `?` asks; any other sets the
                // colour for this pane, if it is one.
                let mut parts = rest.split(';');
                let first = FOREGROUND + number.parse::<u16>().map_or(0, |number| number.saturating_sub(10));
                let mut slots = first..=CURSOR;
                loop {
                    let slot = match (number, slots.next()) {
                        ("4", _) => parts.next().map(|index| index.parse::<u8>().ok().map(u16::from)),
                        (_, slot) => slot.map(Some),
                    };
                    let (Some(slot), Some(part)) = (slot, parts.next()) else { break };
                    match (slot, part) {
                        // (No answer is looked for that has no room, 28 bytes being the longest:
                        // what the string sets after it is set all the same.)
                        (Some(_), "?") if self.out.reply.len() + 28 > REPLY_MAX => self.refuse(DROPPED),
                        // Told what the program set; or else from the last line that starts with
                        // that name, if what follows the name is a colour (`#` and six hex
                        // digits; blanks around them do not matter).
                        (Some(slot), "?") => {
                            let (name, which) = match slot {
                                FOREGROUND => ("foreground".to_string(), "10".to_string()),
                                BACKGROUND => ("background".to_string(), "11".to_string()),
                                CURSOR => ("cursor".to_string(), "12".to_string()),
                                n => (n.to_string(), format!("4;{n}")),
                            };
                            let line = |line: &str| {
                                let mut words = line.split_whitespace();
                                let told = |told: &str| Some(told).filter(|told| told.len() == 7).and_then(colour);
                                (words.next() == Some(name.as_str())).then(|| words.next().and_then(told))
                            };
                            let told = || self.told.colours.lines().rev().find_map(line).flatten();
                            if let Some([r, g, b]) = self.own.get(&slot).copied().or_else(told) {
                                let answer = format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}");
                                self.respond(&format!("\x1b]{which};{answer}{end}"));
                            }
                        }
                        (Some(slot), set) => match colour(set) {
                            Some(rgb) => _ = self.own.insert(slot, rgb),
                            None => self.refuse("a colour was not set: it was not `#` with hex digits or `rgb:R/G/B`"),
                        },
                        (None, "?") => {}
                        (None, _) => self.refuse("a colour was not set: its number was not 0 to 255"),
                    }
                }
            }
            // The colours are given back: every palette number or those named (104), the
            // foreground (110), the background (111), the cursor's (112).
            "104" if rest.is_empty() => self.own.retain(|slot, _| *slot >= FOREGROUND),
            "104" => {
                rest.split(';').filter_map(|n| n.parse::<u8>().ok()).for_each(|n| _ = self.own.remove(&u16::from(n)))
            }
            "110" => _ = self.own.remove(&FOREGROUND),
            "111" => _ = self.own.remove(&BACKGROUND),
            "112" => _ = self.own.remove(&CURSOR),
            // The directory the program is in: `file://HOST/PATH`, the path's odd bytes as `%XX`.
            "7" => {
                let Some(uri) = rest.strip_prefix("file://") else { return };
                let Some((host, path)) = uri.find('/').and_then(|at| uri.split_at_checked(at)) else { return };
                let mut bytes = Vec::new();
                let mut rest = path.bytes();
                while let Some(byte) = rest.next() {
                    let hex = |rest: &mut std::str::Bytes| char::from(rest.next()?).to_digit(16);
                    let pair = |rest: &mut std::str::Bytes| Some((hex(rest)? * 16 + hex(rest)?) as u8);
                    bytes.push(if byte == b'%' { pair(&mut rest).unwrap_or(0) } else { byte });
                }
                // (A path that is no text, has a control character in it or is longer than a path
                // can be is not told: only that the program is on the machine it named.)
                let path = String::from_utf8(bytes)
                    .ok()
                    .filter(|path| path.len() <= 4096 && !path.chars().any(|c| c.is_control() || c == '\u{fffd}'));
                if path.is_some() || !host.is_empty() {
                    self.out.cwd = Some((host.to_string(), path.unwrap_or_default()));
                }
            }
            // A notice: `9;TEXT` (but `9;4;…` is a progress bar's), `777;notify;TITLE;BODY`, `99;…;TEXT`.
            "9" if !rest
                .split_once(';')
                .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
            {
                self.notice(rest)
            }
            "777" => {
                if let Some(text) = rest.strip_prefix("notify;") {
                    self.notice(&text.replacen(';', ": ", 1));
                }
            }
            // A link: `8;PARAMETERS;ADDRESS` starts one for what is printed from here on, and an
            // empty address ends it. An address is plain printable text, 2,048 bytes at most.
            "8" => {
                let address = rest.split_once(';').map_or("", |(_, address)| address);
                let plain =
                    !address.is_empty() && address.len() <= 2048 && address.bytes().all(|b| (0x21..=0x7e).contains(&b));
                self.link = if plain && !self.string_holed { self.link_number(address) } else { 0 };
            }
            // A shell marks its prompts: `133;A` where a prompt starts, `B` where the command
            // typed begins, `C` where its output begins, `D[;CODE]` where it ended. Each is kept
            // as the place the cursor is at.
            "133" if !self.screen.alt => {
                let kind = match rest.split(';').next() {
                    Some("A") => "prompt",
                    Some("B") => "command",
                    Some("C") => "output",
                    Some("D") => "end",
                    _ => return,
                };
                let code = rest.split(';').nth(1).filter(|_| kind == "end").and_then(|code| code.parse::<u8>().ok());
                let (row, col) = self.screen.cursor.place();
                let here = At::Screen(row, col);
                // A prompt before a mark says that mark's text is there no more (the prompt was
                // drawn again higher up): the mark is kept, at the prompt's place, so that what
                // was marked before is still in its order, with nothing between. Only a prompt
                // says so: a program that moved up and ended has erased nothing.
                if kind == "prompt" {
                    self.move_marks(|r, c| (r, c).min((row, col)));
                    // (A prompt drawn again in its place is one prompt.)
                    while self.marks.last().is_some_and(|last| last.0 == here && matches!(last.1, "prompt" | "command"))
                    {
                        self.marks.pop();
                    }
                }
                self.marks.push((here, kind, code));
                let extra = self.marks.len().saturating_sub(1000);
                self.marks.drain(..extra);
            }
            // (Of kitty's form only plain words to show: no questions, no parts, nothing encoded.)
            "99" => {
                let (about, words) = rest.split_once(';').unwrap_or_default();
                let plain = |field: &str| {
                    matches!(
                        field.split_once('='),
                        Some(("i" | "o" | "u" | "a" | "t" | "f" | "c" | "w" | "s" | "g" | "n", _))
                            | Some(("p", "title" | "body"))
                            | Some(("d", "1"))
                            | Some(("e", "0"))
                            | None
                    )
                };
                if !words.is_empty() && about.split(':').all(plain) {
                    self.notice(words);
                }
            }
            "52" => match rest.split_once(';').map(|(_, what)| what) {
                // Setting only: a program is never told what is on the clipboard.
                Some("?") => self.refuse("a program asked to read the clipboard: that is refused"),
                what => match what.and_then(base64_decode) {
                    Some(bytes) if bytes.len() <= 1 << 20 => self.out.clipboard = Some(bytes),
                    Some(_) => self.refuse("a clipboard over 1 MiB was dropped"),
                    None => self.refuse("a clipboard that was not base64 was dropped"),
                },
            },
            _ => {}
        }
    }

    /// Places a sixel picture at the cursor and moves the cursor below it.
    fn picture(&mut self, data: &[u8]) {
        let (width, height, _) = sixel::size(data);
        let cols = width.div_ceil(self.told.cell.0.max(1)).max(1);
        let rows = height.div_ceil(self.told.cell.1.max(1)).max(1);
        if cols > SIZE_MAX || rows > SIZE_MAX {
            return self.refuse("a picture over 500 cells wide or high was dropped");
        }
        if self.screen.pictures.len() >= PICTURES_MAX {
            self.screen.pictures.remove(0);
            self.refuse("a screen keeps 8 pictures: the oldest was dropped for a new one");
        }
        let bytes = Some([b"\x1bP", data, b"\x1b\\"].concat());
        let Cursor { row, col, .. } = self.screen.cursor;
        self.screen.pictures.push(Picture { id: self.next_picture, row, col, cols, rows, off: 0, bytes });
        self.next_picture = self.next_picture.wrapping_add(1);
        // The picture scrolls with the text; of one taller than the screen the last rows stay.
        for _ in 0..rows {
            self.linefeed(false);
        }
        self.move_to_col(0);
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            // After an intermediate byte only the last byte may come: anything else spoils the sequence.
            b'0'..=b'?' if self.csi.inter != 0 => self.csi.too_long = true,
            b'0'..=b'9' => {
                // (A number past what is kept is the largest kept, and remembered as too large.)
                let more = u32::from(self.csi.param.unwrap_or(0)) * 10 + u32::from(byte - b'0');
                self.csi.huge |= more > u32::from(u16::MAX);
                self.csi.param = Some(u16::try_from(more).unwrap_or(u16::MAX));
            }
            b';' | b':' | 0x40..=0x7e => {
                // More parameters than are kept: the whole sequence is ignored, and said so.
                self.csi.too_long |= self.csi.params.len() >= 32;
                if !self.csi.too_long {
                    // (A first number that was not given is 0 here; one sequence tells them apart.)
                    self.csi.bare = if self.csi.params.is_empty() { self.csi.param.is_none() } else { self.csi.bare };
                    self.csi.params.push((self.csi.param.unwrap_or(0), self.csi.sub));
                }
                self.csi.param = None;
                self.csi.sub = byte == b':';
                if byte >= 0x40 {
                    self.state = State::Ground;
                    if self.csi.too_long {
                        self.refuse("a control sequence that was malformed or had more than 32 parameters was ignored");
                    } else {
                        self.dispatch(byte);
                    }
                }
            }
            // `?` and its like count only before the parameters; elsewhere the sequence means nothing.
            b'<'..=b'?' if self.csi.params.is_empty() && self.csi.param.is_none() && self.csi.private == 0 => {
                self.csi.private = byte
            }
            b'<'..=b'?' => self.csi.too_long = true,
            0x20..=0x2f => self.csi.inter = byte,
            _ => self.state = State::Ground,
        }
    }

    /// Parameter `i`, or `default` when it is missing or zero.
    fn arg(&self, i: usize, default: usize) -> usize {
        self.csi.params.get(i).filter(|p| p.0 != 0).map_or(default, |p| usize::from(p.0))
    }

    fn dispatch(&mut self, last: u8) {
        let n = self.arg(0, 1);
        let (row, col) = (self.screen.cursor.row, self.screen.cursor.col);
        // What REP repeats: the character printed just before it, with nothing between (as xterm).
        let printed = self.interrupt();
        match (self.csi.private, self.csi.inter, last) {
            (0, 0, b'A' | b'B') => self.move_rows(n, last == b'B'),
            (0, 0, b'e') => {
                self.screen.cursor.row = row.saturating_add(n).min(self.screen.rows - 1);
                self.screen.cursor.wrap = false;
            }
            (0, 0, b'C' | b'a') => self.move_to_col(col + n),
            (0, 0, b'D') => self.move_to_col(col.saturating_sub(n)),
            (0, 0, b'E' | b'F') => {
                self.move_rows(n, last == b'E');
                self.move_to_col(0);
            }
            (0, 0, b'G' | b'`') => self.move_to_col(n - 1),
            (0, 0, b'H' | b'f') => self.move_to(n - 1, self.arg(1, 1) - 1),
            (0, 0, b'd') => self.move_to(n - 1, col),
            (0, 0, b'I' | b'Z') => self.tab(n, last == b'Z'),
            // (The marks on what is erased are, in their order, where the erased part begins:
            // what they marked is gone, and they mark nothing of what is written there next.)
            (0 | b'?', 0, b'J') => match self.arg(0, 0) {
                0 => {
                    self.erase(row, col, self.screen.cols);
                    (row + 1..self.screen.rows).for_each(|r| self.erase(r, 0, self.screen.cols));
                    self.move_marks(|r, c| (r.min(row), if (r, c) > (row, col) { col } else { c }));
                }
                1 => {
                    (0..row).for_each(|r| self.erase(r, 0, self.screen.cols));
                    self.erase(row, 0, col + 1);
                    // (The place right after what was erased is its end too.)
                    self.move_marks(|r, c| if (r, c) <= (row, col + 1) { (0, 0) } else { (r, c) });
                }
                2 => {
                    (0..self.screen.rows).for_each(|r| self.erase(r, 0, self.screen.cols));
                    self.move_marks(|_, _| (0, 0));
                    // A screen that was cleared is not given back what a resize took off its top.
                    self.lent = self.lent.take().filter(|_| self.screen.alt);
                }
                _ => {}
            },
            (0 | b'?', 0, b'K') => match self.arg(0, 0) {
                0 => self.erase(row, col, self.screen.cols),
                1 => self.erase(row, 0, col + 1),
                2 => self.erase(row, 0, self.screen.cols),
                _ => {}
            },
            (0, 0, b'X') => self.erase(row, col, col.saturating_add(n)),
            (0, 0, b'@' | b'P') => {
                let goes_on = col == 0 && self.screen.grid.get(row).is_some_and(|cells| continues(cells));
                self.split(row, col);
                if last == b'P' {
                    self.split(row, col.saturating_add(n));
                }
                let (blank, cols) = (self.blank(), self.screen.cols);
                // (The marks from there on go with their cells: one on a cell taken out, to where that part begins.)
                let to = |c: usize| if last == b'@' { (c + n).min(cols) } else { c.saturating_sub(n).max(col) };
                self.move_marks(|r, c| (r, if r == row && c >= col { to(c) } else { c }));
                if let Some(rest) = self.screen.grid.get_mut(row).and_then(|r| r.get_mut(col..)) {
                    let n = n.min(rest.len());
                    let keep = rest.len() - n;
                    if last == b'@' {
                        rest.copy_within(..keep, n);
                        rest.split_at_mut(n).0.fill(blank);
                    } else {
                        rest.copy_within(n.., 0);
                        rest.split_at_mut(keep).1.fill(blank);
                    }
                    // (That the row goes on from the one above is said by its first cell, whichever that is now.)
                    rest.iter_mut().for_each(|cell| cell.style.flags &= !CONT);
                    rest.first_mut().into_iter().for_each(|first| first.style.flags |= u16::from(goes_on) * CONT);
                }
                self.tidy(row);
                self.screen.cursor.wrap = false;
            }
            (0, 0, b'L' | b'M') if (self.top..=self.bottom).contains(&row) => {
                if last == b'L' {
                    self.scroll_down(row, n);
                } else {
                    self.scroll_up(row, n, false);
                }
                self.move_to_col(0);
            }
            (0, 0, b'S') => self.scroll_up(self.top, n, false),
            (0, 0, b'T') => self.scroll_down(self.top, n),
            (0, 0, b'b') => {
                for _ in 0..n * usize::from(printed != '\0') {
                    // Each one a character of its own, whatever would join it to the one before.
                    self.cluster = None;
                    self.print(printed);
                }
                self.interrupt();
            }
            (0, 0, b'c') => self.respond("\x1b[?62;4;22c"),
            (b'>', 0, b'q') if self.csi.params.len() == 1 && self.arg(0, 0) == 0 => {
                self.respond(&format!("\x1bP>|{}\x1b\\", self.told.name));
            }
            (b'>', 0, b'c') if self.csi.params.len() == 1 && self.arg(0, 0) == 0 => self.respond("\x1b[>70;0;0c"),
            (0, 0, b'g') => match self.arg(0, 0) {
                0 => {
                    if let Some(stop) = self.tabs.get_mut(col) {
                        *stop = false;
                    }
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, 0, b'h' | b'l') if self.csi.params.iter().any(|p| p.0 == 4) => self.insert = last == b'h',
            (b'?', 0, b'h' | b'l') => {
                for i in 0..self.csi.params.len() {
                    self.private_mode(self.arg(i, 0), last == b'h');
                }
            }
            (0, 0, b'm') => self.sgr(),
            (0, 0, b'n') => match self.arg(0, 0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let row = if self.screen.cursor.origin { row.saturating_sub(self.top) } else { row };
                    self.respond(&format!("\x1b[{};{}R", row + 1, col + 1));
                }
                _ => {}
            },
            (b'?', b'$', b'p') => {
                let mode = self.arg(0, 0);
                let set = match mode {
                    47 | 1047 | 1049 => Some(self.screen.alt),
                    9 | 1000 | 1002 | 1003 => Some(usize::from(self.screen.mouse) == mode),
                    _ => self.switch(mode).map(|on| *on),
                };
                // Text is always cut into grapheme clusters (2027): permanently set.
                let answer = if mode == 2027 { 3 } else { set.map_or(0, |on| 2 - u8::from(on)) };
                self.respond(&format!("\x1b[?{mode};{answer}$y"));
            }
            (0, b'!', b'p') => {
                // A soft reset: modes and the scroll region back to how they start.
                (self.insert, self.autowrap, self.screen.cursor_shown, self.link) = (false, true, true, 0);
                (self.screen.app_cursor, self.screen.app_keypad) = (false, false);
                (self.screen.mouse, self.screen.mouse_sgr) = (0, false);
                (self.top, self.bottom) = (0, self.screen.rows - 1);
                // (The cursor stays where it is, and is otherwise as it starts.)
                let Cursor { row, col, wrap, .. } = self.screen.cursor;
                self.screen.cursor = Cursor { row, col, wrap, ..Cursor::default() };
                self.saved = [Cursor::default(); 2];
            }
            (0, b' ', b'q') if self.arg(0, 0) <= 6 => self.screen.cursor_shape = self.arg(0, 0) as u8,
            (0, 0, b'r') => {
                let (top, bottom) = (n - 1, self.arg(1, self.screen.rows).min(self.screen.rows) - 1);
                if top < bottom {
                    (self.top, self.bottom) = (top, bottom);
                    self.move_to(0, 0);
                }
            }
            (0, 0, b's') => self.save_cursor(),
            (0, 0, b'u') => self.restore_cursor(),
            // The kitty keyboard protocol: a program says how it wants keys told (`=` sets, `>`
            // sets and keeps the old flags to come back to, `<` comes back) and asks what is in
            // force (`?`). Each screen has its own. Only what the terminal that sends the keys
            // takes is put in force, as that is known when the program asks: with no such
            // terminal known, nothing is, and the answer is 0. What a program was told stays so.
            (b'=' | b'>' | b'<' | b'?', 0, b'u') => {
                let alt = usize::from(self.screen.alt);
                // (A number too large to be flags is no flags, not the largest number kept.)
                let given = if self.csi.huge { 0 } else { self.arg(0, 0) };
                let (asked, how) = ((given & usize::from(KEYS & self.told.keyboard)) as u8, self.arg(1, 1));
                // (A flag in force may be put out of force whatever the terminal takes by now.)
                let out = (given & usize::from(KEYS)) as u8;
                // (Going back takes a count: one if none is given, none if it is 0.)
                let (back, which) = (if self.csi.bare { 1 } else { self.arg(0, 0) }, self.csi.private);
                let Some((flags, kept)) = self.screen.keys.get_mut(alt) else { return };
                match which {
                    b'=' => {
                        *flags =
                            [asked, *flags | asked, *flags & !out].get(how.wrapping_sub(1)).copied().unwrap_or(*flags)
                    }
                    b'>' => {
                        // The stack is bounded: the oldest entry makes room.
                        if kept.len() == KEYS_KEPT {
                            kept.remove(0);
                        }
                        kept.push(std::mem::replace(flags, asked));
                    }
                    b'<' => (0..back.min(KEYS_KEPT + 1)).for_each(|_| *flags = kept.pop().unwrap_or(0)),
                    _ => {
                        let answer = format!("\x1b[?{}u", *flags);
                        self.respond(&answer);
                    }
                }
            }
            (0, 0, b't') => {
                let (cols, rows, (w, h)) = (self.screen.cols, self.screen.rows, self.told.cell);
                match self.arg(0, 0) {
                    14 => self.respond(&format!("\x1b[4;{};{}t", rows * h, cols * w)),
                    16 => self.respond(&format!("\x1b[6;{h};{w}t")),
                    18 => self.respond(&format!("\x1b[8;{rows};{cols}t")),
                    // The title put aside (the oldest is forgotten past 8), and brought back.
                    // (`22;1` and `23;1` are the icon's name, which is not kept.)
                    22 if self.arg(1, 0) != 1 => {
                        if self.titles.len() >= 8 {
                            self.titles.remove(0);
                            self.refuse("more than 8 titles were put aside: the oldest was forgotten");
                        }
                        self.titles.push(self.title.clone());
                    }
                    23 if self.arg(1, 0) != 1 => {
                        self.titles.pop().into_iter().for_each(|title| self.title = title);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn move_rows(&mut self, n: usize, down: bool) {
        // The cursor stops at the scroll region's edge, unless it is already past it.
        self.screen.cursor.row = if down {
            let limit = if self.screen.cursor.row <= self.bottom { self.bottom } else { self.screen.rows - 1 };
            self.screen.cursor.row.saturating_add(n).min(limit)
        } else {
            let limit = if self.screen.cursor.row >= self.top { self.top } else { 0 };
            self.screen.cursor.row.saturating_sub(n).max(limit)
        };
        self.screen.cursor.wrap = false;
    }

    fn move_to_col(&mut self, col: usize) {
        self.screen.cursor.col = col.min(self.screen.cols - 1);
        self.screen.cursor.wrap = false;
    }

    /// Where a mode that is one switch is kept: what sets it, resets it and answers for it.
    fn switch(&mut self, mode: usize) -> Option<&mut bool> {
        Some(match mode {
            1 => &mut self.screen.app_cursor,
            6 => &mut self.screen.cursor.origin,
            7 => &mut self.autowrap,
            25 => &mut self.screen.cursor_shown,
            1004 => &mut self.screen.focus,
            1006 => &mut self.screen.mouse_sgr,
            2004 => &mut self.screen.paste,
            2026 => &mut self.screen.sync,
            _ => return None,
        })
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        self.switch(mode).into_iter().for_each(|switch| *switch = on);
        match mode {
            6 => self.move_to(0, 0),
            47 => self.set_alt(on, false),
            1047 => {
                if !on && self.screen.alt {
                    self.clear();
                }
                // Asked for while already there, it is left as it is (as in xterm).
                self.set_alt(on, on && !self.screen.alt);
            }
            1048 if on => self.save_cursor(),
            1048 => self.restore_cursor(),
            1049 if on => {
                self.save_cursor();
                self.set_alt(true, true);
            }
            1049 => {
                self.set_alt(false, false);
                self.restore_cursor();
            }
            9 | 1000 | 1002 | 1003 => self.screen.mouse = if on { mode as u16 } else { 0 },
            _ => {}
        }
    }

    fn sgr(&mut self) {
        let mut i = 0;
        while let Some(&(code, _)) = self.csi.params.get(i) {
            i += 1;
            let flag = |flags: u16, bit: u16, on: bool| if on { flags | bit } else { flags & !bit };
            let style = &mut self.screen.cursor.style;
            match code {
                0 => *style = Style::default(),
                4 | 21 => {
                    // `4:0` is "no underline"; `4:2` to `4:5` (and 21) are double, curly, dotted, dashed.
                    let kind = self.csi.params.get(i).filter(|p| p.1).map_or(if code == 21 { 2 } else { 1 }, |p| p.0);
                    style.flags = flag(style.flags & !KINDS, UNDERLINE, kind != 0);
                    style.flags |= KIND.get(usize::from(kind).wrapping_sub(2)).copied().unwrap_or(0);
                }
                24 => style.flags &= !(UNDERLINE | KINDS),
                // (6 blinks as 5 does.)
                1..=9 | 22..=29 => {
                    for (set, bit, reset) in LOOKS {
                        let on = style.flags & bit != 0 || code == set || (code, set) == (6, 5);
                        style.flags = flag(style.flags, bit, on && code != reset);
                    }
                }
                59 => style.ul = Color::Default,
                30..=37 => style.fg = Color::Indexed((code - 30) as u8),
                39 => style.fg = Color::Default,
                40..=47 => style.bg = Color::Indexed((code - 40) as u8),
                49 => style.bg = Color::Default,
                90..=97 => style.fg = Color::Indexed((code - 82) as u8),
                100..=107 => style.bg = Color::Indexed((code - 92) as u8),
                38 | 48 | 58 => {
                    let (color, used) = self.long_color(i);
                    i += used;
                    match (code, color) {
                        (38, Some(color)) => self.screen.cursor.style.fg = color,
                        (48, Some(color)) => self.screen.cursor.style.bg = color,
                        (58, Some(color)) => self.screen.cursor.style.ul = color,
                        _ => {}
                    }
                }
                _ => {}
            }
            // Skip what is left of this code's own `:` parameters.
            while matches!(self.csi.params.get(i), Some(&(_, true))) {
                i += 1;
            }
        }
    }

    /// The colour after a 38 or 48 at `i`, and how many parameters it used:
    /// `5;N`, `2;R;G;B`, or the same with `:` (where a colour space may come first).
    fn long_color(&self, i: usize) -> (Option<Color>, usize) {
        let value = |k: usize| self.csi.params.get(i + k).and_then(|p| u8::try_from(p.0).ok());
        let rgb = |at: usize| Some(Color::Rgb(value(at)?, value(at + 1)?, value(at + 2)?));
        let colons = self.csi.params.get(i).is_some_and(|p| p.1);
        let count = self.csi.params.iter().skip(i).take_while(|p| p.1).count();
        let left = self.csi.params.len().saturating_sub(i);
        // A colour with a part missing or over 255 sets nothing.
        match self.csi.params.get(i).map(|p| p.0) {
            Some(5) if colons => (value(1).filter(|_| count >= 2).map(Color::Indexed), count),
            Some(5) => (value(1).map(Color::Indexed), left.min(2)),
            Some(2) if colons => {
                let at = if count >= 5 { 2 } else { 1 };
                (rgb(at).filter(|_| at + 2 < count), count)
            }
            Some(2) => (rgb(1), left.min(4)),
            _ => (None, if colons { count } else { 0 }),
        }
    }
}

/// Base64, as the clipboard command carries it. `None` for text that is not:
/// a character outside the set, padding anywhere but at the end, or a length no bytes give.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    // Padding, if there is any, must be the padding of that length; and what is left over of
    // the last character must be nothing.
    let padded = text.len() - text.trim_end_matches('=').len();
    let text = text.trim_end_matches('=');
    if text.len() % 4 == 1 || (padded > 0 && !(text.len() + padded).is_multiple_of(4)) || padded > 2 {
        return None;
    }
    let (mut out, mut bits, mut have) = (Vec::new(), 0u32, 0u32);
    for byte in text.bytes() {
        let six = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        (bits, have) = ((bits << 6) | u32::from(six), have + 6);
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    (bits == 0).then_some(out)
}

pub fn base64_encode(bytes: &[u8]) -> String {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let three = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            let six = SET.get((three >> (18 - 6 * i)) as usize & 63).copied().unwrap_or(b'=');
            out.push(if i <= chunk.len() { char::from(six) } else { '=' });
        }
    }
    out
}

/// Each row of a screen, with whether the next goes on from it.
fn with_next(grid: &[Vec<Cell>]) -> impl Iterator<Item = (&Vec<Cell>, bool)> {
    grid.iter().zip(grid.iter().skip(1).map(|next| continues(next)).chain([false]))
}

/// The cells of a row that are text if its line goes on in the next: all but the gap a wide
/// character left at its end.
fn text_cells(row: &[Cell]) -> &[Cell] {
    let gap = row.last().is_some_and(|cell| cell.style.flags & PAD != 0);
    row.get(..row.len() - usize::from(gap)).unwrap_or(row)
}

/// Whether a row goes on from the one above it.
fn continues(row: &[Cell]) -> bool {
    row.first().is_some_and(|cell| cell.style.flags & CONT != 0)
}

/// The rows of a screen wrapped anew for `cols` columns: a row and those that
/// go on from it are one line, cut again where the new width ends, never
/// inside a character. `places` are rows and columns of the old rows, each
/// with whether it waits to wrap at its row's end, and come back as rows and
/// columns of the new; the first is the cursor, whose line is taken to reach
/// as far as it stands.
fn reflow<'a>(
    old: impl Iterator<Item = &'a Vec<Cell>>,
    cols: usize,
    places: &mut [(usize, usize, bool)],
    keep: std::ops::Range<usize>,
) -> Vec<Vec<Cell>> {
    let blank = Cell::blank(Style::default());
    let plain = |cell: &Cell| Cell { style: Style { flags: cell.style.flags & !MARKS, ..cell.style }, ..*cell };
    // The lines, what fills the rest of each one's last row (the colour of the blanks it ended
    // in), and whether its first row goes on from the row before still (history's last, or a row
    // that is kept as it is); and for each old row the line it is part of and where in it the row starts.
    let mut lines: Vec<(Vec<Cell>, Cell, bool)> = Vec::new();
    let mut origin: Vec<(usize, usize)> = Vec::new();
    let mut old = old.enumerate().peekable();
    while let Some((y, row)) = old.next() {
        if lines.is_empty() || !continues(row) || keep.contains(&y) {
            lines.push((Vec::new(), blank, continues(row)));
        }
        let goes_on = old.peek().is_some_and(|(_, next)| continues(next)) && !keep.contains(&y);
        // (What is cut off a line's end is what is the same as the fill, and nothing else.)
        let end = row.last().map_or(blank, |last| Cell::blank(last.style));
        let mut used = match goes_on {
            true => text_cells(row).len(),
            false => row.iter().rposition(|cell| plain(cell) != end).map_or(0, |last| last + 1),
        };
        if places.first().is_some_and(|cursor| cursor.0 == y) && !goes_on {
            used = used.max(places.first().map_or(0, |cursor| cursor.1).min(row.len()));
        }
        if keep.contains(&y) {
            // Cut, never inside a wide character.
            used = used.min(cols - usize::from(row.get(cols).is_some_and(|half| half.ch == '\0')));
        }
        let number = lines.len().saturating_sub(1);
        let Some((line, fill, _)) = lines.last_mut() else { continue };
        origin.push((number, line.len()));
        line.extend(row.iter().take(used).map(plain));
        *fill = if used < row.len() { end } else { blank };
    }
    // The new rows, and for each the line it is part of and where in it the row starts.
    let (mut rows, mut starts): (Vec<Vec<Cell>>, Vec<(usize, usize)>) = (Vec::new(), Vec::new());
    for (number, (line, fill, on)) in lines.iter().enumerate() {
        let (mut row, first): (Vec<Cell>, usize) = (Vec::with_capacity(cols), rows.len());
        starts.push((number, 0));
        let mut i = 0;
        while let Some(cell) = line.get(i) {
            let unit = 1 + usize::from(line.get(i + 1).is_some_and(|half| half.ch == '\0' && cell.ch != '\0'));
            // A wide character on a screen one column wide: a blank stands where it was.
            let room = unit.min(cols);
            if row.len() + room > cols {
                // The row is full, or a wide character leaves a gap at its end; the next goes on from it.
                row.resize(cols, Cell { style: Style { flags: PAD, ..blank.style }, ..blank });
                rows.push(std::mem::replace(&mut row, Vec::with_capacity(cols)));
                starts.push((number, i));
            }
            let from = row.len();
            match line.get(i..i + unit).filter(|_| room == unit) {
                Some(cells) => row.extend(cells),
                None => row.push(Cell::blank(cell.style)),
            }
            if let Some(first) = row.get_mut(from).filter(|_| from == 0 && i > 0) {
                first.style.flags |= CONT;
            }
            i += unit;
        }
        row.resize(cols, *fill);
        rows.push(row);
        if let Some(cell) = rows.get_mut(first).and_then(|row| row.first_mut()).filter(|_| *on) {
            cell.style.flags |= CONT;
        }
    }
    for place in places.iter_mut() {
        let (line, offset) = origin.get(place.0).map_or((usize::MAX, 0), |&(line, start)| (line, start + place.1));
        // The last new row of that line that starts at or before the place; for one that waits
        // at a row's end, before it: where one row ends and the next starts is two places.
        let last =
            |strict: bool| starts.iter().rposition(|&(of, start)| of == line && start + usize::from(strict) <= offset);
        let row = last(place.2).or_else(|| last(false));
        let column = row.map_or(0, |row| offset - starts.get(row).map_or(0, |start| start.1));
        // Past its line's text a place is beside the text, as many columns on as it was (which
        // is said of it); right after the text of a full row it is the place that waits to wrap
        // (one past the last column).
        let beside = lines.get(line).is_some_and(|(text, ..)| offset > text.len());
        *place = (row.unwrap_or(0), column, beside);
    }
    rows
}

/// The DEC line-drawing set, for programs that draw boxes with `ESC ( 0`.
fn line_drawing(c: char) -> char {
    const SET: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
    match c {
        '`'..='~' => SET.chars().nth(c as usize - '`' as usize).unwrap_or(c),
        '_' => ' ',
        _ => c,
    }
}
