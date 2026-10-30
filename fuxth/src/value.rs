//! Values on the stack: integers, booleans, strings, lists, and the
//! application's own.
use crate::error::{Code, Failure};
use crate::{ImportLease, Imports, Shared};

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) goes no deeper.
pub const NESTING: u8 = 64;

/// An application's own values: what it puts on the stack (a handle, a
/// request being built). Each says what kind it is, for errors, and how it
/// reads as text.
pub trait AppValue: Clone + PartialEq {
    /// A host's allocation grant for one complete text conversion.
    /// The default adds no host limit to the language's own bounds.
    const TEXT_LIMIT: usize = usize::MAX;

    /// Aggregate grant for live imported storage, including strings and lists.
    const IMPORT_LIMIT: usize = 128 * 1024 * 1024;

    /// Reserve each owned allocation, recursively visiting shared children.
    /// Heap-owning values reserve Shared storage here, or carry an ImportLease
    /// when ownership leaves a shared wrapper. Inline values need no reservation.
    fn retain(&self, _imports: &Imports) -> Result<(), Failure> {
        Ok(())
    }

    /// The allocation retained by a native byte payload, including its wrapper.
    /// Its traversal weight still charges work and bounds inclusion in lists.
    fn import_bytes(&self) -> Option<usize> {
        None
    }

