//! Values on the stack: integers, booleans, strings, lists, addresses,
//! execution tokens, failures, the marks of constructs being compiled, and
//! the application's own.
use crate::Code;
use crate::Error;
use crate::Failure;
use crate::machine::Steps;
use crate::reader::Construct;
use crate::token::Token;
use std::rc::Rc;

/// An application's own values: what it puts on the stack (a handle, a
/// request being built). Each says what kind it is, for errors, and how it
/// reads as text.
pub trait AppValue: Clone + PartialEq {
    /// What kind it is, as an error says it: "a pane".
    fn kind(&self) -> &'static str;
    /// What kind it is, as `kind` says it: "pane". By default its
    /// `kind` without the article.
    fn name(&self) -> &'static str {
        bare(self.kind())
    }
    /// Streams how it reads as text into `out`, propagating its failure.
    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        out.write_str(self.kind()).map_err(|_| unwritten())
    }
}

/// A kind said without its article: "an integer" as "integer".
fn bare(kind: &str) -> &str {
    kind.strip_prefix("an ")
        .or_else(|| kind.strip_prefix("a "))
        .unwrap_or(kind)
}

/// Its tag a word of its own, so every value's payload starts at one
/// place and a value is copied whole: copied in parts, as a `u8` tag puts
/// a boolean beside it, a value read back whole waits on the parts (a
/// store the CPU can't forward), which slowed every step.
#[derive(Clone, Debug)]
#[repr(u64)]
pub enum Value<A> {
    Int(i64),
    Str(Rc<Text>),
    /// A cell of the data space.
    Addr(Address),
    List(Rc<List<A>>),
    App(A),
    /// An execution token: a word's code, which `execute` runs.
    Xt(Xt<A>),
    /// An open construct, while a definition is compiled: only its own
    /// definition's words resolve it.
    Mark(Rc<Mark>),
    /// A failure, as a `try`'s handler gets it.
    Failure(Rc<Thrown<A>>),
    Bool(bool),
}

/// Where a cell is: which machine's data space, and which of its cells.
/// Only the machine's words make one (`next-address`, `create`), and an
/// integer never becomes one. A machine takes only its own data space's,
/// as it runs only its own xts; a copy of a machine has the same data
/// space's identity, so the addresses copied with it stay its own. The
/// two are one word, so no value is larger for them and making one
/// allocates nothing: the cell in the low `CELL_BITS` bits, the data
/// space above.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address(u64);

/// How many bits say which cell: 2^40 cells are more than any machine's
/// memory holds (16 bytes or more each), so a data space never reaches
/// more before its allocator refuses it. The 24 above say which data space.
const CELL_BITS: u32 = 40;
/// The bits that say which cell.
const CELL_MASK: u64 = 0xFF_FFFF_FFFF;
/// How many data spaces there may be in a process: 2^24.
pub(crate) const DATA_SPACES: u32 = 0x100_0000;

impl Address {
    /// The address of cell `at` of data space `space`, if they fit: `at`
    /// below 2^40, `space` below `DATA_SPACES`.
    pub(crate) fn new(space: u32, at: usize) -> Option<Self> {
        let at = u64::try_from(at).ok().filter(|at| *at <= CELL_MASK)?;
        let space = (space < DATA_SPACES).then_some(u64::from(space))?;
        Some(Self(space.checked_shl(CELL_BITS)? | at))
    }

    /// Which data space it is of.
    pub(crate) fn space(self) -> u32 {
        u32::try_from(self.0.checked_shr(CELL_BITS).unwrap_or(0)).unwrap_or(u32::MAX)
    }

    /// Which cell it is.
    pub(crate) fn at(self) -> usize {
        usize::try_from(self.0 & CELL_MASK).unwrap_or(usize::MAX)
    }

    /// How many cells on from `other` it is, if they are of one data
    /// space. An address is from 0 to the largest integer
    /// (`Machine::moved_by`), so this never wraps.
    pub(crate) fn cells_from(self, other: Address) -> Option<i64> {
        let at = |a: Address| i64::try_from(a.at()).unwrap_or(i64::MAX);
        (self.space() == other.space()).then(|| at(self).wrapping_sub(at(other)))
    }
}

/// A failure as a value: what a `try`'s handler gets, and `throw` raises
/// again. Its code and where it happened, its number in Forth's table of
/// `throw` codes (its code's, or the one `throw` was given), the value it
/// carries (a failed wait's answer), the steps whose words had taken
/// effect before it (`Machine::acted`), and the calls under way where it
/// happened (`Machine::calls`), if catching it ended some.
#[derive(Clone, Debug)]
pub struct Thrown<A> {
    pub error: Error,
    pub number: Option<i64>,
    pub carried: Option<Value<A>>,
    pub actions: Vec<Token>,
    pub calls: Vec<Token>,
}

/// An execution token: what `tick`, `find` and `code … end` give, and
/// `execute` runs. Two are equal when they are the same word's, the same
/// version of it.
#[derive(Clone, Debug)]
pub struct Xt<A>(pub(crate) Rc<Steps<A>>);

