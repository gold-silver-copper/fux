//! The terminal emulator: bytes in, a grid of cells out. It answers the
//! queries programs wait on, keeps the lines that scroll off the top, and
//! takes sixel pictures whole. It knows nothing of files or processes.

use crate::width::{Joiner, plain, spread, width};
use std::fmt::Write as _;

/// The most bytes one picture may have; a longer one is dropped and noted.
pub const PICTURE_MAX: usize = 4 << 20;
/// The longest title kept (in characters) and the largest clipboard (in bytes); more is cut or refused.
const TITLE_MAX: usize = 256;
const CLIPBOARD_MAX: usize = 1 << 20;
/// The most pictures a screen keeps; a new one past it drops the oldest.
pub const PICTURES_MAX: usize = 8;
const PARAMS_MAX: usize = 32;
/// The most columns and the most rows a screen may have.
pub const SIZE_MAX: usize = 500;
/// The most history kept between two looks by the holder; lines past it are dropped and noted.
const HISTORY_PENDING_MAX: usize = 4 << 20;
/// (Room for an answer to every question about a colour that a viewer asks at its start.)
const REPLY_MAX: usize = 16 << 10;
/// The keyboard flags fux passes on (all of the kitty protocol's: escape codes told apart,
/// presses and releases, alternate keys, every key as an escape code, the text a key gives),
/// and how many sets of flags a program can push aside on a screen.
pub const KEYS: u8 = 31;
const KEYS_KEPT: usize = 16;
/// How many of a shell's marks are kept.
const MARKS_MAX: usize = 1000;
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
pub const ITALIC: u16 = 4;
pub const UNDERLINE: u16 = 8;
pub const BLINK: u16 = 16;
pub const REVERSE: u16 = 32;
pub const HIDDEN: u16 = 64;
pub const STRIKE: u16 = 128;
/// The kind of an underline that is not one plain line (with `UNDERLINE`).
pub const DOUBLE: u16 = 1024;
pub const CURLY: u16 = 2048;
pub const DOTTED: u16 = 4096;
pub const DASHED: u16 = 8192;
const KINDS: u16 = DOUBLE | CURLY | DOTTED | DASHED;
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
    /// The escape code that sets exactly this style.
    pub fn sgr(self, out: &mut String) {
        out.push_str("\x1b[0");
        for (bit, code) in
            [BOLD, DIM, ITALIC, UNDERLINE, BLINK, REVERSE, HIDDEN, STRIKE].iter().zip([1, 2, 3, 4, 5, 7, 8, 9])
        {
            if self.flags & bit != 0 {
                let _ = write!(out, ";{code}");
            }
        }
        for (color, base) in [(self.fg, 30), (self.bg, 40)] {
            let _ = match color {
                Color::Default => Ok(()),
                Color::Indexed(n @ 0..8) => write!(out, ";{}", base + u16::from(n)),
                Color::Indexed(n @ 8..16) => write!(out, ";{}", base + 52 + u16::from(n)),
                Color::Indexed(n) => write!(out, ";{};5;{n}", base + 8),
                Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
            };
        }
        out.push('m');
        // An underline's kind and colour come in a code of their own, in the form with colons:
        // a terminal that does not know that form passes the whole code over, and has the
        // plain underline and the colours from the one before.
        let kind = [DOUBLE, CURLY, DOTTED, DASHED].iter().position(|kind| self.flags & kind != 0);
        let kind = kind.map(|kind| format!("4:{}", kind + 2));
        let colour = match self.ul {
            Color::Default => None,
            Color::Indexed(n) => Some(format!("58:5:{n}")),
            Color::Rgb(r, g, b) => Some(format!("58:2::{r}:{g}:{b}")),
        };
        let parts: Vec<String> = kind.into_iter().chain(colour).collect();
        if !parts.is_empty() {
            let _ = write!(out, "\x1b[{}m", parts.join(";"));
        }
    }
}

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
    /// How many bytes its text is.
    fn bytes(&self) -> usize {
        let more = self.more.iter().filter(|&&m| m != '\0').map(|m| m.len_utf8()).sum::<usize>();
        if self.ch == '\0' { 0 } else { self.ch.len_utf8() + more }
    }
    pub fn push_text(&self, out: &mut String) {
        if self.ch != '\0' {
            out.push(self.ch);
            out.extend(self.more.iter().filter(|&&m| m != '\0'));
        }
    }
}

/// A picture the program drew: where it is, in cells. Its bytes leave in the outbox.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub id: u32,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
}

/// Where a mark is: on the main screen, a row and a column, for as long as its row is there
/// (what is done to the screen's rows is done to it); a line of the pane's text and how many
/// bytes into it, once the row has gone into history.
#[derive(Clone, Copy, PartialEq)]
enum At {
    Screen(usize, usize),
    Text(usize, usize),
}

/// What the emulator produces besides the screen, for the holder to take all at once.
#[derive(Default)]
pub struct Out {
    /// Answers to the program's queries, to be written to it.
    pub reply: Vec<u8>,
    /// Lines that scrolled off the top of the main screen, as plain text; and before them,
    /// how many bytes to take off the end of what history has: a resize took rows back.
    pub history: String,
    pub unhistory: usize,
    /// What was dropped for being over a bound, or refused, for the log (each kind once).
    pub refused: Vec<&'static str>,
    /// New pictures: the number of each and its bytes, a whole sixel string.
    pub pictures: Vec<(u32, Vec<u8>)>,
    /// The window title the program set last, without control characters.
    pub title: Option<String>,
    /// What the program put on the clipboard last.
    pub clipboard: Option<Vec<u8>>,
    /// The directory the program said it is in last (OSC 7): the host it named, and the path.
    pub cwd: Option<(String, String)>,
    /// What the program wanted noticed last (empty for the bell), and how many times it wanted
    /// something noticed since this was taken.
    pub notice: Option<String>,
    pub notices: u32,
}

/// The character printed last: where the cursor was left, and the character's place, its
/// cell as it was left and what was in that cell before (no place, if it found no room).
#[derive(Clone, Copy)]
struct Last {
    at: Option<(usize, usize, Cell, Cell)>,
    cursor: (usize, usize, bool),
}

#[derive(Clone, Copy, Default)]
struct Saved {
    row: usize,
    col: usize,
    style: Style,
    origin: bool,
    graphics: [bool; 2],
    shift: bool,
    wrap: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeInter,
    Csi,
    /// A string (OSC, DCS, APC, SOS, PM), kept in `string` up to its ST (or, for
    /// OSC alone, a BEL): a title, a clipboard or a sixel picture if it says so.
    Str,
    /// ESC seen inside a string: `\` ends it, anything else starts anew.
    StrEscape,
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    grid: Vec<Vec<Cell>>,
    other: Vec<Vec<Cell>>,
    pub alt: bool,
    /// The cursor's row on the main screen while the alternate one is shown.
    main_row: usize,
    /// The kitty keyboard protocol: the flags a program asked for on the main screen and on
    /// the alternate one, with the flags it pushed aside on each; and which flags the terminal
    /// that sends the keys takes (told by whoever keeps this terminal: none, until told).
    keys: [(u8, Vec<u8>); 2],
    pub keyboard: u8,
    pub row: usize,
    pub col: usize,
    style: Style,
    wrap_pending: bool,
    top: usize,
    bottom: usize,
    origin: bool,
    autowrap: bool,
    insert: bool,
    pub cursor_shown: bool,
    pub cursor_shape: u8,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub paste: bool,
    /// The program asked to be told when its pane gains and loses the keys (mode 1004).
    pub focus: bool,
    /// The program is in the middle of drawing (mode 2026): what it has drawn so far is not to be shown yet.
    pub sync: bool,
    /// How many times it has said so.
    pub syncs: u32,
    /// The mouse reports the program asked for: 0 for none, else 9, 1000, 1002 or 1003; and whether as SGR (1006).
    pub mouse: u16,
    pub mouse_sgr: bool,
    tabs: Vec<bool>,
    graphics: [bool; 2],
    shift: bool,
    saved: [Saved; 2],
    last: char,
    /// The character printed last, and what the next one needs to know to join it.
    cluster: Option<Last>,
    /// How many whole lines have gone into history: the number of the line the screen's first row is of.
    pub gone: usize,
    /// The addresses of the links on the two screens (OSC 8), by a cell's `link` less one; and
    /// the link that what is printed now is part of.
    links: Vec<String>,
    link: u16,
    /// The marks a shell put at its prompts (OSC 133): where, kind, exit code. The last thousand.
    marks: Vec<(At, &'static str, Option<u8>)>,
    /// (How many of them are in the rows the last resize sent there.)
    above_lines: usize,
    /// The last row that went to history was not ended: its line goes on in the screen's first
    /// row. With how many bytes of that line history has.
    history_open: Option<usize>,
    /// The rows the last resize sent to history, while they are still its last lines: a later
    /// resize that has room takes them back. With them, how many bytes they are there and
    /// whether history's line was open before them.
    above: Vec<Vec<Cell>>,
    above_bytes: usize,
    above_open: Option<usize>,
    /// A line feed is the text wrapping into the next row.
    wrapping: bool,
    joiner: Joiner,
    state: State,
    params: Vec<(u16, bool)>,
    param: Option<u16>,
    sub: bool,
    private: u8,
    /// The sequence's first number was left out.
    bare: bool,
    /// One of the sequence's numbers was too large to keep.
    huge: bool,
    inter: u8,
    utf8: u32,
    utf8_left: u8,
    /// The smallest character this many bytes may encode: a longer spelling of a smaller one is refused.
    utf8_min: u32,
    /// The sequence being read is malformed or has more parameters than are kept: it will be ignored.
    too_long: bool,
    /// The string being read, after the byte that began it (`]` for OSC, `P` for DCS…); and whether it outgrew its bound.
    string: Vec<u8>,
    string_over: bool,
    /// (A control byte came inside it and was dropped: what is left is not what was sent.)
    string_holed: bool,
    out: Out,
    pub pictures: Vec<Picture>,
    /// The pictures of the screen not shown (the main one while the alternate is).
    pub other_pictures: Vec<Picture>,
    next_picture: u32,
    /// The title now, and the titles a program put aside to bring back (`CSI 22 t`, `CSI 23 t`), at most 8.
    title: String,
    titles: Vec<String>,
    /// The size of a cell in pixels, to know how many cells a picture covers.
    pub cell: (usize, usize),
    /// What the colours are, as whoever keeps this terminal was told: lines of a name
    /// (`foreground`, `background`, a palette number) and `#rrggbb`. A program that asks for a
    /// colour is answered from here, and not at all if its line is missing.
    pub colours: String,
    /// The string being ended ended with BEL, not ESC `\`: an answer to it ends the same way.
    bel: bool,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let (cols, rows) = (cols.clamp(1, SIZE_MAX), rows.clamp(1, SIZE_MAX));
        let grid = vec![vec![Cell::blank(Style::default()); cols]; rows];
        Term {
            cols,
            rows,
            other: grid.clone(),
            grid,
            alt: false,
            main_row: 0,
            keys: [(0, Vec::new()), (0, Vec::new())],
            keyboard: 0,
            row: 0,
            col: 0,
            style: Style::default(),
            wrap_pending: false,
            top: 0,
            bottom: rows - 1,
            origin: false,
            autowrap: true,
            insert: false,
            cursor_shown: true,
            cursor_shape: 0,
            app_cursor: false,
            app_keypad: false,
            paste: false,
            focus: false,
            sync: false,
            syncs: 0,
            mouse: 0,
            mouse_sgr: false,
            tabs: (0..SIZE_MAX).map(|c| c % 8 == 0).collect(),
            graphics: [false; 2],
            shift: false,
            saved: [Saved::default(); 2],
            last: '\0',
            cluster: None,
            gone: 0,
            marks: Vec::new(),
            links: Vec::new(),
            link: 0,
            above_lines: 0,
            history_open: None,
            above: Vec::new(),
            above_bytes: 0,
            above_open: None,
            wrapping: false,
            joiner: Joiner::default(),
            state: State::Ground,
            params: Vec::new(),
            param: None,
            sub: false,
            private: 0,
            bare: false,
            huge: false,
            inter: 0,
            utf8: 0,
            utf8_left: 0,
            utf8_min: 0,
            too_long: false,
            string: Vec::new(),
            string_over: false,
            string_holed: false,
            out: Out::default(),
            pictures: Vec::new(),
            other_pictures: Vec::new(),
            next_picture: 1,
            title: String::new(),
            titles: Vec::new(),
            cell: (10, 20),
            colours: String::new(),
            bel: false,
        }
    }

