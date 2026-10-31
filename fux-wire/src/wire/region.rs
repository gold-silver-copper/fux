//! What decoding, building and encoding make, each allocation charged to a
//! region before it is made: on the server, a request's
//! (`fux_server::request`); a frame decoded on its own has one of its own.
//! The wire's values are std's vectors and strings, as both ends share
//! them: they are made only here, and in `fuxix::Allocation`'s methods, each
//! after its charge. Each `std::vec::Vec` and `std::string::String` written
//! below is such a value, made after its bytes are charged.
fuxth_core::bounded_only!();

use super::{Error, MAX_ALLOCATED, MAX_REPLY, Sink, Wire};
use fuxth_core::{BoundedVec, Region, Reservation};

/// What a request builds, or a client's decoder or record: each allocation
/// charged to its region before it is made, and held there until the
/// budget drops, which is when what it built is gone (a request's reply
/// sent). A limit within it bounds one value's share (`limited`).
pub struct Budget {
    held: Reservation,
    /// What the value being made may still take: no limit but the region's
    /// outside `limited`.
    limit: usize,
}

impl Budget {
    /// Charged to `region`, holding nothing yet.
    pub fn of(region: &Region) -> Self {
        Self {
            held: region.nothing(),
            limit: usize::MAX,
        }
    }

    /// Charged to a region of its own, of `bytes`.
    pub fn with_limit(bytes: usize) -> Self {
        Self::of(&Region::new(bytes))
    }

    pub fn region(&self) -> &Region {
        self.held.region()
    }

    /// `make`, what it charges bounded also by `bytes`, and counted against
    /// any limit around it.
    pub fn limited<T>(&mut self, bytes: usize, make: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.narrow(bytes);
        let made = make(self);
        self.widen(outer);
        made
    }

    /// Narrows the limit to `bytes`: the limit it had and the one it has.
    fn narrow(&mut self, bytes: usize) -> (usize, usize) {
        let outer = self.limit;
        self.limit = outer.min(bytes);
        (outer, self.limit)
    }

    /// The limit `narrow` had, less what was charged since.
    fn widen(&mut self, (outer, inner): (usize, usize)) {
        self.limit = outer.saturating_sub(inner.saturating_sub(self.limit));
    }

    pub fn format(
        &mut self,
        args: std::fmt::Arguments<'_>,
    ) -> Result<std::string::String, crate::code::Error> {
        let mut out = std::string::String::new();
        fuxix::Allocation::write(self, &mut out, args)?;
        Ok(out)
    }

    pub fn optional_text(
        &mut self,
        text: Option<&str>,
    ) -> Result<crate::json::Json, crate::code::Error> {
        match text {
            Some(text) => fuxix::Allocation::string(self, text).map(crate::json::Json::str),
            None => Ok(crate::json::Json::Null),
        }
    }

    /// Move a vector into admitted replacement slots while its old block lives.
    pub fn map<T, U>(
        &mut self,
        input: std::vec::Vec<T>,
        mut map: impl FnMut(T, &mut Self) -> Result<U, crate::code::Error>,
    ) -> Result<std::vec::Vec<U>, crate::code::Error> {
        use fuxix::Allocation;
        let old = input
            .capacity()
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(Self::exceeded)?;
        let mut out = self.vector(input.len())?;
        for value in input {
            out.push(map(value, self)?);
        }
        self.release(old)?;
        Ok(out)
    }

