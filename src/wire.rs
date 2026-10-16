//! Requests on the wire: a typed [`Command`] written as bytes and read back
//! as the same value, so that the server receives what a client's words
//! built, and never reads words itself. Big-endian numbers; a string or a
//! list is its length then its items; an option or an enum its tag first.
use crate::command::{
    AnyRef, CaptureView, Choices, ClientAction, Command, Extent, Input, Kind, Mouse, MoveTo,
    PanePick, PaneResize, Pick, Resized, Sibling, SwapWith, Until, Wait, WsRef,
};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use crate::layout::Axis;
use fuxix::process::Signal;
use std::time::Duration;

/// Why bytes are not the value they should be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes end before the value does.
    Truncated,
    /// A tag no variant has.
    Tag(&'static str, u8),
    /// A string that is not UTF-8.
    NotUtf8,
    /// A value out of what its type takes (a key name, a size of 0).
    Bad(&'static str),
    /// Bytes after the value.
    Trailing,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Truncated => f.write_str("the bytes end early"),
            Error::Tag(what, tag) => write!(f, "no {what} has tag {tag}"),
            Error::NotUtf8 => f.write_str("a text that is not UTF-8"),
            Error::Bad(what) => write!(f, "{what} out of range"),
            Error::Trailing => f.write_str("bytes after the value"),
        }
    }
}

impl std::error::Error for Error {}

/// Bytes being read.
pub struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader(bytes)
    }
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.0.split_first_chunk().ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }
    fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self.0.split_at_checked(len).ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(head)
    }
    /// The whole value: no bytes may follow.
    pub fn whole<T: Wire>(mut self) -> Result<T, Error> {
        let value = T::get(&mut self)?;
        match self.0.is_empty() {
            true => Ok(value),
            false => Err(Error::Trailing),
        }
    }
}

/// A value that goes on the wire.
pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(r: &mut Reader<'_>) -> Result<Self, Error>;
}

/// `value`'s bytes.
pub fn encode<T: Wire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.put(&mut out);
    out
}

/// `int_wire!(u16, …)` expands to `impl Wire for u16` and so on: the
/// number's big-endian bytes.
macro_rules! int_wire {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_be_bytes());
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                r.take().map(<$ty>::from_be_bytes)
            }
        }
    )*};
}
int_wire!(u8, u16, u32, u64, i64);

/// `nonzero_wire!(NonZeroU8 u8 "status", …)` expands to `impl Wire for
/// NonZeroU8` and so on: the number's bytes, 0 refused as `Bad("status")`.
macro_rules! nonzero_wire {
    ($($ty:ident $int:ident $what:literal),*) => {$(
        impl Wire for std::num::$ty {
            fn put(&self, out: &mut Vec<u8>) {
                self.get().put(out);
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                Self::new($int::get(r)?).ok_or(Error::Bad($what))
            }
        }
    )*};
}
nonzero_wire!(NonZeroU8 u8 "status", NonZeroU16 u16 "size");

/// Nothing: a value of no bytes, as an empty frame's payload.
impl Wire for () {
    fn put(&self, _: &mut Vec<u8>) {}
    fn get(_: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(())
    }
}

impl Wire for usize {
    fn put(&self, out: &mut Vec<u8>) {
        u64::try_from(*self).unwrap_or(u64::MAX).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        usize::try_from(u64::get(r)?).map_err(|_| Error::Bad("count"))
    }
}

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(Error::Tag("flag", other)),
        }
    }
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        u32::try_from(self.len()).unwrap_or(u32::MAX).put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = u32::get(r)? as usize;
        let bytes = r.bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::NotUtf8)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            0 => Ok(None),
            1 => T::get(r).map(Some),
            other => Err(Error::Tag("option", other)),
        }
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        u32::try_from(self.len()).unwrap_or(u32::MAX).put(out);
        for item in self {
            item.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let count = u32::get(r)? as usize;
        // Every item takes a byte at least: a count past the bytes left is
        // a lie, not room to make.
        if count > r.0.len() {
            return Err(Error::Truncated);
        }
        (0..count).map(|_| T::get(r)).collect()
    }
}