    /// What waits to be taken, for the tests to look at.
    #[cfg(test)]
    pub fn peek(&self) -> &Out {
        &self.out
    }

    /// Everything produced since the last time this was asked.
    pub fn take(&mut self) -> Out {
        std::mem::take(&mut self.out)
    }

    /// Says of a cell which link it is part of (for a reader that has the links from a file).
    pub fn set_link(&mut self, row: usize, col: usize, link: u16) {
        if let Some(cell) = self.grid.get_mut(row).and_then(|cells| cells.get_mut(col)) {
            cell.link = link;
        }
    }

    pub fn cell_at(&self, row: usize, col: usize) -> Option<&Cell> {
        self.grid.get(row)?.get(col)
    }

    // ---- files -----------------------------------------------------------

    fn row_text(row: &[Cell], out: &mut String) {
        let start = out.len();
        row.iter().for_each(|cell| cell.push_text(out));
        out.truncate(start + out.get(start..).map_or(0, |text| text.trim_end_matches(' ').len()));
    }

    /// A row's text as history keeps it. A row that goes on in the next is not ended, so history
    /// holds whole lines however they were wrapped; `open` says the line before was left so, and
    /// a row that does not go on from it ends it first (what it went on in was erased).
    fn row_history(row: &[Cell], goes_on: bool, open: &mut Option<usize>, out: &mut String) {
        if open.is_some() && !continues(row) {
            out.push('\n');
            *open = None;
        }
        let start = out.len();
        if goes_on {
            let gap = row.last().is_some_and(|cell| cell.style.flags & PAD != 0);
            row.iter().take(row.len() - usize::from(gap)).for_each(|cell| cell.push_text(out));
            *open = Some(open.unwrap_or(0) + out.len() - start);
        } else {
            Term::row_text(row, out);
            out.push('\n');
            *open = None;
        }
    }

    /// For tests: the main screen's text as lines (rows that go on joined), all of it and the part
    /// before the cursor; and whether every wide character on both screens has both its cells.
    #[cfg(test)]
    pub fn lines(&self) -> (String, String, bool) {
        let (main, row) = if self.alt { (&self.other, self.main_row) } else { (&self.grid, self.row) };
        let (mut all, mut before, mut open) = (String::new(), String::new(), self.history_open);
        for (y, cells) in main.iter().enumerate() {
            let goes_on = main.get(y + 1).is_some_and(|next| continues(next));
            if y == row {
                before.clone_from(&all);
                if open.is_some() && !continues(cells) {
                    before.push('\n');
                }
                let upto = self.col + usize::from(self.wrap_pending);
                cells.iter().take(upto).for_each(|cell| cell.push_text(&mut before));
            }
            Term::row_history(cells, goes_on, &mut open, &mut all);
        }
        let whole = [&self.grid, &self.other].iter().all(|grid| {
            grid.len() == self.rows
                && grid.iter().all(|cells| {
                    cells.len() == self.cols
                        && (0..=cells.len()).all(|x| {
                            let half = cells.get(x).is_some_and(|cell| cell.ch == '\0');
                            let lead = x.checked_sub(1).and_then(|x| cells.get(x));
                            half == lead.is_some_and(|cell| cell.ch != '\0' && cell.wide())
                        })
                })
        });
        (all, before, whole)
    }

