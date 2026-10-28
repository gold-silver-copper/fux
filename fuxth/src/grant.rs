//! Checked space shared by observation builders, decoders and live storage.
use std::cell::Cell;
use std::fmt;

/// A fixed allocation allowance. Shared owners attach refundable leases;
/// temporary builders charge it before allocating and release dropped buffers.
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

/// Counts bounded text, then writes it into exactly the admitted capacity.
pub struct Render {
    grant: Grant,
    bytes: usize,
}

impl Render {
    pub fn new(bytes: usize) -> Self {
        Self {
            grant: Grant::new(bytes),
            bytes: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn remaining(&self) -> usize {
        self.grant.remaining()
    }

    pub fn finish<E>(
        self,
        write: impl FnOnce(&mut dyn fmt::Write) -> Result<(), E>,
        exceeded: impl FnOnce() -> E,
    ) -> Result<String, E> {
        struct Output {
            text: String,
            grant: Grant,
        }
        impl fmt::Write for Output {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.grant.charge(text.len())?;
                self.text.push_str(text);
                Ok(())
            }
        }
        let mut text = String::new();
        text.try_reserve_exact(self.bytes).map_err(|_| exceeded())?;
        let mut out = Output {
            text,
            grant: Grant::new(self.bytes),
        };
        write(&mut out)?;
        Ok(out.text)
    }
}

impl fmt::Write for Render {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.grant.charge(text.len())?;
        self.bytes = self.bytes.checked_add(text.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}
