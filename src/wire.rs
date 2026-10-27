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

/// Why bytes are not the value they should be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A finite value exceeds its byte or allocation budget.
    Limit,
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
            Error::Limit => f.write_str("finite reply exceeds its byte or allocation limit"),
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
pub struct Reader<'a>(&'a [u8], usize);

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader(bytes, MAX_ALLOCATED)
    }
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.1 = self.1.checked_sub(bytes).ok_or(Error::Limit)?;
        Ok(())
    }
    pub(crate) fn list<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let count = u32::get(self)? as usize;
        if count > self.0.len() {
            return Err(Error::Truncated);
        }
        self.charge(
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(Error::Limit)?,
        )?;
        let mut out = Vec::new();
        out.try_reserve_exact(count).map_err(|_| Error::Limit)?;
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
        if self.0.len() > MAX_REPLY {
            return Err(Error::Limit);
        }
        let value = T::get(&mut self)?;
        self.0.is_empty().then_some(value).ok_or(Error::Trailing)
    }
}

/// A value that goes on the wire.
pub trait Wire: Sized {
    fn put(&self, out: &mut impl crate::wire::Sink);
    fn get(r: &mut Reader<'_>) -> Result<Self, Error>;
}

/// The default-history styled capture measured 21.9 MB. The next binary
/// power admits that observation, while bounding unfinished Part streams.
pub const MAX_REPLY: usize = 32 << 20;
/// Shared construction and decoding grant. The default styled capture
/// peaked at 69,599,283 construction bytes and charged 48,902,484 decoded
/// bytes. The next binary power above the larger measurement lets the
/// receiver accept the same owned-data grant the builder admits.
pub const MAX_ALLOCATED: usize = 128 << 20;

/// Retained construction space, charged before cloning data or growing lists.
pub(crate) struct Budget(usize);
impl Budget {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_ALLOCATED)
    }
    pub(crate) fn with_limit(bytes: usize) -> Self {
        Self(bytes)
    }
    pub(crate) fn remaining(&self) -> usize {
        self.0
    }
    pub(crate) fn string(&mut self, text: &str) -> Result<String, crate::session::Error> {
        self.charge(text.len())?;
        let mut out = String::new();
        out.try_reserve_exact(text.len())
            .map_err(|_| Self::exceeded())?;
        out.push_str(text);
        Ok(out)
    }
    pub(crate) fn write(
        &mut self,
        out: &mut String,
        args: std::fmt::Arguments<'_>,
    ) -> Result<(), crate::session::Error> {
        struct Formatted<'a> {
            budget: &'a mut Budget,
            out: &'a mut String,
        }
        impl std::fmt::Write for Formatted<'_> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.budget
                    .text(self.out, text)
                    .map_err(|_| std::fmt::Error)
            }
        }
        std::fmt::write(&mut Formatted { budget: self, out }, args).map_err(|_| Self::exceeded())
    }
    pub(crate) fn format(
        &mut self,
        args: std::fmt::Arguments<'_>,
    ) -> Result<String, crate::session::Error> {
        let mut out = String::new();
        self.write(&mut out, args)?;
        Ok(out)
    }
    pub(crate) fn owned_string(
        &mut self,
        text: String,
    ) -> Result<crate::json::Json, crate::session::Error> {
        self.charge(text.capacity())?;
        Ok(crate::json::Json::str(text))
    }
    pub(crate) fn optional_owned(
        &mut self,
        text: Option<String>,
    ) -> Result<crate::json::Json, crate::session::Error> {
        match text {
            Some(text) => self.owned_string(text),
            None => Ok(crate::json::Json::Null),
        }
    }
    pub(crate) fn optional_text(
        &mut self,
        text: Option<&str>,
    ) -> Result<crate::json::Json, crate::session::Error> {
        match text {
            Some(text) => self.string(text).map(crate::json::Json::str),
            None => Ok(crate::json::Json::Null),
        }
    }
    fn named(
        &mut self,
        fields: impl ExactSizeIterator<Item = (&'static str, crate::json::Json)>,
    ) -> Result<Vec<(String, crate::json::Json)>, crate::session::Error> {
        let mut out = self.vector(fields.len())?;
        for (key, value) in fields {
            out.push((self.string(key)?, value));
        }
        Ok(out)
    }
    pub(crate) fn object<const N: usize>(
        &mut self,
        fields: [(&'static str, crate::json::Json); N],
    ) -> Result<crate::json::Json, crate::session::Error> {
        self.named(fields.into_iter())
            .map(crate::json::Json::Object)
    }
    pub(crate) fn fields(
        &mut self,
        fields: crate::json::Fields,
    ) -> Result<Vec<(String, crate::json::Json)>, crate::session::Error> {
        let bytes = fields
            .capacity()
            .checked_mul(std::mem::size_of::<(&str, crate::json::Json)>())
            .ok_or_else(Self::exceeded)?;
        let out = self.named(fields.into_iter())?;
        self.release(bytes)?;
        Ok(out)
    }
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), crate::session::Error> {
        self.0 = self.0.checked_sub(bytes).ok_or_else(Self::exceeded)?;
        Ok(())
    }
    pub(crate) fn slots<T>(&mut self, count: usize) -> Result<(), crate::session::Error> {
        self.charge(
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or_else(Self::exceeded)?,
        )
    }
    pub(crate) fn vector<T>(&mut self, count: usize) -> Result<Vec<T>, crate::session::Error> {
        self.slots::<T>(count)?;
        let mut out = Vec::new();
        out.try_reserve_exact(count).map_err(|_| Self::exceeded())?;
        Ok(out)
    }
    pub(crate) fn release(&mut self, bytes: usize) -> Result<(), crate::session::Error> {
        self.0 = self.0.checked_add(bytes).ok_or_else(Self::exceeded)?;
        Ok(())
    }
    /// Charge the whole replacement block while the old one is live.
    pub(crate) fn reserve<T>(
        &mut self,
        out: &mut Vec<T>,
        more: usize,
    ) -> Result<(), crate::session::Error> {
        let needed = out.len().checked_add(more).ok_or_else(Self::exceeded)?;
        if needed <= out.capacity() {
            return Ok(());
        }
        let capacity = needed
            .checked_next_power_of_two()
            .ok_or_else(Self::exceeded)?
            .max(4);
        self.slots::<T>(capacity)?;
        let old = out
            .capacity()
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(Self::exceeded)?;
        out.try_reserve_exact(capacity.saturating_sub(out.len()))
            .map_err(|_| Self::exceeded())?;
        self.release(old)
    }
    pub(crate) fn text(
        &mut self,
        out: &mut String,
        text: &str,
    ) -> Result<(), crate::session::Error> {
        let needed = out
            .len()
            .checked_add(text.len())
            .ok_or_else(Self::exceeded)?;
        if needed > out.capacity() {
            let capacity = needed
                .checked_next_power_of_two()
                .ok_or_else(Self::exceeded)?
                .max(8);
            self.charge(capacity)?;
            let old = out.capacity();
            out.try_reserve_exact(capacity.saturating_sub(out.len()))
                .map_err(|_| Self::exceeded())?;
            self.release(old)?;
        }
        out.push_str(text);
        Ok(())
    }
    pub(crate) fn exceeded() -> crate::session::Error {
        crate::session::fail(
            crate::code::Raise::TooLong,
            "finite reply exceeds its byte or allocation limit; request a smaller scope",
        )
    }
}

/// Where wire bytes go. Bounded encoders stop before reserving memory.
pub trait Sink {
    fn extend_from_slice(&mut self, bytes: &[u8]);
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte]);
    }
    fn full(&self) -> bool {
        false
    }
}

