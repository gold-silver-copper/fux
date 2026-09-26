//! A seeded source of choices: splitmix64, which is enough for a walk and
//! the same on every platform.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `n`; 0 for an empty range.
    pub fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }

    /// True `percent` times in a hundred.
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next().checked_rem(100).unwrap_or(0) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }

    /// An index into `weights`, each chosen in proportion to its weight.
    pub fn weighted(&mut self, weights: &[u64]) -> usize {
        let total = weights.iter().fold(0u64, |sum, w| sum.saturating_add(*w));
        let mut at = self.next().checked_rem(total).unwrap_or(0);
        for (i, w) in weights.iter().enumerate() {
            if at < *w {
                return i;
            }
            at = at.saturating_sub(*w);
        }
        0
    }
}
