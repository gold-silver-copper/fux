//! Bytes that arrive at one end and are taken from the other: a socket's
//! input waiting to be decoded, its output waiting to be written, a key
//! sequence waiting to complete.

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
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.get(self.taken..).unwrap_or_default()
    }
    pub fn len(&self) -> usize {
        self.bytes.len().saturating_sub(self.taken)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn push(&mut self, more: &[u8]) {
        let left = self.len();
        if self.taken > 0 && self.taken >= left {
            // What is left moves to the front, into the space taken, which is
            // at least as large, so the two do not overlap. Only what is left
            // is copied.
            if let Some((front, back)) = self.bytes.split_at_mut_checked(self.taken) {
                for (to, from) in front.iter_mut().zip(back.iter()) {
                    *to = *from;
                }
            }
            self.bytes.truncate(left);
            self.taken = 0;
        }
        self.bytes.extend_from_slice(more);
    }
    /// Takes `n` bytes from the front, or all there are.
    pub fn take(&mut self, n: usize) {
        self.taken = self.taken.saturating_add(n).min(self.bytes.len());
        if self.taken == self.bytes.len() {
            self.bytes.clear();
            self.taken = 0;
        }
    }
    /// Everything not yet taken, leaving the queue empty.
    pub fn take_all(&mut self) -> Vec<u8> {
        let rest = self.as_slice().to_vec();
        self.bytes.clear();
        self.taken = 0;
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(queue.take_all(), b"abc");
        assert!(queue.is_empty() && queue.as_slice().is_empty());
    }

    #[test]
    fn a_long_stream_through_a_queue_is_unchanged() {
        let stream: Vec<u8> = (0..10_000u32).flat_map(u32::to_be_bytes).collect();
        let mut queue = ByteQueue::default();
        let mut out = Vec::new();
        let mut rest = stream.as_slice();
        // Pushes of 7 and takes of 5 bytes, then a drain at the end.
        while let Some((piece, after)) = rest.split_at_checked(7) {
            queue.push(piece);
            out.extend(queue.as_slice().iter().take(5));
            queue.take(5);
            rest = after;
        }
        queue.push(rest);
        out.extend(queue.take_all());
        assert_eq!(out, stream);
    }
}
