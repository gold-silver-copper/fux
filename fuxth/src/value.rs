//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own.
use crate::Code;
use crate::Failure;
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

#[derive(Clone, Debug)]
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

/// The lists in it are let go one after another, not as a recursion, so a
/// list nested however deep never overflows the stack.
impl<A> Drop for List<A> {
    fn drop(&mut self) {
        let mut items = std::mem::take(&mut self.items);
        while let Some(item) = items.pop() {
            if let Value::List(list) = item
                && let Ok(mut list) = Rc::try_unwrap(list)
            {
                items.append(&mut list.items);
            }
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
                (Self::Str(a), Self::Str(b)) if a == b => {}
                (Self::App(a), Self::App(b)) if a == b => {}
                (Self::Int(_) | Self::Bool(_) | Self::Str(_) | Self::List(_) | Self::App(_), _) => {
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
