//! What a pane and its clients say to each other: one frame at a time, a
//! four-byte big-endian length and one definite-length CBOR array. Every
//! length is checked against what is left before anything is allocated.
use std::fmt;

/// The protocol's revision: a client of another is told this one and closed.
pub const REVISION: u64 = 1;
/// The longest request body.
pub const MAX_REQUEST: usize = 128 << 10;
/// The longest answer body. A whole screen always fits: see
/// [`WORST_SCREEN`].
pub const MAX_ANSWER: usize = 8 << 20;
/// The most rows a pane has.
pub const MAX_ROWS: u16 = 128;
/// The most columns a pane has.
pub const MAX_COLS: u16 = 256;
/// The most rows one history read asks for; an answer holds as many as fit,
/// so a pane's whole history of the default size comes in one.
pub const MAX_HISTORY_ROWS: u64 = 1 << 20;
/// What a history answer leaves free for its head.
pub const HISTORY_HEAD: usize = 64;
const _: () = assert!(WORST_ROW + WORST_SCREEN + HISTORY_HEAD < MAX_ANSWER);
/// The longest a wait lasts; a client renews it.
pub const MAX_WAIT_MS: u32 = 30_000;
/// The most parts one send has.
pub const MAX_PARTS: usize = 1024;
/// The most bytes one row's or one span's fields may claim, as a check on
/// counts read off the wire: a pane's widest row.
const MAX_CELLS_IN_ROW: usize = MAX_COLS as usize;
/// The most labels a pane has beyond its name.
pub const MAX_LABELS: usize = 8;
/// The longest label key.
pub const MAX_LABEL_KEY: usize = 32;
/// The longest label value.
pub const MAX_LABEL_VALUE: usize = 128;

/// The most one cell of a whole screen costs on the wire: fux-vt's 128-byte
/// cluster with its header, in a span of its own, with the longest SGR text
/// `Attributes::write_sgr` writes (every flag, three direct colours) and the
/// span's headers.
const WORST_CELL: usize = 128 + 3 + 96 + 6;
/// The most a whole screen costs: every cell at its worst, a row header for
/// each row and the metadata. Under [`MAX_ANSWER`], so that every legal
/// screen can be read whole.
pub const WORST_SCREEN: usize =
    (MAX_ROWS as usize * MAX_COLS as usize * WORST_CELL) + (MAX_ROWS as usize * 8) + (32 << 10);
const _: () = assert!(WORST_SCREEN < MAX_ANSWER);
/// The most one row costs.
const WORST_ROW: usize = MAX_COLS as usize * WORST_CELL + 8;

/// Why something was refused, by name: the design's closed list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Invalid,
    Protocol,
    Denied,
    Exists,
    Missing,
    Replaced,
    Stale,
    Gone,
    Ended,
    Unsupported,
    Limit,
    Busy,
    Timeout,
    Io,
    Indeterminate,
}

const CODES: [(Code, &str); 15] = [
    (Code::Invalid, "invalid"),
    (Code::Protocol, "protocol"),
    (Code::Denied, "denied"),
    (Code::Exists, "exists"),
    (Code::Missing, "missing"),
    (Code::Replaced, "replaced"),
    (Code::Stale, "stale"),
    (Code::Gone, "gone"),
    (Code::Ended, "ended"),
    (Code::Unsupported, "unsupported"),
    (Code::Limit, "limit"),
    (Code::Busy, "busy"),
    (Code::Timeout, "timeout"),
    (Code::Io, "io"),
    (Code::Indeterminate, "indeterminate"),
];

impl Code {
    pub fn name(self) -> &'static str {
        CODES
            .iter()
            .find(|(code, _)| *code == self)
            .map_or("io", |(_, name)| name)
    }

    pub fn from_name(name: &str) -> Option<Code> {
        CODES
            .iter()
            .find(|(_, known)| *known == name)
            .map(|(code, _)| *code)
    }

    /// The exit code a command ends with when refused so.
    pub fn exit(self) -> u8 {
        match self {
            Code::Invalid => 2,
            Code::Missing | Code::Replaced | Code::Gone => 3,
            Code::Stale => 4,
            Code::Timeout => 5,
            Code::Busy | Code::Limit => 6,
            Code::Indeterminate => 7,
            Code::Protocol
            | Code::Denied
            | Code::Exists
            | Code::Ended
            | Code::Unsupported
            | Code::Io => 1,
        }
    }
}

