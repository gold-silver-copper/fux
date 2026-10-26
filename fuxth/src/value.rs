//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own.
use std::rc::Rc;

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) goes no deeper.
pub const NESTING: u8 = 64;

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
    /// How `string` and `print` show it.
    fn text(&self) -> String {
        self.kind().to_owned()
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
        let marks = marks(&text, 0, 0).collect();
        Text { text, chars, marks }
    }

    /// `other` added at the end: how many characters before it were
    /// marked, all of them the first time it holds more than ASCII.
    pub fn append(&mut self, other: &Text) -> usize {
        let (bytes, chars, ascii) = (self.text.len(), self.chars, self.is_ascii());
        self.text.push_str(&other.text);
        self.chars = chars.saturating_add(other.chars);
        if self.is_ascii() {
            return 0;
        }
        let marked = if ascii {
            let marks = (1..chars.div_ceil(STRIDE)).map(|k| k.saturating_mul(STRIDE));
            self.marks = marks.collect();
            chars
        } else {
            0
        };
        self.marks.extend(marks(&other.text, chars, bytes));
        marked
    }

    /// Character `i` replaced by `other`; nothing if there is none.
    pub fn replace(&mut self, i: usize, other: &Text) {
        if i >= self.chars {
            return;
        }
        let mut text = String::with_capacity(self.text.len().saturating_add(other.text.len()));
        text.push_str(self.part(0, i));
        text.push_str(&other.text);
        text.push_str(self.part(i.saturating_add(1), self.chars));
        *self = Text::new(text);
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

    /// A list of `items`; `None` if it would nest deeper than [`NESTING`]
    /// or hold more than [`TOTAL`] in all.
    pub fn new(items: Vec<Value<A>>) -> Option<Self> {
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

    /// `value` added at the end; `None` past the limits.
    pub fn push(&mut self, value: Value<A>) -> Option<()> {
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
    pub fn put(&mut self, at: usize, value: Value<A>) -> Option<usize> {
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

    /// The value as text, as `string` and `print` show it: digits, `true`
    /// or `false`, a string as it is, a list's values' text with a space
    /// between, the application's text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out, usize::MAX);
        out
    }

    /// The text, if it is at most `most` characters long.
    pub fn text_upto(&self, most: usize) -> Option<Text> {
        let mut out = String::new();
        // A character is at most 4 bytes: past that many, it is too long.
        let bytes = most.saturating_mul(4);
        let text = self.write_text(&mut out, bytes).then(|| Text::new(out))?;
        (text.chars_len() <= most).then_some(text)
    }

    /// Writes the text, stopping once `out` is past `bytes` long: whether
    /// it wasn't.
    fn write_text(&self, out: &mut String, bytes: usize) -> bool {
        match self {
            Self::Int(n) => out.push_str(&n.to_string()),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Str(s) => out.push_str(s),
            // As deep as lists nest, at most `NESTING`.
            Self::List(list) => {
                for (i, item) in list.items.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    if !item.write_text(out, bytes) {
                        return false;
                    }
                }
            }
            Self::App(a) => out.push_str(&a.text()),
        }
        out.len() <= bytes
    }
}

impl<A> Value<A> {
    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(Rc::new(Text::new(text)))
    }

    /// How much it holds in all: a list counts its values' weights and
    /// itself, a string its characters, anything else 1. A walk over the
    /// value goes through about this many things.
    pub fn weight(&self) -> u64 {
        match self {
            Self::List(list) => list.weight,
            Self::Str(s) => u64::try_from(s.chars_len()).unwrap_or(u64::MAX).max(1),
            Self::Int(_) | Self::Bool(_) | Self::App(_) => 1,
        }
    }

    /// How deep it nests: 0 for anything but a list.
    pub fn depth(&self) -> u8 {
        match self {
            Self::List(list) => list.depth,
            Self::Int(_) | Self::Bool(_) | Self::Str(_) | Self::App(_) => 0,
        }
    }
}