    /// What kind it is, as an error says it: "a pane".
    fn kind(&self) -> &'static str;
    /// What kind it is, as `kind` says it: "pane". By default its
    /// `kind` without the article.
    fn name(&self) -> &'static str {
        bare(self.kind())
    }
    /// Retained payload and temporary workspace used by its text writer.
    /// A list charges each reference, conservatively including shared values.
    fn text_bytes(&self) -> usize {
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

    /// Streams the application's text into the aggregate conversion's sink.
    /// Writers propagate sink failure and allocate only the workspace declared
    /// by `text_bytes`, rather than building an intermediate String.
    /// Admission and output call it separately; both must describe the same text.
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
        let mut marks = Vec::with_capacity(marked(text.len(), chars));
        marks.extend(starts(&text, 0, 0));
        Text { text, chars, marks }
    }

    /// Bytes shared storage for a string `bytes` long, of `chars`
    /// characters, holds: what is reserved before it is made.
    pub fn size(bytes: usize, chars: usize) -> usize {
        Self::held((bytes, marked(bytes, chars)))
    }

    /// Bytes shared storage with `room` for bytes and marks holds.
    fn held((bytes, marks): (usize, usize)) -> usize {
        Shared::<Text>::bytes(
            marks
                .saturating_mul(size_of::<usize>())
                .saturating_add(bytes),
        )
    }

    /// Grown in place, if it lacks room for `other` added at the end
    /// ([`grown`]), its `lease` swapped first for `held`'s of what the grown
    /// storage holds.
    pub(crate) fn grow<E>(
        &mut self,
        other: &Text,
        lease: &mut ImportLease,
        held: impl FnOnce(usize) -> Result<ImportLease, E>,
    ) -> Result<(), E> {
        let (text, marks) = (&mut self.text, &mut self.marks);
        let bytes = text.len().saturating_add(other.text.len());
        let index = marked(bytes, self.chars.saturating_add(other.chars));
        let has = (text.capacity(), marks.capacity());
        let room = (
            grown(bytes, text.len(), has.0),
            grown(index, marks.len(), has.1),
        );
        if room != has {
            let new = held(Self::held(room))?;
            text.reserve_exact(room.0.saturating_sub(text.len()));
            marks.reserve_exact(room.1.saturating_sub(marks.len()));
            *lease = new;
        }
        Ok(())
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
            self.marks.extend(marks);
            chars
        } else {
            0
        };
        self.marks.extend(starts(&other.text, chars, bytes));
        marked
    }

    /// Space for shared Text storage and the peak character-index replacement,
    /// excluding the String moved into it. `None` if the size overflows.
    pub fn import_bytes(chars: usize, ascii: bool) -> Option<usize> {
        let marks = if ascii {
            0
        } else {
            chars.saturating_sub(1) / STRIDE
        };
        let index = if marks == 0 {
            0
        } else {
            let capacity = marks.checked_next_power_of_two()?.max(4);
            capacity
                .checked_add(capacity / 2)?
                .checked_mul(std::mem::size_of::<usize>())?
        };
        index.checked_add(Shared::<Text>::bytes(0))
    }

    /// Moves out the String, dropping its character index without a copy.
    pub fn into_string(self) -> String {
        self.text
    }

    /// Bytes retained in the text and its character index.
    pub fn heap_bytes(&self) -> usize {
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

/// The capacity that holds `need`, from `len` in `capacity`: the same if it
/// does, else at least twice `len`, as `Vec` grows, so pushes cost amortized
/// constant time.
fn grown(need: usize, len: usize, capacity: usize) -> usize {
    if need <= capacity {
        capacity
    } else {
        need.max(len.saturating_mul(2))
    }
}

/// How many marks a string `bytes` long, of `chars` characters, has.
fn marked(bytes: usize, chars: usize) -> usize {
    if bytes == chars {
        0
    } else {
        chars.saturating_sub(1) / STRIDE
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

    /// Bytes retained in the item vector, excluding the values' own storage.
    pub fn heap_bytes(&self) -> usize {
        self.items
            .capacity()
            .saturating_mul(std::mem::size_of::<Value<A>>())
    }

    /// Bytes shared storage for a list with room for `capacity` items holds:
    /// what is reserved before it is made.
    pub fn size(capacity: usize) -> usize {
        Shared::<Self>::bytes(capacity.saturating_mul(std::mem::size_of::<Value<A>>()))
    }

    /// Grown in place, if it lacks room for one more item ([`grown`]), its
    /// `lease` swapped first for `held`'s of what the grown storage holds.
    pub(crate) fn grow<E>(
        &mut self,
        lease: &mut ImportLease,
        held: impl FnOnce(usize) -> Result<ImportLease, E>,
    ) -> Result<(), E> {
        let (len, has) = (self.items.len(), self.items.capacity());
        let room = grown(len.saturating_add(1), len, has);
        if room != has {
            let new = held(Self::size(room))?;
            self.items.reserve_exact(room.saturating_sub(len));
            *lease = new;
        }
        Ok(())
    }
}

impl<A: AppValue> List<A> {
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

    /// The value as text, with one admission for retained inputs, formatter
    /// workspace and the entire output, including nested application values.
    pub fn text(&self) -> Result<String, Failure> {
        self.render_text(usize::MAX, None)?.ok_or_else(text_limit)
    }

    /// The text, if it is at most `most` characters long.
    pub fn text_upto(&self, most: usize) -> Result<Option<Text>, Failure> {
        Ok(self.render_text(most, None)?.map(Text::new))
    }

    /// `text_upto` as a string whose storage is reserved from `imports`
    /// once the text is counted, before it is made.
    pub fn string_upto(&self, most: usize, imports: &Imports) -> Result<Option<Self>, Failure> {
        let lease = ImportLease::default();
        let text = self.render_text(most, Some((imports, &lease)))?;
        Ok(text.map(|text| Self::Str(Shared::leased(Text::new(text), lease))))
    }

    fn own_bytes(&self) -> usize {
        match self {
            Self::Str(text) => Shared::<Text>::bytes(text.heap_bytes()),
            Self::List(list) => Shared::<List<A>>::bytes(list.heap_bytes()),
            Self::Int(_) | Self::Bool(_) | Self::App(_) => 0,
        }
    }

    /// Retained storage, counting each reference, with the application's own
    /// payload or formatter workspace. `None` when the aggregate overflows.
    pub fn storage_bytes(&self, app: impl Fn(&A) -> usize + Copy) -> Option<usize> {
        match self {
            Self::List(list) => list.items.iter().try_fold(self.own_bytes(), |n, item| {
                n.checked_add(item.storage_bytes(app)?)
            }),
            Self::App(value) => Some(app(value)),
            Self::Int(_) | Self::Bool(_) | Self::Str(_) => Some(self.own_bytes()),
        }
    }

    /// The text, if it is at most `most` characters long, `held` reserved
    /// for it once it is counted, before it is made.
    fn render_text(
        &self,
        most: usize,
        held: Option<(&Imports, &ImportLease)>,
    ) -> Result<Option<String>, Failure> {
        struct Count {
            output: crate::Render,
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
                self.output.write_str(text)
            }
        }
        let left = A::TEXT_LIMIT
            .checked_sub(
                self.storage_bytes(AppValue::text_bytes)
                    .ok_or_else(text_limit)?,
            )
            .ok_or_else(text_limit)?;
        let mut count = Count {
            output: crate::Render::new(left),
            chars: 0,
            most,
            long: false,
        };
        if let Err(failure) = self.write_text(&mut count) {
            return if count.long { Ok(None) } else { Err(failure) };
        }
        if let Some((imports, lease)) = held {
            lease.reserve(imports, Text::size(count.output.bytes(), count.chars))?;
        }
        count
            .output
            .finish(|out| self.write_text(out), text_limit)
            .map(Some)
    }

    fn write_text(&self, out: &mut dyn std::fmt::Write) -> Result<(), Failure> {
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

fn text_limit() -> Failure {
    Failure::new(Code::TooLong, "the text exceeds its allocation limit")
}

impl<A> Value<A> {
    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(Shared::new(Text::new(text)))
    }
}

impl<A: AppValue> Value<A> {
    /// Reserve live shared storage once per machine. Projected children retain
    /// their own leases even after a parent list or record has been dropped.
    pub fn retain(&self, imports: &Imports) -> Result<(), Failure> {
        match self {
            Self::Int(_) | Self::Bool(_) => Ok(()),
            Self::Str(text) => text.reserve(imports, self.own_bytes()),
            Self::List(list) => {
                list.reserve(imports, self.own_bytes())?;
                for value in list.items() {
                    value.retain(imports)?;
                }
                Ok(())
            }
            Self::App(app) => app.retain(imports),
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
