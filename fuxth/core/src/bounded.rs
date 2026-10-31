//! Stores made in a region: every growth is reserved from it before the
//! store allocates, and refused when the region can't hold it.
use crate::region::{Refused, Region, Reservation};
use std::collections::TryReserveError;
use std::fmt;
use std::mem::size_of;
use std::ops::{Deref, DerefMut};

/// A store whose memory its region holds: what it has allocated, and while
/// it grows, both its old and its new block. It derefs only to what can't
/// grow (a slice, a `str`), so it grows through its own fallible methods.
///
/// Only a vector or a string can be one, as only their empty forms are
/// known to allocate nothing:
///
/// ```compile_fail,E0599
/// let region = fuxth_core::Region::new(0);
/// let boxed = fuxth_core::Bounded::<Box<[u64; 32]>>::new(&region);
/// ```
pub struct Bounded<S> {
    store: S,
    held: Reservation,
}

/// Its store alone: what its region holds for it is the region's to tell.
impl<S: fmt::Debug> fmt::Debug for Bounded<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.store.fmt(f)
    }
}

/// A vector in a region.
pub type BoundedVec<T> = Bounded<Vec<T>>;
/// A string in a region.
pub type BoundedString = Bounded<String>;

impl<S> Bounded<S> {
    /// The bytes its region holds for it.
    pub fn held(&self) -> usize {
        self.held.bytes()
    }

    pub fn region(&self) -> &Region {
        self.held.region()
    }
}

/// A vector's or a string's buffer, which grows by moving to a larger one,
/// and whose empty form allocates nothing.
trait Room: Default {
    /// Bytes an element takes.
    const SIZE: usize;
    /// The least it grows to, as std's vector does: few allocations for
    /// small elements, no waste for large ones.
    const LEAST: usize = match Self::SIZE {
        1 => 8,
        2..=1024 => 4,
        _ => 1,
    };
    fn len(&self) -> usize;
    fn capacity(&self) -> usize;
    fn try_reserve_exact(&mut self, more: usize) -> Result<(), TryReserveError>;
}

impl<T> Room for Vec<T> {
    const SIZE: usize = size_of::<T>();
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

#[expect(
    private_bounds,
    reason = "Room is how a vector and a string grow alike"
)]
impl<S: Room> Bounded<S> {
    /// Empty, holding nothing yet.
    pub fn new(region: &Region) -> Self {
        Self {
            store: S::default(),
            held: region.nothing(),
        }
    }

    /// Empty, with room for exactly `capacity` elements, reserved first.
    pub fn with_capacity(region: &Region, capacity: usize) -> Result<Self, Refused> {
        let mut made = Self::new(region);
        made.regrow(capacity, false)?;
        Ok(made)
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.store.capacity()
    }

    /// Room for `more` elements past its length. Its capacity at least
    /// doubles when it grows, so growing one at a time costs amortized
    /// constant time.
    #[inline]
    pub fn reserve(&mut self, more: usize) -> Result<(), Refused> {
        match self.capacity().saturating_sub(self.len()) >= more {
            true => Ok(()),
            false => self.regrow(more, true),
        }
    }

    /// Moves to a buffer with room for `more`: its bytes are reserved
    /// beside the old buffer's before it is allocated, and the old's given
    /// back once it is freed. A length or size past `usize` is refused
    /// before anything changes.
    #[cold]
    fn regrow(&mut self, more: usize, double: bool) -> Result<(), Refused> {
        let len = self.len();
        let needed = len.checked_add(more).ok_or(Refused::Overflow)?;
        let doubled = self.capacity().checked_mul(2).unwrap_or(needed);
        let capacity = match double {
            true => needed.max(doubled).max(S::LEAST),
            false => needed,
        };
        let before = self.held.bytes();
        let new = capacity.checked_mul(S::SIZE).ok_or(Refused::Overflow)?;
        let moving = before.checked_add(new).ok_or(Refused::Overflow)?;
        self.held.resize(moving)?;
        if self
            .store
            .try_reserve_exact(capacity.saturating_sub(len))
            .is_err()
        {
            self.held.resize(before)?;
            return Err(Refused::Alloc { bytes: new });
        }
        let now = self.capacity().checked_mul(S::SIZE);
        self.held.resize(now.ok_or(Refused::Overflow)?)
    }
}

