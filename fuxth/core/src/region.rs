//! Regions: byte budgets that owners carve and stores reserve from.
use crate::Grant;
use std::fmt;
use std::rc::Rc;

/// A byte budget: every byte a store in it holds is reserved before it is
/// allocated, and given back when the store drops. A clone is the same
/// budget. A child is carved from its parent whole, so a request's region
/// comes out of its server's, and gives it back when the last of it drops.
#[derive(Clone)]
pub struct Region(Rc<Pool>);

struct Pool {
    left: Grant,
    budget: usize,
    /// What this region holds of its parent's, if it was carved from one:
    /// given back as the region drops.
    carved: Option<Reservation>,
}

impl Drop for Pool {
    /// Gives back what each ancestor it alone keeps alive carved, in a loop:
    /// a chain of children, however long, drops in constant stack.
    fn drop(&mut self) {
        let mut carved = self.carved.take();
        while let Some(mut held) = carved {
            // The parent's own carving, taken out when this is the last of
            // the parent, so the parent drops holding nothing more.
            carved = Rc::get_mut(&mut held.region.0).and_then(|parent| parent.carved.take());
        }
    }
}

/// Its own budget only: a parent's is its parent's to tell.
impl fmt::Debug for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Region")
            .field("budget", &self.budget())
            .field("remaining", &self.remaining())
            .finish()
    }
}

impl Region {
    /// What a region takes on the heap beside its budget, charged to the
    /// parent when it carves a child.
    pub const OVERHEAD: usize = std::mem::size_of::<([usize; 2], Pool)>();

    pub fn new(budget: usize) -> Self {
        Self::of(budget, None)
    }

    fn of(budget: usize, carved: Option<Reservation>) -> Self {
        Self(Rc::new(Pool {
            left: Grant::new(budget),
            budget,
            carved,
        }))
    }

    /// A region of `budget` bytes taken from this one, before it is made.
    pub fn child(&self, budget: usize) -> Result<Region, Refused> {
        let whole = budget.checked_add(Self::OVERHEAD);
        let carved = self.reserve(whole.ok_or(Refused::Overflow)?)?;
        Ok(Self::of(budget, Some(carved)))
    }

    pub fn budget(&self) -> usize {
        self.0.budget
    }

    /// Whether `other` is this same budget: a clone of it, not one alike.
    pub fn same(&self, other: &Region) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// Bytes not yet reserved.
    pub fn remaining(&self) -> usize {
        self.0.left.remaining()
    }

    /// `bytes` held from this region until the reservation drops.
    #[inline]
    pub fn reserve(&self, bytes: usize) -> Result<Reservation, Refused> {
        self.0.left.charge(bytes).map_err(|_| self.full(bytes))?;
        Ok(Reservation {
            region: self.clone(),
            bytes,
        })
    }

    /// A reservation holding nothing yet.
    pub(crate) fn nothing(&self) -> Reservation {
        Reservation {
            region: self.clone(),
            bytes: 0,
        }
    }

    #[cold]
    fn full(&self, asked: usize) -> Refused {
        Refused::Full {
            asked,
            left: self.remaining(),
            budget: self.budget(),
        }
    }
}

/// Bytes held from a region by what they pay for: taken before it is
/// allocated, and given back when it drops.
#[derive(Debug)]
pub struct Reservation {
    region: Region,
    bytes: usize,
}

impl Reservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn region(&self) -> &Region {
        &self.region
    }

    /// Holds `bytes` now: more is taken first, or refused with nothing
    /// changed; less is given back.
    pub fn resize(&mut self, bytes: usize) -> Result<(), Refused> {
        let left = &self.region.0.left;
        let moved = match bytes.checked_sub(self.bytes) {
            Some(more) => left.charge(more),
            None => left.release(self.bytes.saturating_sub(bytes)),
        };
        moved.map_err(|_| self.region.full(bytes.saturating_sub(self.bytes)))?;
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // It gives back only what it took, so the budget has room for it.
        let _ = self.region.0.left.release(self.bytes);
    }
}

/// Why a region or a store in it can't hold more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The region has less left than asked.
    Full {
        asked: usize,
        left: usize,
        budget: usize,
    },
    /// The system had no memory for what the region admitted.
    Alloc { bytes: usize },
    /// A length or a size past what a `usize` holds.
    Overflow,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Full {
                asked,
                left,
                budget,
            } => write!(
                f,
                "{asked} bytes more than the {left} left of a {budget}-byte budget"
            ),
            Refused::Alloc { bytes } => write!(f, "the system refused {bytes} bytes"),
            Refused::Overflow => f.write_str("a size past what a usize holds"),
        }
    }
}

impl std::error::Error for Refused {}

impl From<Refused> for fmt::Error {
    fn from(_: Refused) -> Self {
        fmt::Error
    }
}

#[cfg(test)]
mod tests {
    use super::{Refused, Region};

    #[test]
    fn a_child_is_carved_whole_from_its_parent_and_given_back_when_dropped() -> Result<(), Refused>
    {
        let parent = Region::new(100 + Region::OVERHEAD);
        let child = parent.child(100)?;
        assert_eq!(parent.remaining(), 0);
        assert!(matches!(
            parent.child(0),
            Err(Refused::Full { left: 0, .. })
        ));
        let mut held = child.reserve(60)?;
        assert_eq!(
            child.reserve(41).err(),
            Some(Refused::Full {
                asked: 41,
                left: 40,
                budget: 100
            })
        );
        held.resize(20)?;
        assert_eq!(child.remaining(), 80);
        drop(child);
        assert_eq!(parent.remaining(), 0, "the reservation keeps its region");
        drop(held);
        assert_eq!(parent.remaining(), 100 + Region::OVERHEAD);
        assert_eq!(
            Region::new(usize::MAX).child(usize::MAX).err(),
            Some(Refused::Overflow)
        );
        Ok(())
    }

    #[test]
    fn a_deep_child_whose_ancestors_it_alone_keeps_drops_in_constant_stack() -> Result<(), Refused>
    {
        const DEPTH: usize = 100_000;
        let root = Region::new(DEPTH * Region::OVERHEAD);
        let mut deepest = root.child(root.budget() - Region::OVERHEAD)?;
        for _ in 1..DEPTH {
            deepest = deepest.child(deepest.budget() - Region::OVERHEAD)?;
        }
        assert_eq!(deepest.budget(), 0);
        assert_eq!(format!("{deepest:?}"), "Region { budget: 0, remaining: 0 }");
        drop(deepest);
        assert_eq!(root.remaining(), DEPTH * Region::OVERHEAD);
        Ok(())
    }
}
