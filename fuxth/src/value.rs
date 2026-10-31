//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own. A string's and a list's stores are a region's, so
//! what a program builds is held by its run's region before it is made.
use crate::{BoundedString, BoundedVec, Code, Failure, NESTING, Refused, Region, Shared};

fuxth_core::bounded_only!();

/// An application's own values: what it puts on the stack (a handle, a
/// request being built). Each says what kind it is, for errors, and how it
/// reads as text.
pub trait AppValue: Clone + PartialEq {
    /// Holds what it keeps on the heap in `region`, as it is admitted into
    /// a run: a value in shared storage reserves it there
    /// ([`Shared::reserve`], or a [`Lease`](crate::Lease) it carries), visiting the
    /// values it holds. A value that holds nothing on the heap holds
    /// nothing.
    fn retain(&self, _region: &Region) -> Result<(), Failure> {
        Ok(())
    }

    /// Whether it is a native byte payload (a capture), which the run's
    /// region bounds once it holds it, rather than [`TOTAL`]. Its weight
    /// still charges work and bounds its place in lists.
    fn native(&self) -> bool {
        false
    }

    /// What kind it is, as an error says it: "a pane".
    fn kind(&self) -> &'static str;
    /// What kind it is, as `kind` says it: "pane". By default its
    /// `kind` without the article.
    fn name(&self) -> &'static str {
        bare(self.kind())
    }

    /// The workspace its text writer allocates while it writes, reserved
    /// from the run's region beside the text before it is written.
    fn scratch(&self) -> usize {
        0
    }

    /// Expanded work of traversing this value, including shared children.
    fn weight(&self) -> u64 {
        1
    }

    /// Nesting below this value, including opaque records.
    fn depth(&self) -> u8 {
        0
    }

    /// Streams the application's text into the conversion's sink. Writers
    /// propagate the sink's failure and allocate only the workspace
    /// [`scratch`](AppValue::scratch) says, rather than building an
    /// intermediate String. Counting and writing call it separately; both
    /// must describe the same text.
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
/// from the mark before it, or the start. Both are stores of its region.
#[derive(Debug)]
pub struct Text {
    text: BoundedString,
    chars: usize,
    marks: Option<BoundedVec<usize>>,
}

/// Two strings are equal when their text is: the rest follows from it.
impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        *self.text == *other.text
    }
}

impl Eq for Text {}

impl Text {
    /// `text`, made in `region`.
    pub fn new(region: &Region, text: &str) -> Result<Text, Refused> {
        Self::made(BoundedString::of(region, text)?)
    }

    /// `text`, its characters counted and, unless all ASCII, marked, in
    /// its region.
    pub fn made(text: BoundedString) -> Result<Text, Refused> {
        if text.is_ascii() {
            let chars = text.len();
            return Ok(Text {
                text,
                chars,
                marks: None,
            });
        }
        let chars = text.chars().count();
        let room = chars.saturating_sub(1) / STRIDE;
        let mut marks = BoundedVec::with_capacity(text.region(), room)?;
        marks.extend(starts(&text, 0, 0))?;
        Ok(Text {
            text,
            chars,
            marks: Some(marks),
        })
    }

    /// The same text, in stores of its own in the same region.
    pub(crate) fn copy(&self) -> Result<Text, Refused> {
        Ok(Text {
            text: self.text.copy()?,
            chars: self.chars,
            marks: self.marks.as_ref().map(BoundedVec::copy).transpose()?,
        })
    }

    /// `other` added at the end, in place: how many characters before it
    /// were marked, all of them the first time it holds more than ASCII.
    /// Its text and its marks grow as a vector does, reserved first;
    /// refused, it holds what it held.
    pub(crate) fn append(&mut self, other: &Text) -> Result<usize, Refused> {
        let (bytes, chars, ascii) = (self.text.len(), self.chars, self.is_ascii());
        let count = chars.saturating_add(other.chars);
        let all = bytes.saturating_add(other.text.len());
        if all != count {
            let region = self.text.region();
            let marks = (self.marks).get_or_insert_with(|| BoundedVec::new(region));
            let need = count.saturating_sub(1) / STRIDE;
            marks.reserve(need.saturating_sub(marks.len()))?;
        }
        self.text.push_str(&other.text)?;
        self.chars = count;
        let Some(marks) = self.marks.as_mut().filter(|_| all != count) else {
            return Ok(0);
        };
        let marked = if ascii {
            marks.extend((1..chars.div_ceil(STRIDE)).map(|k| k.saturating_mul(STRIDE)))?;
            chars
        } else {
            0
        };
        marks.extend(starts(&other.text, chars, bytes))?;
        Ok(marked)
    }

    /// The region its stores are in.
    pub fn region(&self) -> &Region {
        self.text.region()
    }

    /// The room its text and marks have, in all: what moves as it grows.
    pub(crate) fn capacity(&self) -> usize {
        let marks = self.marks.as_ref().map_or(0, BoundedVec::capacity);
        self.text.capacity().saturating_add(marks)
    }

