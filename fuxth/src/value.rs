//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own.
use crate::error::{Code, Failure};
use crate::{Grant, Render, Reservation, Shared};
use std::rc::Rc;

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) goes no deeper.
pub const NESTING: u8 = 64;

/// An application's own values: what it puts on the stack (a handle, a
/// request being built). Each says what kind it is, for errors, and how it
/// reads as text. Storage it holds is reserved, as it is made, from the
/// run's grant.
pub trait AppValue: Clone + PartialEq {
    /// What kind it is, as an error says it: "a pane".
    fn kind(&self) -> &'static str;
    /// What kind it is, as `kind` says it: "pane". By default its
    /// `kind` without the article.
    fn name(&self) -> &'static str {
        bare(self.kind())
    }

    /// Expanded work of traversing this value, including shared children.
    fn weight(&self) -> u64 {
        1
    }

    /// Nesting below this value, including opaque records.
    fn depth(&self) -> u8 {
        0
    }

    /// Streams the application's text into the conversion's sink, which
    /// counts it before the text is made, then writes it. Both passes must
    /// write the same text.
    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        out.write_str(self.kind()).map_err(|_| text_limit())
    }
}

/// A kind said without its article: "an integer" as "integer".
fn bare(kind: &str) -> &str {
    kind.strip_prefix("an ")
        .or_else(|| kind.strip_prefix("a "))
        .unwrap_or(kind)
}

/// Expands `Variant "said", ...`, a row for each variant of `Value`, to the
/// enum `Kind` of those variants; `of`, a value's; `bit`, its bit in a
/// set of kinds; and `said`, each one's column:
/// `Int "an integer",` gives `Kind::Int`, said "an integer".
macro_rules! kinds {
    ($($variant:ident $said:literal,)*) => {
        /// What kind a value is, without what an application's value holds.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Kind {
            $($variant,)*
        }

        impl Kind {
            pub(crate) const ALL: [Kind; [$($said),*].len()] = [$(Kind::$variant),*];

            /// Its bit, in a set of kinds: an application's value's is
            /// the first of the bits its kinds take (`check::HOST`).
            pub(crate) const fn bit(self) -> u32 {
                1 << self as u32
            }

            pub(crate) fn of<A>(value: &Value<A>) -> Kind {
                match value {
                    $(Value::$variant(_) => Kind::$variant,)*
                }
            }

            /// As an error says it: "an integer". An application's value
            /// says its own kind, where the value is known.
            pub(crate) fn said(self) -> &'static str {
                match self {
                    $(Kind::$variant => $said,)*
                }
            }

            /// As an effect names it: "integer".
            pub(crate) fn name(self) -> &'static str {
                bare(self.said())
            }
        }
    };
}

