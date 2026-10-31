//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own.
use crate::Failure;
use crate::{Code, NESTING};
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
    /// Nesting below this value, including opaque records.
    fn depth(&self) -> u8 {
        0
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
    Str(Rc<Text>),
    List(Rc<List<A>>),
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

/// A list's values, and how deep it nests (1 for a list of no lists).
#[derive(Clone, Debug)]
pub struct List<A> {
    items: Vec<Value<A>>,
    depth: u8,
}

/// Two lists are equal when their values are.
impl<A: PartialEq> PartialEq for List<A> {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl<A> List<A> {
    /// Its values. Only `List`'s own ways change them, so how deep it
    /// nests stays true.
    pub fn items(&self) -> &[Value<A>] {
        &self.items
    }
}

impl<A: AppValue> List<A> {
    /// A list of `items`; `None` if it would nest deeper than [`NESTING`].
    pub fn new(items: Vec<Value<A>>) -> Option<Self> {
        let inner = items.iter().map(Value::depth).max().unwrap_or(0);
        let depth = inner.checked_add(1).filter(|d| *d <= NESTING)?;
        Some(Self { items, depth })
    }

    /// `value` added at the end; `None` past [`NESTING`].
    pub fn push(&mut self, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        self.depth = self.depth.max(depth);
        self.items.push(value);
        Some(())
    }

    /// Item `at` replaced by `value`; `None` past [`NESTING`] or the end.
    pub fn put(&mut self, at: usize, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        let slot = self.items.get_mut(at)?;
        let old_depth = std::mem::replace(slot, value).depth();
        // The deepest item may be the one replaced: then look again.
        if old_depth.saturating_add(1) >= self.depth && depth < self.depth {
            let inner = self.items.iter().map(Value::depth).max().unwrap_or(0);
            self.depth = inner.saturating_add(1);
            return Some(());
        }
        self.depth = self.depth.max(depth);
        Some(())
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

    /// The value as text.
    pub fn text(&self) -> Result<String, Failure> {
        let mut out = String::new();
        self.write_text(&mut out)?;
        Ok(out)
    }

    /// Streams its text into `out`, without a whole rendered copy.
    pub fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
        match self {
            Self::Int(n) => write!(out, "{n}").map_err(|_| unwritten()),
            Self::Bool(b) => write!(out, "{b}").map_err(|_| unwritten()),
            Self::Str(text) => out.write_str(text).map_err(|_| unwritten()),
            Self::List(list) => {
                for (i, item) in list.items.iter().enumerate() {
                    if i > 0 {
                        out.write_char(' ').map_err(|_| unwritten())?;
                    }
                    item.write_text(out)?;
                }
                Ok(())
            }
            Self::App(app) => app.write_text(out),
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

impl<A: AppValue> Value<A> {
    /// How deep it nests: 0 for anything but a list.
    pub fn depth(&self) -> u8 {
        match self {
            Self::List(list) => list.depth,
            Self::App(app) => app.depth(),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) => 0,
        }
    }
}
