//! The unit tests' generator: splitmix64 (`tests/corpus`'s `splitmix`),
//! seeded by each test, so that a case is the same on every run.

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        crate::parser::test_corpus::splitmix(&mut self.0)
    }
    /// Below `n`, or 0 for an `n` of 0.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }
    /// Below `n`, as a `u16`.
    pub(crate) fn small(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(usize::from(n))).unwrap_or(0)
    }
    pub(crate) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    pub(crate) fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        items.get(self.below(items.len())).copied()
    }
    /// From `low` to `high`, both included.
    pub(crate) fn byte_in(&mut self, low: u8, high: u8) -> u8 {
        let span = usize::from(high.saturating_sub(low)).saturating_add(1);
        low.saturating_add(u8::try_from(self.below(span)).unwrap_or(0))
    }
}