impl<A> PartialEq for Xt<A> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<A> Xt<A> {
    /// The name it was defined with: empty for `code … end`'s.
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// The words `code … end` read between, as read, so `code` WORDS
    /// `end` read again compiles them again: none for any other word's,
    /// nor for code opened or closed by another word.
    pub fn words(&self) -> Option<&[Token]> {
        self.0.words.as_ref().map(crate::token::Words::tokens)
    }
}

/// A construct open while compiling: what opened it, in which definition,
/// at which of its steps, and where in the source.
#[derive(Debug)]
pub struct Mark {
    pub(crate) construct: Construct,
    pub(crate) of: u64,
    pub(crate) at: usize,
    pub(crate) token: Option<Token>,
}

/// How many characters apart a string's marks are.
const STRIDE: usize = 64;

/// A string's text, with how many characters it has and, unless they are
/// all ASCII (then character `i` starts at byte `i`), its marks: where
/// characters `STRIDE`, 2 × `STRIDE`, … start, so a character is found
/// from the mark before it, or the start.
#[derive(Clone, Debug)]
pub struct Text {
    text: String,
    chars: usize,
    marks: Vec<usize>,
}

/// Two strings are equal when their text is: the rest follows from it.
impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Text {}

impl Text {
    pub fn new(text: impl Into<String>) -> Text {
        let text = text.into();
        if text.is_ascii() {
            let chars = text.len();
            return Text {
                text,
                chars,
                marks: Vec::new(),
            };
        }
        let chars = text.chars().count();
        let mut marks = Vec::with_capacity(chars.saturating_sub(1) / STRIDE);
        marks.extend(starts(&text, 0, 0));
        Text { text, chars, marks }
    }

    /// `other` added at the end, in place.
    pub(crate) fn append(&mut self, other: &Text) {
        let (bytes, chars, ascii) = (self.text.len(), self.chars, self.is_ascii());
        self.text.push_str(&other.text);
        self.chars = chars.saturating_add(other.chars);
        if self.is_ascii() {
            return;
        }
        if ascii {
            let marks = (1..chars.div_ceil(STRIDE)).map(|k| k.saturating_mul(STRIDE));
            self.marks.extend(marks);
        }
        self.marks.extend(starts(&other.text, chars, bytes));
    }

    /// How many characters it has.
    pub fn chars_len(&self) -> usize {
        self.chars
    }

    fn is_ascii(&self) -> bool {
        self.chars == self.text.len()
    }

    /// The characters from `from` up to before `to`.
    pub fn part(&self, from: usize, to: usize) -> &str {
        let (start, end) = (self.byte(from), self.byte(to));
        self.text.get(start..end).unwrap_or_default()
    }

    /// The byte where character `i` starts; the end, for no such character.
    fn byte(&self, i: usize) -> usize {
        let len = self.text.len();
        if self.is_ascii() {
            return i.min(len);
        }
        let mark = match (i / STRIDE).checked_sub(1) {
            None => 0,
            Some(k) => match self.marks.get(k) {
                Some(&mark) => mark,
                None => return len,
            },
        };
        let rest = self.text.get(mark..).unwrap_or_default();
        let found = rest.char_indices().nth(i % STRIDE);
        found.map_or(len, |(at, _)| mark.saturating_add(at))
    }
}

/// The marks in `text` when its first character is character `first`,
/// and its first byte byte `byte`, of a string.
fn starts(text: &str, first: usize, byte: usize) -> impl Iterator<Item = usize> {
    (text.char_indices().zip(first..))
        .filter(|(_, i)| *i != 0 && i % STRIDE == 0)
        .map(move |((at, _), _)| byte.saturating_add(at))
}

impl std::ops::Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// A list's values.
#[derive(Clone, Debug)]
pub struct List<A> {
    items: Vec<Value<A>>,
}

/// Two lists are equal when their values are.
impl<A: PartialEq> PartialEq for List<A> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

/// The lists and code in it are let go one after another, not as a
/// recursion, so values nested however deep never overflow the stack.
impl<A> Drop for List<A> {
    fn drop(&mut self) {
        release(std::mem::take(&mut self.items));
    }
}

