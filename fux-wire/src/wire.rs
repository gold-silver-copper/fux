//! Requests on the wire: a typed [`Command`] written as bytes and read back
//! as the same value, so that the server receives what a client's words
//! built, and never reads words itself. Big-endian numbers; a string or a
//! list is its length then its items; an option or an enum its tag first.
use crate::command::{Limit, Percent};
use fux_vt::keys::mouse::MouseButton;
use fux_vt::keys::{Direction, KeyPress};
use fuxix::process::Signal;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

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
    /// A list: its count, then its items. Every item takes a byte at least,
    /// so a count past the bytes left is refused before anything is made.
    pub(crate) fn list<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let count = length(self)?;
        if count > self.0.len() {
            return Err(Error::Truncated);
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(read(self)?);
        }
        Ok(out)
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
        self.0.is_empty().then_some(value).ok_or(Error::Trailing)
    }
}

/// A value that goes on the wire.
pub trait Wire: Sized {
    fn put(&self, out: &mut impl crate::wire::Sink);
    fn get(r: &mut Reader<'_>) -> Result<Self, Error>;
}

/// fuxix's allocations, with no limit of fux's own: only the OS refuses
/// one, as `memory`.
pub struct Memory;

impl fuxix::Allocation for Memory {
    type Error = crate::code::Error;
    fn remaining(&self) -> usize {
        usize::MAX
    }
    fn charge(&mut self, _bytes: usize) -> Result<(), Self::Error> {
        Ok(())
    }
    fn release(&mut self, _bytes: usize) -> Result<(), Self::Error> {
        Ok(())
    }
    fn exceeded() -> Self::Error {
        crate::code::fail(crate::code::Raise::Memory, "the OS refused memory")
    }
}

/// Where wire bytes go.
pub trait Sink {
    fn extend_from_slice(&mut self, bytes: &[u8]);
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte]);
    }
}

impl Sink for Vec<u8> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
}

/// A value's bytes.
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
            fn put(&self, out: &mut impl crate::wire::Sink) {
                out.extend_from_slice(&self.to_be_bytes());
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                r.take().map(<$ty>::from_be_bytes)
            }
        }
    )*};
}
int_wire!(u8, u16, u32, u64, i32, i64);

impl Wire for std::time::Duration {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.as_secs().put(out);
        self.subsec_nanos().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (secs, nanos) = (u64::get(r)?, u32::get(r)?);
        if nanos >= 1_000_000_000 {
            return Err(Error::Bad("duration nanoseconds"));
        }
        Ok(Self::new(secs, nanos))
    }
}

// Its fields are std's own: as `put` writes them.
#[cfg(test)]
impl Reach for std::time::Duration {
    fn reach(out: &mut Shapes) {
        out.first::<Self>(
            "Duration",
            &[("Duration", &[("secs", "u64"), ("nanos", "u32")])],
        );
    }
}

impl Wire for std::num::NonZeroU32 {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.get().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Self::new(u32::get(r)?).ok_or(Error::Bad("share"))
    }
}

impl Wire for usize {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        u64::try_from(*self).unwrap_or(u64::MAX).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        usize::try_from(u64::get(r)?).map_err(|_| Error::Bad("count"))
    }
}