impl<T: Wire> Wire for Box<T> {
    fn put(&self, out: &mut Vec<u8>) {
        (**self).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        T::get(r).map(Box::new)
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

impl<A: Wire, B: Wire, C: Wire> Wire for (A, B, C) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
        self.2.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?, C::get(r)?))
    }
}

impl Wire for Duration {
    fn put(&self, out: &mut Vec<u8>) {
        u64::try_from(self.as_millis()).unwrap_or(u64::MAX).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        u64::get(r).map(Duration::from_millis)
    }
}

/// `text_wire!(Ty, "what")` expands to `impl Wire for Ty`: its text
/// (`%3`, `C-a`), read back through `FromStr` (else `Error::Bad("what")`).
macro_rules! text_wire {
    ($ty:ty, $sigil:literal) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                self.to_string().put(out);
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                String::get(r)?.parse().map_err(|_| Error::Bad($sigil))
            }
        }
    };
}
text_wire!(PaneId, "pane id");
text_wire!(TabId, "tab id");
text_wire!(WsId, "workspace id");
text_wire!(ClientId, "client id");
text_wire!(KeyPress, "key");
text_wire!(crate::code::Code, "code");

wire_enum!(Axis, "axis" {
    0 => Horizontal,
    1 => Vertical,
});
wire_enum!(Kind, "kind" {
    0 => Pane,
    1 => Tab,
    2 => Workspace,
});
wire_enum!(Sibling, "sibling" {
    0 => Next,
    1 => Previous,
});

/// `closed_wire!(Ty "what", …)` expands to `impl Wire for Ty`, for a
/// fieldless enum whose `ALL` lists every variant in declaration order: a
/// variant's tag is its place there. No variant can be missed, nor two share
/// a tag; reordering the declaration (fux-vt's, fuxix's) changes the wire.
macro_rules! closed_wire {
    ($($ty:ty, $what:literal);* $(;)?) => {$(
        impl $crate::wire::Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                out.push(*self as u8);
            }
            fn get(r: &mut $crate::wire::Reader<'_>) -> Result<Self, $crate::wire::Error> {
                let tag = <u8 as $crate::wire::Wire>::get(r)?;
                let all = Self::ALL.get(usize::from(tag)).copied();
                all.ok_or($crate::wire::Error::Tag($what, tag))
            }
        }
    )*};
}
pub(crate) use closed_wire;

closed_wire!(Direction, "direction"; MouseButton, "mouse button"; Signal, "signal");

impl Wire for fux_vt::Size {
    fn put(&self, out: &mut Vec<u8>) {
        self.rows().put(out);
        self.cols().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (rows, cols) = (u16::get(r)?, u16::get(r)?);
        fux_vt::Size::new(rows, cols).map_err(|_| Error::Bad("size"))
    }
}

/// `wire_enum!(Ty, "what" { 0 => Unit, 1 => Tuple(a, b), 2 => Named { x, y } })`
/// expands to `impl Wire for Ty`: `put` writes a variant's tag, then its
/// fields in the order listed; `get` reads the tag, then each field back in
/// that order (an unknown tag is `Error::Tag("what", tag)`). One list says
/// both, so the two cannot drift apart. `Ty<T>` takes a parameter that is
/// itself `Wire`.
macro_rules! wire_enum {
    ($ty:ident $(<$g:ident>)?, $what:literal {
        $($tag:literal => $var:ident $(( $($p:ident),* ))? $({ $($f:ident),* })?),* $(,)?
    }) => {
        impl$(<$g: $crate::wire::Wire>)? $crate::wire::Wire for $ty$(<$g>)? {
            fn put(&self, out: &mut Vec<u8>) {
                match self {$(
                    Self::$var $(( $($p),* ))? $({ $($f),* })? => {
                        out.push($tag);
                        $($($crate::wire::Wire::put($p, out);)*)?
                        $($($crate::wire::Wire::put($f, out);)*)?
                    }
                )*}
            }
            fn get(
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<Self, $crate::wire::Error> {
                Ok(match <u8 as $crate::wire::Wire>::get(r)? {
                    $($tag => {
                        $($(let $p = $crate::wire::Wire::get(r)?;)*)?
                        $($(let $f = $crate::wire::Wire::get(r)?;)*)?
                        Self::$var $(( $($p),* ))? $({ $($f),* })?
                    })*
                    other => return Err($crate::wire::Error::Tag($what, other)),
                })
            }
        }
    };
}
pub(crate) use wire_enum;

/// `wire_struct!(Ty { a, b })` expands to `impl Wire for Ty`: its fields
/// one after another, in the order listed (which is the wire's, not
/// always the declaration's).
macro_rules! wire_struct {
    ($ty:path { $($f:ident),* $(,)? }) => {
        impl $crate::wire::Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                $($crate::wire::Wire::put(&self.$f, out);)*
            }
            fn get(
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<Self, $crate::wire::Error> {
                Ok(Self { $($f: $crate::wire::Wire::get(r)?),* })
            }
        }
    };
}
pub(crate) use wire_struct;

