//! Regions: byte budgets that owners carve and stores reserve from.
use crate::Grant;
use std::fmt;
use std::rc::Rc;

/// A byte budget: every byte a store in it holds is reserved before it is
/// allocated, and given back when the store drops. A clone is the same
/// budget. A child is carved from its parent whole, so a request's region
/// comes out of its server's, and gives it back when the last of it drops.
#[derive(Clone, Debug)]
pub struct Region(Rc<Pool>);

#[derive(Debug)]
struct Pool {
    left: Grant,
    budget: usize,
    /// What this region holds of its parent's, if it was carved from one:
    /// given back as the region drops.
    _carved: Option<Reservation>,
}

impl Region {
    /// What a region takes on the heap beside its budget, charged to the
    /// parent when it carves a child.
    pub const OVERHEAD: usize =
        std::mem::size_of::<Pool>().saturating_add(std::mem::size_of::<[usize; 2]>());

    pub fn new(budget: usize) -> Self {
        Self::of(budget, None)
    }

    fn of(budget: usize, carved: Option<Reservation>) -> Self {
        Self(Rc::new(Pool {
            left: Grant::new(budget),
            budget,
            _carved: carved,
        }))
    }

    /// A region of `budget` bytes taken from this one, before it is made.
    pub fn child(&self, budget: usize) -> Result<Region, Refused> {
        let carved = self.reserve(budget.saturating_add(Self::OVERHEAD))?;
        Ok(Self::of(budget, Some(carved)))
    }

    pub fn budget(&self) -> usize {
        self.0.budget
    }

    /// Bytes not yet reserved.
    pub fn remaining(&self) -> usize {
        self.0.left.remaining()
    }

    /// `bytes` held from this region until the reservation drops.
    pub fn reserve(&self, bytes: usize) -> Result<Reservation, Refused> {
        let mut held = self.nothing();
        held.resize(bytes)?;
        Ok(held)
    }

    /// A reservation holding nothing yet.
    pub(crate) fn nothing(&self) -> Reservation {
        Reservation {
            region: self.clone(),
            bytes: 0,
        }
    }

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
        Ok(())
    }
}
