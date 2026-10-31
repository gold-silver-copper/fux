//! A counter of checked space: what a region holds, and what a decoder
//! or a builder may still make.
use std::cell::Cell;
use std::fmt;

/// A fixed allocation allowance: charged before what it pays for is
/// allocated, and given back as that is freed.
#[derive(Debug)]
pub struct Grant(Cell<usize>);

impl Grant {
    pub fn new(bytes: usize) -> Self {
        Self(Cell::new(bytes))
    }

    pub fn remaining(&self) -> usize {
        self.0.get()
    }

    pub fn charge(&self, bytes: usize) -> fmt::Result {
        self.0
            .set(self.remaining().checked_sub(bytes).ok_or(fmt::Error)?);
        Ok(())
    }

    pub fn release(&self, bytes: usize) -> fmt::Result {
        self.0
            .set(self.remaining().checked_add(bytes).ok_or(fmt::Error)?);
        Ok(())
    }
}