    /// The screen as plain text, one line per row, blanks at the end cut.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        for (y, row) in self.grid.iter().enumerate() {
            // A row that goes on in the next keeps the blanks at its end: they are in the
            // middle of its line (but not the gap a wide character left).
            if self.grid.get(y + 1).is_some_and(|next| continues(next)) {
                let gap = row.last().is_some_and(|cell| cell.style.flags & PAD != 0);
                row.iter().take(row.len() - usize::from(gap)).for_each(|cell| cell.push_text(&mut out));
            } else {
                Term::row_text(row, &mut out);
            }
            out.push('\n');
        }
        out
    }

    /// The screen with colours: text and SGR codes only, each row on a line
    /// that starts and ends in the default style.
    pub fn styled(&self) -> String {
        let mut out = String::new();
        let plain = Style::default();
        for (y, row) in self.grid.iter().enumerate() {
            // (The marks are no looks.)
            let look = |cell: &Cell| Cell { style: Style { flags: cell.style.flags & !MARKS, ..cell.style }, ..*cell };
            // (A row that goes on in the next keeps the blanks at its end, as in `plain`.)
            let gap = row.last().is_some_and(|cell| cell.style.flags & PAD != 0);
            let used = match self.grid.get(y + 1).is_some_and(|next| continues(next)) {
                true => row.len() - usize::from(gap),
                false => row.iter().rposition(|c| look(c) != Cell::blank(plain)).map_or(0, |last| last + 1),
            };
            let (mut style, mut joiner) = (plain, Joiner::default());
            for cell in row.iter().take(used).filter(|cell| cell.ch != '\0') {
                if look(cell).style != style {
                    style = look(cell).style;
                    style.sgr(&mut out);
                }
                // Two characters that would read back as one are kept apart by U+200B, which
                // takes no room and is no part of either.
                if joiner.joins(cell.ch) {
                    out.push('\u{200b}');
                    joiner = Joiner::default();
                    joiner.joins(cell.ch);
                }
                cell.more.iter().take_while(|&&c| c != '\0').for_each(|&c| _ = joiner.joins(c));
                cell.push_text(&mut out);
            }
            if style != plain {
                out.push_str("\x1b[m");
            }
            out.push('\n');
        }
        out
    }

    /// The modes the `modes` file lists by name, and where each is kept.
    fn modes(&mut self) -> [(&'static str, &mut bool); 5] {
        [
            ("appcursor", &mut self.app_cursor),
            ("keypad", &mut self.app_keypad),
            ("paste", &mut self.paste),
            ("focus", &mut self.focus),
            ("alt", &mut self.alt),
        ]
    }

    /// The keyboard flags in force: what the program on the screen shown asked for, as far as
    /// the terminal that sends the keys took it when the program asked.
    pub fn keys(&self) -> u8 {
        self.keys.get(usize::from(self.alt)).map_or(0, |(flags, _)| *flags)
    }

    /// How input is to be sent to the program, as words: what the `modes` file says.
    pub fn words(&mut self) -> String {
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
        (self.keys, self.keyboard) = ([(keys, Vec::new()), (keys, Vec::new())], KEYS);
    }

    /// How many lines are before the main screen's first row: those gone into history whole,
    /// and one more that history has the start of, if what it went on in was erased.
    pub fn top(&self) -> usize {
        let first = if self.alt { self.other.first() } else { self.grid.first() };
        self.gone + usize::from(self.history_open.is_some() && !first.is_some_and(|row| continues(row)))
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
            let cells = self.grid.iter().chain(&self.other).chain(&self.above).flatten().chain(&under);
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
        let (mut out, mut line, mut base) = (String::new(), self.top(), 0);
        let mut run: Option<(usize, usize, usize, u16)> = None;
        // (A list that would be longer than a reader reads ends before the run that makes it so.)
        let mut full = false;
        let mut put = |(line, start, length, link): (usize, usize, usize, u16)| {
            let address = usize::from(link).checked_sub(1).and_then(|at| self.links.get(at));
            let record = format!("{line} {start} {length} {}\n", address.map_or("", String::as_str));
            full |= out.len() + record.len() > LINKS_LIST_MAX;
            if address.is_some_and(|address| !address.is_empty()) && !full {
                out.push_str(&record);
            }
        };
        // (Of the screen shown: a program on the alternate screen has links too. Its lines are
        // numbered on from `top` as the main screen's are, though none of them goes to history.)
        for (y, row) in self.grid.iter().enumerate() {
            (line, base) = if y > 0 && !continues(row) { (line + 1, 0) } else { (line, base) };
            for (x, cell) in row.iter().enumerate() {
                let at = base + x;
                run = match run {
                    Some((l, start, len, link)) if l == line && start + len == at && link == cell.link => {
                        Some((l, start, len + 1, link))
                    }
                    ended => {
                        ended.into_iter().for_each(&mut put);
                        (cell.link != 0).then_some((line, at, 1, cell.link))
                    }
                };
            }
            base += row.len();
        }
        run.into_iter().for_each(&mut put);
        out
    }

    /// The main screen's rows, whichever screen is shown.
    fn main(&self) -> &[Vec<Cell>] {
        if self.alt { &self.other } else { &self.grid }
    }

    /// Where in the pane's text a place on the main screen is: its line's number, and how many
    /// bytes of the line's text are before it (those history has of it among them).
    fn text_place(&self, row: usize, col: usize) -> (usize, usize) {
        let main = self.main();
        let row = row.min(main.len().saturating_sub(1));
        let goes_on = |y: usize| main.get(y).is_some_and(|cells| continues(cells));
        let first = (1..=row).rev().find(|&y| !goes_on(y)).unwrap_or(0);
        let line = self.top() + (1..=first).filter(|&y| !goes_on(y)).count();
        let mut bytes = if first == 0 && goes_on(0) { self.history_open.unwrap_or(0) } else { 0 };
        for (y, cells) in main.iter().enumerate().take(row + 1).skip(first) {
            // (A row that goes on is in the text whole, but for the gap a wide character left.)
            let gap = cells.last().is_some_and(|cell| cell.style.flags & PAD != 0);
            let upto = if y < row { cells.len() } else { col }.min(cells.len() - usize::from(gap));
            bytes += cells.iter().take(upto).map(Cell::bytes).sum::<usize>();
        }
        (line, bytes)
    }

    /// The same for many places at once: each row's line and the bytes of the line before the
    /// row are counted once, so a thousand marks cost one walk over the screen.
    fn text_places(&self, places: impl Iterator<Item = (usize, usize)>) -> Vec<(usize, usize)> {
        let main = self.main();
        let text = |cells: &[Cell], upto: usize| {
            let gap = cells.last().is_some_and(|cell| cell.style.flags & PAD != 0);
            cells.iter().take(upto.min(cells.len() - usize::from(gap))).map(Cell::bytes).sum::<usize>()
        };
        let (mut line, mut bytes, mut starts) = (self.top(), 0, Vec::with_capacity(main.len()));
        for (y, cells) in main.iter().enumerate() {
            match (y, continues(cells)) {
                (0, true) => bytes = self.history_open.unwrap_or(0),
                (0, false) | (_, true) => {}
                (_, false) => (line, bytes) = (line + 1, 0),
            }
            starts.push((line, bytes));
            bytes += text(cells, cells.len());
        }
        let last = main.len().saturating_sub(1);
        let place = |(row, col): (usize, usize)| {
            let (line, before) = starts.get(row.min(last)).copied().unwrap_or((self.top(), 0));
            (line, before + main.get(row.min(last)).map_or(0, |cells| text(cells, col)))
        };
        places.map(place).collect()
    }

    /// The place on the main screen of a line of the pane's text and so many bytes into it, if
    /// that is on the screen (and not in history, or nowhere).
    fn screen_place(&self, line: usize, bytes: usize) -> Option<(usize, usize)> {
        let main = self.main();
        let goes_on = |y: usize| main.get(y).is_some_and(|cells| continues(cells));
        let mut number = self.top();
        let first = (0..main.len()).find(|&y| {
            number += usize::from(y > 0 && !goes_on(y));
            number == line && (y > 0 || !goes_on(0) || self.history_open.is_some())
        })?;
        let before = if first == 0 && goes_on(0) { self.history_open.unwrap_or(0) } else { 0 };
        let mut left = bytes.checked_sub(before)?;
        let mut last = (first, 0);
        for (y, cells) in main.iter().enumerate().skip(first).take_while(|(y, _)| *y == first || goes_on(*y)) {
            let gap = goes_on(y + 1) && cells.last().is_some_and(|cell| cell.style.flags & PAD != 0);
            for (x, cell) in cells.iter().enumerate().take(cells.len() - usize::from(gap)) {
                if left < cell.bytes() || (left == 0 && cell.ch != '\0') {
                    return Some((y, x));
                }
                left -= cell.bytes();
            }
            last = (y, cells.len());
            // (At a row's end rather than at the start of the row the line goes on in: where
            // the cursor waits to wrap, which is where a mark is put.)
            if left == 0 {
                break;
            }
        }
        Some(last)
    }

    /// The shell's marks, a line each: `LINE AT KIND`, and the command's exit code after `end`.
    pub fn marks(&self) -> String {
        let screen = self.marks.iter().filter_map(|mark| match mark.0 {
            At::Screen(row, col) => Some((row, col)),
            At::Text(..) => None,
        });
        let mut places = self.text_places(screen).into_iter();
        let mut out = String::new();
        for (at, kind, code) in &self.marks {
            let (line, bytes) = match *at {
                At::Screen(..) => places.next().unwrap_or_default(),
                At::Text(line, bytes) => (line, bytes),
            };
            let _ = write!(out, "{line} {bytes} {kind}");
            let _ = writeln!(out, "{}", code.map_or(String::new(), |code| format!(" {code}")));
        }
        out
    }

    /// What is done to rows `from..=to` of the main screen is done to the marks on them: `to`
    /// says where a mark at a row and column goes.
    fn move_marks(&mut self, to: impl Fn(&Term, usize, usize) -> At) {
        if self.alt {
            return;
        }
        let moved = self.marks.iter().map(|mark| match mark.0 {
            At::Screen(row, col) => to(self, row, col),
            text => text,
        });
        let moved: Vec<At> = moved.collect();
        self.marks.iter_mut().zip(moved).for_each(|(mark, at)| mark.0 = at);
    }

    /// When a line that history has the start of no longer goes on in the screen's first row
    /// (that row was erased), history's line is ended there.
    fn settle(&mut self) {
        if self.history_open.is_some() && !self.main().first().is_some_and(|row| continues(row)) {
            self.out.history.push('\n');
            (self.gone, self.history_open) = (self.gone + 1, None);
            // (What comes after them in history makes the rows a resize sent there stay there.)
            self.above.clear();
        }
    }

    /// The rows of the screen that go on from the row above (the text wrapped into them), as
    /// numbers from 0: what `plain` cannot say. The first row may go on from history's last line.
    pub fn wraps(&self) -> String {
        let open = self.history_open.is_some() && !self.alt;
        let rows = self.grid.iter().enumerate().filter(|(y, row)| continues(row) && (*y > 0 || open));
        rows.map(|(y, _)| y.to_string()).collect::<Vec<String>>().join(" ")
    }

    // ---- the grid --------------------------------------------------------

    fn blank(&self) -> Cell {
        Cell::blank(self.style)
    }

    /// Repairs a row after cells moved: half of a wide character becomes a blank.
    fn tidy(&mut self, row: usize) {
        let Some(cells) = self.grid.get_mut(row) else {
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
        if let Some(cells) = self.grid.get_mut(row).filter(|cells| cells.get(at).is_some_and(|c| c.ch == '\0'))
            && let Some(pair) = cells.get_mut(at.saturating_sub(1)..=at)
        {
            pair.fill(blank);
        }
    }

    /// Blanks columns `from..to` of a row.
    /// Row `row` no longer goes on from the one above it: its neighbour is another now.
    fn parts(&mut self, row: usize) {
        if let Some(first) = self.grid.get_mut(row).and_then(|cells| cells.first_mut()) {
            first.style.flags &= !CONT;
        }
    }

    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let (blank, to) = (self.blank(), to.min(self.cols));
        if from == 0 && to == self.cols {
            // A row erased whole is an empty line: the next row does not go on from it.
            self.parts(row + 1);
        }
        self.split(row, from);
        self.split(row, to);
        if let Some(cells) = self.grid.get_mut(row).and_then(|r| r.get_mut(from..to)) {
            cells.fill(blank);
        }
        self.wrap_pending = false;
        self.pictures.retain(|p| !((p.row..p.row + p.rows).contains(&row) && p.col < to && from < p.col + p.cols));
    }

    /// Pictures move with rows `from..=bottom` when those scroll by `n`; one
    /// that would leave those rows, or lies across their edge, goes.
    fn scroll_pictures(&mut self, from: usize, n: usize, up: bool) {
        let bottom = self.bottom;
        self.pictures.retain_mut(|p| {
            let end = p.row + p.rows;
            if end <= from || p.row > bottom {
                return true;
            }
            let stays =
                p.row >= from && end <= bottom + 1 && if up { p.row >= from + n } else { end + n <= bottom + 1 };
            p.row = if up { p.row.saturating_sub(n) } else { p.row + n };
            stays
        });
    }

    /// Moves rows `from..=bottom` up by `n`, blank rows coming in below.
    fn scroll_up(&mut self, from: usize, n: usize) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = vec![self.blank(); self.cols];
        let kept = from == 0 && !self.alt;
        // A mark on a row that goes into history is from then on a place in the text; on a row
        // that is dropped, it is where the rows that stay begin; on a row that moves, it moves.
        let bottom = self.bottom;
        self.move_marks(|term, row, col| match row {
            row if row < from || row > bottom => At::Screen(row, col),
            row if row >= from + n => At::Screen(row - n, col),
            _ if kept => {
                let (line, bytes) = term.text_place(row, col);
                At::Text(line, bytes)
            }
            _ => At::Screen(from, 0),
        });
        if kept {
            // What comes after them in history makes the rows a resize sent there stay there.
            self.above.clear();
        }
        let Some(region) = self.grid.get_mut(from..=self.bottom) else {
            return;
        };
        let (mut full, before) = (false, self.out.history.len());
        for i in 0..n {
            // (The row the text is just wrapping out of has no next row yet.)
            let goes_on = region.get(i + 1).map_or(self.wrapping, |next| continues(next));
            let Some(row) = region.get_mut(i) else { break };
            if kept && self.out.history.len() < HISTORY_PENDING_MAX {
                Term::row_history(row, goes_on, &mut self.history_open, &mut self.out.history);
            } else if kept {
                (full, self.history_open) = (true, None);
            }
            row.clone_from(&blank);
        }
        region.rotate_left(n);
        self.gone += self.out.history.get(before..).map_or(0, |new| new.matches('\n').count());
        self.scroll_pictures(from, n, true);
        // The rows have new neighbours at both ends of what moved. (The row now at the top of
        // the main screen may go on from the line that history has the start of.)
        if !kept {
            self.parts(from);
        }
        self.parts(self.bottom + 1);
        if full {
            self.refuse("lines were not kept in history: 4 MiB of it were waiting to be written");
        }
    }

    /// Moves rows `from..=bottom` down by `n`, blank rows coming in at `from`.
    fn scroll_down(&mut self, from: usize, n: usize) {
        let n = n.min(self.bottom + 1 - from.min(self.bottom));
        let blank = vec![self.blank(); self.cols];
        let bottom = self.bottom;
        self.move_marks(|_, row, col| match row {
            row if row < from || row > bottom => At::Screen(row, col),
            row if row + n > bottom => At::Screen(bottom, usize::MAX),
            row => At::Screen(row + n, col),
        });
        let Some(region) = self.grid.get_mut(from..=self.bottom) else {
            return;
        };
        region.rotate_right(n);
        region.iter_mut().take(n).for_each(|row| row.clone_from(&blank));
        self.scroll_pictures(from, n, false);
        // Rows have new neighbours at both ends of what moved.
        self.parts(from + n);
        self.parts(self.bottom + 1);
    }

    fn linefeed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.bottom {
            self.scroll_up(self.top, 1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    fn reverse_linefeed(&mut self) {
        self.wrap_pending = false;
        if self.row == self.top {
            self.scroll_down(self.top, 1);
        } else {
            self.row = self.row.saturating_sub(1);
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        let (low, high) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.row = (low + row).min(high);
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn print(&mut self, mut c: char) {
        if self.graphics.get(usize::from(self.shift)) == Some(&true) {
            c = line_drawing(c);
        }
        // Text goes on from the character printed last while the cursor is where that left it
        // and its cell is as it left it: a change of style or a title between a letter and its
        // accent moves nothing, a move or an erase does.
        let here = (self.row, self.col, self.wrap_pending);
        let goes_on = self.cluster.is_some_and(|last| {
            last.cursor == here && last.at.is_none_or(|(row, col, cell, _)| self.cell_at(row, col) == Some(&cell))
        });
        if !goes_on {
            (self.joiner, self.cluster) = (Joiner::default(), None);
        }
        if self.joiner.joins(c) {
            self.join(c);
        } else if width(c, &[]) == 0 {
            // A mark or a joiner with nothing to go on: it has no place on a screen.
            self.cluster = None;
        } else {
            // Until it is placed it has no cell: what joins a character that found no room goes with it.
            (self.last, self.cluster) = (c, Some(Last { at: None, cursor: here }));
            let new = Cell { ch: c, more: ['\0'; MORE], style: self.style, link: self.link };
            self.place(new, width(c, &[]) == 2, self.insert);
        }
        // What the next character must find to go on from this one.
        let here = (self.row, self.col, self.wrap_pending);
        if let Some(last) = &mut self.cluster {
            last.cursor = here;
            last.at = last.at.and_then(|(row, col, _, under)| Some((row, col, *self.grid.get(row)?.get(col)?, under)));
        }
    }

    /// Adds `c` to the character printed last: it is the same cluster (UAX #29).
    fn join(&mut self, c: char) {
        let Some((row, col, _, under)) = self.cluster.and_then(|last| last.at) else { return };
        let insert = self.insert;
        let Some(cell) = self.grid.get_mut(row).and_then(|r| r.get_mut(col)) else { return };
        let (whole, was_wide) = (*cell, cell.wide());
        let Some(slot) = cell.more.iter_mut().find(|m| **m == '\0') else {
            return self.refuse("a character lost its end: it had more than ten parts");
        };
        *slot = c;
        let grown = *cell;
        if grown.wide() && !was_wide {
            // U+FE0F asked an emoji for its picture, which takes a second column: the
            // character is put again, and at a row's end it wraps whole.
            if self.cols < 2 || (col + 2 > self.cols && !self.autowrap) {
                *cell = whole;
                return;
            }
            if col + 2 > self.cols {
                // It leaves the row's last cell as it found it.
                *cell = under;
            } else if insert && let Some(rest) = self.grid.get_mut(row).and_then(|r| r.get_mut(col + 1..)) {
                // In insert mode its second column is one more column put in.
                rest.rotate_right(1);
            }
            (self.col, self.wrap_pending) = (col, false);
            self.place(grown, true, false);
            if insert {
                self.tidy(row);
            }
        }
    }

    /// Puts a character at the cursor, on the next row if this one has no room, and moves on.
    fn place(&mut self, mut new: Cell, wide: bool, insert: bool) {
        if wide && self.cols < 2 {
            return;
        }
        if self.wrap_pending || (wide && self.col + 2 > self.cols) {
            if self.autowrap {
                // The text goes on in the next row, whose first cell will say so. A wide
                // character that had no room in this row's last cell leaves it as no text, if
                // nothing is in it.
                let waiting = self.wrap_pending;
                if let Some(last) = self.grid.get_mut(self.row).and_then(|row| row.last_mut())
                    && !waiting
                    && *last == Cell::blank(last.style)
                {
                    last.style.flags |= PAD;
                }
                self.col = 0;
                self.wrapping = true;
                self.linefeed();
                self.wrapping = false;
                new.style.flags |= CONT;
            } else if wide {
                return; // No room and no wrapping: it is dropped, as xterm drops it.
            }
            self.wrap_pending = false;
        }
        let (row, col) = (self.row, self.col);
        let cells = 1 + usize::from(wide);
        let blank = self.blank();
        let mut under = blank;
        if let Some(line) = self.grid.get_mut(row) {
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
        if !self.pictures.is_empty() {
            self.pictures
                .retain(|p| !((p.row..p.row + p.rows).contains(&row) && p.col < col + cells && col < p.col + p.cols));
        }
        if let Some(last) = &mut self.cluster {
            last.at = Some((row, col, new, under));
        }
        self.col += cells;
        if self.col >= self.cols {
            self.col = self.cols - 1;
            // The wrap waits even while wrapping is off, as in xterm: turned on again, it is done.
            self.wrap_pending = true;
        }
    }

    fn save_cursor(&mut self) {
        let saved = Saved {
            row: self.row,
            col: self.col,
            style: self.style,
            origin: self.origin,
            graphics: self.graphics,
            shift: self.shift,
            wrap: self.wrap_pending,
        };
        if let Some(slot) = self.saved.get_mut(usize::from(self.alt)) {
            *slot = saved;
        }
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved.get(usize::from(self.alt)).copied().unwrap_or_default();
        self.row = saved.row.min(self.rows - 1);
        self.col = saved.col.min(self.cols - 1);
        self.style = saved.style;
        self.origin = saved.origin;
        self.graphics = saved.graphics;
        self.shift = saved.shift;
        self.wrap_pending = saved.wrap && saved.col + 1 == self.cols;
    }

    fn set_alt(&mut self, alt: bool, clear: bool) {
        if alt != self.alt {
            std::mem::swap(&mut self.grid, &mut self.other);
            // (A character still being joined is of the screen it was printed on.)
            self.cluster = None;
            std::mem::swap(&mut self.pictures, &mut self.other_pictures);
            self.alt = alt;
            self.main_row = self.row;
        }
        if alt && clear {
            self.clear();
        }
    }

    fn clear(&mut self) {
        let blank = vec![Cell::blank(Style::default()); self.cols];
        self.grid.iter_mut().for_each(|row| row.clone_from(&blank));
        self.pictures.clear();
    }

    /// Takes a new size, or refuses it and changes nothing. The main screen's
    /// text is wrapped anew: a line that was wrapped is one line again, and
    /// rows that no longer fit go to history from the top. The alternate
    /// screen is cut or padded where it is, and none of it goes to history.
    /// The new screens are made whole first and then take the old ones' place.
    pub fn resize(&mut self, cols: usize, rows: usize) -> bool {
        if !(1..=SIZE_MAX).contains(&cols) || !(1..=SIZE_MAX).contains(&rows) {
            self.refuse("a size was refused: columns and rows are each 1 to 500");
            return false;
        }
        if (cols, rows) == (self.cols, self.rows) {
            return true;
        }
        // (The marks on the screen are places in the text while it is wrapped anew.)
        let texts = self.marks.iter().map(|mark| match mark.0 {
            At::Screen(row, col) => {
                let (line, bytes) = self.text_place(row, col);
                At::Text(line, bytes)
            }
            text => text,
        });
        let texts: Vec<At> = texts.collect();
        self.marks.iter_mut().zip(texts).for_each(|(mark, at)| mark.0 = at);
        let blank = Cell::blank(Style::default());
        let (main, other) = if self.alt { (&self.other, &self.grid) } else { (&self.grid, &self.other) };
        let (main_pictures, other_pictures) =
            if self.alt { (&self.other_pictures, &self.pictures) } else { (&self.pictures, &self.other_pictures) };
        // Places on the main screen that stay on their character, each with whether it waits to
        // wrap at its row's end. First the cursor, whose line is taken to reach as far as it
        // stands (behind the alternate screen, the saved cursor is the one that will come back);
        // then the saved cursor, the row to come back to, the character printed last, and two
        // corners of each picture.
        let saved = self.saved.first().map_or((0, 0, false), |s| (s.row, s.col + usize::from(s.wrap), s.wrap));
        let cursor = (self.row, self.col + usize::from(self.wrap_pending), self.wrap_pending);
        let last = self.cluster.and_then(|last| last.at).filter(|_| !self.alt);
        let mut places = vec![if self.alt { saved } else { cursor }, saved, (self.main_row, 0, false)];
        places.push(last.map_or(cursor, |(row, col, ..)| (row, col, false)));
        places.extend(main_pictures.iter().flat_map(|p| [(p.row, p.col, false), (p.row + p.rows - 1, p.col, false)]));
        // The rows the last resize sent to history are wrapped along: they come back if there is room.
        let held = self.above.len();
        places.iter_mut().for_each(|place| place.0 += held);
        let old: Vec<Vec<Cell>> = self.above.iter().chain(main).cloned().collect();
        let mut wrapped = reflow(&old, cols, &mut places);
        let at = |i: usize| places.get(i).map_or((0, 0), |place| (place.0, place.1));
        // Blank rows below the cursor go first; a row with something on it is never dropped:
        // rows go to history from the top, and the cursor with them if text below it needs the room.
        let lowest = at(0).0.max(if self.alt { at(2).0 } else { 0 });
        while wrapped.len() > rows.max(lowest + 1) && wrapped.last().is_some_and(|row| row.iter().all(|c| *c == blank))
        {
            wrapped.pop();
        }
        let up = wrapped.len().saturating_sub(rows);
        // History is given the rows above the screen again (what it had of them is taken back);
        // the last of them, a screen's worth at most, are kept to come back another time.
        let keep = up.saturating_sub(ABOVE_MAX / cols);
        let (mut history, mut open) = (String::new(), if held > 0 { self.above_open } else { self.history_open });
        let (mut before, mut was_open) = (0, open);
        for (y, row) in wrapped.iter().take(up).enumerate() {
            if y == keep {
                (before, was_open) = (history.len(), open);
            }
            Term::row_history(row, wrapped.get(y + 1).is_some_and(|next| continues(next)), &mut open, &mut history);
        }
        let above: Vec<Vec<Cell>> = wrapped.drain(..up).skip(keep).collect();
        wrapped.resize(rows, vec![blank; cols]);
        // The other screen keeps its rows: one that loses half a wide character loses it whole.
        let mut cut = other.clone();
        cut.resize(rows, Vec::new());
        for row in &mut cut {
            if row.get(cols).is_some_and(|c| c.ch == '\0')
                && let Some(lead) = row.get_mut(cols - 1)
            {
                *lead = Cell::blank(lead.style);
            }
            row.resize(cols, blank);
        }
        // A picture stays with the cells it was over, if they are still one under the other and
        // it is whole on the screen.
        let mut kept: Vec<Picture> = Vec::new();
        for (i, picture) in main_pictures.iter().enumerate() {
            let ((row, col), (below, under)) = (at(4 + 2 * i), at(5 + 2 * i));
            if row >= up
                && under == col
                && below == row + picture.rows - 1
                && below - up < rows
                && col + picture.cols <= cols
            {
                kept.push(Picture { row: row - up, col, ..picture.clone() });
            }
        }
        let mut still = other_pictures.clone();
        still.retain(|p| p.row + p.rows <= rows && p.col + p.cols <= cols);
        // Nothing was changed until here: now the new state takes the old one's place.
        let fit = |(row, col): (usize, usize)| (row.saturating_sub(up).min(rows - 1), col);
        let ((row, col), saved_at, main_row) = (fit(at(0)), fit(at(1)), fit(at(2)).0);
        if let Some(slot) = self.saved.first_mut() {
            (slot.row, slot.col, slot.wrap) = (saved_at.0, saved_at.1.min(cols - 1), saved_at.1 >= cols);
        }
        if let Some(slot) = self.saved.get_mut(1) {
            (slot.row, slot.col) = (slot.row.min(rows - 1), slot.col.min(cols - 1));
        }
        self.interrupt();
        if self.alt {
            (self.grid, self.other, self.pictures, self.other_pictures) = (cut, wrapped, still, kept);
            (self.main_row, self.cluster) = (main_row, None);
            (self.row, self.col, self.wrap_pending) = (self.row.min(rows - 1), self.col.min(cols - 1), false);
        } else {
            (self.grid, self.other, self.pictures, self.other_pictures) = (wrapped, cut, kept, still);
            // A cursor past its row's last column is the one that waits to wrap there.
            (self.row, self.col, self.wrap_pending) = (row, col.min(cols - 1), col >= cols);
            // The character printed last is where it was in the text: what comes next may join it.
            let here = (self.row, self.col, self.wrap_pending);
            let (at_row, at_col) = fit(at(3));
            self.cluster = last.filter(|_| at(3).0 >= up).and_then(|(.., under)| {
                Some(Last { at: Some((at_row, at_col, *self.cell_at(at_row, at_col)?, under)), cursor: here })
            });
        }
        let back = if held > 0 { self.above_bytes } else { 0 };
        let waiting = self.out.history.len().saturating_sub(back);
        self.out.unhistory += back.saturating_sub(self.out.history.len());
        self.out.history = self.out.history.get(..waiting).unwrap_or_default().to_string() + &history;
        let lines = |text: Option<&str>| text.map_or(0, |text| text.matches('\n').count());
        self.gone = (self.gone + lines(Some(&history))).saturating_sub(if held > 0 { self.above_lines } else { 0 });
        self.above_lines = lines(history.get(before..));
        (self.above_bytes, self.above_open, self.above) = (history.len() - before, was_open, above);
        self.history_open = open;
        (self.cols, self.rows) = (cols, rows);
        (self.top, self.bottom) = (0, rows - 1);
        self.settle();
        let places = self.marks.iter().map(|mark| match mark.0 {
            At::Text(line, bytes) => self.screen_place(line, bytes).map_or(mark.0, |(row, col)| At::Screen(row, col)),
            screen => screen,
        });
        let places: Vec<At> = places.collect();
        self.marks.iter_mut().zip(places).for_each(|(mark, at)| mark.0 = at);
        true
    }

    /// Notes a refusal for the log, once until the holder has taken it.
    fn refuse(&mut self, why: &'static str) {
        if !self.out.refused.contains(&why) {
            self.out.refused.push(why);
        }
    }

    fn respond(&mut self, text: &str) {
        if self.out.reply.len() + text.len() > REPLY_MAX {
            self.refuse("an answer to the program was dropped: 16 KiB of answers were waiting");
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
                0x07 if self.string.first() == Some(&b']') => {
                    self.bel = true;
                    self.end_string(true);
                    self.bel = false;
                }
                0x18 | 0x1a => self.end_string(false),
                0x00..=0x1f | 0x7f => self.string_holed = true,
                _ if self.string.len() <= PICTURE_MAX => self.string.push(byte),
                // Past its bound a string is dropped, and said so at once: it may never end.
                _ if self.string_over => {}
                _ => {
                    self.string_over = true;
                    self.refuse("a control string over 4 MiB (a picture, a clipboard, a title) was dropped");
                }
            },
            State::StrEscape => {
                self.end_string(byte == b'\\');
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
                    State::EscapeInter => self.escape_inter(byte),
                    _ => self.csi(byte),
                },
            },
        }
    }

    fn ground(&mut self, byte: u8) {
        if self.utf8_left > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8 = (self.utf8 << 6) | u32::from(byte & 0x3f);
                self.utf8_left -= 1;
                // A sequence of the right length for no character (written too long, a surrogate,
                // past the last one) shows one mark, as in xterm.
                match char::from_u32(self.utf8).filter(|_| self.utf8 >= self.utf8_min) {
                    _ if self.utf8_left > 0 => {}
                    // A C1 control written as UTF-8 is no character and, as in xterm, no control: nothing.
                    Some('\u{80}'..'\u{a0}') => {}
                    whole => self.print(whole.unwrap_or('\u{fffd}')),
                }
                return;
            }
            self.utf8_left = 0;
            self.print('\u{fffd}');
        }
        match byte {
            0x1b => self.state = State::Escape,
            0x00..=0x1f => self.control(byte),
            0x20..=0x7e => self.print(char::from(byte)),
            0xc2..=0xdf => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x1f), 1, 0x80),
            0xe0..=0xef => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x0f), 2, 0x800),
            0xf0..=0xf4 => (self.utf8, self.utf8_left, self.utf8_min) = (u32::from(byte & 0x07), 3, 0x10000),
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
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => self.tab(1),
            0x0a..=0x0c => self.linefeed(),
            0x0d => {
                self.col = 0;
                self.wrap_pending = false;
            }
            0x0e => self.shift = true,
            0x0f => self.shift = false,
            _ => {}
        }
    }

    fn tab(&mut self, n: usize) {
        for _ in 0..n {
            let next = (self.col + 1..self.cols).find(|&c| self.tabs.get(c) == Some(&true));
            self.col = next.unwrap_or(self.cols - 1);
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = State::Ground;
        if byte != b'[' {
            self.interrupt();
        }
        match byte {
            b'[' => {
                self.params.clear();
                (self.param, self.sub, self.private, self.inter, self.too_long) = (None, false, 0, 0, false);
                self.huge = false;
                self.state = State::Csi;
            }
            b']' | b'P' | b'X' | b'^' | b'_' => {
                (self.string, self.string_over, self.string_holed) = (vec![byte], false, false);
                self.state = State::Str;
            }
            0x20..=0x2f => {
                self.inter = byte;
                self.state = State::EscapeInter;
            }
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(),
            b'E' => {
                self.col = 0;
                self.linefeed();
            }
            b'M' => self.reverse_linefeed(),
            b'H' => {
                if let Some(stop) = self.tabs.get_mut(self.col) {
                    *stop = true;
                }
            }
            b'=' => self.app_keypad = true,
            b'>' => self.app_keypad = false,
            b'c' => {
                // A full reset. What was produced stays to be taken, and picture
                // numbers go on, so no file's name is used twice.
                let (cell, out, title) = (self.cell, std::mem::take(&mut self.out), std::mem::take(&mut self.title));
                let titles = std::mem::take(&mut self.titles);
                *self = Term {
                    cell,
                    out,
                    title,
                    titles,
                    next_picture: self.next_picture,
                    history_open: self.history_open,
                    gone: self.gone,
                    syncs: self.syncs,
                    marks: std::mem::take(&mut self.marks),
                    colours: std::mem::take(&mut self.colours),
                    keyboard: self.keyboard,
                    ..Term::new(self.cols, self.rows)
                };
                self.move_marks(|_, _, _| At::Screen(0, 0));
            }
            _ => {}
        }
    }

    fn escape_inter(&mut self, byte: u8) {
        if (0x20..=0x2f).contains(&byte) {
            return;
        }
        self.state = State::Ground;
        match (self.inter, byte) {
            (b'(' | b')', _) => {
                if let Some(set) = self.graphics.get_mut(usize::from(self.inter == b')')) {
                    *set = byte == b'0';
                }
            }
            (b'#', b'8') => {
                let fill = Cell { ch: 'E', ..Cell::blank(Style::default()) };
                self.grid.iter_mut().for_each(|row| row.fill(fill));
                // And, as in xterm, the whole screen scrolls again and the cursor goes home.
                (self.top, self.bottom, self.origin) = (0, self.rows - 1, false);
                self.move_to(0, 0);
            }
            _ => {}
        }
    }

    /// The end of a string: whole (it reached its BEL or ST) or broken off.
    fn end_string(&mut self, whole: bool) {
        self.state = State::Ground;
        let string = std::mem::take(&mut self.string);
        if std::mem::take(&mut self.string_over) {
            return;
        }
        match string.split_first().filter(|_| whole) {
            Some((b'P', data)) => {
                let header = data.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
                if data.get(header) == Some(&b'q') && data.iter().all(|b| (0x20..=0x7e).contains(b)) {
                    self.picture(data);
                }
            }
            Some((b']', data)) => self.command(&String::from_utf8_lossy(data)),
            _ => {}
        }
    }

    /// Something the program wants noticed: text without control characters, a kibibyte at most.
    fn notice(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let cut = (0..=text.len().min(1024)).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        (self.out.notice, self.out.notices) = (text.get(..cut).map(str::to_string), self.out.notices.saturating_add(1));
    }

    /// An operating system command: a title (0, 2), the clipboard (52), or a question about a
    /// colour (4, 10, 11 with `?`). No other is kept: setting a colour changes nothing.
    fn command(&mut self, text: &str) {
        let (number, rest) = text.split_once(';').unwrap_or((text, ""));
        match number {
            "0" | "2" => {
                let mut title = rest.chars().filter(|c| !c.is_control());
                self.title = title.by_ref().take(TITLE_MAX).collect();
                if title.next().is_some() {
                    self.refuse("a title over 256 characters was cut");
                }
                self.out.title = Some(self.title.clone());
            }
            "4" | "10" | "11" => {
                // `4` takes pairs of a palette number and what to do with it; `10` and `11` take
                // one part each for the colours from theirs on (`10;?;?` asks for both). A part
                // that is `?` asks; any other would set the colour, and changes nothing.
                let mut parts = rest.split(';');
                for count in 0.. {
                    let (name, which, part) = match (number, count) {
                        ("4", _) => match parts.next().map(|index| index.parse::<u8>()) {
                            Some(Ok(n)) => (n.to_string(), format!("4;{n}"), parts.next()),
                            Some(Err(_)) => (String::new(), String::new(), parts.next()),
                            None => break,
                        },
                        ("10", 0) => ("foreground".to_string(), "10".to_string(), parts.next()),
                        ("10", 1) | ("11", 0) => ("background".to_string(), "11".to_string(), parts.next()),
                        _ => break,
                    };
                    // Told from the last line that starts with that name, if what follows the
                    // name is a colour (`#` and six hex digits; blanks around them do not matter).
                    let line = |line: &str| {
                        let mut words = line.split_whitespace();
                        (words.next() == Some(name.as_str())).then(|| words.next().unwrap_or_default().to_string())
                    };
                    let told =
                        self.colours.lines().rev().filter(|_| part == Some("?") && !name.is_empty()).find_map(line);
                    let told = told.as_deref().and_then(|told| told.strip_prefix('#'));
                    let told = told.filter(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
                    if let Some(value) = told.and_then(|hex| u32::from_str_radix(hex, 16).ok()) {
                        let [_, r, g, b] = value.to_be_bytes();
                        let end = if self.bel { "\x07" } else { "\x1b\\" };
                        self.respond(&format!("\x1b]{which};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{end}"));
                    }
                    if part.is_none() || self.out.reply.len() >= REPLY_MAX {
                        break;
                    }
                }
            }
            // The directory the program is in: `file://HOST/PATH`, the path's odd bytes as `%XX`.
            "7" => {
                let named = rest.starts_with("file://");
                let (host, path) = rest
                    .strip_prefix("file://")
                    .and_then(|rest| rest.split_at_checked(rest.find('/')?))
                    .unwrap_or_default();
                let mut bytes = Vec::new();
                let mut rest = path.bytes();
                while let Some(byte) = rest.next() {
                    let hex = |rest: &mut std::str::Bytes| char::from(rest.next()?).to_digit(16);
                    let pair = |rest: &mut std::str::Bytes| Some((hex(rest)? * 16 + hex(rest)?) as u8);
                    bytes.push(if byte == b'%' { pair(&mut rest).unwrap_or(0) } else { byte });
                }
                // (A path that is no text, has a control character in it or is longer than a path
                // can be is not told: only that the program is on the machine it named.)
                let path = String::from_utf8(bytes).ok().filter(|path| {
                    !path.is_empty() && path.len() <= 4096 && !path.chars().any(|c| c.is_control() || c == '\u{fffd}')
                });
                if named && (path.is_some() || !host.is_empty()) {
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
            "133" if !self.alt => {
                let kind = match rest.split(';').next() {
                    Some("A") => "prompt",
                    Some("B") => "command",
                    Some("C") => "output",
                    Some("D") => "end",
                    _ => return,
                };
                let code = rest.split(';').nth(1).filter(|_| kind == "end").and_then(|code| code.parse::<u8>().ok());
                let here = At::Screen(self.row, self.col + usize::from(self.wrap_pending));
                // A prompt before a mark says that mark's text is there no more (the prompt was
                // drawn again higher up): the mark is kept, at the prompt's place, so that what
                // was marked before is still in its order, with nothing between. Only a prompt
                // says so: a program that moved up and ended has erased nothing.
                if kind == "prompt" {
                    let (row, col) = (self.row, self.col + usize::from(self.wrap_pending));
                    self.move_marks(|_, r, c| if (r, c) > (row, col) { here } else { At::Screen(r, c) });
                    // (A prompt drawn again in its place is one prompt.)
                    while self.marks.last().is_some_and(|last| last.0 == here && matches!(last.1, "prompt" | "command"))
                    {
                        self.marks.pop();
                    }
                }
                self.marks.push((here, kind, code));
                let extra = self.marks.len().saturating_sub(MARKS_MAX);
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
                    Some(bytes) if bytes.len() <= CLIPBOARD_MAX => self.out.clipboard = Some(bytes),
                    Some(_) => self.refuse("a clipboard over 1 MiB was dropped"),
                    None => self.refuse("a clipboard that was not base64 was dropped"),
                },
            },
            _ => {}
        }
    }

    /// Places a sixel picture at the cursor and moves the cursor below it.
    fn picture(&mut self, data: &[u8]) {
        let (width, height) = sixel_size(data);
        let cols = width.div_ceil(self.cell.0.max(1)).max(1);
        let rows = height.div_ceil(self.cell.1.max(1)).max(1);
        if cols > SIZE_MAX || rows > SIZE_MAX {
            self.refuse("a picture over 500 cells wide or high was dropped");
            return;
        }
        if self.pictures.len() >= PICTURES_MAX {
            let oldest = self.pictures.remove(0).id;
            self.out.pictures.retain(|(id, _)| *id != oldest);
            self.refuse("a screen keeps 8 pictures: the oldest was dropped for a new one");
        }
        let mut whole = Vec::with_capacity(data.len() + 4);
        whole.extend_from_slice(b"\x1bP");
        whole.extend_from_slice(data);
        whole.extend_from_slice(b"\x1b\\");
        self.pictures.push(Picture { id: self.next_picture, row: self.row, col: self.col, cols, rows });
        self.out.pictures.push((self.next_picture, whole));
        self.next_picture = self.next_picture.wrapping_add(1);
        // The picture scrolls with the text; one taller than the screen leaves it.
        for _ in 0..rows.min(self.rows + 1) {
            self.linefeed();
        }
        self.col = 0;
        self.wrap_pending = false;
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            // After an intermediate byte only the last byte may come: anything else spoils the sequence.
            b'0'..=b'?' if self.inter != 0 => self.too_long = true,
            b'0'..=b'9' => {
                // (A number past what is kept is the largest kept, and remembered as too large.)
                let more = u32::from(self.param.unwrap_or(0)) * 10 + u32::from(byte - b'0');
                self.huge |= more > u32::from(u16::MAX);
                self.param = Some(u16::try_from(more).unwrap_or(u16::MAX));
            }
            b';' | b':' | 0x40..=0x7e => {
                // More parameters than are kept: the whole sequence is ignored, and said so.
                self.too_long |= self.params.len() >= PARAMS_MAX;
                if !self.too_long {
                    // (A first number that was not given is 0 here; one sequence tells them apart.)
                    self.bare = if self.params.is_empty() { self.param.is_none() } else { self.bare };
                    self.params.push((self.param.unwrap_or(0), self.sub));
                }
                self.param = None;
                self.sub = byte == b':';
                if byte >= 0x40 {
                    self.state = State::Ground;
                    if self.too_long {
                        self.refuse("a control sequence that was malformed or had more than 32 parameters was ignored");
                    } else {
                        self.dispatch(byte);
                    }
                }
            }
            // `?` and its like count only before the parameters; elsewhere the sequence means nothing.
            b'<'..=b'?' if self.params.is_empty() && self.param.is_none() && self.private == 0 => self.private = byte,
            b'<'..=b'?' => self.too_long = true,
            0x20..=0x2f => self.inter = byte,
            _ => self.state = State::Ground,
        }
    }

    /// Parameter `i`, or `default` when it is missing or zero.
    fn arg(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&(value, _)) if value != 0 => usize::from(value),
            _ => default,
        }
    }

    fn dispatch(&mut self, last: u8) {
        let n = self.arg(0, 1);
        let (row, col) = (self.row, self.col);
        // What REP repeats: the character printed just before it, with nothing between (as xterm).
        let printed = self.interrupt();
        match (self.private, self.inter, last) {
            (0, 0, b'A') => self.move_rows(n, false),
            (0, 0, b'B') => self.move_rows(n, true),
            (0, 0, b'e') => {
                self.row = row.saturating_add(n).min(self.rows - 1);
                self.wrap_pending = false;
            }
            (0, 0, b'C' | b'a') => self.move_to_col(col + n),
            (0, 0, b'D') => self.move_to_col(col.saturating_sub(n)),
            (0, 0, b'E') => {
                self.move_rows(n, true);
                self.move_to_col(0);
            }
            (0, 0, b'F') => {
                self.move_rows(n, false);
                self.move_to_col(0);
            }
            (0, 0, b'G' | b'`') => self.move_to_col(n - 1),
            (0, 0, b'H' | b'f') => self.move_to(n - 1, self.arg(1, 1) - 1),
            (0, 0, b'd') => {
                self.move_to(n - 1, col);
            }
            (0, 0, b'I') => self.tab(n),
            (0, 0, b'Z') => {
                for _ in 0..n {
                    self.col = (0..self.col).rev().find(|&c| self.tabs.get(c) == Some(&true)).unwrap_or(0);
                }
                self.wrap_pending = false;
            }
            // (The marks on what is erased are, in their order, where the erased part begins:
            // what they marked is gone, and they mark nothing of what is written there next.)
            (0 | b'?', 0, b'J') => match self.arg(0, 0) {
                0 => {
                    self.erase(row, col, self.cols);
                    (row + 1..self.rows).for_each(|r| self.erase(r, 0, self.cols));
                    self.move_marks(|_, r, c| At::Screen(r.min(row), if (r, c) > (row, col) { col } else { c }));
                }
                1 => {
                    (0..row).for_each(|r| self.erase(r, 0, self.cols));
                    self.erase(row, 0, col + 1);
                    // (The place right after what was erased is its end too.)
                    self.move_marks(
                        |_, r, c| if (r, c) <= (row, col + 1) { At::Screen(0, 0) } else { At::Screen(r, c) },
                    );
                }
                2 => {
                    (0..self.rows).for_each(|r| self.erase(r, 0, self.cols));
                    self.move_marks(|_, _, _| At::Screen(0, 0));
                    // A screen that was cleared is not given back what a resize took off its top.
                    if !self.alt {
                        self.above.clear();
                    }
                }
                _ => {}
            },
            (0 | b'?', 0, b'K') => match self.arg(0, 0) {
                0 => self.erase(row, col, self.cols),
                1 => self.erase(row, 0, col + 1),
                2 => self.erase(row, 0, self.cols),
                _ => {}
            },
            (0, 0, b'X') => self.erase(row, col, col.saturating_add(n)),
            (0, 0, b'@' | b'P') => {
                self.split(row, col);
                if last == b'P' {
                    self.split(row, col.saturating_add(n));
                }
                let blank = self.blank();
                if let Some(rest) = self.grid.get_mut(row).and_then(|r| r.get_mut(col..)) {
                    let n = n.min(rest.len());
                    let gap = if last == b'@' {
                        rest.rotate_right(n);
                        rest.get_mut(..n)
                    } else {
                        rest.rotate_left(n);
                        let keep = rest.len() - n;
                        rest.get_mut(keep..)
                    };
                    if let Some(gap) = gap {
                        gap.fill(blank);
                    }
                }
                self.tidy(row);
                self.wrap_pending = false;
            }
            (0, 0, b'L' | b'M') if (self.top..=self.bottom).contains(&row) => {
                if last == b'L' {
                    self.scroll_down(row, n);
                } else {
                    self.scroll_up(row, n);
                }
                self.col = 0;
                self.wrap_pending = false;
            }
            (0, 0, b'S') => self.scroll_up(self.top, n),
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
            (b'>', 0, b'c') => self.respond("\x1b[>1;10;0c"),
            (0, 0, b'g') => match self.arg(0, 0) {
                0 => {
                    if let Some(stop) = self.tabs.get_mut(col) {
                        *stop = false;
                    }
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            (0, 0, b'h' | b'l') => {
                if self.params.iter().any(|p| p.0 == 4) {
                    self.insert = last == b'h';
                }
            }
            (b'?', 0, b'h' | b'l') => {
                for i in 0..self.params.len() {
                    self.private_mode(self.arg(i, 0), last == b'h');
                }
            }
            (0, 0, b'm') => self.sgr(),
            (0, 0, b'n') => match self.arg(0, 0) {
                5 => self.respond("\x1b[0n"),
                6 => {
                    let row = if self.origin { row.saturating_sub(self.top) } else { row };
                    self.respond(&format!("\x1b[{};{}R", row + 1, col + 1));
                }
                _ => {}
            },
            (b'?', b'$', b'p') => {
                let mode = self.arg(0, 0);
                let set = match mode {
                    1 => Some(self.app_cursor),
                    6 => Some(self.origin),
                    7 => Some(self.autowrap),
                    25 => Some(self.cursor_shown),
                    47 | 1047 | 1049 => Some(self.alt),
                    9 | 1000 | 1002 | 1003 => Some(usize::from(self.mouse) == mode),
                    1006 => Some(self.mouse_sgr),
                    2004 => Some(self.paste),
                    1004 => Some(self.focus),
                    2026 => Some(self.sync),
                    _ => None,
                };
                // Text is always cut into grapheme clusters (2027): permanently set.
                let answer = if mode == 2027 { 3 } else { set.map_or(0, |on| 2 - u8::from(on)) };
                self.respond(&format!("\x1b[?{mode};{answer}$y"));
            }
            (0, b'!', b'p') => {
                // A soft reset: modes and the scroll region back to how they start.
                (self.insert, self.origin, self.autowrap, self.cursor_shown) = (false, false, true, true);
                (self.app_cursor, self.app_keypad, self.graphics, self.shift) = (false, false, [false; 2], false);
                (self.mouse, self.mouse_sgr) = (0, false);
                (self.top, self.bottom, self.style, self.link) = (0, self.rows - 1, Style::default(), 0);
                self.saved = [Saved::default(); 2];
            }
            (0, b' ', b'q') => self.cursor_shape = u8::try_from(self.arg(0, 0)).unwrap_or(0),
            (0, 0, b'r') => {
                let (top, bottom) = (n - 1, self.arg(1, self.rows).min(self.rows) - 1);
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
                let alt = usize::from(self.alt);
                // (A number too large to be flags is no flags, not the largest number kept.)
                let given = if self.huge { 0 } else { self.arg(0, 0) };
                let (asked, how) = ((given & usize::from(KEYS & self.keyboard)) as u8, self.arg(1, 1));
                // (A flag in force may be put out of force whatever the terminal takes by now.)
                let out = (given & usize::from(KEYS)) as u8;
                // (Going back takes a count: one if none is given, none if it is 0.)
                let (back, which) = (if self.bare { 1 } else { self.arg(0, 0) }, self.private);
                let Some((flags, kept)) = self.keys.get_mut(alt) else { return };
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
                let (cols, rows, (w, h)) = (self.cols, self.rows, self.cell);
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
                        if let Some(title) = self.titles.pop() {
                            self.title = title;
                            self.out.title = Some(self.title.clone());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn move_rows(&mut self, n: usize, down: bool) {
        // The cursor stops at the scroll region's edge, unless it is already past it.
        self.row = if down {
            let limit = if self.row <= self.bottom { self.bottom } else { self.rows - 1 };
            self.row.saturating_add(n).min(limit)
        } else {
            let limit = if self.row >= self.top { self.top } else { 0 };
            self.row.saturating_sub(n).max(limit)
        };
        self.wrap_pending = false;
    }

    fn move_to_col(&mut self, col: usize) {
        self.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        match mode {
            1 => self.app_cursor = on,
            6 => {
                self.origin = on;
                self.move_to(0, 0);
            }
            7 => self.autowrap = on,
            25 => self.cursor_shown = on,
            1004 => self.focus = on,
            2026 => (self.sync, self.syncs) = (on, self.syncs.wrapping_add(u32::from(on && !self.sync))),
            47 => self.set_alt(on, false),
            1047 => {
                if !on && self.alt {
                    self.clear();
                }
                // Asked for while already there, it is left as it is (as in xterm).
                self.set_alt(on, on && !self.alt);
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
            2004 => self.paste = on,
            9 | 1000 | 1002 | 1003 => self.mouse = if on { mode as u16 } else { 0 },
            1006 => self.mouse_sgr = on,
            _ => {}
        }
    }

    fn sgr(&mut self) {
        let mut i = 0;
        while let Some(&(code, _)) = self.params.get(i) {
            i += 1;
            let flag = |flags: u16, bit: u16, on: bool| if on { flags | bit } else { flags & !bit };
            let style = &mut self.style;
            match code {
                0 => *style = Style::default(),
                1 => style.flags |= BOLD,
                2 => style.flags |= DIM,
                3 => style.flags |= ITALIC,
                4 | 21 => {
                    // `4:0` is "no underline"; `4:2` to `4:5` (and 21) are double, curly, dotted, dashed.
                    let kind = self.params.get(i).filter(|p| p.1).map_or(if code == 21 { 2 } else { 1 }, |p| p.0);
                    style.flags = flag(style.flags & !KINDS, UNDERLINE, kind != 0);
                    style.flags |=
                        [DOUBLE, CURLY, DOTTED, DASHED].get(usize::from(kind).wrapping_sub(2)).copied().unwrap_or(0);
                }
                5 | 6 => style.flags |= BLINK,
                7 => style.flags |= REVERSE,
                8 => style.flags |= HIDDEN,
                9 => style.flags |= STRIKE,
                22 => style.flags &= !(BOLD | DIM),
                23 => style.flags &= !ITALIC,
                24 => style.flags &= !(UNDERLINE | KINDS),
                59 => style.ul = Color::Default,
                25 => style.flags &= !BLINK,
                27 => style.flags &= !REVERSE,
                28 => style.flags &= !HIDDEN,
                29 => style.flags &= !STRIKE,
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
                        (38, Some(color)) => self.style.fg = color,
                        (48, Some(color)) => self.style.bg = color,
                        (58, Some(color)) => self.style.ul = color,
                        _ => {}
                    }
                }
                _ => {}
            }
            // Skip what is left of this code's own `:` parameters.
            while matches!(self.params.get(i), Some(&(_, true))) {
                i += 1;
            }
        }
    }

    /// The colour after a 38 or 48 at `i`, and how many parameters it used:
    /// `5;N`, `2;R;G;B`, or the same with `:` (where a colour space may come first).
    fn long_color(&self, i: usize) -> (Option<Color>, usize) {
        let value = |k: usize| self.params.get(i + k).and_then(|p| u8::try_from(p.0).ok());
        let rgb = |at: usize| Some(Color::Rgb(value(at)?, value(at + 1)?, value(at + 2)?));
        let colons = self.params.get(i).is_some_and(|p| p.1);
        let count = self.params.iter().skip(i).take_while(|p| p.1).count();
        let left = self.params.len().saturating_sub(i);
        // A colour with a part missing or over 255 sets nothing.
        match self.params.get(i).map(|p| p.0) {
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
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
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
fn reflow(old: &[Vec<Cell>], cols: usize, places: &mut [(usize, usize, bool)]) -> Vec<Vec<Cell>> {
    let blank = Cell::blank(Style::default());
    let text = |cell: &Cell| {
        cell.ch != ' ' || cell.more.first() != Some(&'\0') || cell.style.flags & !MARKS != 0 || cell.link != 0
    };
    // The lines, what fills the rest of each one's last row (the colour of the blanks it ended
    // in), and for each old row the line it is part of and where in it the row starts.
    let mut lines: Vec<(Vec<Cell>, Cell)> = Vec::new();
    let mut origin: Vec<(usize, usize)> = Vec::new();
    for (y, row) in old.iter().enumerate() {
        if lines.is_empty() || !continues(row) {
            lines.push((Vec::new(), blank));
        }
        let goes_on = old.get(y + 1).is_some_and(|next| continues(next));
        let gap = row.last().is_some_and(|cell| cell.style.flags & PAD != 0);
        let mut used = match goes_on {
            true => row.len() - usize::from(gap),
            false => row.iter().rposition(text).map_or(0, |last| last + 1),
        };
        if places.first().is_some_and(|cursor| cursor.0 == y) && !goes_on {
            used = used.max(places.first().map_or(0, |cursor| cursor.1).min(row.len()));
        }
        let number = lines.len().saturating_sub(1);
        let Some((line, fill)) = lines.last_mut() else { continue };
        origin.push((number, line.len()));
        let plain = |cell: &Cell| Cell { style: Style { flags: cell.style.flags & !MARKS, ..cell.style }, ..*cell };
        line.extend(row.iter().take(used).map(plain));
        *fill = row.last().filter(|_| used < row.len()).map_or(blank, |last| Cell::blank(last.style));
    }
    // The new rows, and for each the line it is part of and where in it the row starts.
    let (mut rows, mut starts): (Vec<Vec<Cell>>, Vec<(usize, usize)>) = (Vec::new(), Vec::new());
    for (number, (line, fill)) in lines.iter().enumerate() {
        let mut row: Vec<Cell> = Vec::with_capacity(cols);
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
    }
    // The first row may itself go on from a line that history has the start of.
    if let Some(first) =
        rows.first_mut().and_then(|row| row.first_mut()).filter(|_| old.first().is_some_and(|row| continues(row)))
    {
        first.style.flags |= CONT;
    }
    for place in places.iter_mut() {
        let (line, offset) = origin.get(place.0).map_or((usize::MAX, 0), |&(line, start)| (line, start + place.1));
        // The last new row of that line that starts at or before the place; for one that waits
        // at a row's end, before it: where one row ends and the next starts is two places.
        let last =
            |strict: bool| starts.iter().rposition(|&(of, start)| of == line && start + usize::from(strict) <= offset);
        let row = last(place.2).or_else(|| last(false));
        let column = row.map_or(0, |row| offset - starts.get(row).map_or(0, |start| start.1));
        // Past its line's text a place is only kept inside the row; right after the text of a
        // full row it is the place that waits to wrap (one past the last column).
        let beside = lines.get(line).is_some_and(|(text, _)| offset > text.len());
        (place.0, place.1) = (row.unwrap_or(0), if beside { column.min(cols - 1) } else { column });
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

/// The size of a sixel picture in pixels: the larger of what its raster
/// attributes say and what its data paints.
fn sixel_size(data: &[u8]) -> (usize, usize) {
    let body = data.iter().copied().skip_while(|&b| b != b'q').skip(1);
    let (mut x, mut band, mut painted, mut declared) = (0usize, 0usize, (0usize, 0usize), (0usize, 0usize));
    // Whose numbers are being read: `!` a repeat count, `#` a colour, `"` the raster attributes.
    let (mut owner, mut numbers) = (b' ', vec![0usize]);
    for byte in body.chain(std::iter::once(b'$')) {
        if byte.is_ascii_digit() {
            if let Some(n) = numbers.last_mut() {
                *n = n.saturating_mul(10).saturating_add(usize::from(byte - b'0')).min(1 << 20);
            }
        } else if byte == b';' {
            if numbers.len() < 8 {
                numbers.push(0);
            }
        } else {
            if owner == b'"' {
                let size = |i: usize| numbers.get(i).copied().unwrap_or(0);
                declared = (declared.0.max(size(2)), declared.1.max(size(3)));
            }
            let repeat = if owner == b'!' { numbers.first().copied().unwrap_or(1).max(1) } else { 1 };
            (owner, numbers) = (b' ', vec![0]);
            match byte {
                b'!' | b'#' | b'"' => owner = byte,
                b'?'..=b'~' => {
                    x = x.saturating_add(repeat);
                    painted = (painted.0.max(x), (band + 1) * 6);
                }
                b'$' => x = 0,
                b'-' => (x, band) = (0, band + 1),
                _ => {}
            }
        }
    }
    (painted.0.max(declared.0), painted.1.max(declared.1))
}