impl Sink for Vec<u8> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
}

/// A fallible wire writer, or a count-only preflight of the same encoding.
pub struct Writer {
    bytes: Option<Vec<u8>>,
    left: usize,
    failed: bool,
}

impl Writer {
    pub fn counting(limit: usize) -> Self {
        Self {
            bytes: None,
            left: limit,
            failed: false,
        }
    }
    pub(crate) fn check_ref(&self) -> Result<(), Error> {
        if self.failed {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
    pub fn check(self) -> Result<(), Error> {
        if self.failed {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
}

impl Sink for Writer {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if self.failed {
            return;
        }
        let Some(left) = self.left.checked_sub(bytes.len()) else {
            self.failed = true;
            return;
        };
        if let Some(out) = &mut self.bytes
            && append(out, bytes).is_err()
        {
            self.failed = true;
            return;
        }
        self.left = left;
    }
    fn full(&self) -> bool {
        self.failed
    }
}

/// Admit a reply fragment before growing its accumulator. Reservations
/// stop at the same cap as its length, including at the edge.
pub(crate) fn append(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    let needed = out.len().checked_add(bytes.len()).ok_or(Error::Limit)?;
    if needed > MAX_REPLY {
        return Err(Error::Limit);
    }
    if needed > out.capacity() {
        let capacity = out.capacity().saturating_mul(2).max(needed).min(MAX_REPLY);
        out.try_reserve_exact(capacity.saturating_sub(out.len()))
            .map_err(|_| Error::Limit)?;
    }
    out.extend_from_slice(bytes);
    Ok(())
}

/// Size of the same bounded encoding, without allocating its bytes.
pub(crate) fn size<T: Wire>(value: &T) -> Result<usize, Error> {
    let mut out = Writer::counting(MAX_REPLY);
    value.put(&mut out);
    out.check_ref()?;
    Ok(MAX_REPLY.saturating_sub(out.left))
}

/// A finite reply's bytes, refused before a write exceeds the cap.
pub fn encode<T: Wire>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Writer {
        bytes: Some(Vec::new()),
        left: MAX_REPLY,
        failed: false,
    };
    value.put(&mut out);
    if out.failed {
        return Err(Error::Limit);
    }
    Ok(out.bytes.unwrap_or_default())
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

/// Values, after their count: a list's, variables', a system string's
/// bytes.
fn put_list<T: Wire>(items: &[T], out: &mut impl crate::wire::Sink) {
    u32::try_from(items.len()).unwrap_or(u32::MAX).put(out);
    for item in items {
        if out.full() {
            break;
        }
        item.put(out);
    }
}

fn put_bytes(bytes: &[u8], out: &mut impl Sink) {
    u32::try_from(bytes.len()).unwrap_or(u32::MAX).put(out);
    out.extend_from_slice(bytes);
}

impl Wire for String {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_bytes(self.as_bytes(), out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let len = u32::get(r)? as usize;
        let bytes = r.bytes(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| Error::NotUtf8)?;
        r.charge(len)?;
        let mut out = String::new();
        out.try_reserve_exact(len).map_err(|_| Error::Limit)?;
        out.push_str(text);
        Ok(out)
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

/// Variables, each name checked as it is read: one no shell takes is
/// refused.
impl Wire for Env {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        put_list(self.as_slice(), out);
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
        r.charge(std::mem::size_of::<T>())?;
        T::get(r).map(Box::new)
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

impl<A: Wire, B: Wire, C: Wire> Wire for (A, B, C) {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.0.put(out);
        self.1.put(out);
        self.2.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok((A::get(r)?, B::get(r)?, C::get(r)?))
    }
}

/// `text_wire!(Ty, "what")` expands to `impl Wire for Ty`: its text
/// (`%3`, `C-a`), read back through `FromStr` (else `Error::Bad("what")`).
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
                if value.to_string() == text {
                    Ok(value)
                } else {
                    Err(Error::Bad($sigil))
                }
            }
        }
    };
}
text_wire!(PaneId, "pane id");
text_wire!(TabId, "tab id");
text_wire!(WsId, "workspace id");
text_wire!(ClientId, "client id");
text_wire!(crate::id::BufferId, "buffer id");
text_wire!(KeyPress, "key");
text_wire!(crate::code::Code, "code");
text_wire!(crate::command::Version, "version");
text_wire!(crate::command::Run, "server run");
text_wire!(crate::events::Cursor, "cursor");

/// Its seconds, read back only if a wait may wait that long.
impl Wire for crate::config::Limit {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.seconds().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let secs = u32::get(r)?;
        crate::config::Limit::secs(u64::from(secs)).ok_or(Error::Bad("wait limit"))
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

impl Wire for crate::view::Screen {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        self.size().put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let size = fux_vt::Size::get(r)?;
        crate::view::Screen::new(size.rows(), size.cols()).map_err(|_| Error::Bad("size"))
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
/// one at least, one after another, in the order listed. For a type
/// declared elsewhere; one declared here says its fields once, in `wire!`.
macro_rules! wire_struct {
    ($ty:path { $($f:ident),+ $(,)? }) => {
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
/// field's place on the wire are their places in the declaration.
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
            $($(#[$fm:meta])* $fvis:vis $f:ident: $ft:ty),+ $(,)?
        }
    ) => {
        $(#[$m])*
        $vis struct $ty {
            $($(#[$fm])* $fvis $f: $ft),*
        }
        $crate::wire::wire_struct!($ty { $($f),+ });
    };
}
pub(crate) use wire;

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
    use crate::command::{Command, Extent};

    /// An ordinary large styled reply fits; crossing the wire cap fails
    /// before the accumulator's length or capacity crosses it.
    #[test]
    fn finite_bytes_are_admitted_before_allocation() -> Result<(), Box<dyn std::error::Error>> {
        let mut bytes = vec![0; 22 << 20];
        let mut writer = Writer::counting(MAX_REPLY);
        writer.extend_from_slice(&bytes);
        writer.check()?;
        bytes.resize(MAX_REPLY, 0);
        let capacity = bytes.capacity();
        assert_eq!(append(&mut bytes, &[1]), Err(Error::Limit));
        assert_eq!((bytes.len(), bytes.capacity()), (MAX_REPLY, capacity));
        // String length alone pushes this value past the cap.
        assert_eq!(
            encode(&std::iter::repeat_n('x', MAX_REPLY).collect::<String>()),
            Err(Error::Limit)
        );
        Ok(())
    }

    /// Small encodings can claim far more decoded space. Charge nested
    /// list slots and string data against one budget before reserving.
    #[test]
    fn decoded_allocations_share_one_budget() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = encode(&vec![vec!["abc".to_owned()]; 2])?;
        let slots = 2usize
            .saturating_mul(std::mem::size_of::<Vec<String>>())
            .saturating_add(2usize.saturating_mul(std::mem::size_of::<String>()));
        let mut reader = Reader::new(&bytes);
        reader.1 = slots.saturating_add(5);
        assert_eq!(reader.whole::<Vec<Vec<String>>>(), Err(Error::Limit));
        let bytes = [5, 0, 0, 0, 4, 0, 0, 0, 0]; // four JSON nulls
        let mut reader = Reader::new(&bytes);
        reader.1 = std::mem::size_of::<crate::json::Json>();
        assert_eq!(reader.whole::<crate::json::Json>(), Err(Error::Limit));
        Ok(())
    }

    /// The audited default-history shape needs more than the proposed
    /// 32 MiB of decoded storage, but fits the shared allocation grant.
    #[test]
    fn default_styled_capture_fits_decoded_budget() -> Result<(), Box<dyn std::error::Error>> {
        use crate::capture::{Captured, Cut, Of};
        use crate::protocol::{Body, Reply};
        let line: Vec<_> = (0u8..100)
            .map(|color| Cut {
                text: "x".to_owned(),
                cells: 1,
                style: fux_vt::Attributes::new(fux_vt::Color::Idx(color), fux_vt::Color::Default),
            })
            .collect();
        let captured = Captured {
            of: Of::Client(ClientId::of(1)),
            screen_rows: 40,
            screen_cols: 120,
            screen_cursor: None,
            lines: std::iter::repeat_n(line, 9931).collect(),
            styles: true,
            marks: false,
        };
        let bytes = encode(&Reply::from(Body::Capture(Box::new(captured))))?;
        let mut reader = Reader::new(&bytes);
        let reply = Reply::get(&mut reader)?;
        let charged = MAX_ALLOCATED.saturating_sub(reader.1);
        assert!(charged > 32 << 20 && charged <= MAX_ALLOCATED, "{charged}");
        assert!(matches!(
            reply.response,
            crate::protocol::Response::Returned(Body::Capture(_))
        ));
        Ok(())
    }

    /// Each variant of an enum `closed_wire!` writes reads back as itself,
    /// and its tag is its place in `ALL`.
    #[test]
    fn closed_enums_go_on_the_wire_as_their_place() {
        fn each<T: Wire + PartialEq + std::fmt::Debug + Copy>(all: &[T]) {
            for (place, value) in all.iter().enumerate() {
                let bytes = encode(value).unwrap_or_default();
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

    /// A size a request gives is a screen's, read as it was written; one
    /// past `view::MOST`, which a dump could not write, is not
    /// read at all. Every screen fits a pane.
    #[test]
    fn a_size_past_a_screens_is_refused_as_it_is_read() -> Result<(), String> {
        use crate::view::{MOST, Screen};
        let most = Screen::new(MOST, MOST)?;
        assert_eq!(
            Screen::get(&mut Reader::new(&encode(&most).unwrap_or_default())).ok(),
            Some(most)
        );
        assert!(fux_vt::fits(most.size()));
        let past = fux_vt::Size::new(10, MOST.saturating_add(1)).map_err(|e| e.to_string())?;
        assert!(Screen::get(&mut Reader::new(&encode(&past).unwrap_or_default())).is_err());
        Ok(())
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

    /// A value written as text reads back only from the text it writes:
    /// `S-.` names the key `.` too, but decoded it would encode as `.`,
    /// so a frame would not encode to the bytes it came from (the
    /// `protocol` fuzz target's
    /// `fuzz/corpus/protocol/regression-a-key-spelled-otherwise`).
    #[test]
    fn a_value_written_as_text_reads_back_only_from_that_text() {
        let text = |t: &str| encode(&t.to_owned()).unwrap_or_default();
        assert!(KeyPress::get(&mut Reader::new(&text("S-."))).is_err());
        let dot = KeyPress::get(&mut Reader::new(&text("."))).ok();
        assert_eq!(dot.map(|k| encode(&k).unwrap_or_default()), Some(text(".")));
    }

    /// A variable's name from the wire is one a shell takes, as from the
    /// `env` word: `A=B` (or a NUL) would set another, or none.
    #[test]
    fn a_variable_name_no_shell_takes_is_refused() {
        let vars =
            |name: &str| encode(&vec![(name.to_owned(), "x".to_owned())]).unwrap_or_default();
        assert!(Env::get(&mut Reader::new(&vars("A=B"))).is_err());
        assert!(Env::get(&mut Reader::new(&vars("A\0"))).is_err());
        let fine = Env::get(&mut Reader::new(&vars("A_1"))).ok();
        assert_eq!(
            fine.map(|env| encode(&env).unwrap_or_default()),
            Some(vars("A_1"))
        );
    }

    /// A command's directory is the path's bytes, UTF-8 or not: one that
    /// is not still reaches the session as it was named.
    #[test]
    fn a_directory_goes_as_its_bytes() {
        let dir = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9"));
        let split = Command::Split {
            axis: crate::layout::Axis::Vertical,
            target: crate::id::PaneId::of(1),
            argv: Vec::new(),
            cwd: Some(dir.clone()),
            env: Env::default(),
            shares: None,
        };
        let read = Command::get(&mut Reader::new(&encode(&split).unwrap_or_default())).ok();
        assert!(matches!(read, Some(Command::Split { cwd: Some(cwd), .. }) if cwd == dir));
    }

    /// A share of a tab is 1 to 100 percent, from the wire too: 250 would
    /// give a pane two and a half times the tab.
    #[test]
    fn a_percent_past_100_is_refused() {
        let read = |n: u8| Extent::get(&mut Reader::new(&[1, n])).ok();
        assert!(read(250).is_none() && read(0).is_none());
        assert_eq!(
            read(100).map(|e| encode(&e).unwrap_or_default()),
            Some(vec![1, 100])
        );
    }
}
