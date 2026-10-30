//! Shared storage, born holding the reservation that pays for it.
use crate::Reservation;
use std::rc::Rc;

/// A string's or list's storage, shared by every value that holds it. It
/// holds the reservation for itself and its payload, so k references cost
/// one allocation, and the last one to drop gives it back.
#[derive(Debug)]
pub struct Shared<T>(Rc<Storage<T>>);

#[derive(Debug)]
struct Storage<T> {
    value: T,
    held: Reservation,
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// Two are equal when their values are.
impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.value == other.0.value
    }
}

impl<T> Shared<T> {
    /// `value` shared, holding `held`: taken, before `value` was made, for
    /// `Shared::<T>::bytes` of its payload.
    pub fn new(value: T, held: Reservation) -> Self {
        Self(Rc::new(Storage { value, held }))
    }

    /// Its value and reservation, to change in place: only when nothing
    /// else holds it.
    pub fn get_mut(this: &mut Self) -> Option<(&mut T, &mut Reservation)> {
        Rc::get_mut(&mut this.0).map(|storage| (&mut storage.value, &mut storage.held))
    }

    /// Its value and reservation, moved out when nothing else holds it.
    pub fn try_unwrap(this: Self) -> Result<(T, Reservation), Self> {
        Rc::try_unwrap(this.0)
            .map(|storage| (storage.value, storage.held))
            .map_err(Self)
    }

    /// Payload allocation bytes, plus this shared allocation's wrapper.
    pub fn bytes(payload: usize) -> usize {
        payload
            .saturating_add(std::mem::size_of::<Storage<T>>())
            .saturating_add(std::mem::size_of::<[usize; 2]>())
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
