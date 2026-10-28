//! Admission shared by OS observations and the values built from them.

/// A caller's remaining allocation grant. Returned buffers keep their charge;
/// temporary storage releases it only after its allocation has been dropped.
pub trait Allocation {
    /// The caller's refusal type.
    type Error;
    /// Bytes still available.
    fn remaining(&self) -> usize;
    /// Admit an allocation before requesting it.
    fn charge(&mut self, bytes: usize) -> Result<(), Self::Error>;
    /// Release an allocation that has been dropped.
    fn release(&mut self, bytes: usize) -> Result<(), Self::Error>;
    /// An allocation or size that could not be admitted.
    fn exceeded() -> Self::Error;

    /// A vector with exactly the admitted element capacity.
    fn vector<T>(&mut self, count: usize) -> Result<Vec<T>, Self::Error> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(Self::exceeded)?;
        self.charge(bytes)?;
        let mut out = Vec::new();
        if out.try_reserve_exact(count).is_err() {
            self.release(bytes)?;
            return Err(Self::exceeded());
        }
        Ok(out)
    }

    /// Grow while admitting both the old block and its replacement.
    fn reserve<T>(&mut self, out: &mut Vec<T>, more: usize) -> Result<(), Self::Error> {
        let needed = out.len().checked_add(more).ok_or_else(Self::exceeded)?;
        if needed <= out.capacity() {
            return Ok(());
        }
        let capacity = needed
            .checked_next_power_of_two()
            .ok_or_else(Self::exceeded)?
            .max(4);
        let bytes = capacity
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(Self::exceeded)?;
        let old = out
            .capacity()
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(Self::exceeded)?;
        self.charge(bytes)?;
        if out
            .try_reserve_exact(capacity.saturating_sub(out.len()))
            .is_err()
        {
            self.release(bytes)?;
            return Err(Self::exceeded());
        }
        self.release(old)
    }

    /// Copy text without an uncharged temporary or amortized growth.
    fn string(&mut self, text: &str) -> Result<String, Self::Error> {
        self.charge(text.len())?;
        let mut out = String::new();
        if out.try_reserve_exact(text.len()).is_err() {
            self.release(text.len())?;
            return Err(Self::exceeded());
        }
        out.push_str(text);
        Ok(out)
    }

    /// Append text, including the peak replacement capacity.
    fn text(&mut self, out: &mut String, text: &str) -> Result<(), Self::Error> {
        let needed = out
            .len()
            .checked_add(text.len())
            .ok_or_else(Self::exceeded)?;
        if needed > out.capacity() {
            let capacity = needed
                .checked_next_power_of_two()
                .ok_or_else(Self::exceeded)?
                .max(8);
            let old = out.capacity();
            self.charge(capacity)?;
            if out
                .try_reserve_exact(capacity.saturating_sub(out.len()))
                .is_err()
            {
                self.release(capacity)?;
                return Err(Self::exceeded());
            }
            self.release(old)?;
        }
        out.push_str(text);
        Ok(())
    }

    /// Format through the admitted String sink, including replacement peaks.
    fn write(
        &mut self,
        out: &mut String,
        args: std::fmt::Arguments<'_>,
    ) -> Result<(), Self::Error> {
        struct Formatted<'a, A: Allocation + ?Sized> {
            budget: &'a mut A,
            out: &'a mut String,
        }
        impl<A: Allocation + ?Sized> std::fmt::Write for Formatted<'_, A> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                self.budget
                    .text(self.out, text)
                    .map_err(|_| std::fmt::Error)
            }
        }
        std::fmt::write(&mut Formatted { budget: self, out }, args).map_err(|_| Self::exceeded())
    }

    /// Copy OS bytes with their exact admitted capacity.
    fn os_string(&mut self, bytes: &[u8]) -> Result<std::ffi::OsString, Self::Error> {
        use std::os::unix::ffi::OsStringExt;
        let mut out = self.vector(bytes.len())?;
        out.extend_from_slice(bytes);
        Ok(std::ffi::OsString::from_vec(out))
    }

    /// UTF-8 text with each invalid sequence replaced by U+FFFD, admitted
    /// at its exact encoded length before allocating the String.
    fn lossy(&mut self, bytes: &[u8]) -> Result<String, Self::Error> {
        let mut len = 0usize;
        for chunk in bytes.utf8_chunks() {
            len = len
                .checked_add(chunk.valid().len())
                .and_then(|n| n.checked_add(if chunk.invalid().is_empty() { 0 } else { 3 }))
                .ok_or_else(Self::exceeded)?;
        }
        self.charge(len)?;
        let mut out = String::new();
        if out.try_reserve_exact(len).is_err() {
            self.release(len)?;
            return Err(Self::exceeded());
        }
        for chunk in bytes.utf8_chunks() {
            out.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                out.push('\u{fffd}');
            }
        }
        Ok(out)
    }
}

impl Allocation for usize {
    type Error = std::io::Error;
    fn remaining(&self) -> usize {
        *self
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Self::Error> {
        *self = self.checked_sub(bytes).ok_or_else(Self::exceeded)?;
        Ok(())
    }
    fn release(&mut self, bytes: usize) -> Result<(), Self::Error> {
        *self = self.checked_add(bytes).ok_or_else(Self::exceeded)?;
        Ok(())
    }
    fn exceeded() -> Self::Error {
        std::io::Error::other("observation exceeds its allocation grant")
    }
}