impl Wire for bool {
    fn put(&self, out: &mut impl crate::wire::Sink) {
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

/// A count or a length: four bytes; or, for one of `u32::MAX` or more,
/// `u32::MAX` and then eight, so any length a value may have goes whole,
/// and a shorter one as it always has (a `Hello`'s, which every version
/// reads).
fn put_length(length: usize, out: &mut impl Sink) {
    match u32::try_from(length) {
        Ok(short) if short < u32::MAX => short.put(out),
        _ => {
            u32::MAX.put(out);
            length.put(out);
        }
    }
}

/// A count or a length, as `put_length` writes it.
fn length(r: &mut Reader<'_>) -> Result<usize, Error> {
    match u32::get(r)? {
        u32::MAX => usize::get(r),
        short => usize::try_from(short).map_err(|_| Error::Bad("count")),
    }
}

/// Values, after their count: a list's, variables', a system string's
/// bytes.
fn put_list<T: Wire>(items: &[T], out: &mut impl crate::wire::Sink) {
    put_length(items.len(), out);
    for item in items {
        item.put(out);
    }
}

fn put_bytes(bytes: &[u8], out: &mut impl Sink) {
    put_length(bytes.len(), out);
    out.extend_from_slice(bytes);
}

impl Wire for String {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_bytes(self.as_bytes(), out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = length(r)?;
        let bytes = r.bytes(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| Error::NotUtf8)?;
        Ok(text.to_owned())
    }
}

/// A system's string, as its bytes: on Unix a path or a variable's value
/// is any bytes, never made text.
impl Wire for std::ffi::OsString {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_bytes(self.as_bytes(), out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Vec::get(r).map(Self::from_vec)
    }
}

/// A path, as its bytes.
impl Wire for std::path::PathBuf {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_bytes(self.as_os_str().as_bytes(), out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        std::ffi::OsString::get(r).map(Self::from)
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut impl crate::wire::Sink) {
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
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_list(self, out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        // Every item takes a byte at least, as no `Wire` impl is empty: the
        // macros' structs have a field and their enums a tag, and each
        // written by hand puts a number, a length or a tag. A count past
        // the bytes left is a lie, not room to make.
        r.list(T::get)
    }
}

impl<T: Wire> Wire for Box<T> {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        (**self).put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        T::get(r).map(Box::new)
    }
}

macro_rules! tuple_wire {
    ($(($($ty:ident: $field:tt),+)),+) => {$ (
        impl<$($ty: Wire),+> Wire for ($($ty,)+) {
            fn put(&self, out: &mut impl Sink) { $(self.$field.put(out);)+ }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                Ok(($($ty::get(r)?,)+))
            }
        }
    )+};
}
tuple_wire!((A: 0, B: 1), (A: 0, B: 1, C: 2));

/// `text_wire!(Ty, "what")` expands to `impl Wire for Ty`: its text
/// (`C-a`, `closed`), read back through `FromStr` (else `Error::Bad("what")`).
macro_rules! text_wire {
    ($ty:ty, $sigil:literal) => {
        impl Wire for $ty {
            fn put(&self, out: &mut impl crate::wire::Sink) {
                self.to_string().put(out);
            }
            /// Only the text `put` writes for the value: another
            /// spelling of it (`S-.` for `.`) would decode to a value
            /// that encodes to other bytes.
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                let text = String::get(r)?;
                let value: Self = text.parse().map_err(|_| Error::Bad($sigil))?;
                crate::words::writes_exactly(&text, |out| write!(out, "{value}"))
                    .then_some(value)
                    .ok_or(Error::Bad($sigil))
            }
        }
    };
}
text_wire!(KeyPress, "key");
text_wire!(crate::code::Code, "code");

/// Its seconds.
impl Wire for Limit {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.seconds().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        u32::get(r).map(Limit::from_secs)
    }
}

