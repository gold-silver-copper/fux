//! Reservations follow shared imported storage, not stack slots or run totals.
use crate::{Code, Failure};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

#[derive(Debug)]
struct Grant {
    limit: usize,
    used: Cell<usize>,
}

/// One machine's fixed grant for live imported allocations.
#[derive(Debug)]
pub struct Imports(Rc<Grant>);

impl Imports {
    pub fn new(limit: usize) -> Self {
        Self(Rc::new(Grant {
            limit,
            used: Cell::new(0),
        }))
    }
}

#[derive(Debug)]
struct Reservation {
    grant: Weak<Grant>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(grant) = self.grant.upgrade() {
            grant.used.set(grant.used.get().saturating_sub(self.bytes));
        }
    }
}

/// Kept inside an allocation. Cloning the allocation starts a new reservation;
/// cloning its Rc keeps this one. Dead machines leave no live grant behind.
#[derive(Debug, Default)]
pub struct ImportLease(RefCell<Vec<Reservation>>);

impl Clone for ImportLease {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for ImportLease {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ImportLease {
    pub fn reserve(&self, imports: &Imports, bytes: usize) -> Result<(), Failure> {
        let mut held = self
            .0
            .try_borrow_mut()
            .map_err(|_| Failure::new(Code::Contract, "recursive storage admission"))?;
        held.retain(|r| r.grant.strong_count() != 0);
        let owner = Rc::downgrade(&imports.0);
        if !held.iter().any(|r| r.grant.ptr_eq(&owner)) {
            held.try_reserve_exact(1).map_err(|_| full())?;
        }
        let bytes = bytes
            .checked_add(
                held.capacity()
                    .checked_mul(std::mem::size_of::<Reservation>())
                    .ok_or_else(full)?,
            )
            .ok_or_else(full)?;
        if let Some(reservation) = held.iter_mut().find(|r| r.grant.ptr_eq(&owner)) {
            // Mutable storage can grow before the host returns it again.
            let used = imports.0.used.get().saturating_sub(reservation.bytes);
            let used = used
                .checked_add(bytes)
                .filter(|n| *n <= imports.0.limit)
                .ok_or_else(full)?;
            imports.0.used.set(used);
            reservation.bytes = bytes;
        } else {
            let used = imports
                .0
                .used
                .get()
                .checked_add(bytes)
                .filter(|n| *n <= imports.0.limit)
                .ok_or_else(full)?;
            imports.0.used.set(used);
            held.push(Reservation {
                grant: owner,
                bytes,
            });
        }
        Ok(())
    }
}

fn full() -> Failure {
    Failure::new(
        Code::TooLong,
        "live imported storage exceeds its allocation grant",
    )
}

/// An immutable native allocation with a reservation shared by every clone.
#[derive(Debug, PartialEq)]
pub struct Shared<T>(Rc<Storage<T>>);

#[derive(Debug, PartialEq)]
struct Storage<T> {
    value: T,
    lease: ImportLease,
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Shared<T> {
    pub fn new(value: T) -> Self {
        Self(Rc::new(Storage {
            value,
            lease: ImportLease::default(),
        }))
    }

    /// Payload allocation bytes, plus this shared allocation's wrapper.
    pub fn bytes(payload: usize) -> usize {
        payload
            .saturating_add(std::mem::size_of::<Storage<T>>())
            .saturating_add(std::mem::size_of::<[usize; 2]>())
    }

    pub fn reserve(&self, imports: &Imports, bytes: usize) -> Result<(), Failure> {
        self.0.lease.reserve(imports, bytes)
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}
