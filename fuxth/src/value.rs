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
/// enum `Kind` of those variants; `of`, a value's; `BITS`, every kind's
/// `bit` (`Kind::Int.bit() | …`); and `said`, each one's column:
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

            /// Its bit, in a set of kinds.
            pub(crate) const fn bit(self) -> u8 {
                1 << self as u8
            }

            /// Every kind's bit.
            pub(crate) const BITS: u8 = 0 $(| Kind::$variant.bit())*;

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

/// A string's text, with how many characters it has and whether they are
/// all ASCII: then a character is found by its byte, at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    text: String,
    chars: usize,
    ascii: bool,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Text {
        let text = text.into();
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        Text { text, chars, ascii }
    }

    /// `other` added at the end.
    pub fn append(&mut self, other: &Text) {
        self.text.push_str(&other.text);
        self.chars = self.chars.saturating_add(other.chars);
        self.ascii &= other.ascii;
    }

    /// Character `i` replaced by `other`; nothing if there is none.
    pub fn replace(&mut self, i: usize, other: &Text) {
        let start = if self.ascii {
            Some(i).filter(|i| *i < self.text.len())
        } else {
            self.text.char_indices().nth(i).map(|(at, _)| at)
        };
        let Some(start) = start else {
            return;
        };
        let end = (self.text.get(start..))
            .and_then(|rest| rest.chars().next())
            .map_or(start, |c| start.saturating_add(c.len_utf8()));
        let (head, tail) = (self.text.get(..start), self.text.get(end..));
        let mut text = String::with_capacity(self.text.len().saturating_add(other.text.len()));
        text.push_str(head.unwrap_or_default());
        text.push_str(&other.text);
        text.push_str(tail.unwrap_or_default());
        self.text = text;
        self.chars = self.chars.saturating_sub(1).saturating_add(other.chars);
        self.ascii = (self.ascii && other.ascii) || self.text.is_ascii();
    }

    /// How many characters it has.
    pub fn chars_len(&self) -> usize {
        self.chars
    }

    /// Whether every character is ASCII.
    pub fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// Character `i`, from 0.
    pub fn char_at(&self, i: usize) -> Option<char> {
        if self.ascii {
            self.text.as_bytes().get(i).map(|b| char::from(*b))
        } else {
            self.text.chars().nth(i)
        }
    }

    /// The characters from `from` up to before `to`.
    pub fn part(&self, from: usize, to: usize) -> &str {
        if self.ascii {
            self.text.get(from..to).unwrap_or_default()
        } else {
            let mut at = self
                .text
                .char_indices()
                .map(|(i, _)| i)
                .chain([self.text.len()]);
            let start = at.nth(from).unwrap_or(self.text.len());
            let end = self
                .text
                .char_indices()
                .map(|(i, _)| i)
                .chain([self.text.len()])
                .nth(to)
                .unwrap_or(self.text.len());
            self.text.get(start..end).unwrap_or_default()
        }
    }
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
    fn eq(&self, other: &List<A>) -> bool {
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
    pub fn new(items: Vec<Value<A>>) -> Option<List<A>> {
        let inner = items.iter().map(Value::depth).max().unwrap_or(0);
        let depth = inner.checked_add(1).filter(|d| *d <= NESTING)?;
        let weight = items
            .iter()
            .try_fold(0u64, |all, item| all.checked_add(item.weight()))?;
        let weight = weight.checked_add(1).filter(|w| *w <= TOTAL)?;
        Some(List {
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

    /// Item `at` replaced by `value`; `None` past the limits or the end.
    pub fn put(&mut self, at: usize, value: Value<A>) -> Option<()> {
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
        self.depth = if old_depth.saturating_add(1) >= self.depth && depth < self.depth {
            let inner = self.items.iter().map(Value::depth).max().unwrap_or(0);
            inner.saturating_add(1)
        } else {
            self.depth.max(depth)
        };
        Some(())
    }
}

impl<A: AppValue> Value<A> {
    /// What kind it is, as an error says it.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::App(a) => a.kind(),
            Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) => {
                Kind::of(self).said()
            }
        }
    }

    /// What kind it is, as `kind` says it: `integer`, `boolean`, `string`,
    /// `list`, or the application's kind without its article (`pane`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::App(a) => a.name(),
            Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) => bare(self.kind()),
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
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Str(s) => out.push_str(s),
            // As deep as lists nest, at most `NESTING`.
            Value::List(list) => {
                for (i, item) in list.items.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    if !item.write_text(out, bytes) {
                        return false;
                    }
                }
            }
            Value::App(a) => out.push_str(&a.text()),
        }
        out.len() <= bytes
    }
}

impl<A> Value<A> {
    pub fn str(text: impl Into<String>) -> Value<A> {
        Value::Str(Rc::new(Text::new(text)))
    }

    /// How much it holds in all: a list counts its values' weights and
    /// itself, a string its characters, anything else 1. A walk over the
    /// value goes through about this many things.
    pub fn weight(&self) -> u64 {
        match self {
            Value::List(list) => list.weight,
            Value::Str(s) => u64::try_from(s.chars_len()).unwrap_or(u64::MAX).max(1),
            Value::Int(_) | Value::Bool(_) | Value::App(_) => 1,
        }
    }

    /// How deep it nests: 0 for anything but a list.
    pub fn depth(&self) -> u8 {
        match self {
            Value::List(list) => list.depth,
            Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::App(_) => 0,
        }
    }

    /// How many elements a string or list has; 0 for anything else.
    pub fn size(&self) -> usize {
        match self {
            Value::Str(s) => s.chars_len(),
            Value::List(list) => list.items.len(),
            Value::Int(_) | Value::Bool(_) | Value::App(_) => 0,
        }
    }
}