/// A refusal: its code and what it found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.name(), self.message)
    }
}

impl std::error::Error for Failure {}

/// One life of a pane: random, never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct PaneId(pub [u8; 16]);

impl PaneId {
    pub fn random() -> PaneId {
        use std::hash::{BuildHasher, Hasher};
        // std seeds each `RandomState` from the system's randomness.
        let half = |salt: u64| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(salt);
            hasher.write_u32(std::process::id());
            hasher.finish().to_be_bytes()
        };
        let mut id = [0; 16];
        for (slot, byte) in id.iter_mut().zip(half(1).into_iter().chain(half(2))) {
            *slot = byte;
        }
        PaneId(id)
    }

    /// Its hex digits.
    pub fn parse(text: &str) -> Option<PaneId> {
        let mut id = [0; 16];
        let mut digits = text.chars().map(|c| c.to_digit(16));
        for slot in &mut id {
            let high = digits.next()??;
            let low = digits.next()??;
            *slot = u8::try_from(high.checked_mul(16)?.checked_add(low)?).ok()?;
        }
        digits.next().is_none().then_some(PaneId(id))
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// A place in a pane's life: `<id>@<version>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Token {
    pub pane: PaneId,
    pub version: u64,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.pane, self.version)
    }
}

impl std::str::FromStr for Token {
    type Err = Failure;
    fn from_str(text: &str) -> Result<Token, Failure> {
        let bad = || Failure::new(Code::Invalid, format!("not a token: {text:?}"));
        let (pane, version) = text.split_once('@').ok_or_else(bad)?;
        Ok(Token {
            pane: PaneId::parse(pane).ok_or_else(bad)?,
            version: version.parse().map_err(|_| bad())?,
        })
    }
}

/// One piece of what a send types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// These bytes, as they are.
    Bytes(Vec<u8>),
    /// A key by name (`Enter`, `C-c`), encoded in the program's key mode.
    Key(String),
    /// Text pasted, bracketed if the program asked.
    Paste(String),
}

/// What a client asks of a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hello {
        revision: u64,
    },
    Send {
        parts: Vec<Part>,
        expected: Option<Token>,
    },
    Resize {
        cols: u16,
        rows: u16,
        expected: Option<Token>,
    },
    Signal {
        name: String,
        expected: Option<Token>,
    },
    Screen {
        since: Option<Token>,
    },
    History {
        epoch: u64,
        first: u64,
        count: u64,
    },
    Wait {
        after: Token,
        ms: u32,
    },
    Kill {
        expected: Option<Token>,
    },
}

/// How a program ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    Code(i32),
    Signal {
        number: i32,
        name: String,
        core: bool,
    },
}

impl fmt::Display for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ending::Code(code) => write!(f, "exit {code}"),
            Ending::Signal { name, core, .. } => {
                write!(f, "signal {name}{}", if *core { " (core)" } else { "" })
            }
        }
    }
}

/// Where a pane's program is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// It ended, and its last screen is frozen: `drained` says whether its
    /// terminal was read to its end, or reading stopped at the time bound;
    /// `unwritten` counts input it was never given.
    Exited {
        ending: Ending,
        drained: bool,
        unwritten: u64,
    },
    /// The pane could not go on.
    Failed {
        why: String,
    },
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            State::Running => f.write_str("running"),
            State::Exited {
                ending,
                drained,
                unwritten,
            } => {
                write!(f, "exited ({ending})")?;
                if !drained {
                    f.write_str(" output-cut-off")?;
                }
                if *unwritten > 0 {
                    write!(f, " unwritten={unwritten}")?;
                }
                Ok(())
            }
            State::Failed { why } => write!(f, "failed ({why})"),
        }
    }
}

