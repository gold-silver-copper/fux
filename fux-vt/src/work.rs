//! The work fux-vt does, counted in deterministic units (threats.md 4.1),
//! with the `checks` feature: what a bound on the work per input byte is
//! stated in, and checked against by the bounds tests and the fuzz targets.
//!
//! Each counter adds in a method of its own, with saturating arithmetic.
//! The counts are kept per thread, so that every part of fux-vt counts into
//! them without being handed anything: a grid a resize or a reset makes
//! anew starts with no counter to lose. A probe reads [`Work::now`] before
//! and after what it measures, on the thread that runs it, and takes the
//! difference ([`Work::since`]).
//!
//! Without the feature none of this is built, and nothing is counted: each
//! count is a `work!` that expands to nothing.

use std::cell::RefCell;

/// Work done, by kind, in units each counter states (threats.md 4.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Input bytes examined by the parser's loop and its bulk scans.
    pub bytes: u64,
    /// Bytes [`crate::FrameScan::end`] examines.
    pub scanned: u64,
    /// Grid cells written, cleared, moved or read: erases, fills, edits,
    /// copies, scrolls, checksums, history copies, placement, and REP's
    /// copies tried or skipped.
    pub cells: u64,
    /// Row slots moved.
    pub slots: u64,
    /// History rows pushed, popped, read or searched, and rows walked by
    /// resize, reflow, row lookups, `pictures()` and the mark scans.
    pub rows: u64,
    /// Row text bytes copied, compacted, cloned or rescanned.
    pub text: u64,
    /// Sixel pixels painted or filled, and the bytes growth copies or
    /// zeroes, a pixel for each four.
    pub pixels: u64,
    /// Cells visited by style sweeps.
    pub styles: u64,
    /// New styles put in a table.
    pub inserts: u64,
    /// Style-table finds by hash.
    pub lookups: u64,
    /// Runs, rows, `ids` entries and sort steps of the link sweep.
    pub links: u64,
    /// Bytes recorded into [`crate::Prints`].
    pub prints: u64,
    /// Bytes the key decoder scans, and the bytes it copies.
    pub keys: u64,
}

thread_local! {
    /// The work this thread's calls into fux-vt have done.
    static WORK: RefCell<Work> = const { RefCell::new(Work::ZERO) };
}

/// Adds to this thread's counts what `count` adds: `work!` calls it.
pub(crate) fn add(count: impl FnOnce(&mut Work)) {
    WORK.with_borrow_mut(count);
}

/// `n` as units, the most there are if it does not fit.
pub(crate) fn units(n: impl TryInto<u64>) -> u64 {
    n.try_into().unwrap_or(u64::MAX)
}

impl Work {
    /// No work.
    pub const ZERO: Self = Self {
        bytes: 0,
        scanned: 0,
        cells: 0,
        slots: 0,
        rows: 0,
        text: 0,
        pixels: 0,
        styles: 0,
        inserts: 0,
        lookups: 0,
        links: 0,
        prints: 0,
        keys: 0,
    };

    /// The most weighted work one call may do: U, 50,000,000 units
    /// (threats.md 4.4, P14), in the twentieths [`Work::cost`] counts.
    pub const CEILING: u64 = 50_000_000 * 20;

    /// The work this thread has done in fux-vt so far.
    pub fn now() -> Self {
        WORK.with_borrow(|work| *work)
    }

    /// The work done since `earlier`, a [`Work::now`] taken before.
    pub fn since(self, earlier: Self) -> Self {
        let mut done = Self::ZERO;
        for ((slot, now), then) in done
            .counters_mut()
            .into_iter()
            .zip(self.counters())
            .zip(earlier.counters())
        {
            *slot.1 = now.1.saturating_sub(then.1);
        }
        done
    }