    /// In shared storage reserved from its region: a string value.
    pub fn value<A>(self) -> Result<Value<A>, Refused> {
        let region = self.region().clone();
        Ok(Value::Str(Shared::made(&region, self)?))
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
            Some(k) => match self.marks.as_deref().and_then(|marks| marks.get(k)) {
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

/// The most a list may hold in all, counting through the lists and strings
/// in it (a list held twice counts twice): what bounds every walk over a
/// value, comparing or printing it.
pub const TOTAL: u64 = 16_000_000;

/// A list's values, how deep it nests (1 for a list of no lists), and how
/// much it holds in all ([`Value::weight`]). Its values are a store of its
/// region.
#[derive(Debug)]
pub struct List<A> {
    items: BoundedVec<Value<A>>,
    depth: u8,
    weight: u64,
}

/// Two lists are equal when their values are.
impl<A: PartialEq> PartialEq for List<A> {
    fn eq(&self, other: &Self) -> bool {
        *self.items == *other.items
    }
}

impl<A> List<A> {
    /// Its values. Only `List`'s own ways change them, so how deep it
    /// nests and how much it holds stay true.
    pub fn items(&self) -> &[Value<A>] {
        &self.items
    }

    /// The region its values' store is in.
    pub fn region(&self) -> &Region {
        self.items.region()
    }

    /// The values it has room for.
    pub(crate) fn capacity(&self) -> usize {
        self.items.capacity()
    }

    /// Room for `more` values past its last, reserved first: it grows as
    /// a vector does.
    pub(crate) fn reserve(&mut self, more: usize) -> Result<(), Refused> {
        self.items.reserve(more)
    }

    /// In shared storage reserved from its region: a list value.
    pub fn value(self) -> Result<Value<A>, Refused> {
        let region = self.region().clone();
        Ok(Value::List(Shared::made(&region, self)?))
    }
}

impl<A: Clone> List<A> {
    /// The same values, in a store of its own in the same region.
    pub(crate) fn copy(&self) -> Result<Self, Refused> {
        Ok(Self {
            items: self.items.copy()?,
            depth: self.depth,
            weight: self.weight,
        })
    }
}

impl<A: AppValue> List<A> {
    /// A list of `items`; `None` if it would nest deeper than [`NESTING`]
    /// or hold more than [`TOTAL`] in all.
    pub fn new(items: BoundedVec<Value<A>>) -> Option<Self> {
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

    /// `value` added at the end, in room [`reserve`](List::reserve)d for
    /// it; `None` past the limits.
    pub(crate) fn push(&mut self, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        let weight = self
            .weight
            .checked_add(value.weight())
            .filter(|w| *w <= TOTAL)?;
        self.items.push(value).ok()?;
        self.depth = self.depth.max(depth);
        self.weight = weight;
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

    /// The value as text, made in `region`: counted first, so its room
    /// and its writers' workspace are reserved before either is made.
    pub fn text(&self, region: &Region) -> Result<BoundedString, Failure> {
        self.render(usize::MAX, region)?.ok_or_else(text_limit)
    }

    /// The text, if it is at most `most` characters long, made in `region`.
    pub fn text_upto(&self, most: usize, region: &Region) -> Result<Option<Text>, Failure> {
        let text = self.render(most, region)?;
        Ok(text.map(Text::made).transpose()?)
    }

    /// `text_upto` as a string value.
    pub fn string_upto(&self, most: usize, region: &Region) -> Result<Option<Self>, Failure> {
        let text = self.text_upto(most, region)?;
        Ok(text.map(Text::value).transpose()?)
    }

    /// The text, if it is at most `most` characters long: counted, then
    /// written into exactly the room reserved for it, beside the workspace
    /// its writers take, which is held while they count and write.
    fn render(&self, most: usize, region: &Region) -> Result<Option<BoundedString>, Failure> {
        struct Count {
            bytes: usize,
            left: usize,
            chars: usize,
            most: usize,
            long: bool,
        }
        impl std::fmt::Write for Count {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.chars = self
                    .chars
                    .checked_add(text.chars().count())
                    .ok_or(std::fmt::Error)?;
                if self.chars > self.most {
                    self.long = true;
                    return Err(std::fmt::Error);
                }
                self.bytes = (self.bytes.checked_add(text.len()))
                    .filter(|bytes| *bytes <= self.left)
                    .ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        let workspace = region.reserve(self.scratch())?;
        let mut count = Count {
            bytes: 0,
            left: region.remaining(),
            chars: 0,
            most,
            long: false,
        };
        if let Err(failure) = self.write_text(&mut count) {
            return if count.long { Ok(None) } else { Err(failure) };
        }
        let mut text = BoundedString::with_capacity(region, count.bytes)?;
        self.write_text(&mut text)?;
        drop(workspace);
        Ok(Some(text))
    }

    /// The most workspace one of its application values' writers takes:
    /// they write one at a time.
    fn scratch(&self) -> usize {
        match self {
            Self::List(list) => list.items.iter().map(Self::scratch).max().unwrap_or(0),
            Self::App(app) => app.scratch(),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) => 0,
        }
    }

    /// Streams the text without allocating a whole rendered copy.
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
}

#[cold]
fn text_limit() -> Failure {
    Failure::new(Code::TooLong, "the text exceeds its allocation limit")
}

impl<A> Value<A> {
    /// `text` as a string value, made in `region`.
    pub fn str(region: &Region, text: &str) -> Result<Self, Refused> {
        Text::new(region, text)?.value()
    }
}

impl<A: AppValue> Value<A> {
    /// Held in `region` as it is admitted into a run: the box of a string
    /// or list a host made without one (their stores are their region's),
    /// the values in a list, and an application value's own bytes.
    pub fn retain(&self, region: &Region) -> Result<(), Failure> {
        match self {
            Self::Int(_) | Self::Bool(_) => Ok(()),
            Self::Str(text) => text.boxed(region),
            Self::List(list) => {
                list.boxed(region)?;
                for value in list.items() {
                    value.retain(region)?;
                }
                Ok(())
            }
            Self::App(app) => app.retain(region),
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