wire_enum!(WsRef, "workspace" { 0 => Id(id), 1 => Name(name) });
wire_enum!(AnyRef, "target" { 0 => Pane(p), 1 => Tab(t), 2 => Workspace(w) });
wire_enum!(Pick<T>, "pick" { 0 => Step(s), 1 => Id(id) });
wire_enum!(PanePick, "pane pick" { 0 => Step(s), 1 => Id(id), 2 => Last, 3 => Toward(d) });
wire_enum!(MoveTo, "move" {
    0 => Tab(t),
    1 => Workspace(w),
    2 => NewTab,
    3 => NewWorkspace,
    4 => Beside(d),
});
wire_enum!(SwapWith, "swap" { 0 => Pane(p), 1 => Toward(d) });
wire_enum!(Extent, "extent" { 0 => Cells(n), 1 => Percent(n) });
wire_enum!(PaneResize, "resize" { 0 => Border(d, n), 1 => To(axis, extent) });
wire_enum!(Resized, "resized" { 0 => Tabs(t, size), 1 => Client(c, size) });
wire_enum!(Input, "input" { 0 => Key(k), 1 => Text(t), 2 => Paste(t) });
wire_struct!(CaptureView {
    full,
    join,
    styles,
    main,
    alt,
    last_output,
    marks,
    rows,
    cols
});
wire_enum!(Until, "condition" { 0 => Text(t), 1 => Gone(t), 4 => Exit, 6 => Bell, 7 => Settled });
wire_struct!(Wait {
    until,
    timeout,
    print,
    after,
    marks
});
wire_enum!(Mouse, "mouse" {
    0 => Click(b),
    1 => Press(b),
    2 => Release(b),
    3 => Wheel(b),
    4 => Move,
});
wire_enum!(Choices, "choices" { 0 => Tabs, 1 => Workspaces, 2 => Panes(pane) });
wire_enum!(ClientAction, "client action" {
    0 => Detach,
    1 => Capture,
    2 => Choices(choices),
    3 => Run(code),
    4 => Keys(inputs),
    10 => Zoom,
    11 => SelectPane(p),
    12 => SelectTab(p),
    13 => SelectWorkspace(p),
});

wire_enum!(Command, "command" {
    0 => Ls { target, get },
    1 => KillServer { force },
    2 => NewWorkspace { name, cmd, size, cwd, env, ids },
    3 => NewTab { target, name, cmd, size, cwd, env, ids },
    4 => Split { axis, target, cmd, cwd, env, run, ids, then },
    5 => Run { workspace, name, size, cwd, env, argv, log, ids, then },
    6 => Respawn { target, kill, env, argv, then },
    7 => Notify { target, text },
    8 => Resize(what),
    9 => Wait { target, wait },
    10 => SendMouse { target, mouse, row, col, then },
    11 => Kill { target },
    12 => Signal { target, signal, then },
    15 => Rename { target, name },
    16 => MovePane { target, to },
    17 => SwapPane { target, with },
    18 => ResizePane { target, by },
    19 => SendKeys { target, inputs, then },
    20 => Events { target, json },
    21 => DumpLayout { target },
    22 => Grant { workspace, socket },
    33 => Revoke { socket },
    23 => Log { target, file },
    24 => ClearHistory { target },
    25 => CapturePane { target, history, view, settle },
    26 => Reorder { target, toward },
    27 => Set { option, value },
    28 => Resolve { text, kind },
    29 => ListBuffers,
    30 => ShowBuffer { index },
    31 => PasteBuffer { index, target },
    32 => Client { client, action },
});
