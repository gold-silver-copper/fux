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
        let kind = self.kind();
        kind.strip_prefix("an ")
            .or_else(|| kind.strip_prefix("a "))
            .unwrap_or(kind)
    }
    /// How `string` and `print` show it.
    fn text(&self) -> String {
        self.kind().to_owned()
    }
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
    text: Box<str>,
    chars: usize,
    ascii: bool,
}

impl Text {
    pub fn new(text: impl Into<Box<str>>) -> Text {
        let text = text.into();
        let ascii = text.is_ascii();
        let chars = if ascii {
            text.len()
        } else {
            text.chars().count()
        };
        Text { text, chars, ascii }
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

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        *self.text == *other
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// A list's values, and how deep it nests (1 for a list of no lists).
#[derive(Clone, Debug, PartialEq)]
pub struct List<A> {
    pub items: Vec<Value<A>>,
    depth: u8,
}

impl<A> List<A> {
    /// A list of `items`; `None` if it would nest deeper than [`NESTING`].
    pub fn new(items: Vec<Value<A>>) -> Option<List<A>> {
        let inner = items.iter().map(Value::depth).max().unwrap_or(0);
        let depth = inner.checked_add(1).filter(|d| *d <= NESTING)?;
        Some(List { items, depth })
    }

    pub fn depth(&self) -> u8 {
        self.depth
    }

    /// `value` added at the end; `None` past the nesting limit.
    pub fn push(&mut self, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        self.depth = self.depth.max(depth);
        self.items.push(value);
        Some(())
    }

    /// Item `at` replaced by `value`; `None` past the nesting limit or the
    /// end. The depth may stay higher than it now is: it bounds, it isn't
    /// exact.
    pub fn put(&mut self, at: usize, value: Value<A>) -> Option<()> {
        let depth = value.depth().checked_add(1).filter(|d| *d <= NESTING)?;
        let slot = self.items.get_mut(at)?;
        *slot = value;
        self.depth = self.depth.max(depth);
        Some(())
    }
}

impl<A: AppValue> Value<A> {
    /// What kind it is, as an error says it.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "an integer",
            Value::Bool(_) => "a boolean",
            Value::Str(_) => "a string",
            Value::List(_) => "a list",
            Value::App(a) => a.kind(),
        }
    }

    /// What kind it is, as `kind` says it: `integer`, `boolean`, `string`,
    /// `list`, or the application's kind without its article (`pane`).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "integer",
            Value::Bool(_) => "boolean",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::App(a) => a.name(),
        }
    }

    /// The value as text, as `string` and `print` show it: digits, `true`
    /// or `false`, a string as it is, a list's values' text with a space
    /// between, the application's text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write_text(&mut out);
        out
    }

    fn write_text(&self, out: &mut String) {
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
                    item.write_text(out);
                }
            }
            Value::App(a) => out.push_str(&a.text()),
        }
    }
}

impl<A> Value<A> {
    pub fn str(text: impl Into<Box<str>>) -> Value<A> {
        Value::Str(Rc::new(Text::new(text)))
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