kinds! {
    Int "an integer",
    Bool "a boolean",
    Str "a string",
    List "a list",
    App "an application value",
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value<A> {
    Int(i64),
    Bool(bool),
    Str(Shared<Text>),
    List(Shared<List<A>>),
    App(A),
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
    /// `parts` joined, as a string's shared storage: its size summed and
    /// reserved from `grant` before any of it is made.
    pub fn build<'a>(
        grant: &Rc<Grant>,
        parts: impl Iterator<Item = &'a str> + Clone,
    ) -> Result<Shared<Text>, Failure> {
        let len = (parts.clone()).try_fold(0usize, |n, part| n.checked_add(part.len()));
        let len = len.ok_or_else(|| grant.full())?;
        let held = Reservation::take(grant, len)?;
        let mut text = String::new();
        text.try_reserve_exact(len).map_err(|_| grant.full())?;
        parts.for_each(|part| text.push_str(part));
        Text::adopt(text, held)
    }

    /// One character, as a string's shared storage, reserved first.
    pub(crate) fn char(grant: &Rc<Grant>, c: char) -> Result<Shared<Text>, Failure> {
        let mut buffer = [0; 4];
        let c = c.encode_utf8(&mut buffer);
        let held = Reservation::take(grant, Shared::<Text>::bytes(c.len()))?;
        let text = Text {
            text: String::from(&*c),
            chars: 1,
            marks: Vec::new(),
        };
        Ok(Shared::new(text, held))
    }

    /// `text`, made already and held by `held`, as a string's shared
    /// storage: its marks and wrapper are reserved before they are made.
    pub fn adopt(text: String, mut held: Reservation) -> Result<Shared<Text>, Failure> {
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        let marked = marked(chars, ascii);
        let bytes = (marked.checked_mul(std::mem::size_of::<usize>()))
            .and_then(|marks| marks.checked_add(text.capacity()));
        held.resize(Shared::<Text>::bytes(bytes.unwrap_or(usize::MAX)))?;
        let mut marks = Vec::new();
        marks.try_reserve_exact(marked).map_err(|_| text_limit())?;
        if !ascii {
            marks.extend(self::marks(&text, 0, 0));
        }
        Ok(Shared::new(Text { text, chars, marks }, held))
    }

    /// `other` added at the end, the room it takes reserved in `held`
    /// first: how many characters before it were marked, all of them the
    /// first time it holds more than ASCII.
    pub(crate) fn append(
        &mut self,
        other: &Text,
        held: &mut Reservation,
    ) -> Result<usize, Failure> {
        let (bytes, chars, ascii) = (self.text.len(), self.chars, self.is_ascii());
        let all = chars.saturating_add(other.chars);
        let marked = marked(all, ascii && other.is_ascii());
        held.grow(&mut self.text, other.text.len())?;
        let more = marked.saturating_sub(self.marks.len());
        held.grow(&mut self.marks, more)?;
        self.text.push_str(&other.text);
        self.chars = all;
        if self.is_ascii() {
            return Ok(0);
        }
        let marked = if ascii {
            let marks = (1..chars.div_ceil(STRIDE)).map(|k| k.saturating_mul(STRIDE));
            self.marks.extend(marks);
            chars
        } else {
            0
        };
        self.marks.extend(marks(&other.text, chars, bytes));
        Ok(marked)
    }

    /// Bytes allocated for the text and its marks.
    pub fn bytes(&self) -> usize {
        self.text.capacity().saturating_add(
            self.marks
                .capacity()
                .saturating_mul(std::mem::size_of::<usize>()),
        )
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

/// How many marks a string of `chars` characters has.
fn marked(chars: usize, ascii: bool) -> usize {
    if ascii {
        0
    } else {
        chars.saturating_sub(1) / STRIDE
    }
}

/// The marks in `text` when its first character is character `first`,
/// and its first byte byte `byte`, of a string.
fn marks(text: &str, first: usize, byte: usize) -> impl Iterator<Item = usize> {
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

/// The most a list may hold in all, counting through the lists and strings
/// in it (a list held twice counts twice): what bounds every walk over a
/// value, comparing or printing it.
pub const TOTAL: u64 = 16_000_000;

/// A list's values, how deep it nests (1 for a list of no lists), and how
/// much it holds in all ([`Value::weight`]).
#[derive(Clone, Debug)]
pub struct List<A> {
    items: Vec<Value<A>>,
    depth: u8,
    weight: u64,
}

/// Two lists are equal when their values are.
impl<A: PartialEq> PartialEq for List<A> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl<A> List<A> {
    /// Its values. Only `List`'s own ways change them, so how deep it
    /// nests and how much it holds stay true.
    pub fn items(&self) -> &[Value<A>] {
        &self.items
    }

    /// Room for `more` items past its length, reserved in `held` first.
    pub(crate) fn grow(&mut self, held: &mut Reservation, more: usize) -> Result<(), Failure> {
        held.grow(&mut self.items, more)
    }
}

impl<A: AppValue> List<A> {
    /// A list of `items`; `None` if it would nest deeper than [`NESTING`]
    /// or hold more than [`TOTAL`] in all.
    fn new(items: Vec<Value<A>>) -> Option<Self> {
        let inner = items.iter().map(Value::depth).max().unwrap_or(0);
        let depth = inner.checked_add(1).filter(|d| *d <= NESTING)?;
        let weight = items
            .iter()
            .try_fold(0u64, |all, item| all.checked_add(item.weight()))?;
        let weight = weight.checked_add(1).filter(|w| *w <= TOTAL)?;
        Some(Self {
            items,
            depth,
            weight,
        })
    }

    /// `value` added at the end, within the room grown for it; `None` past
    /// the limits.
    pub(crate) fn push(&mut self, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        let weight = self
            .weight
            .checked_add(value.weight())
            .filter(|w| *w <= TOTAL)?;
        self.depth = self.depth.max(depth);
        self.weight = weight;
        self.items.push(value);
        Some(())
    }

    /// Item `at` replaced by `value`: how many items it looked through
    /// for how deep it nests now; `None` past the limits or the end.
    pub(crate) fn put(&mut self, at: usize, value: Value<A>) -> Option<usize> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        let old = self.items.get(at)?;
        let (old_depth, old_weight) = (old.depth(), old.weight());
        let weight = (self.weight.saturating_sub(old_weight))
            .checked_add(value.weight())
            .filter(|w| *w <= TOTAL)?;
        let slot = self.items.get_mut(at)?;
        *slot = value;
        self.weight = weight;
        // The deepest item may be the one replaced: then look again.
        if old_depth.saturating_add(1) >= self.depth && depth < self.depth {
            let inner = self.items.iter().map(Value::depth).max().unwrap_or(0);
            self.depth = inner.saturating_add(1);
            return Some(self.items.len());
        }
        self.depth = self.depth.max(depth);
        Some(0)
    }
}

/// A list being made: room for its items reserved from a grant before
/// it is allocated.
pub struct Items<A> {
    items: Vec<Value<A>>,
    held: Reservation,
}

impl<A: AppValue> Items<A> {
    /// Room for `count` items, reserved from `grant` first.
    pub fn new(grant: &Rc<Grant>, count: usize) -> Result<Self, Failure> {
        let bytes = count.checked_mul(std::mem::size_of::<Value<A>>());
        let bytes = bytes.ok_or_else(|| grant.full())?;
        let held = Reservation::take(grant, Shared::<List<A>>::bytes(bytes))?;
        let mut items = Vec::new();
        items.try_reserve_exact(count).map_err(|_| grant.full())?;
        Ok(Self { items, held })
    }

    /// `value` added at the end: past the room reserved, more is first.
    pub fn push(&mut self, value: Value<A>) -> Result<(), Failure> {
        self.held.grow(&mut self.items, 1)?;
        self.items.push(value);
        Ok(())
    }

    /// `values` added at the end: past the room reserved, more is first.
    pub fn extend(
        &mut self,
        values: impl ExactSizeIterator<Item = Value<A>>,
    ) -> Result<(), Failure> {
        self.held.grow(&mut self.items, values.len())?;
        self.items.extend(values);
        Ok(())
    }

    /// The list; `None` if it would nest deeper than [`NESTING`] or hold
    /// more than [`TOTAL`] in all.
    pub fn finish(self) -> Option<Value<A>> {
        let list = List::new(self.items)?;
        Some(Value::List(Shared::new(list, self.held)))
    }
}

impl<A: AppValue> Value<A> {
    /// What kind it is, as an error says it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::App(a) => a.kind(),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) | Self::List(_) => Kind::of(self).said(),
        }
    }

    /// What kind it is, as `kind` says it: `integer`, `boolean`, `string`,
    /// `list`, or the application's kind without its article (`pane`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::App(a) => a.name(),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) | Self::List(_) => bare(self.kind()),
        }
    }

    /// The value as text: counted, its bytes reserved from `grant`, then
    /// written into exactly that capacity.
    pub fn text(&self, grant: &Rc<Grant>) -> Result<(String, Reservation), Failure> {
        crate::written(grant, |out| self.write_text(out))
    }

    /// The text, if it is at most `most` bytes long: counted first, then
    /// written into exactly its capacity.
    pub fn text_upto(&self, most: usize) -> Result<Option<String>, Failure> {
        let mut count = Render::new(most);
        if self.write_text(&mut count).is_err() {
            return Ok(None);
        }
        count
            .finish(|out| self.write_text(out), text_limit)
            .map(Some)
    }

    /// Streams its text to `out`, as `print` shows it.
    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        match self {
            Self::Int(n) => write!(out, "{n}").map_err(|_| text_limit()),
            Self::Bool(b) => write!(out, "{b}").map_err(|_| text_limit()),
            Self::Str(text) => out.write_str(text).map_err(|_| text_limit()),
            Self::List(list) => {
                for (i, item) in list.items.iter().enumerate() {
                    if i > 0 {
                        out.write_char(' ').map_err(|_| text_limit())?;
                    }
                    item.write_text(out)?;
                }
                Ok(())
            }
            Self::App(app) => app.write_text(out),
        }
    }

    /// How much it holds in all: a list counts its values' weights and
    /// itself, a string its characters, anything else 1. A walk over the
    /// value goes through about this many things.
    pub fn weight(&self) -> u64 {
        match self {
            Self::List(list) => list.weight,
            Self::Str(s) => u64::try_from(s.chars_len()).unwrap_or(u64::MAX).max(1),
            Self::App(app) => app.weight(),
            Self::Int(_) | Self::Bool(_) => 1,
        }
    }

    /// How deep it nests: 0 for anything but a list.
    pub fn depth(&self) -> u8 {
        match self {
            Self::List(list) => list.depth,
            Self::App(app) => app.depth(),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) => 0,
        }
    }
}

fn text_limit() -> Failure {
    Failure::new(Code::TooLong, "the text exceeds its allocation limit")
}

impl<A> Value<A> {
    /// A string of `text`, its storage reserved from `grant` first.
    pub fn str(grant: &Rc<Grant>, text: &str) -> Result<Self, Failure> {
        Text::build(grant, std::iter::once(text)).map(Self::Str)
    }

    /// A program's literal string: its source bounds it.
    pub(crate) fn literal(text: &str) -> Self {
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        let marks = marks(text, 0, 0).take(marked(chars, ascii)).collect();
        let text = Text {
            text: text.to_owned(),
            chars,
            marks,
        };
        Self::Str(Shared::new(text, Reservation::none()))
    }
}
