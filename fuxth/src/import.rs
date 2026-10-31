//! Reservations follow shared storage, imported or built, not stack slots.
use crate::{Code, Failure, Grant};
use std::cell::RefCell;
use std::rc::Rc;

/// One machine's fixed allowance for the live storage of every string and
/// list it builds or imports. Only leases can reserve or refund it; hosts
/// cannot change the counter directly; a clone is the same grant.
#[derive(Clone, Debug)]
pub struct Imports(Rc<Grant>);
impl Imports {
    pub fn new(limit: usize) -> Self {
        Self(Rc::new(Grant::new(limit)))
    }

    /// Bytes not yet reserved.
    pub fn remaining(&self) -> usize {
        self.0.remaining()
    }

    /// `bytes` reserved before what they pay for is made: shared storage
    /// born holding it ([`Shared::leased`]) gives them back when it drops.
    pub fn lease(&self, bytes: usize) -> Result<ImportLease, Failure> {
        let lease = ImportLease::default();
        lease.reserve(self, bytes)?;
        Ok(lease)
    }
}

#[derive(Debug)]
struct Reservation {
    grant: Rc<Grant>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = self.grant.release(self.bytes);
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
        let same = held.as_mut().filter(|r| Rc::ptr_eq(&r.grant, &imports.0));
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
                grant: Rc::clone(&imports.0),
                bytes,
            });
        }
        Ok(())
    }
}

fn full() -> Failure {
    Failure::new(
        Code::TooLong,
        "the strings and lists a run holds exceed its allocation grant",
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
        Self::leased(value, ImportLease::default())
    }

    /// `value`, born holding `lease`: reserved, before `value` was made,
    /// for [`Shared::bytes`] of its payload.
    pub fn leased(value: T, lease: ImportLease) -> Self {
        Self(Rc::new(Storage { value, lease }))
    }

    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        Rc::get_mut(&mut this.0).map(|storage| &mut storage.value)
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
    /// Its value and lease, to change in place: copied first, holding
    /// nothing, when something else holds it.
    pub(crate) fn make_mut(this: &mut Self) -> (&mut T, &mut ImportLease) {
        let storage = Rc::make_mut(&mut this.0);
        (&mut storage.value, &mut storage.lease)
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
