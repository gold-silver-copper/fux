//! The work fux-vt does, counted in deterministic units (THREATS.md 4.1),
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

/// Work done, by kind, in units each counter states (THREATS.md 4.1).
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
    /// Cells DECRQCRA's checksum reads, each decoded with its attributes:
    /// several times a cell another edit moves or fills costs.
    pub sums: u64,
    /// Rows a scroll inside left and right margins copies a span of, each
    /// a few buffers made, whatever the span's width.
    pub spans: u64,
    /// Cells a rectangle edit rewrites one at a time, each read and made
    /// again: DECCRA's copy, DECFRA's, DECERA's and DECSERA's fills, and
    /// DECCARA's and DECRARA's attributes.
    pub edits: u64,
}

thread_local! {
    /// The work this thread's calls into fux-vt have done.
    static WORK: RefCell<Work> = const { RefCell::new(Work::ZERO) };
}

/// Adds to this thread's counts what `count` adds: `work!` calls it.
pub(crate) fn add(count: impl FnOnce(&mut Work)) {
    WORK.with_borrow_mut(count);
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
        sums: 0,
        spans: 0,
        edits: 0,
    };

    /// The most weighted work one call may do: U, 50,000,000 units
    /// (THREATS.md 4.4), in the twentieths [`Work::cost`] counts.
    pub const CEILING: u64 = crate::budget::CEILING;

    /// The most weighted work one call of `process_until_stop` does at a
    /// screen of `size`, in twentieths: U, or past it at a screen where
    /// one screen-wide sequence alone costs more, the most that sequence
    /// may cost (`budget::sequence_most`): the parser looks before such a
    /// sequence and leaves it for the next call when it would take the
    /// call past U, so it is carried out alone (THREATS.md 4.4).
    pub fn ceiling(size: crate::Size) -> u64 {
        Self::CEILING.max(crate::budget::sequence_most(size))
    }

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

    /// Each counter, by name, in the order THREATS.md 4.1 lists them.
    pub fn counters(&self) -> [(&'static str, u64); 16] {
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
            ("sums", self.sums),
            ("spans", self.spans),
            ("edits", self.edits),
        ]
    }

    /// Each counter, by name, to write.
    fn counters_mut(&mut self) -> [(&'static str, &mut u64); 16] {
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
            ("sums", &mut self.sums),
            ("spans", &mut self.spans),
            ("edits", &mut self.edits),
        ]
    }

    /// Each counter's weight, in twentieths of a unit, so that every
    /// weight is whole: an upper bound of its ns a unit on an i5-9600K,
    /// times 20 (`budget::weight`, THREATS.md 4.4).
    pub const WEIGHTS: [(&'static str, u64); 16] = {
        use crate::budget::weight::*;
        [
            ("bytes", bytes),
            ("scanned", scanned),
            ("cells", cells),
            ("slots", slots),
            ("rows", rows),
            ("text", text),
            ("pixels", pixels),
            ("styles", styles),
            ("inserts", inserts),
            ("lookups", lookups),
            ("links", links),
            ("prints", prints),
            ("keys", keys),
            ("sums", sums),
            ("spans", spans),
            ("edits", edits),
        ]
    };

    /// The weighted work W (THREATS.md 4.4): each counter times its weight,
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
    /// Counts cells a checksum read.
    pub(crate) fn sums(&mut self, n: u64) {
        self.sums = self.sums.saturating_add(n);
    }
    /// Counts rows a margin scroll copied a span of.
    pub(crate) fn spans(&mut self, n: u64) {
        self.spans = self.spans.saturating_add(n);
    }
    /// Counts cells a rectangle edit rewrote.
    pub(crate) fn edits(&mut self, n: u64) {
        self.edits = self.edits.saturating_add(n);
    }
}

#[cfg(test)]
mod tests {
    use super::Work;

    /// Each counter's method adds to its own counter alone, the cost
    /// weighs each by THREATS.md 4.4's weights, and `since` takes the
    /// difference: a counter left at 0, or added to another, would show.
    #[test]
    fn each_counter_counts_its_own_work_and_is_weighed_by_its_weight() {
        let before = Work::now();
        let adds: [fn(&mut Work, u64); 16] = [
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
            Work::sums,
            Work::spans,
            Work::edits,
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
        // Each counter i (from 1) counted i units: the cost is each times
        // its own weight, in the order the counters are named.
        let weighed = (Work::WEIGHTS.iter().zip(1u64..))
            .map(|((_, weight), i)| i.saturating_mul(*weight))
            .sum::<u64>();
        assert_eq!(done.cost(), weighed);
        assert_eq!(Work::CEILING, 1_000_000_000);
    }
}
