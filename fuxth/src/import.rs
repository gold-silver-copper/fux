//! Reservations follow shared imported storage, not stack slots or run totals.
use crate::{Code, Failure, Grant};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// One machine's fixed allowance for live imported storage. Only leases can
/// reserve or refund it; hosts cannot change the counter directly.
#[derive(Debug)]
pub struct Imports(Rc<Grant>);
impl Imports {
    pub fn new(limit: usize) -> Self {
        Self(Rc::new(Grant::new(limit)))
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
            let _ = grant.release(self.bytes);
        }
    }
}

/// A reservation carried with owned values that leave shared storage.
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
    pub fn reserve(&self, imports: &Imports, bytes: usize) -> Result<(), Failure> {
        let mut held = self
            .0
            .try_borrow_mut()
            .map_err(|_| Failure::new(Code::Contract, "recursive storage admission"))?;
        let owner = Rc::downgrade(&imports.0);
        let same = held.as_mut().filter(|r| r.grant.ptr_eq(&owner));
        let old = same.as_ref().map_or(0, |r| r.bytes);
        if bytes >= old {
            imports.0.charge(bytes.saturating_sub(old))
        } else {
            imports.0.release(old.saturating_sub(bytes))
        }
        .map_err(|_| full())?;
        if let Some(reservation) = same {
            reservation.bytes = bytes;
        } else {
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

/// Shared storage owns its reservation. Editing the value cannot release it;
/// copying the storage starts a fresh reservation.
#[derive(Debug, PartialEq)]
pub struct Shared<T>(Rc<Storage<T>>);

#[derive(Clone, Debug, PartialEq)]
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

    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        Rc::get_mut(&mut this.0).map(|storage| &mut storage.value)
    }

    pub(crate) fn copied(&self, elements: usize) -> usize {
        if Rc::strong_count(&self.0) > 1 {
            elements
        } else {
            0
        }
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

impl<T: Clone> Shared<T> {
    pub fn make_mut(this: &mut Self) -> &mut T {
        &mut Rc::make_mut(&mut this.0).value
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}

impl<T> AsRef<T> for Shared<T> {
    fn as_ref(&self) -> &T {
        self
    }
}