/// Lets `values` go, and what each held that nothing else holds: lists'
/// items, and code's values (a list in code in a list…), from a stack of
/// its own, not as a recursion.
pub(crate) fn release<A>(mut values: Vec<Value<A>>) {
    while let Some(value) = values.pop() {
        match value {
            Value::List(list) => {
                if let Ok(mut list) = Rc::try_unwrap(list) {
                    values.append(&mut list.items);
                }
            }
            Value::Xt(Xt(code)) => {
                if let Ok(mut code) = Rc::try_unwrap(code) {
                    code.held(&mut values);
                }
            }
            Value::Failure(thrown) => {
                if let Ok(thrown) = Rc::try_unwrap(thrown) {
                    values.extend(thrown.carried);
                }
            }
            Value::Int(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::App(_)
            | Value::Mark(_)
            | Value::Addr(_) => {}
        }
    }
}

impl<A> List<A> {
    pub fn new(items: Vec<Value<A>>) -> Self {
        Self { items }
    }

    pub fn items(&self) -> &[Value<A>] {
        &self.items
    }

    /// `value` added at the end.
    pub fn push(&mut self, value: Value<A>) {
        self.items.push(value);
    }

    /// Item `at` replaced by `value`; `None` past the end.
    pub fn put(&mut self, at: usize, value: Value<A>) -> Option<()> {
        *self.items.get_mut(at)? = value;
        Some(())
    }
}

/// Two values are equal when they are of one kind and hold the same: lists
/// element by element, walked with a stack of their own, not as a
/// recursion, so lists nested however deep compare.
impl<A: PartialEq> PartialEq for Value<A> {
    fn eq(&self, other: &Self) -> bool {
        let mut pairs = vec![(self, other)];
        while let Some(pair) = pairs.pop() {
            match pair {
                (Self::List(a), Self::List(b)) if a.items.len() == b.items.len() => {
                    if !Rc::ptr_eq(a, b) {
                        pairs.extend(a.items.iter().zip(&b.items));
                    }
                }
                (Self::Int(a), Self::Int(b)) if a == b => {}
                (Self::Bool(a), Self::Bool(b)) if a == b => {}
                (Self::Addr(a), Self::Addr(b)) if a == b => {}
                (Self::Str(a), Self::Str(b)) if a == b => {}
                (Self::App(a), Self::App(b)) if a == b => {}
                (Self::Xt(a), Self::Xt(b)) if a == b => {}
                (Self::Mark(a), Self::Mark(b)) if Rc::ptr_eq(a, b) => {}
                (Self::Failure(a), Self::Failure(b))
                    if (&a.error, a.number, &a.actions, &a.calls)
                        == (&b.error, b.number, &b.actions, &b.calls) =>
                {
                    match (&a.carried, &b.carried) {
                        (Some(a), Some(b)) => pairs.push((a, b)),
                        (None, None) => {}
                        (Some(_), None) | (None, Some(_)) => return false,
                    }
                }
                (
                    Self::Int(_)
                    | Self::Bool(_)
                    | Self::Str(_)
                    | Self::List(_)
                    | Self::App(_)
                    | Self::Xt(_)
                    | Self::Mark(_)
                    | Self::Failure(_)
                    | Self::Addr(_),
                    _,
                ) => {
                    return false;
                }
            }
        }
        true
    }
}

impl<A: AppValue> Value<A> {
    /// What kind it is, as an error says it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::App(a) => a.kind(),
            Self::Int(_) => "an integer",
            Self::Bool(_) => "a boolean",
            Self::Str(_) => "a string",
            Self::List(_) => "a list",
            Self::Xt(_) => "an xt",
            Self::Mark(_) => "a mark",
            Self::Failure(_) => "a failure",
            Self::Addr(_) => "an address",
        }
    }

    /// What kind it is, as `kind` says it: `integer`, `boolean`, `string`,
    /// `list`, `address`, or the application's kind without its article
    /// (`pane`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::App(a) => a.name(),
            Self::Int(_)
            | Self::Bool(_)
            | Self::Str(_)
            | Self::List(_)
            | Self::Xt(_)
            | Self::Mark(_)
            | Self::Failure(_)
            | Self::Addr(_) => bare(self.kind()),
        }
    }

    /// The value as text.
    pub fn text(&self) -> Result<String, Failure> {
        let mut out = String::new();
        self.write_text(&mut out)?;
        Ok(out)
    }

    /// Streams its text into `out`, without a whole rendered copy: a
    /// list's items a space apart, walked with a stack of the lists under
    /// way, not as a recursion, so lists nested however deep are written.
    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        let mut lists: Vec<(std::slice::Iter<'_, Self>, bool)> = Vec::new();
        let mut value = self;
        loop {
            match value {
                Self::Int(n) => write!(out, "{n}").map_err(|_| unwritten())?,
                Self::Bool(b) => write!(out, "{b}").map_err(|_| unwritten())?,
                Self::Str(text) => out.write_str(text).map_err(|_| unwritten())?,
                Self::List(list) => lists.push((list.items.iter(), false)),
                Self::App(app) => app.write_text(out)?,
                Self::Xt(xt) => write!(out, "(xt {})", xt.name()).map_err(|_| unwritten())?,
                Self::Mark(mark) => {
                    write!(out, "(mark {})", mark.construct.name()).map_err(|_| unwritten())?
                }
                Self::Failure(thrown) => {
                    write!(out, "{}", thrown.error).map_err(|_| unwritten())?
                }
                Self::Addr(at) => write!(out, "(address {})", at.at()).map_err(|_| unwritten())?,
            }
            // The next item of the innermost list under way.
            value = loop {
                let Some((items, started)) = lists.last_mut() else {
                    return Ok(());
                };
                if let Some(item) = items.next() {
                    if std::mem::replace(started, true) {
                        out.write_char(' ').map_err(|_| unwritten())?;
                    }
                    break item;
                }
                lists.pop();
            };
        }
    }
}

/// A text its sink refused.
fn unwritten() -> Failure {
    Failure::new(Code::Host("output"), "the text could not be written")
}

impl<A> Value<A> {
    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(Rc::new(Text::new(text)))
    }
}