    pub fn object<const N: usize>(
        &mut self,
        fields: [(&'static str, crate::json::Json); N],
    ) -> Result<crate::json::Json, crate::code::Error> {
        use fuxix::Allocation;
        let mut out = self.vector(N)?;
        for (key, value) in fields {
            out.push((self.string(key)?, value));
        }
        Ok(crate::json::Json::Object(out))
    }

    pub fn fields(
        &mut self,
        fields: crate::json::Fields,
    ) -> Result<std::vec::Vec<(std::string::String, crate::json::Json)>, crate::code::Error> {
        self.map(fields, |(key, value), budget| {
            Ok((fuxix::Allocation::string(budget, key)?, value))
        })
    }

    pub fn slots<T>(&mut self, count: usize) -> Result<(), crate::code::Error> {
        use fuxix::Allocation;
        self.charge(
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or_else(Self::exceeded)?,
        )
    }
}

impl fuxix::Allocation for Budget {
    type Error = crate::code::Error;
    fn remaining(&self) -> usize {
        self.limit.min(self.region().remaining())
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Self::Error> {
        let limit = self.limit.checked_sub(bytes);
        let held = self.held.bytes().checked_add(bytes);
        let (Some(limit), Some(held)) = (limit, held) else {
            return Err(Self::exceeded());
        };
        self.held.resize(held).map_err(|_| Self::exceeded())?;
        self.limit = limit;
        Ok(())
    }
    fn release(&mut self, bytes: usize) -> Result<(), Self::Error> {
        let held = self.held.bytes().checked_sub(bytes);
        self.held
            .resize(held.ok_or_else(Self::exceeded)?)
            .map_err(|_| Self::exceeded())?;
        self.limit = self.limit.saturating_add(bytes);
        Ok(())
    }
    fn exceeded() -> Self::Error {
        crate::code::fail(
            crate::code::Raise::TooLong,
            "finite reply exceeds its byte or allocation limit; request a smaller scope",
        )
    }
}

/// Bytes being read, what they make charged to a budget.
pub struct Reader<'a> {
    bytes: &'a [u8],
    budget: &'a mut Budget,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], budget: &'a mut Budget) -> Reader<'a> {
        Reader { bytes, budget }
    }
    /// `read`, what it makes bounded also by `bytes`.
    pub(crate) fn limited<T>(
        &mut self,
        bytes: usize,
        read: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let outer = self.budget.narrow(bytes);
        let result = read(self);
        self.budget.widen(outer);
        result
    }
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        fuxix::Allocation::charge(self.budget, bytes).map_err(|_| Error::Limit)
    }
    /// The count a list or a text begins with, not yet read.
    fn count(&self) -> Result<usize, Error> {
        let count = self.bytes.first_chunk().ok_or(Error::Truncated)?;
        Ok(u32::from_be_bytes(*count) as usize)
    }
    pub(crate) fn list<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<std::vec::Vec<T>, Error> {
        let count = u32::get(self)? as usize;
        if count > self.bytes.len() {
            return Err(Error::Truncated);
        }
        self.charge(
            count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(Error::Limit)?,
        )?;
        let mut out = std::vec::Vec::new();
        out.try_reserve_exact(count).map_err(|_| Error::Limit)?;
        for _ in 0..count {
            out.push(read(self)?);
        }
        Ok(out)
    }
    pub(crate) fn bounded_list<T>(
        &mut self,
        max: usize,
        read: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<std::vec::Vec<T>, Error> {
        if self.count()? > max {
            return Err(Error::Bad("list length"));
        }
        self.list(read)
    }
    /// A text: its length, then its bytes, which must be UTF-8.
    pub(crate) fn text(&mut self) -> Result<std::string::String, Error> {
        let len = u32::get(self)? as usize;
        let bytes = self.bytes(len)?;
        let text = std::str::from_utf8(bytes).map_err(|_| Error::NotUtf8)?;
        self.charge(len)?;
        let mut out = std::string::String::new();
        out.try_reserve_exact(len).map_err(|_| Error::Limit)?;
        out.push_str(text);
        Ok(out)
    }
    pub(crate) fn bounded_text(&mut self, max: usize) -> Result<std::string::String, Error> {
        if self.count()? > max {
            return Err(Error::Bad("text length"));
        }
        self.text()
    }
    pub(crate) fn boxed<T: Wire>(&mut self) -> Result<Box<T>, Error> {
        self.charge(std::mem::size_of::<T>())?;
        T::get(self).map(Box::new)
    }
    pub(crate) fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.bytes.split_first_chunk().ok_or(Error::Truncated)?;
        self.bytes = rest;
        Ok(*head)
    }
    fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self.bytes.split_at_checked(len).ok_or(Error::Truncated)?;
        self.bytes = rest;
        Ok(head)
    }
    /// The whole value: no bytes may follow.
    pub fn whole<T: Wire>(mut self) -> Result<T, Error> {
        if self.bytes.len() > MAX_REPLY {
            return Err(Error::Limit);
        }
        let value = T::get(&mut self)?;
        self.bytes
            .is_empty()
            .then_some(value)
            .ok_or(Error::Trailing)
    }
}

