//! Checked space: a run's one grant, and the reservations that hold it.
use crate::error::{Code, Failure};
use std::cell::Cell;
use std::collections::TryReserveError;
use std::fmt;
use std::rc::Rc;

/// A fixed allocation allowance. Every byte a run holds is reserved from
/// it before it is allocated, and given back once the allocation drops.
#[derive(Debug)]
pub struct Grant {
    left: Cell<usize>,
    bytes: usize,
}

impl Grant {
    pub fn new(bytes: usize) -> Self {
        Self {
            left: Cell::new(bytes),
            bytes,
        }
    }

    pub fn remaining(&self) -> usize {
        self.left.get()
    }

    pub fn charge(&self, bytes: usize) -> fmt::Result {
        self.left
            .set(self.remaining().checked_sub(bytes).ok_or(fmt::Error)?);
        Ok(())
    }

    pub fn release(&self, bytes: usize) -> fmt::Result {
        self.left
            .set(self.remaining().checked_add(bytes).ok_or(fmt::Error)?);
        Ok(())
    }

    /// What a run is told when it would hold more than its grant.
    pub fn full(&self) -> Failure {
        Failure::new(
            Code::TooLong,
            format!("the run holds more than {} bytes", self.bytes),
        )
    }
}

/// Bytes held from a grant by the allocation it lives in: taken before the
/// allocation is made, and given back when it drops.
#[derive(Debug)]
pub struct Reservation {
    grant: Option<Rc<Grant>>,
    bytes: usize,
}

impl Reservation {
    /// `bytes` held from `grant`, or its refusal.
    pub fn take(grant: &Rc<Grant>, bytes: usize) -> Result<Self, Failure> {
        grant.charge(bytes).map_err(|_| grant.full())?;
        Ok(Self {
            grant: Some(Rc::clone(grant)),
            bytes,
        })
    }

    /// Nothing held yet from `grant`.
    pub fn empty(grant: &Rc<Grant>) -> Self {
        Self {
            grant: Some(Rc::clone(grant)),
            bytes: 0,
        }
    }

    /// Nothing held: a program's own literals, bounded by its source.
    pub(crate) fn none() -> Self {
        Self {
            grant: None,
            bytes: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Why it can't hold more.
    fn full(&self) -> Failure {
        match &self.grant {
            Some(grant) => grant.full(),
            None => Failure::new(Code::Contract, "a literal's storage grew"),
        }
    }

    /// Holds `bytes` now: more is taken first, less given back.
    pub fn resize(&mut self, bytes: usize) -> Result<(), Failure> {
        let Some(grant) = &self.grant else {
            return Err(self.full());
        };
        if bytes > self.bytes {
            grant
                .charge(bytes.saturating_sub(self.bytes))
                .map_err(|_| grant.full())?;
        } else {
            // A release past the grant can't be: it gave these bytes.
            let _ = grant.release(self.bytes.saturating_sub(bytes));
        }
        self.bytes = bytes;
        Ok(())
    }

    /// Room for `more` items past `items`' length, each `size` bytes:
    /// the grown capacity is held, beside the old, while the items move,
    /// and the old given back after. Capacity at least doubles, so pushes
    /// cost amortized constant time, and never shrinks while it is held.
    #[inline]
    pub fn grow<R: Room>(&mut self, items: &mut R, more: usize) -> Result<(), Failure> {
        match items.capacity().saturating_sub(items.len()) >= more {
            true => Ok(()),
            false => self.regrow(items, more),
        }
    }

    #[cold]
    fn regrow<R: Room>(&mut self, items: &mut R, more: usize) -> Result<(), Failure> {
        let needed = items.len().checked_add(more).ok_or_else(|| self.full())?;
        let size = R::SIZE;
        let capacity = needed.max(items.capacity().saturating_mul(2)).max(4);
        let old = items.capacity().saturating_mul(size);
        let new = capacity.checked_mul(size).ok_or_else(|| self.full())?;
        let before = self.bytes;
        self.resize(before.checked_add(new).ok_or_else(|| self.full())?)?;
        if items
            .try_reserve_exact(capacity.saturating_sub(items.len()))
            .is_err()
        {
            self.resize(before)?;
            return Err(self.full());
        }
        let now = items.capacity().saturating_mul(size);
        self.resize(before.saturating_sub(old).saturating_add(now))
    }
}

/// What a reservation grows: a vector, or a string's bytes.
pub trait Room {
    /// Bytes an item takes.
    const SIZE: usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn capacity(&self) -> usize;
    fn try_reserve_exact(&mut self, more: usize) -> Result<(), TryReserveError>;
}

impl<T> Room for Vec<T> {
    const SIZE: usize = std::mem::size_of::<T>();
    fn len(&self) -> usize {
        self.len()
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn try_reserve_exact(&mut self, more: usize) -> Result<(), TryReserveError> {
        self.try_reserve_exact(more)
    }
}

impl Room for String {
    const SIZE: usize = 1;
    fn len(&self) -> usize {
        self.len()
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn try_reserve_exact(&mut self, more: usize) -> Result<(), TryReserveError> {
        self.try_reserve_exact(more)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(grant) = &self.grant {
            // It gives back only what it took.
            let _ = grant.release(self.bytes);
        }
    }
}

/// Text `write` writes: counted first, its bytes reserved from `grant`,
/// then written into exactly that capacity.
pub fn written(
    grant: &Rc<Grant>,
    mut write: impl FnMut(&mut dyn fmt::Write) -> Result<(), Failure>,
) -> Result<(String, Reservation), Failure> {
    let mut count = Render::new(grant.remaining());
    write(&mut count).map_err(|_| grant.full())?;
    let held = Reservation::take(grant, count.bytes())?;
    let text = count.finish(write, || grant.full())?;
    Ok((text, held))
}

/// Counts bounded text, then writes it into exactly the admitted capacity.
pub struct Render {
    grant: Grant,
    bytes: usize,
    text: Option<String>,
}

impl Render {
    pub fn new(bytes: usize) -> Self {
        Self {
            grant: Grant::new(bytes),
            bytes: 0,
            text: None,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn finish<E>(
        mut self,
        write: impl FnOnce(&mut dyn fmt::Write) -> Result<(), E>,
        exceeded: impl FnOnce() -> E,
    ) -> Result<String, E> {
        let mut text = String::new();
        text.try_reserve_exact(self.bytes).map_err(|_| exceeded())?;
        self.grant = Grant::new(self.bytes);
        self.text = Some(text);
        write(&mut self)?;
        Ok(self.text.unwrap_or_default())
    }
}

impl fmt::Write for Render {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.grant.charge(text.len())?;
        if let Some(out) = &mut self.text {
            out.push_str(text);
        } else {
            self.bytes = self.bytes.checked_add(text.len()).ok_or(fmt::Error)?;
        }
        Ok(())
    }
}