/// The greeting: who the pane is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub revision: u64,
    pub id: PaneId,
    pub labels: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub state: State,
    /// The daemon's process.
    pub pid: u32,
}

impl Hello {
    /// The value of label `key`.
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// What a screen read says besides its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub cols: u16,
    pub rows: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub cursor_visible: bool,
    pub cursor_shape: u16,
    pub alternate: bool,
    pub history_start: u64,
    pub history_end: u64,
    pub epoch: u64,
    /// What this pane's terminal lost or could not do, in words.
    pub limitations: Vec<String>,
    pub state: State,
}

/// One cell of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    /// A glyph, empty for a blank.
    Text(String),
    /// The second column of the wide glyph before it.
    Tail,
}

/// Cells that share a look: `sgr` is the parameters after a reset, `;1;31`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub sgr: String,
    pub cells: Vec<Cell>,
}

/// One row; blank cells past its last span are left out.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Row {
    /// 0 single width, otherwise double (each cell two columns wide).
    pub rendition: u8,
    pub spans: Vec<Span>,
}

impl Row {
    /// Its text as the first part of a line that goes on in the next row:
    /// blanks kept, out to `width` cells.
    pub fn text_to(&self, width: u64) -> String {
        let mut text = String::new();
        let mut cells = 0u64;
        for cell in self.spans.iter().flat_map(|span| &span.cells) {
            cells = cells.saturating_add(1);
            match cell {
                Cell::Text(glyph) if glyph.is_empty() => text.push(' '),
                Cell::Text(glyph) => text.push_str(glyph),
                Cell::Tail => {}
            }
        }
        let blanks = usize::try_from(width.saturating_sub(cells)).unwrap_or(0);
        text.extend(std::iter::repeat_n(' ', blanks));
        text
    }

    /// Its text, trailing blanks dropped.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for cell in self.spans.iter().flat_map(|span| &span.cells) {
            match cell {
                Cell::Text(glyph) if glyph.is_empty() => text.push(' '),
                Cell::Text(glyph) => text.push_str(glyph),
                Cell::Tail => {}
            }
        }
        text.trim_end().to_owned()
    }
}

/// A row read by number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    pub number: u64,
    /// The row's width if its line goes on in the next row, else 0.
    pub wrapped_at: u64,
    pub row: Row,
}

/// Rows as lines of text, blank rows at the end left out.
pub fn screen_text(rows: &[Row]) -> String {
    let mut lines: Vec<String> = rows.iter().map(Row::text).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut text = lines.join("\n");
    if !lines.is_empty() {
        text.push('\n');
    }
    text
}

/// How a screen differs from the one a token saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Unchanged,
    Whole(Vec<Row>),
    /// Move the rows held up by `moved`, then replace these.
    Patch {
        moved: u16,
        rows: Vec<(u16, Row)>,
    },
}

/// A pane's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Hello(Hello),
    Sent {
        bytes: u64,
        token: Token,
    },
    Resized {
        cols: u16,
        rows: u16,
        token: Token,
    },
    Signalled {
        delivery: String,
    },
    Screen {
        token: Token,
        meta: Box<Meta>,
        change: Change,
    },
    /// Numbered rows of the primary screen: history is `start..end`, and
    /// the screen below it runs on to `screen_end`.
    History {
        epoch: u64,
        start: u64,
        end: u64,
        screen_end: u64,
        rows: Vec<HistoryRow>,
    },
    Waited {
        token: Token,
        state: State,
    },
    Killed {
        complete: bool,
        detail: String,
    },
    Refused(Failure),
}