/// The whole value `bytes` are, made within a budget of its own of
/// `MAX_ALLOCATED`: a frame or a reply a client decodes.
pub fn decode<T: Wire>(bytes: &[u8]) -> Result<T, Error> {
    Reader::new(bytes, &mut Budget::with_limit(MAX_ALLOCATED)).whole()
}

/// Where an encoding's bytes are kept: grown fallibly, nothing past what is
/// asked.
pub trait Store {
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

/// A client's or a test's bytes, admitted up to the reply's cap.
impl Store for std::vec::Vec<u8> {
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        append(self, bytes)
    }
}

/// A request's reply, in its region.
impl Store for BoundedVec<u8> {
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.extend_from_slice(bytes).map_err(|_| Error::Limit)
    }
}

/// A fallible wire writer into a store, or a count-only preflight of the
/// same encoding.
pub struct Writer<S = std::vec::Vec<u8>> {
    bytes: Option<S>,
    left: Option<usize>,
}

impl Writer {
    pub fn counting(limit: usize) -> Self {
        Self {
            bytes: None,
            left: Some(limit),
        }
    }
}

impl<S> Writer<S> {
    pub fn check_ref(&self) -> Result<(), Error> {
        self.left.map(|_| ()).ok_or(Error::Limit)
    }
    pub fn check(self) -> Result<(), Error> {
        self.check_ref()
    }
}

impl<S: Store> Sink for Writer<S> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.left = self.left.and_then(|left| left.checked_sub(bytes.len()));
        if self.left.is_some()
            && let Some(out) = &mut self.bytes
            && out.append(bytes).is_err()
        {
            self.left = None;
        }
    }
    fn full(&self) -> bool {
        self.left.is_none()
    }
}

/// Admit a reply fragment before growing its accumulator. Reservations
/// stop at the same cap as its length, including at the edge.
pub fn append(out: &mut std::vec::Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
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

/// Test a bounded encoding's size without constructing its byte vector.
#[cfg(any(test, feature = "testing"))]
pub fn size<T: Wire>(value: &T) -> Result<usize, Error> {
    let mut out = Writer::counting(MAX_REPLY);
    value.put(&mut out);
    out.check_ref()?;
    Ok(MAX_REPLY.saturating_sub(out.left.ok_or(Error::Limit)?))
}

/// The bytes of `value`, written into `out` and refused before a write
/// passes the reply's cap or the store refuses.
fn encode_into<T: Wire, S: Store>(value: &T, out: S) -> Result<S, Error> {
    let mut out = Writer {
        bytes: Some(out),
        left: Some(MAX_REPLY),
    };
    value.put(&mut out);
    out.check_ref()?;
    out.bytes.ok_or(Error::Limit)
}

/// A finite reply's bytes, refused before a write exceeds the cap.
pub fn encode<T: Wire>(value: &T) -> Result<std::vec::Vec<u8>, Error> {
    encode_into(value, std::vec::Vec::new())
}

/// A finite reply's bytes in `region`, each growth reserved there first,
/// refused before a write exceeds the cap or the region.
pub fn encode_in<T: Wire>(value: &T, region: &Region) -> Result<BoundedVec<u8>, Error> {
    encode_into(value, BoundedVec::new(region))
}
