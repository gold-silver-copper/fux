//! Requests on the wire: a typed [`Command`] written as bytes and read back
//! as the same value, so that the server receives what a client's words
//! built, and never reads words itself. Big-endian numbers; a string or a
//! list is its length then its items; an option or an enum its tag first.
use crate::command::{Env, Percent};
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keys::mouse::MouseButton;
use crate::keys::{Direction, KeyPress};
use fuxix::process::Signal;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
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

/// An exit status that is not 0: 0 is refused.
impl Wire for std::num::NonZeroU8 {
    fn put(&self, out: &mut Vec<u8>) {
        self.get().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Self::new(u8::get(r)?).ok_or(Error::Bad("status"))
    }
}

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

/// A path, as its bytes: on Unix a path is any bytes, never made text.
impl Wire for std::path::PathBuf {
    fn put(&self, out: &mut Vec<u8>) {
        let bytes = self.as_os_str().as_bytes();
        u32::try_from(bytes.len()).unwrap_or(u32::MAX).put(out);
        out.extend_from_slice(bytes);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = u32::get(r)? as usize;
        Ok(std::ffi::OsString::from_vec(r.bytes(len)?.to_vec()).into())
    }
}

/// Variables, each name checked as it is read: one no shell takes is
/// refused.
impl Wire for Env {
    fn put(&self, out: &mut Vec<u8>) {
        let vars = self.as_slice();
        u32::try_from(vars.len()).unwrap_or(u32::MAX).put(out);
        vars.iter().for_each(|var| var.put(out));
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let mut env = Env::default();
        for (name, value) in Vec::<(String, String)>::get(r)? {
            env.push(name, value)
                .map_err(|_| Error::Bad("variable name"))?;
        }
        Ok(env)
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

/// `closed_wire!(Ty, "what"; Ty2, "what2")` expands to `impl Wire for` each,
/// a fieldless enum whose `ALL` lists every variant: a variant's one-byte
/// tag is its place there, written and read alike. No variant can be missed, nor two
/// share a tag; reordering `ALL` (fux-vt's, fuxix's) changes the wire. Each
/// also gets a `const` assertion that `ALL` has at most 256 places, so
/// every place is a byte: a longer one doesn't compile.
macro_rules! closed_wire {
    ($($ty:ty, $what:literal);* $(;)?) => {$(
        const _: () = assert!(<$ty>::ALL.len() <= 256);
        impl $crate::wire::Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                let place = Self::ALL.iter().position(|v| v == self);
                out.push(place.and_then(|p| u8::try_from(p).ok()).unwrap_or(u8::MAX));
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

/// `wire_enum!(Ty { Unit, Tuple(a, b), Named { x, y } })` expands to
/// `impl Wire for Ty`: `put` writes a variant's tag, one byte that is its
/// place in the list from 0 (`Unit` 0, `Tuple` 1, `Named` 2), then its
/// fields in the order listed; `get` reads the tag, then each field back
/// in that order (an unknown tag is `Error::Tag("Ty", tag)`). The places
/// are those of a hidden `#[repr(u8)] enum Tag { Unit, Tuple, Named }`
/// beside the impl, so no two variants share one, and a list longer than
/// 256 does not compile. Reordering the list changes the wire, as any
/// change to it does (`protocol::PROTOCOL`). `Ty<T>`, of one parameter,
/// is `Wire` where `T` is. This is for a type declared in another crate
/// (fux-vt's): a type of fux's own is declared in `wire!`, which writes
/// this list from the declaration.
macro_rules! wire_enum {
    ($ty:ident $(<$g:ident>)? {
        $($var:ident $(( $($p:ident),+ ))? $({ $($f:ident),* })?),* $(,)?
    }) => {
        const _: () = {
            #[repr(u8)]
            enum Tag {
                $($var),*
            }
            impl$(<$g: $crate::wire::Wire>)? $crate::wire::Wire for $ty$(<$g>)? {
                fn put(&self, out: &mut Vec<u8>) {
                    match self {$(
                        Self::$var $(( $($p),+ ))? $({ $($f),* })? => {
                            out.push(Tag::$var as u8);
                            $($($crate::wire::Wire::put($p, out);)+)?
                            $($($crate::wire::Wire::put($f, out);)*)?
                        }
                    )*}
                }
                fn get(
                    r: &mut $crate::wire::Reader<'_>,
                ) -> Result<Self, $crate::wire::Error> {
                    let tag = <u8 as $crate::wire::Wire>::get(r)?;
                    $(if tag == Tag::$var as u8 {
                        $($(let $p = $crate::wire::Wire::get(r)?;)+)?
                        $($(let $f = $crate::wire::Wire::get(r)?;)*)?
                        return Ok(Self::$var $(( $($p),+ ))? $({ $($f),* })?);
                    })*
                    Err($crate::wire::Error::Tag(stringify!($ty), tag))
                }
            }
        };
    };
}
pub(crate) use wire_enum;

/// `wire_struct!(Ty { a, b })` expands to `impl Wire for Ty`: its fields
/// one after another, in the order listed. For a type declared elsewhere;
/// one declared here says its fields once, in `wire!`.
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

/// A type declared and put on the wire at once, its fields written once:
///
/// ```ignore
/// wire! {
///     #[derive(Debug)]
///     pub enum Ty {
///         Unit,
///         Tuple(a: A, b: B),
///         Named { x: X },
///     }
/// }
/// ```
///
/// expands to the enum as written (its docs and attributes too), its tuple
/// variants' fields bare (`Tuple(A, B)`), and then `wire_enum!(Ty { Unit,
/// Tuple(a, b), Named { x } })`: `a` and `b` only name the fields for its
/// `Wire` impl, as a macro can't make up those names. A struct, `wire! {
/// pub struct Ty { pub x: X, y: Y } }`, expands to itself and
/// `wire_struct!(Ty { x, y })`. So a variant's tag is its place in the
/// declaration, from 0, and a field's place on the wire its place there.
macro_rules! wire {
    (
        $(#[$m:meta])*
        $vis:vis enum $ty:ident $(<$g:ident>)? {
            $(
                $(#[$vm:meta])*
                $var:ident
                $(( $($p:ident: $pt:ty),+ $(,)? ))?
                $({ $($(#[$fm:meta])* $f:ident: $ft:ty),* $(,)? })?
            ),* $(,)?
        }
    ) => {
        $(#[$m])*
        $vis enum $ty $(<$g>)? {
            $(
                $(#[$vm])*
                $var $(( $($pt),+ ))? $({ $($(#[$fm])* $f: $ft),* })?,
            )*
        }
        $crate::wire::wire_enum!($ty $(<$g>)? {
            $($var $(( $($p),+ ))? $({ $($f),* })?),*
        });
    };
    (
        $(#[$m:meta])*
        $vis:vis struct $ty:ident {
            $($(#[$fm:meta])* $fvis:vis $f:ident: $ft:ty),* $(,)?
        }
    ) => {
        $(#[$m])*
        $vis struct $ty {
            $($(#[$fm])* $fvis $f: $ft),*
        }
        $crate::wire::wire_struct!($ty { $($f),* });
    };
}
pub(crate) use wire;

impl Wire for Percent {
    fn put(&self, out: &mut Vec<u8>) {
        self.get().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Percent::new(u8::get(r)?).ok_or(Error::Bad("percent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CaptureView, Extent, Source};

    /// Each variant of an enum `closed_wire!` writes reads back as itself,
    /// and its tag is its place in `ALL`.
    #[test]
    fn closed_enums_go_on_the_wire_as_their_place() {
        fn each<T: Wire + PartialEq + std::fmt::Debug + Copy>(all: &[T]) {
            for (place, value) in all.iter().enumerate() {
                let bytes = encode(value);
                assert_eq!(bytes, [u8::try_from(place).unwrap_or(u8::MAX)], "{value:?}");
                assert_eq!(T::get(&mut Reader::new(&bytes)).ok(), Some(*value));
            }
        }
        each(&Direction::ALL);
        each(&MouseButton::ALL);
        each(&Signal::ALL);
        each(&fux_vt::ModeStatus::ALL);
        each(&crate::keys::colour::Scheme::ALL);
    }

    /// A button `send-mouse` presses is never the wheel, and the wheel
    /// turns only as the wheel: each reports as what it is.
    #[test]
    fn buttons_and_the_wheel_report_as_themselves() {
        use crate::command::{Button, Wheel};
        let buttons = [
            Button::Left,
            Button::Middle,
            Button::Right,
            Button::Back,
            Button::Forward,
        ];
        assert!(buttons.iter().all(|b| !b.reported().is_wheel()));
        let ways = [Wheel::Up, Wheel::Down, Wheel::Left, Wheel::Right];
        assert!(ways.iter().all(|w| w.reported().is_wheel()));
    }

    /// A variable's name from the wire is one a shell takes, as from the
    /// `env` word: `A=B` (or a NUL) would set another, or none.
    #[test]
    fn a_variable_name_no_shell_takes_is_refused() {
        let vars = |name: &str| encode(&vec![(name.to_owned(), "x".to_owned())]);
        assert!(Env::get(&mut Reader::new(&vars("A=B"))).is_err());
        assert!(Env::get(&mut Reader::new(&vars("A\0"))).is_err());
        let fine = Env::get(&mut Reader::new(&vars("A_1"))).ok();
        assert_eq!(fine.map(|env| encode(&env)), Some(vars("A_1")));
    }

    /// A capture's view goes as full, join, styles, source, marks, then
    /// its rows and columns, whatever order its fields are declared in.
    #[test]
    fn a_capture_view_keeps_its_bytes() {
        let view = CaptureView {
            full: true,
            join: false,
            rows: Some((1, 2)),
            cols: None,
            styles: true,
            source: Source::Main,
            marks: false,
        };
        assert_eq!(encode(&view), [1, 0, 1, 1, 0, 1, 0, 1, 0, 2, 0]);
    }

    /// A share of a tab is 1 to 100 percent, from the wire too: 250 would
    /// give a pane two and a half times the tab.
    #[test]
    fn a_percent_past_100_is_refused() {
        let read = |n: u8| Extent::get(&mut Reader::new(&[1, n])).ok();
        assert!(read(250).is_none() && read(0).is_none());
        assert_eq!(read(100).map(|e| encode(&e)), Some(vec![1, 100]));
    }
}