/// Writes CBOR.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    fn head(&mut self, major: u8, n: u64) {
        let major = major << 5;
        if let Ok(small) = u8::try_from(n)
            && small < 24
        {
            self.0.push(major | small);
        } else if let Ok(n) = u8::try_from(n) {
            self.0.extend([major | 24, n]);
        } else if let Ok(n) = u16::try_from(n) {
            self.0.push(major | 25);
            self.0.extend(n.to_be_bytes());
        } else if let Ok(n) = u32::try_from(n) {
            self.0.push(major | 26);
            self.0.extend(n.to_be_bytes());
        } else {
            self.0.push(major | 27);
            self.0.extend(n.to_be_bytes());
        }
    }

    pub fn uint(&mut self, n: u64) {
        self.head(0, n);
    }

    fn int(&mut self, n: i64) {
        match u64::try_from(n) {
            Ok(n) => self.head(0, n),
            Err(_) => self.head(1, n.unsigned_abs().saturating_sub(1)),
        }
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.head(2, count(bytes.len()));
        self.0.extend_from_slice(bytes);
    }

    pub fn text(&mut self, text: &str) {
        self.head(3, count(text.len()));
        self.0.extend_from_slice(text.as_bytes());
    }

    pub fn array(&mut self, len: usize) {
        self.head(4, count(len));
    }

    pub fn null(&mut self) {
        self.0.push(0xf6);
    }

    pub fn bool(&mut self, value: bool) {
        self.0.push(if value { 0xf5 } else { 0xf4 });
    }

    pub fn token(&mut self, token: Token) {
        self.array(2);
        self.bytes(&token.pane.0);
        self.uint(token.version);
    }

    fn maybe_token(&mut self, token: Option<Token>) {
        match token {
            Some(token) => self.token(token),
            None => self.null(),
        }
    }

    pub fn state(&mut self, state: &State) {
        match state {
            State::Running => {
                self.array(1);
                self.uint(0);
            }
            State::Exited {
                ending,
                drained,
                unwritten,
            } => {
                self.array(4);
                self.uint(1);
                match ending {
                    Ending::Code(code) => {
                        self.array(1);
                        self.int(i64::from(*code));
                    }
                    Ending::Signal { number, name, core } => {
                        self.array(3);
                        self.int(i64::from(*number));
                        self.text(name);
                        self.bool(*core);
                    }
                }
                self.bool(*drained);
                self.uint(*unwritten);
            }
            State::Failed { why } => {
                self.array(2);
                self.uint(2);
                self.text(why);
            }
        }
    }

    pub fn meta(&mut self, meta: &Meta) {
        self.array(12);
        self.uint(meta.cols.into());
        self.uint(meta.rows.into());
        self.uint(meta.cursor_row.into());
        self.uint(meta.cursor_col.into());
        self.bool(meta.cursor_visible);
        self.uint(meta.cursor_shape.into());
        self.bool(meta.alternate);
        self.uint(meta.history_start);
        self.uint(meta.history_end);
        self.uint(meta.epoch);
        self.array(meta.limitations.len());
        for limitation in &meta.limitations {
            self.text(limitation);
        }
        self.state(&meta.state);
    }

    /// One row of a terminal, as [`Row`] reads it.
    pub fn screen_row(&mut self, row: &fux_vt::Row<'_>) {
        let cells: Vec<_> = row.cells().take(row.stored_len()).collect();
        let default = fux_vt::Attributes::default();
        let used = cells
            .iter()
            .rposition(|cell| {
                !cell.contents().is_empty()
                    || cell.is_wide_continuation()
                    || cell.attributes() != default
            })
            .map_or(0, |last| last.saturating_add(1));
        let cells = cells.get(..used).unwrap_or_default();
        let spans: Vec<_> = cells
            .chunk_by(|a, b| a.attributes() == b.attributes())
            .collect();
        self.array(2);
        self.uint(row.rendition() as u64);
        self.array(spans.len());
        let mut sgr = Vec::new();
        for span in spans {
            sgr.clear();
            if let Some(first) = span.first() {
                first
                    .attributes()
                    .write_sgr(&mut sgr, fux_vt::SgrForm::Paint { styles: false });
            }
            self.array(2);
            self.head(3, count(sgr.len()));
            self.0.extend_from_slice(&sgr);
            self.array(span.len());
            for cell in span {
                if cell.is_wide_continuation() {
                    self.null();
                } else {
                    self.text(cell.contents());
                }
            }
        }
    }
}

fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// Reads CBOR, every count checked against the bytes left.
pub struct Reader<'a>(&'a [u8]);

fn malformed(what: &str) -> Failure {
    Failure::new(Code::Protocol, format!("malformed frame: {what}"))
}

type Read<T> = Result<T, Failure>;

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader(bytes)
    }

    fn take(&mut self, n: usize) -> Read<&'a [u8]> {
        let (taken, rest) = self
            .0
            .split_at_checked(n)
            .ok_or_else(|| malformed("cut short"))?;
        self.0 = rest;
        Ok(taken)
    }

    fn peek(&self) -> Read<u8> {
        self.0
            .first()
            .copied()
            .ok_or_else(|| malformed("cut short"))
    }

    fn head(&mut self) -> Read<(u8, u64)> {
        let first = self.take(1)?.first().copied().unwrap_or(0);
        let (major, info) = (first >> 5, first & 0x1f);
        let mut wide = |n: usize| -> Read<u64> {
            let bytes = self.take(n)?;
            Ok(bytes.iter().fold(0u64, |sum, b| (sum << 8) | u64::from(*b)))
        };
        let value = match info {
            0..=23 => u64::from(info),
            24 => wide(1)?,
            25 => wide(2)?,
            26 => wide(4)?,
            27 => wide(8)?,
            _ => return Err(malformed("an indefinite or reserved length")),
        };
        Ok((major, value))
    }

    fn of(&mut self, major: u8) -> Read<u64> {
        match self.head()? {
            (found, value) if found == major => Ok(value),
            (found, _) => Err(malformed(&format!("type {found}, not {major}"))),
        }
    }

    pub fn uint(&mut self) -> Read<u64> {
        self.of(0)
    }

    fn small<T: TryFrom<u64>>(&mut self) -> Read<T> {
        T::try_from(self.uint()?).map_err(|_| malformed("a number out of range"))
    }

    fn int(&mut self) -> Read<i32> {
        let (major, value) = self.head()?;
        let value = i64::try_from(value).map_err(|_| malformed("a number out of range"))?;
        let value = match major {
            0 => value,
            1 => value
                .checked_add(1)
                .map_or(i64::MIN, |n| n.saturating_neg()),
            _ => return Err(malformed("not an integer")),
        };
        i32::try_from(value).map_err(|_| malformed("a number out of range"))
    }

    fn sized(&mut self, major: u8) -> Read<&'a [u8]> {
        let len = usize::try_from(self.of(major)?).map_err(|_| malformed("a length"))?;
        self.take(len)
    }

    fn bytes(&mut self) -> Read<&'a [u8]> {
        self.sized(2)
    }

    pub fn text(&mut self) -> Read<&'a str> {
        std::str::from_utf8(self.sized(3)?).map_err(|_| malformed("text that is not UTF-8"))
    }

    /// An array of at most `most` items, each at least a byte.
    pub fn array(&mut self, most: usize) -> Read<usize> {
        let len = usize::try_from(self.of(4)?).map_err(|_| malformed("a count"))?;
        if len > most || len > self.0.len() {
            return Err(malformed(&format!("{len} items, at most {most}")));
        }
        Ok(len)
    }

    /// An array of exactly `len` items.
    fn fields(&mut self, len: usize) -> Read<()> {
        match self.array(len)? {
            found if found == len => Ok(()),
            found => Err(malformed(&format!("{found} fields, not {len}"))),
        }
    }

    fn null(&mut self) -> Read<bool> {
        let null = self.peek()? == 0xf6;
        if null {
            self.take(1)?;
        }
        Ok(null)
    }

    fn bool(&mut self) -> Read<bool> {
        match self.take(1)?.first() {
            Some(0xf5) => Ok(true),
            Some(0xf4) => Ok(false),
            _ => Err(malformed("not a boolean")),
        }
    }

    pub fn token(&mut self) -> Read<Token> {
        self.fields(2)?;
        let pane = self.bytes()?;
        let pane = pane
            .try_into()
            .map_err(|_| malformed("a pane id's length"))?;
        Ok(Token {
            pane: PaneId(pane),
            version: self.uint()?,
        })
    }

    fn maybe_token(&mut self) -> Read<Option<Token>> {
        if self.null()? {
            return Ok(None);
        }
        self.token().map(Some)
    }

    fn state(&mut self) -> Read<State> {
        let fields = self.array(4)?;
        match (self.uint()?, fields) {
            (0, 1) => Ok(State::Running),
            (1, 4) => {
                let ending = match self.array(3)? {
                    1 => Ending::Code(self.int()?),
                    3 => Ending::Signal {
                        number: self.int()?,
                        name: self.text()?.to_owned(),
                        core: self.bool()?,
                    },
                    _ => return Err(malformed("an ending")),
                };
                Ok(State::Exited {
                    ending,
                    drained: self.bool()?,
                    unwritten: self.uint()?,
                })
            }
            (2, 2) => Ok(State::Failed {
                why: self.text()?.to_owned(),
            }),
            _ => Err(malformed("a state")),
        }
    }

    fn meta(&mut self) -> Read<Meta> {
        self.fields(12)?;
        Ok(Meta {
            cols: self.small()?,
            rows: self.small()?,
            cursor_row: self.small()?,
            cursor_col: self.small()?,
            cursor_visible: self.bool()?,
            cursor_shape: self.small()?,
            alternate: self.bool()?,
            history_start: self.uint()?,
            history_end: self.uint()?,
            epoch: self.uint()?,
            limitations: {
                let len = self.array(16)?;
                (0..len)
                    .map(|_| Ok(self.text()?.to_owned()))
                    .collect::<Read<_>>()?
            },
            state: self.state()?,
        })
    }

    fn row(&mut self) -> Read<Row> {
        self.fields(2)?;
        let rendition = self.small()?;
        let spans = self.array(MAX_CELLS_IN_ROW)?;
        let spans = (0..spans)
            .map(|_| {
                self.fields(2)?;
                let sgr = self.text()?.to_owned();
                let cells = self.array(MAX_CELLS_IN_ROW)?;
                let cells = (0..cells)
                    .map(|_| {
                        Ok(match self.null()? {
                            true => Cell::Tail,
                            false => Cell::Text(self.text()?.to_owned()),
                        })
                    })
                    .collect::<Read<_>>()?;
                Ok(Span { sgr, cells })
            })
            .collect::<Read<_>>()?;
        Ok(Row { rendition, spans })
    }

    fn end(&self) -> Read<()> {
        match self.0.is_empty() {
            true => Ok(()),
            false => Err(malformed("bytes after its end")),
        }
    }
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        match self {
            Request::Hello { revision } => {
                w.array(2);
                w.uint(0);
                w.uint(*revision);
            }
            Request::Send { parts, expected } => {
                w.array(3);
                w.uint(1);
                w.array(parts.len());
                for part in parts {
                    w.array(2);
                    match part {
                        Part::Bytes(bytes) => {
                            w.uint(0);
                            w.bytes(bytes);
                        }
                        Part::Key(name) => {
                            w.uint(1);
                            w.text(name);
                        }
                        Part::Paste(text) => {
                            w.uint(2);
                            w.text(text);
                        }
                    }
                }
                w.maybe_token(*expected);
            }
            Request::Resize {
                cols,
                rows,
                expected,
            } => {
                w.array(4);
                w.uint(2);
                w.uint((*cols).into());
                w.uint((*rows).into());
                w.maybe_token(*expected);
            }
            Request::Signal { name, expected } => {
                w.array(3);
                w.uint(3);
                w.text(name);
                w.maybe_token(*expected);
            }
            Request::Screen { since } => {
                w.array(2);
                w.uint(4);
                w.maybe_token(*since);
            }
            Request::History {
                epoch,
                first,
                count,
            } => {
                w.array(4);
                w.uint(5);
                w.uint(*epoch);
                w.uint(*first);
                w.uint(*count);
            }
            Request::Wait { after, ms } => {
                w.array(3);
                w.uint(6);
                w.token(*after);
                w.uint((*ms).into());
            }
            Request::Kill { expected } => {
                w.array(2);
                w.uint(7);
                w.maybe_token(*expected);
            }
        }
        w.0
    }

    pub fn decode(body: &[u8]) -> Result<Request, Failure> {
        let mut r = Reader::new(body);
        let fields = r.array(4)?;
        let request = match (r.uint()?, fields) {
            (0, 2) => Request::Hello {
                revision: r.uint()?,
            },
            (1, 3) => {
                let parts = r.array(MAX_PARTS)?;
                let parts = (0..parts)
                    .map(|_| {
                        r.fields(2)?;
                        Ok(match r.uint()? {
                            0 => Part::Bytes(r.bytes()?.to_vec()),
                            1 => Part::Key(r.text()?.to_owned()),
                            2 => Part::Paste(r.text()?.to_owned()),
                            _ => return Err(malformed("a part")),
                        })
                    })
                    .collect::<Read<_>>()?;
                Request::Send {
                    parts,
                    expected: r.maybe_token()?,
                }
            }
            (2, 4) => Request::Resize {
                cols: r.small()?,
                rows: r.small()?,
                expected: r.maybe_token()?,
            },
            (3, 3) => Request::Signal {
                name: r.text()?.to_owned(),
                expected: r.maybe_token()?,
            },
            (4, 2) => Request::Screen {
                since: r.maybe_token()?,
            },
            (5, 4) => Request::History {
                epoch: r.uint()?,
                first: r.uint()?,
                count: r.uint()?,
            },
            (6, 3) => Request::Wait {
                after: r.token()?,
                ms: r.small()?,
            },
            (7, 2) => Request::Kill {
                expected: r.maybe_token()?,
            },
            (tag, _) => return Err(malformed(&format!("request {tag}"))),
        };
        r.end()?;
        Ok(request)
    }
}

