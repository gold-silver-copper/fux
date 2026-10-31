//! Shared storage: a value many places hold, and the bytes a region holds
//! for it while it lives.
use crate::{Failure, Refused, Region, Reservation};
use std::cell::RefCell;
use std::rc::Rc;

fuxth_core::bounded_only!();

/// The bytes a region holds for shared storage until it drops: taken
/// before the storage is made in the region, or as a host's storage, made
/// outside, is admitted into a run. A clone holds nothing: a copy of the
/// storage takes its own.
#[derive(Debug, Default)]
pub struct Lease(RefCell<Option<Reservation>>);

impl Clone for Lease {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Lease {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Lease {
    /// Holds `bytes` of `region`: more taken first, or refused with nothing
    /// changed; less given back. What it held of another region it gives
    /// back once this one holds them.
    pub fn reserve(&self, region: &Region, bytes: usize) -> Result<(), Failure> {
        let mut held = self.0.try_borrow_mut().map_err(|_| recursive())?;
        match held.as_mut().filter(|r| r.region().same(region)) {
            Some(reservation) => reservation.resize(bytes)?,
            None => *held = Some(region.reserve(bytes)?),
        }
        Ok(())
    }

    /// Whether it holds anything, of any region.
    fn holds(&self) -> bool {
        self.0.try_borrow().is_ok_and(|held| held.is_some())
    }
}

#[cold]
fn recursive() -> Failure {
    Failure::new(crate::Code::Contract, "recursive storage admission")
}

/// Shared storage, holding its lease: editing the value can't give it
/// back, and copying the storage starts a fresh one.
#[derive(Debug, PartialEq)]
pub struct Shared<T>(Rc<Storage<T>>);

#[derive(Debug, PartialEq)]
struct Storage<T> {
    value: T,
    lease: Lease,
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<T> Shared<T> {
    /// What the shared box takes beside the value's own stores: its counts
    /// and its lease.
    pub const BOX: usize = std::mem::size_of::<([usize; 2], Storage<T>)>();

    /// A host's `value`, holding nothing yet: admitted into a run, it holds
    /// its bytes there ([`Shared::reserve`]).
    pub fn new(value: T) -> Self {
        Self(Rc::new(Storage {
            value,
            lease: Lease::default(),
        }))
    }

    /// `value` in a box reserved from `region` before it is made.
    pub fn made(region: &Region, value: T) -> Result<Self, Refused> {
        let held = region.reserve(Self::BOX)?;
        Ok(Self(Rc::new(Storage {
            value,
            lease: Lease(RefCell::new(Some(held))),
        })))
    }

    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        Rc::get_mut(&mut this.0).map(|storage| &mut storage.value)
    }

    /// Holds `bytes` of `region` for it: a host's storage, as it is
    /// admitted ([`Lease::reserve`]).
    pub fn reserve(&self, region: &Region, bytes: usize) -> Result<(), Failure> {
        self.0.lease.reserve(region, bytes)
    }

    /// Its box held in `region`, unless some region holds it already: a
    /// string or list a host made without one, as it is admitted.
    pub(crate) fn boxed(&self, region: &Region) -> Result<(), Failure> {
        match self.0.lease.holds() {
            true => Ok(()),
            false => self.reserve(region, Self::BOX),
        }
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
