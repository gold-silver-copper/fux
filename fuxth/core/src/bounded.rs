//! Stores made in a region: every growth is reserved from it before the
//! store allocates, and refused when the region can't hold it.
use crate::region::{Refused, Region, Reservation};
use std::borrow::Borrow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, TryReserveError};
use std::fmt;
use std::mem::{align_of, size_of};
use std::ops::{Deref, DerefMut};

/// A store whose memory its region holds: what it has allocated, and while
/// it grows, both its old and its new block. It derefs only to what can't
/// grow (a slice, a `str`), so it grows through its own fallible methods.
#[derive(Debug)]
pub struct Bounded<S> {
    store: S,
    held: Reservation,
}

/// A vector in a region.
pub type BoundedVec<T> = Bounded<Vec<T>>;
/// A string in a region.
pub type BoundedString = Bounded<String>;
/// An ordered map in a region. Its keys' and values' own heap is theirs to
/// reserve: the map holds its nodes.
pub type BoundedMap<K, V> = Bounded<BTreeMap<K, V>>;

impl<S: Default> Bounded<S> {
    /// Empty, holding nothing yet.
    pub fn new(region: &Region) -> Self {
        Self {
            store: S::default(),
            held: region.nothing(),
        }
    }
}

impl<S> Bounded<S> {
    /// The bytes its region holds for it.
    pub fn held(&self) -> usize {
        self.held.bytes()
    }

    pub fn region(&self) -> &Region {
        self.held.region()
    }
}

/// A vector's or a string's buffer, which grows by moving to a larger one.
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
    /// back once it is freed. A size past `usize` asks for all of it, which
    /// no region has left.
    #[cold]
    fn regrow(&mut self, more: usize, double: bool) -> Result<(), Refused> {
        let len = self.len();
        let needed = len.saturating_add(more);
        let capacity = match double {
            true => needed.max(self.capacity().saturating_mul(2)).max(S::LEAST),
            false => needed,
        };
        let before = self.held.bytes();
        let new = capacity.saturating_mul(S::SIZE);
        self.held.resize(before.saturating_add(new))?;
        if self
            .store
            .try_reserve_exact(capacity.saturating_sub(len))
            .is_err()
        {
            self.held.resize(before)?;
            return Err(Refused::Alloc { bytes: new });
        }
        self.held.resize(self.capacity().saturating_mul(S::SIZE))
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

    pub fn pop(&mut self) -> Option<T> {
        self.store.pop()
    }

    /// Drops the elements past `len`, keeping the capacity it holds.
    pub fn truncate(&mut self, len: usize) {
        self.store.truncate(len);
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

/// The most bytes a map of `len` entries holds, from std's B-tree: every
/// node but the root keeps 5 to 11 entries, so `len` entries take at most
/// `len / 5 + 1` nodes, none larger than an internal one (11 keys, 11
/// values, 12 edges, a parent and two counts, and their padding). A node
/// is made, never freed, as an entry goes in, so this bounds its peak too.
fn map_bytes<K, V>(len: usize) -> usize {
    let align = align_of::<K>()
        .max(align_of::<V>())
        .max(align_of::<usize>());
    let node = size_of::<[K; 11]>()
        .saturating_add(size_of::<[V; 11]>())
        .saturating_add(size_of::<[usize; 13]>())
        .saturating_add(size_of::<[u16; 2]>())
        .saturating_add(align.saturating_mul(3));
    (len / 5).saturating_add(1).saturating_mul(node)
}

impl<K: Ord, V> Bounded<BTreeMap<K, V>> {
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Puts `value` at `key`, giving back the value it replaces. A new key
    /// reserves its room first.
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, Refused> {
        let grown = map_bytes::<K, V>(self.store.len().saturating_add(1));
        match self.store.entry(key) {
            Entry::Occupied(mut at) => Ok(Some(at.insert(value))),
            Entry::Vacant(at) => {
                if grown > self.held.bytes() {
                    self.held.resize(grown)?;
                }
                at.insert(value);
                Ok(None)
            }
        }
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let removed = self.store.remove(key)?;
        // Smaller is always admitted.
        let _ = self.held.resize(map_bytes::<K, V>(self.store.len()));
        Some(removed)
    }

    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.store.get(key)
    }

    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        self.store.get_mut(key)
    }

    pub fn contains_key<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.store.contains_key(key)
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, K, V> {
        self.store.iter()
    }

    pub fn clear(&mut self) {
        self.store.clear();
        let _ = self.held.resize(0);
    }
}
