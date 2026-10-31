//! Owned immutable callback arguments, shared by screens and the worker.
use crate::act::Line;
use crate::command::{AnyRef, Run};
use crate::id::ClientId;
use crate::wire::{Error, Reader, Wire, wire};

/// A screen retains no interpreter or thread-local reference counted value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Data {
    Int(i64),
    Bool(bool),
    Text(String),
    List(Vec<Data>),
    Record {
        of: Option<Run>,
        fields: Vec<(String, Data)>,
    },
    All,
    Null,
    Handle(AnyRef, Option<Run>),
    Client(ClientId, Option<Run>),
}

impl Data {
    fn read(r: &mut Reader<'_>, depth: u8) -> Result<Self, Error> {
        let depth = depth.checked_sub(1).ok_or(Error::Bad("callback depth"))?;
        Ok(match u8::get(r)? {
            0 => Self::Int(i64::get(r)?),
            1 => Self::Bool(bool::get(r)?),
            2 => Self::Text(r.bounded_text(fuxth_core::LONGEST.saturating_mul(4))?),
            3 => Self::List(r.bounded_list(fuxth_core::LONGEST, |r| Self::read(r, depth))?),
            4 => Self::Record {
                of: Option::get(r)?,
                fields: r.bounded_list(crate::inspect::NODES, |r| {
                    Ok((
                        r.bounded_text(fuxth_core::LONGEST.saturating_mul(4))?,
                        Self::read(r, depth)?,
                    ))
                })?,
            },
            5 => Self::Null,
            6 => Self::Handle(AnyRef::get(r)?, Option::get(r)?),
            7 => Self::Client(ClientId::get(r)?, Option::get(r)?),
            8 => Self::All,
            tag => return Err(Error::Tag("callback value", tag)),
        })
    }

    /// Retained bytes, including owned containers, used before queuing a job.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(match self {
            Self::Text(s) => s.capacity(),
            Self::List(v) => v.iter().fold(
                v.capacity().saturating_mul(std::mem::size_of::<Data>()),
                |n, v| n.saturating_add(v.bytes()),
            ),
            Self::Record { fields, .. } => fields.iter().fold(
                fields
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(String, Data)>()),
                |n, (k, v)| n.saturating_add(k.capacity()).saturating_add(v.bytes()),
            ),
            Self::Int(_)
            | Self::Bool(_)
            | Self::Null
            | Self::All
            | Self::Handle(..)
            | Self::Client(..) => 0,
        })
    }
}

// In the order of their tags.
crate::wire::shaped!(enum Data {
    Int(v: i64),
    Bool(v: bool),
    Text(v: String),
    List(v: Vec<Data>),
    Record { of: Option<Run>, fields: Vec<(String, Data)> },
    Null,
    Handle(v: AnyRef, run: Option<Run>),
    Client(v: ClientId, run: Option<Run>),
    All,
});

impl Wire for Data {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        match self {
            Self::Int(v) => {
                out.push(0);
                v.put(out);
            }
            Self::Bool(v) => {
                out.push(1);
                v.put(out);
            }
            Self::Text(v) => {
                out.push(2);
                v.put(out);
            }
            Self::List(v) => {
                out.push(3);
                v.put(out);
            }
            Self::Record { of, fields } => {
                out.push(4);
                of.put(out);
                fields.put(out);
            }
            Self::All => out.push(8),
            Self::Null => out.push(5),
            Self::Handle(v, run) => {
                out.push(6);
                v.put(out);
                run.put(out);
            }
            Self::Client(v, run) => {
                out.push(7);
                v.put(out);
                run.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Self::read(r, fuxth_core::NESTING.saturating_add(1))
    }
}

wire! {
    /// Explicit executable source and its inert, typed captured arguments.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Callback {
        pub code: String,
        pub args: Vec<Data>,
    }
}

impl From<String> for Callback {
    fn from(code: String) -> Self {
        Self {
            code,
            args: Vec::new(),
        }
    }
}
impl From<&str> for Callback {
    fn from(code: &str) -> Self {
        code.to_owned().into()
    }
}

/// The most bytes of code the programs waiting for a client hold, and of
/// what a screen it is sent retains.
pub const JOB_BYTES: usize = 4 << 20;

/// A typed screen request; only Callback.code is executable source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    List(ListScreen),
    Prompt(Prompt),
    Copy,
}

crate::wire::shaped!(enum Open {
    List(list: ListScreen),
    Prompt(prompt: Prompt),
    Copy,
});

impl Wire for Open {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        match self {
            Self::List(list) => {
                out.push(0);
                list.put(out);
            }
            Self::Prompt(prompt) => {
                out.push(1);
                prompt.put(out);
            }
            Self::Copy => out.push(2),
        }
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        r.limited(JOB_BYTES, |r| {
            Ok(match u8::get(r)? {
                0 => Self::List(ListScreen::get(r)?),
                1 => Self::Prompt(Prompt::get(r)?),
                2 => Self::Copy,
                tag => return Err(crate::wire::Error::Tag("screen", tag)),
            })
        })
    }
}

wire! {
    /// An admitted chooser with one callback and independent current and selection.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ListScreen {
        pub title: String,
        pub items: Vec<Item>,
        pub current: Option<usize>,
        pub selected: usize,
        pub about: Option<(AnyRef, Option<crate::command::Run>)>,
        pub action: Callback,
        pub client: Data,
    }
}
wire! {
    /// A row's inert label and actual immutable payload.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Item {
        pub label: String,
        pub payload: Data,
    }
}

/// A line being typed, and what Enter does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub line: Line,
    /// None: the line is fuxth to run. Some: fuxth run with the line on its
    /// stack, a string.
    pub action: Option<Callback>,
}

// Its line goes on the wire as its text.
crate::wire::shaped!(
    struct Prompt {
        title: String,
        line: String,
        action: Option<Callback>,
    }
);

impl Wire for Prompt {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.title.put(out);
        self.line.clone().text().put(out);
        self.action.put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        let title = String::get(r)?;
        let text = r.bounded_text(crate::act::LINE_MAX)?;
        let mut line = Line::new(String::new());
        if text.contains(char::is_control) {
            return Err(crate::wire::Error::Bad("prompt text"));
        }
        line.edit(&crate::act::Edit::Insert(text))
            .map_err(|_| crate::wire::Error::Bad("prompt length"))?;
        Ok(Self {
            title,
            line,
            action: Option::get(r)?,
        })
    }
}

impl Open {
    /// Aggregate retained storage, including callback code and row arguments.
    pub fn bytes(&self) -> usize {
        let callback = |c: &Callback| {
            c.args
                .iter()
                .fold(c.code.capacity(), |n, a| n.saturating_add(a.bytes()))
        };
        std::mem::size_of::<Self>().saturating_add(match self {
            Open::List(l) => l.items.iter().fold(
                l.items
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Item>())
                    .saturating_add(
                        l.title
                            .capacity()
                            .saturating_add(callback(&l.action))
                            .saturating_add(l.client.bytes()),
                    ),
                |n, i| {
                    n.saturating_add(i.label.capacity())
                        .saturating_add(i.payload.bytes())
                },
            ),
            Open::Prompt(p) => p
                .title
                .capacity()
                .saturating_add(p.line.before().len().saturating_add(p.line.after().len()))
                .saturating_add(p.action.as_ref().map_or(0, callback)),
            Open::Copy => 0,
        })
    }
}
