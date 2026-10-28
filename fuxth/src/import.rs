//! Reservations follow shared imported storage, not stack slots or run totals.
use crate::{Code, Failure, Grant};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

#[derive(Debug)]
struct Reservation {
    grant: Weak<Grant>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(grant) = self.grant.upgrade() {
            let _ = grant.release(self.bytes);
        }
    }
}

/// Kept inside an allocation. Cloning the allocation starts a new reservation;
/// cloning its Rc keeps this one. Dead machines leave no live grant behind.
#[derive(Debug, Default)]
pub struct ImportLease(RefCell<Option<Reservation>>);

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
    pub fn reserve(&self, imports: &Rc<Grant>, bytes: usize) -> Result<(), Failure> {
        let mut held = self
            .0
            .try_borrow_mut()
            .map_err(|_| Failure::new(Code::Contract, "recursive storage admission"))?;
        let owner = Rc::downgrade(imports);
        if let Some(reservation) = held.as_mut().filter(|r| r.grant.ptr_eq(&owner)) {
            // Mutable storage can grow before the host returns it again.
            if bytes >= reservation.bytes {
                imports.charge(bytes.saturating_sub(reservation.bytes))
            } else {
                imports.release(reservation.bytes.saturating_sub(bytes))
            }
            .map_err(|_| full())?;
            reservation.bytes = bytes;
        } else {
            imports.charge(bytes).map_err(|_| full())?;
            *held = Some(Reservation {
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

    pub fn reserve(&self, imports: &Rc<Grant>, bytes: usize) -> Result<(), Failure> {
        self.0.lease.reserve(imports, bytes)
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}