    /// Each counter, by name, in the order threats.md 4.1 lists them.
    pub fn counters(&self) -> [(&'static str, u64); 13] {
        [
            ("bytes", self.bytes),
            ("scanned", self.scanned),
            ("cells", self.cells),
            ("slots", self.slots),
            ("rows", self.rows),
            ("text", self.text),
            ("pixels", self.pixels),
            ("styles", self.styles),
            ("inserts", self.inserts),
            ("lookups", self.lookups),
            ("links", self.links),
            ("prints", self.prints),
            ("keys", self.keys),
        ]
    }

    /// Each counter, by name, to write.
    fn counters_mut(&mut self) -> [(&'static str, &mut u64); 13] {
        [
            ("bytes", &mut self.bytes),
            ("scanned", &mut self.scanned),
            ("cells", &mut self.cells),
            ("slots", &mut self.slots),
            ("rows", &mut self.rows),
            ("text", &mut self.text),
            ("pixels", &mut self.pixels),
            ("styles", &mut self.styles),
            ("inserts", &mut self.inserts),
            ("lookups", &mut self.lookups),
            ("links", &mut self.links),
            ("prints", &mut self.prints),
            ("keys", &mut self.keys),
        ]
    }

    /// Each counter's weight, in twentieths of a unit, so that every
    /// weight is whole: threats.md 4.4's frozen weights (P23), in ns per
    /// unit for this machine, times 20.
    pub const WEIGHTS: [(&'static str, u64); 13] = [
        ("bytes", 20),
        ("scanned", 10),
        ("cells", 5),
        ("slots", 20),
        ("rows", 100),
        ("text", 1),
        ("pixels", 5),
        ("styles", 10),
        ("inserts", 600),
        ("lookups", 300),
        ("links", 100),
        ("prints", 2),
        ("keys", 20),
    ];

    /// The weighted work W (threats.md 4.4): each counter times its weight,
    /// summed, in twentieths of a unit; at most [`Work::CEILING`] a call.
    pub fn cost(&self) -> u64 {
        (self.counters().into_iter().zip(Self::WEIGHTS))
            .map(|((_, n), (_, weight))| n.saturating_mul(weight))
            .fold(0, u64::saturating_add)
    }

    /// Counts input bytes the parser examined.
    pub(crate) fn bytes(&mut self, n: u64) {
        self.bytes = self.bytes.saturating_add(n);
    }
    /// Counts bytes the frame scan examined.
    pub(crate) fn scanned(&mut self, n: u64) {
        self.scanned = self.scanned.saturating_add(n);
    }
    /// Counts grid cells touched.
    pub(crate) fn cells(&mut self, n: u64) {
        self.cells = self.cells.saturating_add(n);
    }
    /// Counts row slots moved.
    pub(crate) fn slots(&mut self, n: u64) {
        self.slots = self.slots.saturating_add(n);
    }
    /// Counts rows pushed, popped, read, searched or walked.
    pub(crate) fn rows(&mut self, n: u64) {
        self.rows = self.rows.saturating_add(n);
    }
    /// Counts row text bytes copied or rescanned.
    pub(crate) fn text(&mut self, n: u64) {
        self.text = self.text.saturating_add(n);
    }
    /// Counts sixel pixels painted, filled, copied or zeroed.
    pub(crate) fn pixels(&mut self, n: u64) {
        self.pixels = self.pixels.saturating_add(n);
    }
    /// Counts cells a style sweep visited.
    pub(crate) fn styles(&mut self, n: u64) {
        self.styles = self.styles.saturating_add(n);
    }
    /// Counts new styles put in a table.
    pub(crate) fn inserts(&mut self, n: u64) {
        self.inserts = self.inserts.saturating_add(n);
    }
    /// Counts style-table finds.
    pub(crate) fn lookups(&mut self, n: u64) {
        self.lookups = self.lookups.saturating_add(n);
    }
    /// Counts the link sweep's steps.
    pub(crate) fn links(&mut self, n: u64) {
        self.links = self.links.saturating_add(n);
    }
    /// Counts bytes recorded into `Prints`.
    pub(crate) fn prints(&mut self, n: u64) {
        self.prints = self.prints.saturating_add(n);
    }
    /// Counts bytes the key decoder scanned or copied.
    pub(crate) fn keys(&mut self, n: u64) {
        self.keys = self.keys.saturating_add(n);
    }
}

#[cfg(test)]
mod tests {
    use super::Work;

    /// Each counter's method adds to its own counter alone, the cost
    /// weighs each by threats.md's weight, and `since` takes the
    /// difference: a counter left at 0, or added to another, would show.
    #[test]
    fn each_counter_counts_its_own_work_and_is_weighed_by_its_weight() {
        let before = Work::now();
        let adds: [fn(&mut Work, u64); 13] = [
            Work::bytes,
            Work::scanned,
            Work::cells,
            Work::slots,
            Work::rows,
            Work::text,
            Work::pixels,
            Work::styles,
            Work::inserts,
            Work::lookups,
            Work::links,
            Work::prints,
            Work::keys,
        ];
        for (i, add) in adds.iter().enumerate() {
            let n = u64::try_from(i).unwrap_or(0).saturating_add(1);
            super::add(|w| add(w, n));
        }
        let done = Work::now().since(before);
        for (i, ((name, n), (weighed, weight))) in
            done.counters().into_iter().zip(Work::WEIGHTS).enumerate()
        {
            assert_eq!(name, weighed);
            assert_eq!(n, u64::try_from(i).unwrap_or(0).saturating_add(1), "{name}");
            assert!(weight > 0, "{name}");
        }
        assert_eq!(Work::ZERO.cost(), 0);
        // 1 × 20 + 2 × 10 + 3 × 5 + 4 × 20 + 5 × 100 + 6 × 1 + 7 × 5 + 8 × 10
        // + 9 × 600 + 10 × 300 + 11 × 100 + 12 × 2 + 13 × 20.
        assert_eq!(done.cost(), 10_540);
        assert_eq!(Work::CEILING, 1_000_000_000);
    }
}
