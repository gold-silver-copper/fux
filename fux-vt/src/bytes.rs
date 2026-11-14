//! Bytes that arrive at one end and are taken from the other: a socket's
//! input waiting to be decoded, its output waiting to be written.
use std::ops::Range;

/// `slice::copy_within`, checked: `None` unless both runs lie within
/// `slice`. `copy_within` panics out of bounds, so fux calls it only here
/// and in `Grid::move_row` (clippy.toml says why).
fn copy_within<T: Copy>(slice: &mut [T], src: Range<usize>, dest: usize) -> Option<()> {
    let fits = src.start <= src.end
        && src.end <= slice.len()
        && dest
            .checked_add(src.len())
            .is_some_and(|end| end <= slice.len());
    fits.then(|| slice.copy_within(src, dest))
}

/// Makes room in `bytes` for `additional` more, or says the OS gave none:
/// `bytes` is then as it was. What is then appended within that room
/// allocates nothing, so a process that must not die of a refusal (a pane's
/// own) reserves so before it writes.
pub fn try_reserve(bytes: &mut Vec<u8>, additional: usize) -> Result<(), crate::Error> {
    #[cfg(feature = "checks")]
    if crate::check::refusing() {
        return Err(crate::Error::Capacity);
    }
    bytes
        .try_reserve(additional)
        .map_err(|_| crate::Error::Capacity)
}

/// Appends `more` to `bytes`, or says the OS gave no room for it: `bytes`
/// is then as it was. For bytes another party sends, which fux joins only
/// as far as memory is given (`fux_wire::protocol::Joining`).
pub fn try_extend(bytes: &mut Vec<u8>, more: &[u8]) -> Result<(), crate::Error> {
    try_reserve(bytes, more.len())?;
    bytes.extend_from_slice(more);
    Ok(())
}

/// A queue of bytes. Taking from the front moves an offset, not the bytes
/// behind it; the space taken is reclaimed when the queue empties, or on the
/// next push once at least as much has been taken as is left, so each byte
/// is moved at most once on average.
#[derive(Debug, Default)]
pub struct ByteQueue {
    bytes: Vec<u8>,
    /// How many bytes at the front have been taken.
    taken: usize,
}

