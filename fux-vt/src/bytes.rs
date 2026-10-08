//! Bytes that arrive at one end and are taken from the other: a socket's
//! input waiting to be decoded, its output waiting to be written, a key
//! sequence waiting to complete.
use std::ops::Range;

/// `slice::copy_within`, checked: copies the run `src` of `slice` to start at
/// `dest`, if both runs lie within it.
///
/// `copy_within` panics on a range out of bounds, so fux calls it here, after
/// this check, and nowhere else but `Grid::move_row`, whose ranges are its
/// run's own (clippy.toml says why).
fn copy_within<T: Copy>(slice: &mut [T], src: Range<usize>, dest: usize) -> Option<()> {
    let fits = src.start <= src.end
        && src.end <= slice.len()
        && dest
            .checked_add(src.len())
            .is_some_and(|end| end <= slice.len());
    fits.then(|| slice.copy_within(src, dest))
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
    /// The bytes already there are the queue's: `write` only adds after
    /// them, or takes back what it added.
    pub fn push_with<T>(&mut self, write: impl FnOnce(&mut Vec<u8>) -> T) -> T {
        self.compact();
        write(&mut self.bytes)
    }
    /// Reclaims the space taken, once at least as much has been taken as is
    /// left.
    fn compact(&mut self) {
        let (taken, end) = (self.taken, self.bytes.len());
        let left = self.len();
        // What is left moves to the front, into the space taken, which is at
        // least as large. Only what is left is copied.
        if taken > 0 && taken >= left && copy_within(&mut self.bytes, taken..end, 0).is_some() {
            self.bytes.truncate(left);
            self.taken = 0;
        }
    }
    /// Gives back the buffer's memory beyond `keep` bytes, once no more than
    /// that is queued: a queue that once held a large frame or paint does
    /// not keep its size for the life of the connection.
    pub fn shrink(&mut self, keep: usize) {
        if self.len() > keep || self.bytes.capacity() <= keep {
            return;
        }
        let end = self.bytes.len();
        if self.taken > 0 && copy_within(&mut self.bytes, self.taken..end, 0).is_some() {
            let left = end.saturating_sub(self.taken);
            self.bytes.truncate(left);
            self.taken = 0;
        }
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
        // Bytes taken from the front are lent until the next push, which
        // reclaims their space once the queue is empty.
        queue.push(b"frame");
        assert_eq!(queue.take_front(3), b"fra");
        assert_eq!(queue.take_front(9), b"me");
        assert!(queue.is_empty());
        queue.push(b"next");
        assert_eq!((queue.as_slice(), queue.taken), (&b"next"[..], 0));
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

    #[test]
    fn a_long_stream_through_a_queue_is_unchanged() {
        let stream: Vec<u8> = (0..10_000u32).flat_map(u32::to_be_bytes).collect();
        let mut queue = ByteQueue::default();
        let mut out: Vec<u8> = Vec::new();
        let mut rest = stream.as_slice();
        // Pushes of 7 and takes of 5 bytes, then a drain at the end.
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
}