/// The first part of a screen answer; the caller writes its change.
pub fn screen_head(w: &mut Writer, token: Token, meta: &Meta) {
    w.array(4);
    w.uint(4);
    w.token(token);
    w.meta(meta);
}

/// The first part of a history answer; the caller writes `rows` rows, each
/// after its number in a pair.
pub fn history_head(w: &mut Writer, epoch: u64, range: (u64, u64, u64), rows: usize) {
    w.array(6);
    w.uint(5);
    w.uint(epoch);
    w.uint(range.0);
    w.uint(range.1);
    w.uint(range.2);
    w.array(rows);
}

impl Answer {
    /// The answer, or the refusal it is.
    pub fn accepted(self) -> Result<Answer, Failure> {
        match self {
            Answer::Refused(failure) => Err(failure),
            Answer::Hello(_)
            | Answer::Sent { .. }
            | Answer::Resized { .. }
            | Answer::Signalled { .. }
            | Answer::Screen { .. }
            | Answer::History { .. }
            | Answer::Waited { .. }
            | Answer::Killed { .. } => Ok(self),
        }
    }

    /// Every answer but a screen's and a history's, which their pane writes
    /// straight from its terminal.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        match self {
            Answer::Hello(hello) => {
                w.array(8);
                w.uint(0);
                w.uint(hello.revision);
                w.bytes(&hello.id.0);
                w.array(hello.labels.len());
                for (key, value) in &hello.labels {
                    w.array(2);
                    w.text(key);
                    w.text(value);
                }
                w.uint(hello.cols.into());
                w.uint(hello.rows.into());
                w.state(&hello.state);
                w.uint(hello.pid.into());
            }
            Answer::Sent { bytes, token } => {
                w.array(3);
                w.uint(1);
                w.uint(*bytes);
                w.token(*token);
            }
            Answer::Resized { cols, rows, token } => {
                w.array(4);
                w.uint(2);
                w.uint((*cols).into());
                w.uint((*rows).into());
                w.token(*token);
            }
            Answer::Signalled { delivery } => {
                w.array(2);
                w.uint(3);
                w.text(delivery);
            }
            Answer::Screen { .. } | Answer::History { .. } => {
                w.array(3);
                w.uint(8);
                w.text(Code::Io.name());
                w.text("a screen is written from its terminal");
            }
            Answer::Waited { token, state } => {
                w.array(3);
                w.uint(6);
                w.token(*token);
                w.state(state);
            }
            Answer::Killed { complete, detail } => {
                w.array(3);
                w.uint(7);
                w.bool(*complete);
                w.text(detail);
            }
            Answer::Refused(failure) => {
                w.array(3);
                w.uint(8);
                w.text(failure.code.name());
                w.text(&failure.message);
            }
        }
        w.0
    }

    pub fn decode(body: &[u8]) -> Result<Answer, Failure> {
        let mut r = Reader::new(body);
        let fields = r.array(8)?;
        let answer = match (r.uint()?, fields) {
            (0, 8) => {
                let revision = r.uint()?;
                let id = r.bytes()?;
                let id = PaneId(id.try_into().map_err(|_| malformed("a pane id's length"))?);
                let labels = r.array(MAX_LABELS.saturating_add(1))?;
                let labels = (0..labels)
                    .map(|_| {
                        r.fields(2)?;
                        Ok((r.text()?.to_owned(), r.text()?.to_owned()))
                    })
                    .collect::<Read<_>>()?;
                Answer::Hello(Hello {
                    revision,
                    id,
                    labels,
                    cols: r.small()?,
                    rows: r.small()?,
                    state: r.state()?,
                    pid: r.small()?,
                })
            }
            (1, 3) => Answer::Sent {
                bytes: r.uint()?,
                token: r.token()?,
            },
            (2, 4) => Answer::Resized {
                cols: r.small()?,
                rows: r.small()?,
                token: r.token()?,
            },
            (3, 2) => Answer::Signalled {
                delivery: r.text()?.to_owned(),
            },
            (4, 4) => {
                let token = r.token()?;
                let meta = Box::new(r.meta()?);
                let kind = r.array(3)?;
                let change = match (r.uint()?, kind) {
                    (0, 1) => Change::Unchanged,
                    (1, 2) => {
                        let rows = r.array(MAX_ROWS.into())?;
                        Change::Whole((0..rows).map(|_| r.row()).collect::<Read<_>>()?)
                    }
                    (2, 3) => {
                        let moved = r.small()?;
                        let rows = r.array(MAX_ROWS.into())?;
                        let rows = (0..rows)
                            .map(|_| {
                                r.fields(2)?;
                                Ok((r.small()?, r.row()?))
                            })
                            .collect::<Read<_>>()?;
                        Change::Patch { moved, rows }
                    }
                    _ => return Err(malformed("a change")),
                };
                Answer::Screen {
                    token,
                    meta,
                    change,
                }
            }
            (5, 6) => {
                let (epoch, start, end, screen_end) = (r.uint()?, r.uint()?, r.uint()?, r.uint()?);
                let most = usize::try_from(MAX_HISTORY_ROWS).unwrap_or(usize::MAX);
                let rows = r.array(most)?;
                let rows = (0..rows)
                    .map(|_| {
                        r.fields(3)?;
                        Ok(HistoryRow {
                            number: r.uint()?,
                            wrapped_at: match r.uint()? {
                                width if width <= u64::from(MAX_COLS) => width,
                                _ => return Err(malformed("a row wider than a pane")),
                            },
                            row: r.row()?,
                        })
                    })
                    .collect::<Read<_>>()?;
                Answer::History {
                    epoch,
                    start,
                    end,
                    screen_end,
                    rows,
                }
            }
            (6, 3) => Answer::Waited {
                token: r.token()?,
                state: r.state()?,
            },
            (7, 3) => Answer::Killed {
                complete: r.bool()?,
                detail: r.text()?.to_owned(),
            },
            (8, 3) => {
                let code = r.text()?;
                let code = Code::from_name(code).ok_or_else(|| malformed("an error code"))?;
                Answer::Refused(Failure::new(code, r.text()?))
            }
            (tag, _) => return Err(malformed(&format!("answer {tag}"))),
        };
        r.end()?;
        Ok(answer)
    }
}