/// `closed_wire!(Ty, "what"; Ty2, "what2")` expands to `impl Wire for` each,
/// a fieldless enum whose `ALL` lists every variant once (as fux-vt's
/// `closed!` and fuxix's `signals!` make it): a variant's one-byte tag is
/// its place there. An `ALL` past 256 places does not compile.
macro_rules! closed_wire {
    ($($ty:ty, $what:literal);* $(;)?) => {$(
        const _: () = assert!(<$ty>::ALL.len() <= 256);
        impl $crate::wire::Wire for $ty {
            fn put(&self, out: &mut impl crate::wire::Sink) {
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

// Its fields are fux-vt's own: as `put` writes them.
#[cfg(test)]
impl Reach for fux_vt::Size {
    fn reach(out: &mut Shapes) {
        out.first::<Self>("Size", &[("Size", &[("rows", "u16"), ("cols", "u16")])]);
    }
}

impl Wire for fux_vt::Size {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.rows().put(out);
        self.cols().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let (rows, cols) = (u16::get(r)?, u16::get(r)?);
        fux_vt::Size::new(rows, cols).map_err(|_| Error::Bad("size"))
    }
}

impl Wire for crate::command::Screen {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.size().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let size = fux_vt::Size::get(r)?;
        crate::command::Screen::new(size.rows(), size.cols()).map_err(|_| Error::Bad("size"))
    }
}

/// `wire_enum!(Ty { Unit, Tuple(a, b), Named { x, y } })` expands to
/// `impl Wire for Ty`: a variant's tag, one byte that is its place in the
/// list (a hidden `#[repr(u8)] enum Tag`, so no two share one and a list
/// past 256 does not compile), then its fields in the order listed. `Ty<T>`
/// is `Wire` where `T` is. For a type declared in another crate; fux's own
/// are declared in `wire!`, which writes this list from the declaration.
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
                fn put(&self, out: &mut impl crate::wire::Sink) {
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

/// `wire_struct!(Ty { a, b })` expands to `impl Wire for Ty`: its fields,
/// one at least, in order, by name or tuple index (`Ty { 0 }`). For a type
/// declared elsewhere; one declared here says its fields once, in `wire!`.
macro_rules! wire_struct {
    ($ty:path { $($f:tt),+ $(,)? }) => {
        impl $crate::wire::Wire for $ty {
            fn put(&self, out: &mut impl crate::wire::Sink) {
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
/// expands to the enum as written, its tuple variants' fields bare
/// (`Tuple(A, B)`), and `wire_enum!(Ty { Unit, Tuple(a, b), Named { x } })`:
/// `a` and `b` only name the fields, as a macro can't make up names. A
/// struct expands to itself and `wire_struct!`. A variant's tag and a
/// field's place on the wire are their places in the declaration. In a
/// test build, each is also `Reach`: its variants (a struct's one, its
/// own name) and their fields, as declared, and what those hold.
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
        #[cfg(test)]
        impl$(<$g: $crate::wire::Reach>)? $crate::wire::Reach for $ty$(<$g>)? {
            fn reach(out: &mut $crate::wire::Shapes) {
                let shape: &[(&str, &[(&str, &str)])] = &[$((
                    stringify!($var),
                    &[
                        $($((stringify!($p), stringify!($pt)),)+)?
                        $($((stringify!($f), stringify!($ft)),)*)?
                    ],
                )),*];
                if out.first::<Self>(stringify!($ty), shape) {
                    $(
                        $($(<$pt as $crate::wire::Reach>::reach(out);)+)?
                        $($(<$ft as $crate::wire::Reach>::reach(out);)*)?
                    )*
                }
            }
        }
    };
    (
        $(#[$m:meta])*
        $vis:vis struct $ty:ident {
            $($(#[$fm:meta])* $fvis:vis $f:ident: $ft:ty),+ $(,)?
        }
    ) => {
        $(#[$m])*
        $vis struct $ty {
            $($(#[$fm])* $fvis $f: $ft),*
        }
        $crate::wire::wire_struct!($ty { $($f),+ });
        #[cfg(test)]
        impl $crate::wire::Reach for $ty {
            fn reach(out: &mut $crate::wire::Shapes) {
                let shape: &[(&str, &[(&str, &str)])] =
                    &[(stringify!($ty), &[$((stringify!($f), stringify!($ft))),+])];
                if out.first::<Self>(stringify!($ty), shape) {
                    $(<$ft as $crate::wire::Reach>::reach(out);)+
                }
            }
        }
    };
}
pub(crate) use wire;

/// `shaped!(struct Ty { a: A, b: B })`, `shaped!(struct Ty(a: A))` or
/// `shaped!(enum Ty { Unit, Tuple(a: A), Named { b: B } })`, its variants in
/// the order of their tags; `shaped!(codec struct Ty { … })` also gives
/// `put_fields` and `get_fields`, so that a `Wire` that only checks what it
/// reads puts and gets its fields in the order listed, never another:
/// in a test build, `Reach` for a type whose `Wire` is written by hand, its
/// fields as they go on the wire, in order. A field or variant the type
/// has and the list does not fails to compile; their types are as listed.
macro_rules! shaped {
    (codec struct $ty:path { $($f:ident: $ft:ty),+ $(,)? }) => {
        $crate::wire::shaped!(struct $ty { $($f: $ft),+ });
        impl $ty {
            /// Its fields on the wire, in the order its shape lists them.
            fn put_fields(&self, out: &mut impl $crate::wire::Sink) {
                $($crate::wire::Wire::put(&self.$f, out);)+
            }
            /// Its fields read back in that order, for its `get` to check.
            fn get_fields(
                r: &mut $crate::wire::Reader<'_>,
            ) -> Result<($($ft,)+), $crate::wire::Error> {
                Ok(($(<$ft as $crate::wire::Wire>::get(r)?,)+))
            }
        }
    };
    (struct $ty:ident ( $($f:ident: $ft:ty),+ $(,)? )) => {
        #[cfg(test)]
        impl $crate::wire::Reach for $ty {
            fn reach(out: &mut $crate::wire::Shapes) {
                let name = stringify!($ty).rsplit("::").next().unwrap_or_default();
                let shape: &[(&str, &[(&str, &str)])] =
                    &[(name, &[$((stringify!($f), stringify!($ft))),+])];
                if out.first::<Self>(name, shape) {
                    $(<$ft as $crate::wire::Reach>::reach(out);)+
                }
                let _every_field = |v: &Self| {
                    let Self($($f),+) = v;
                    $(let _ = $f;)+
                };
            }
        }
    };
    (struct $ty:path { $($f:ident: $ft:ty),+ $(,)? }) => {
        #[cfg(test)]
        impl $crate::wire::Reach for $ty {
            fn reach(out: &mut $crate::wire::Shapes) {
                let name = stringify!($ty).rsplit("::").next().unwrap_or_default();
                let shape: &[(&str, &[(&str, &str)])] =
                    &[(name, &[$((stringify!($f), stringify!($ft))),+])];
                if out.first::<Self>(name, shape) {
                    $(<$ft as $crate::wire::Reach>::reach(out);)+
                }
                let _every_field = |v: &Self| {
                    let Self { $($f: _),+ } = v;
                };
            }
        }
    };
    (enum $ty:path {
        $($var:ident $(( $($p:ident: $pt:ty),+ ))? $({ $($f:ident: $ft:ty),+ })?),+ $(,)?
    }) => {
        #[cfg(test)]
        impl $crate::wire::Reach for $ty {
            fn reach(out: &mut $crate::wire::Shapes) {
                let name = stringify!($ty).rsplit("::").next().unwrap_or_default();
                let shape: &[(&str, &[(&str, &str)])] = &[$((
                    stringify!($var),
                    &[
                        $($((stringify!($p), stringify!($pt)),)+)?
                        $($((stringify!($f), stringify!($ft)),)+)?
                    ],
                )),+];
                if out.first::<Self>(name, shape) {
                    $(
                        $($(<$pt as $crate::wire::Reach>::reach(out);)+)?
                        $($(<$ft as $crate::wire::Reach>::reach(out);)+)?
                    )+
                }
                let _every_variant = |v: &Self| match v {
                    $(Self::$var $(( $($p),+ ))? $({ $($f),+ })? => {
                        $($(let _ = $p;)+)?
                        $($(let _ = $f;)+)?
                    })+
                };
            }
        }
    };
}
pub(crate) use shaped;

/// What a request reaches on the wire: a type with fields lists its
/// variants (a struct's one, its own name) and their fields' names and
/// types, in their places on the wire, then each type they hold, each
/// once (`wire!`, `shaped!`); a value with none (a number, text, an id)
/// lists nothing. Every field's type is one, so none is left out.
#[cfg(test)]
pub(crate) trait Reach {
    fn reach(_: &mut Shapes) {}
}

/// The shapes reached so far: each type's lines, `Ty::Variant fields` (a
/// struct, `Ty fields`), in the order first reached.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Shapes {
    seen: Vec<&'static str>,
    pub lines: Vec<String>,
}

#[cfg(test)]
impl Shapes {
    /// Lists `T`, named `name`, by `shape`, unless it was reached before:
    /// whether it is new. A generic type's other instances share its lines.
    pub(crate) fn first<T: ?Sized>(
        &mut self,
        name: &str,
        shape: &[(&str, &[(&str, &str)])],
    ) -> bool {
        let key = std::any::type_name::<T>();
        if self.seen.contains(&key) {
            return false;
        }
        self.seen.push(key);
        for (variant, fields) in shape {
            let fields: Vec<String> = fields
                .iter()
                .map(|(field, ty)| format!("{field}: {}", ty.replace("crate::", "")))
                .collect();
            let head = match *variant == name {
                true => name.to_owned(),
                false => format!("{name}::{variant}"),
            };
            let line = format!("{head} {}", fields.join(", "))
                .trim_end()
                .to_owned();
            if !self.lines.contains(&line) {
                self.lines.push(line);
            }
        }
        true
    }
}

/// `Reach` for what goes on the wire as one value with no fields of its
/// own; every type with fields has its own `Reach` (`wire!`, `shaped!`).
#[cfg(test)]
macro_rules! leaves {
    ($($ty:ty),* $(,)?) => {$(impl Reach for $ty {})*};
}
#[cfg(test)]
leaves!(
    // A number.
    u8,
    u16,
    u32,
    u64,
    i32,
    i64,
    usize,
    std::num::NonZeroU32,
    crate::command::Limit,
    crate::command::Percent,
    crate::command::ScrollLines,
    // A flag.
    bool,
    // A string, or a system string's bytes.
    String,
    std::path::PathBuf,
    std::ffi::OsString,
    // An id: one number.
    crate::id::PaneId,
    crate::id::TabId,
    crate::id::WsId,
    crate::id::ClientId,
    crate::events::Seq,
    // Text read back by its spelling (`text_wire!`).
    KeyPress,
    crate::code::Code,
    // One byte of a closed set (`closed_wire!`).
    Direction,
    MouseButton,
    Signal,
    crate::answer::Change,
    fux_vt::ModeStatus,
    fux_vt::keys::colour::Scheme,
);
#[cfg(test)]
impl<const MIN: usize, const MAX: usize> Reach for crate::command::Number<MIN, MAX> {}

/// What holds values reaches what they are.
#[cfg(test)]
impl<T: Reach> Reach for Option<T> {
    fn reach(out: &mut Shapes) {
        T::reach(out);
    }
}
#[cfg(test)]
impl<T: Reach> Reach for Vec<T> {
    fn reach(out: &mut Shapes) {
        T::reach(out);
    }
}
#[cfg(test)]
impl<T: Reach> Reach for Box<T> {
    fn reach(out: &mut Shapes) {
        T::reach(out);
    }
}
#[cfg(test)]
impl<A: Reach, B: Reach> Reach for (A, B) {
    fn reach(out: &mut Shapes) {
        A::reach(out);
        B::reach(out);
    }
}
#[cfg(test)]
impl<A: Reach, B: Reach, C: Reach> Reach for (A, B, C) {
    fn reach(out: &mut Shapes) {
        A::reach(out);
        B::reach(out);
        C::reach(out);
    }
}

impl Wire for Percent {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.get().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Percent::new(u8::get(r)?).ok_or(Error::Bad("percent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, Extent, Program};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
        each(&fux_vt::keys::colour::Scheme::ALL);
    }

    #[test]
    fn a_size_past_a_screens_is_refused_as_it_is_read() -> TestResult {
        use crate::command::Screen;
        let big = Screen::new(4096, 4096)?;
        assert_eq!(Screen::get(&mut Reader::new(&encode(&big))).ok(), Some(big));
        let past = fux_vt::Size::new(u16::MAX, u16::MAX)?;
        assert!(!fux_vt::fits(past));
        assert!(Screen::get(&mut Reader::new(&encode(&past))).is_err());
        Ok(())
    }

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

    #[test]
    fn a_value_written_as_text_reads_back_only_from_that_text() {
        let text = |t: &str| encode(&t.to_owned());
        assert!(KeyPress::get(&mut Reader::new(&text("S-."))).is_err());
        let dot = KeyPress::get(&mut Reader::new(&text("."))).ok();
        assert_eq!(dot.map(|k| encode(&k)), Some(text(".")));
    }

    /// A length goes whole whatever its size: as four bytes below
    /// `u32::MAX`, as it always has, and as twelve from there, never cut
    /// short; one claiming more than the bytes left is refused.
    #[test]
    fn a_length_of_any_size_goes_whole() -> TestResult {
        let most = usize::try_from(u32::MAX)?;
        for (n, bytes) in [
            (0, 4),
            (most - 1, 4),
            (most, 12),
            (most + 1, 12),
            (usize::MAX, 12),
        ] {
            let mut out = Vec::new();
            put_length(n, &mut out);
            assert_eq!(out.len(), bytes, "{n}");
            assert_eq!(length(&mut Reader::new(&out)), Ok(n));
        }
        let mut claim = Vec::new();
        put_length(most + 1, &mut claim);
        claim.extend_from_slice(b"short");
        assert_eq!(Reader::new(&claim).whole::<String>(), Err(Error::Truncated));
        Ok(())
    }

    #[test]
    fn numeric_tokens_round_trip() -> TestResult {
        fn token<T: Wire + std::str::FromStr + std::fmt::Debug + PartialEq>(
            text: &str,
            size: usize,
        ) -> TestResult {
            let value: T = text.parse().map_err(|_| "token")?;
            let bytes = encode(&value);
            assert_eq!(bytes.len(), size);
            assert_eq!(Reader::new(&bytes).whole::<T>()?, value);
            Ok(())
        }
        token::<crate::id::PaneId>("%4294967295", 4)?;
        token::<crate::id::TabId>("@4294967295", 4)?;
        token::<crate::id::WsId>("+4294967295", 4)?;
        token::<crate::id::ClientId>("c4294967295", 4)?;
        token::<crate::command::Run>("4294967295-ffffffffffffffff", 12)?;
        token::<crate::command::Version>(
            "%4294967295:18446744073709551615.18446744073709551615.18446744073709551615.18446744073709551615.1:4294967295-ffffffffffffffff",
            49,
        )?;
        token::<crate::command::LayoutRevision>(
            "@4294967295:18446744073709551615:4294967295-ffffffffffffffff",
            24,
        )?;
        token::<crate::events::Cursor>("18446744073709551615:4294967295-ffffffffffffffff", 20)
    }

    #[test]
    fn numeric_tokens_keep_their_value_checks() -> TestResult {
        let mut counts = encode(&crate::command::Counts::default());
        *counts.last_mut().ok_or("ended byte")? = 2;
        assert_eq!(
            Reader::new(&counts).whole::<crate::command::Counts>(),
            Err(Error::Tag("flag", 2))
        );
        Ok(())
    }

    #[test]
    fn argv_is_nonempty_and_checked_at_decode() -> TestResult {
        for (program, args) in [("", vec![]), ("/bin/sh", vec!["bad\0arg".to_owned()])] {
            assert!(Program::new(program.into(), args.clone()).is_err());
            let mut bytes = encode(&program.to_owned());
            args.put(&mut bytes);
            assert!(Reader::new(&bytes).whole::<Program>().is_err());
        }
        let program = Program::new("a program".into(), vec!["".into(), "an argument".into()])?;
        assert_eq!(
            Reader::new(&encode(&program)).whole::<Program>(),
            Ok(program)
        );
        Ok(())
    }

    #[test]
    fn a_directory_goes_as_its_bytes() -> TestResult {
        let dir = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9"));
        let split = Command::Split {
            options: crate::command::PaneOptions {
                history_bytes: crate::command::HistoryBytes::new(8 << 20).ok_or("history")?,
                program_clipboard: true,
                reflow: true,
                alternate_scroll_lines: crate::command::ScrollLines::new(5).ok_or("scroll")?,
                picture_bytes: crate::command::PictureBytes::new(96 << 20).ok_or("pictures")?,
            },
            axis: crate::layout::Axis::Vertical,
            target: crate::id::PaneId::of(1),
            start: crate::command::Start::Shell(Program::new("/bin/sh".into(), Vec::new())?),
            cwd: Some(dir.clone()),
            shares: None,
        };
        let read = Command::get(&mut Reader::new(&encode(&split))).ok();
        assert!(matches!(read, Some(Command::Split {
            cwd: Some(cwd), .. }) if cwd == dir));
        Ok(())
    }

    #[test]
    fn creation_policy_requires_a_checked_budget_and_permission() -> TestResult {
        use crate::command::{HistoryBytes, PaneOptions};
        let policy = PaneOptions {
            history_bytes: HistoryBytes::new(0).ok_or("budget")?,
            program_clipboard: false,
            reflow: true,
            alternate_scroll_lines: crate::command::ScrollLines::new(5).ok_or("scroll lines")?,
            picture_bytes: crate::command::PictureBytes::new(100663296).ok_or("pictures")?,
        };
        let mut invalid_scroll = Vec::new();
        32768u16.put(&mut invalid_scroll);
        assert_eq!(
            Reader::new(&invalid_scroll).whole::<crate::command::ScrollLines>(),
            Err(Error::Bad("scroll lines"))
        );
        let encoded = encode(&policy);
        assert_eq!(Reader::new(&encoded).whole::<PaneOptions>(), Ok(policy));
        let mut invalid = Vec::new();
        usize::MAX.put(&mut invalid);
        false.put(&mut invalid);
        assert_eq!(
            Reader::new(&invalid).whole::<PaneOptions>(),
            Err(Error::Bad("number"))
        );
        assert!(
            Reader::new(
                encoded
                    .get(..encoded.len().saturating_sub(1))
                    .ok_or("bytes")?
            )
            .whole::<PaneOptions>()
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn a_percent_past_100_is_refused() {
        let read = |n: u8| Extent::get(&mut Reader::new(&[1, n])).ok();
        assert!(read(250).is_none() && read(0).is_none());
        assert_eq!(read(100).map(|e| encode(&e)), Some(vec![1, 100]));
    }
}
