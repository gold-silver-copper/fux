//! Owned immutable callback arguments, shared by screens and the worker.
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
    Record(Vec<(String, Data)>),
    Snapshot(Box<crate::inspect::Snapshot>),
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
            2 => Self::Text(r.bounded_text(fuxth::LONGEST.saturating_mul(4))?),
            3 => Self::List(r.bounded_list(fuxth::LONGEST, |r| Self::read(r, depth))?),
            4 => Self::Record(r.bounded_list(crate::inspect::NODES, |r| {
                Ok((
                    r.bounded_text(fuxth::LONGEST.saturating_mul(4))?,
                    Self::read(r, depth)?,
                ))
            })?),
            5 => Self::Null,
            6 => Self::Handle(AnyRef::get(r)?, Option::get(r)?),
            7 => Self::Client(ClientId::get(r)?, Option::get(r)?),
            8 => Self::Snapshot(Box::get(r)?),
            tag => return Err(Error::Tag("callback value", tag)),
        })
    }

    /// Retained bytes, including owned containers, used before queuing a job.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(match self {
            Self::Text(s) => s.capacity(),
            Self::Snapshot(s) => s.heap_bytes().unwrap_or(usize::MAX),
            Self::List(v) => v.iter().fold(
                v.capacity().saturating_mul(std::mem::size_of::<Data>()),
                |n, v| n.saturating_add(v.bytes()),
            ),
            Self::Record(v) => v.iter().fold(
                v.capacity()
                    .saturating_mul(std::mem::size_of::<(String, Data)>()),
                |n, (k, v)| n.saturating_add(k.capacity()).saturating_add(v.bytes()),
            ),
            Self::Int(_) | Self::Bool(_) | Self::Null | Self::Handle(..) | Self::Client(..) => 0,
        })
    }
}

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
            Self::Record(v) => {
                out.push(4);
                v.put(out);
            }
            Self::Snapshot(v) => {
                out.push(8);
                v.put(out);
            }
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
        Self::read(r, fuxth::NESTING.saturating_add(1))
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

impl Callback {
    /// Instantiate a checked callback without constructing source from data.
    pub fn job(&self, args: impl IntoIterator<Item = Data>) -> crate::screens::Job {
        crate::screens::Job {
            code: self.code.clone(),
            given: Some(self.args.iter().cloned().chain(args).collect()),
            screen: None,
        }
    }
}