impl<T> Bounded<Vec<T>> {
    pub fn push(&mut self, item: T) -> Result<(), Refused> {
        self.reserve(1)?;
        self.store.push(item);
        Ok(())
    }

    pub fn extend_from_slice(&mut self, items: &[T]) -> Result<(), Refused>
    where
        T: Clone,
    {
        self.reserve(items.len())?;
        self.store.extend_from_slice(items);
        Ok(())
    }

    /// Each of `items`, room for as many as they say they are reserved
    /// first.
    pub fn extend(&mut self, items: impl IntoIterator<Item = T>) -> Result<(), Refused> {
        let items = items.into_iter();
        self.reserve(items.size_hint().0)?;
        for item in items {
            self.push(item)?;
        }
        Ok(())
    }

    /// `len` long: cut there, or `value` added till it is.
    pub fn resize(&mut self, len: usize, value: T) -> Result<(), Refused>
    where
        T: Clone,
    {
        self.reserve(len.saturating_sub(self.len()))?;
        self.store.resize(len, value);
        Ok(())
    }

    /// A copy in the same region, exactly as long as it.
    pub fn copy(&self) -> Result<Self, Refused>
    where
        T: Clone,
    {
        let mut copy = Self::with_capacity(self.region(), self.len())?;
        copy.store.extend_from_slice(&self.store);
        Ok(copy)
    }

    pub fn pop(&mut self) -> Option<T> {
        self.store.pop()
    }

    /// Drops the elements past `len`, keeping the capacity it holds.
    pub fn truncate(&mut self, len: usize) {
        self.store.truncate(len);
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.store.retain(keep);
    }

    pub fn clear(&mut self) {
        self.store.clear();
    }
}

impl<T> Deref for Bounded<Vec<T>> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.store
    }
}

impl<T> DerefMut for Bounded<Vec<T>> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.store
    }
}

impl Bounded<String> {
    /// `text`, in exactly its room in `region`.
    pub fn of(region: &Region, text: &str) -> Result<Self, Refused> {
        let mut made = Self::with_capacity(region, text.len())?;
        made.store.push_str(text);
        Ok(made)
    }

    /// A copy in the same region, in exactly its room.
    pub fn copy(&self) -> Result<Self, Refused> {
        Self::of(self.region(), self)
    }

    pub fn push(&mut self, c: char) -> Result<(), Refused> {
        self.reserve(c.len_utf8())?;
        self.store.push(c);
        Ok(())
    }

    pub fn push_str(&mut self, text: &str) -> Result<(), Refused> {
        self.reserve(text.len())?;
        self.store.push_str(text);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<char> {
        self.store.pop()
    }

    pub fn clear(&mut self) {
        self.store.clear();
    }
}

impl Deref for Bounded<String> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.store
    }
}

impl fmt::Write for Bounded<String> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        Ok(self.push_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedVec;
    use crate::{Refused, Region};

    #[test]
    fn lengths_past_usize_and_what_the_system_refuses_change_nothing() {
        let none = Region::new(0);
        let mut units = BoundedVec::<()>::new(&none);
        assert_eq!(units.push(()), Ok(()));
        assert_eq!(units.reserve(usize::MAX), Err(Refused::Overflow));
        let mut full = BoundedVec::<()>::new(&none);
        assert_eq!(full.extend_from_slice(&[(); usize::MAX]), Ok(()));
        assert_eq!(full.push(()), Err(Refused::Overflow));
        assert_eq!((units.len(), full.len()), (1, usize::MAX));
        // A region as large as memory admits it; the system can't.
        let all = Region::new(usize::MAX);
        let mut words = BoundedVec::<u64>::new(&all);
        assert!(matches!(
            words.reserve(usize::MAX / 8),
            Err(Refused::Alloc { .. })
        ));
        assert_eq!((words.held(), all.remaining()), (0, usize::MAX));
    }
}