impl ByteQueue {
    /// The bytes not yet taken.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.get(self.taken..).unwrap_or_default()
    }
    /// The number of bytes not yet taken.
    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len().saturating_sub(self.taken)
    }
    /// Whether every byte has been taken.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Append `more` after the bytes not yet taken.
    #[inline]
    pub fn push(&mut self, more: &[u8]) {
        self.compact();
        self.bytes.extend_from_slice(more);
    }
    /// Appends what `write` adds to the end of the queue's buffer, in place.
    /// `write` should only add after the queued bytes; one that truncates
    /// them leaves the queue holding what is left.
    pub fn push_with<T>(&mut self, write: impl FnOnce(&mut Vec<u8>) -> T) -> T {
        self.compact();
        let wrote = write(&mut self.bytes);
        self.clamp_taken();
        wrote
    }
    /// `push`, if the OS gives the room: refused, the queue is as it was.
    pub fn try_push(&mut self, more: &[u8]) -> Result<(), crate::Error> {
        self.try_push_with(more.len(), |out| out.extend_from_slice(more))
    }
    /// `push_with`, once the OS gave room for `additional` bytes, which is
    /// all `write` should add: within it, `write` allocates nothing.
    /// Refused, `write` is not called and the queue is as it was.
    pub fn try_push_with<T>(
        &mut self,
        additional: usize,
        write: impl FnOnce(&mut Vec<u8>) -> T,
    ) -> Result<T, crate::Error> {
        self.try_reserve(additional)?;
        Ok(self.push_with(write))
    }
    /// Makes room for `additional` more bytes, or says the OS gave none:
    /// what is then pushed within it allocates nothing.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), crate::Error> {
        self.compact();
        try_reserve(&mut self.bytes, additional)
    }
    /// Keeps `taken` within a buffer `push_with`'s closure cut short.
    fn clamp_taken(&mut self) {
        if self.taken > self.bytes.len() {
            self.taken = self.bytes.len();
        }
    }
    /// Reclaims the space taken once it is at least as large as what is
    /// left, so each byte is copied at most once on average.
    fn compact(&mut self) {
        if self.taken >= self.len() {
            self.move_to_front();
        }
    }
    fn move_to_front(&mut self) {
        let (taken, end) = (self.taken, self.bytes.len());
        if taken > 0 && copy_within(&mut self.bytes, taken..end, 0).is_some() {
            self.bytes.truncate(end.saturating_sub(taken));
            self.taken = 0;
        }
    }
    /// Gives back the buffer's memory beyond `keep` bytes, once no more than
    /// that is queued, so one large frame does not pin its size.
    pub fn shrink(&mut self, keep: usize) {
        if self.len() > keep || self.bytes.capacity() <= keep {
            return;
        }
        self.move_to_front();
        self.bytes.shrink_to(keep);
    }
    /// Takes `n` bytes from the front, or all there are.
    #[inline]
    pub fn take(&mut self, n: usize) {
        let taken = self.taken.saturating_add(n);
        if taken >= self.bytes.len() {
            self.bytes.clear();
            self.taken = 0;
        } else {
            self.taken = taken;
        }
    }
    /// Takes `n` bytes from the front, or all there are, and returns them,
    /// borrowed: their space is reclaimed by a later push.
    #[inline]
    pub fn take_front(&mut self, n: usize) -> &[u8] {
        let start = self.taken;
        self.taken = self.taken.saturating_add(n).min(self.bytes.len());
        self.bytes.get(start..self.taken).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queue_gives_back_what_a_large_frame_took() {
        let mut queue = ByteQueue::default();
        queue.push(&vec![7u8; 1 << 20]);
        queue.take((1 << 20) - 10);
        // More than `keep` queued: nothing is given back.
        queue.shrink(4);
        assert!(queue.bytes.capacity() >= 1 << 20);
        // Little enough: the rest moves to the front and the memory goes.
        queue.shrink(64);
        assert_eq!(queue.as_slice(), &[7u8; 10]);
        assert!(queue.bytes.capacity() < 1 << 20);
        assert_eq!(queue.taken, 0);
    }

    /// A push refused memory leaves the queue as it was, and the next one
    /// goes on.
    #[cfg(feature = "checks")]
    #[test]
    fn a_push_refused_memory_leaves_the_queue_as_it_was() {
        let mut queue = ByteQueue::default();
        queue.push(b"held");
        queue.take(1);
        crate::check::refuse_next(1);
        assert!(queue.try_push(b"more").is_err());
        assert_eq!(queue.as_slice(), b"eld");
        crate::check::refuse_next(1);
        let wrote = queue.try_push_with(4, |out| out.extend_from_slice(b"more"));
        assert!(wrote.is_err());
        assert_eq!(queue.as_slice(), b"eld");
        assert!(queue.try_push(b"more").is_ok());
        assert_eq!(queue.as_slice(), b"eldmore");
        let mut bytes = b"ab".to_vec();
        crate::check::refuse_next(1);
        assert!(try_extend(&mut bytes, b"cd").is_err());
        assert!(try_extend(&mut bytes, b"cd").is_ok());
        assert_eq!(bytes, b"abcd");
    }

    #[test]
    fn bytes_leave_in_the_order_they_came() {
        let mut queue = ByteQueue::default();
        assert!(queue.is_empty());
        queue.push(b"hello");
        queue.take(2);
        assert_eq!(queue.as_slice(), b"llo");
        // As much taken as is left: the next push moves the rest to the front.
        queue.take(1);
        queue.push(b" world");
        assert_eq!(queue.as_slice(), b"lo world");
        assert_eq!(queue.taken, 0);
        assert_eq!(queue.len(), 8);
        // Taking more than there is takes what there is.
        queue.take(100);
        assert!(queue.is_empty());
        queue.push(b"abc");
        let len = queue.len();
        assert_eq!(queue.take_front(len), b"abc");
        assert!(queue.is_empty() && queue.as_slice().is_empty());
        queue.push(b"frame");
        assert_eq!(queue.take_front(3), b"fra");
        assert_eq!(queue.take_front(9), b"me");
        assert!(queue.is_empty());
        queue.push(b"next");
        assert_eq!((queue.as_slice(), queue.taken), (&b"next"[..], 0));
        let stream: Vec<u8> = (0..10_000u32).flat_map(u32::to_be_bytes).collect();
        let mut queue = ByteQueue::default();
        let mut out: Vec<u8> = Vec::new();
        let mut rest = stream.as_slice();
        while let Some((piece, after)) = rest.split_at_checked(7) {
            queue.push(piece);
            out.extend(queue.as_slice().iter().take(5));
            queue.take(5);
            rest = after;
        }
        queue.push(rest);
        out.extend(queue.as_slice());
        assert_eq!(out, stream);
    }

    #[test]
    fn copies_within_happen_only_in_bounds() {
        let mut bytes = *b"abcdef";
        assert_eq!(copy_within(&mut bytes, 3..6, 0), Some(()));
        assert_eq!(&bytes, b"defdef");
        assert_eq!(copy_within(&mut bytes, 1..3, 2), Some(()));
        assert_eq!(&bytes, b"deefef");
        let backwards = Range { start: 3, end: 1 };
        for (src, dest) in [(4..7, 0), (0..3, 4), (3..6, usize::MAX), (backwards, 0)] {
            assert_eq!(
                copy_within(&mut bytes, src.clone(), dest),
                None,
                "{src:?} {dest}"
            );
        }
        assert_eq!(&bytes, b"deefef");
    }
}
