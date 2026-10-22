//! The work fux-vt does, counted in deterministic units (THREATS.md 4.1),
//! with the `checks` feature: what a bound on the work per input byte is
//! stated in, and checked against by the bounds tests and the fuzz targets.
//!
//! The counts are kept per thread, so that every part of fux-vt counts into
//! them without being handed anything. A probe reads [`Work::now`] before
//! and after what it measures, on the thread that runs it, and takes the
//! difference ([`Work::since`]). Without the feature nothing is counted:
//! each count is a `work!` that expands to nothing.

use std::cell::RefCell;

/// The counters, each with what it counts, in the order THREATS.md 4.1
/// lists them: `Work`'s fields, and their names and weights.
macro_rules! counters {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// Work done, by kind, in units each counter states (THREATS.md 4.1).
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct Work {
            $($(#[$doc])* pub $name: u64,)*
        }

        impl Work {
            /// No work.
            pub const ZERO: Self = Self { $($name: 0,)* };

            /// Each counter's weight, in cells (`budget::weight`, THREATS.md
            /// 4.4).
            pub const WEIGHTS: [(&'static str, u64); COUNTERS] =
                [$((stringify!($name), crate::budget::weight::$name),)*];

            /// Each counter, by name, in the order THREATS.md 4.1 lists them.
            pub fn counters(&self) -> [(&'static str, u64); COUNTERS] {
                [$((stringify!($name), self.$name),)*]
            }

            /// The work done since `earlier`, a [`Work::now`] taken before.
            pub fn since(self, earlier: Self) -> Self {
                Self { $($name: self.$name.saturating_sub(earlier.$name),)* }
            }
        }
    };
}

/// How many counters there are.
const COUNTERS: usize = 16;

counters! {
    /// Input bytes examined by the parser's loop and its bulk scans.
    bytes,
    /// Bytes [`crate::FrameScan::end`] examines.
    scanned,
    /// Grid cells written, cleared, moved or read: erases, fills, edits,
    /// copies, scrolls, checksums, history copies, placement, and REP's
    /// copies tried or skipped.
    cells,
    /// Row slots moved.
    slots,
    /// History rows pushed, popped, read or searched, and rows walked by
    /// resize, reflow, row lookups, `pictures()` and the mark scans.
    rows,
    /// Row text bytes copied, compacted, cloned or rescanned.
    text,
    /// Sixel pixels painted or filled, and the bytes growth copies or
    /// zeroes, a pixel for each four.
    pixels,
    /// Cells visited by style sweeps.
    styles,
    /// New styles put in a table.
    inserts,
    /// Style-table finds by hash.
    lookups,
    /// Runs, rows, `ids` entries and sort steps of the link sweep.
    links,
    /// Bytes recorded into [`crate::Prints`].
    prints,
    /// Bytes the key decoder scans, and the bytes it copies.
    keys,
    /// Cells DECRQCRA's checksum reads, each decoded with its attributes:
    /// several times a cell another edit moves or fills costs.
    sums,
    /// Rows a scroll inside left and right margins copies a span of, each
    /// a few buffers made, whatever the span's width.
    spans,
    /// Cells a rectangle edit rewrites one at a time, each read and made
    /// again: DECCRA's copy, DECFRA's, DECERA's and DECSERA's fills, and
    /// DECCARA's and DECRARA's attributes.
    edits,
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
    /// The most work one call may do: U (THREATS.md 4.4), in the cells
    /// [`Work::cost`] counts.
    pub const CEILING: u64 = crate::budget::CEILING;

    /// The most work one call of `process_until_stop` does at a screen of
    /// `size`: U, or past it at a screen where one screen-wide sequence
    /// alone costs more, the most that sequence may cost
    /// (`budget::sequence_most`): the parser looks before such a sequence
    /// and leaves it for the next call when it would take the call past U,
    /// so it is carried out alone (THREATS.md 4.4).
    pub fn ceiling(size: crate::Size) -> u64 {
        Self::CEILING.max(crate::budget::sequence_most(size))
    }

    /// The work this thread has done in fux-vt so far.
    pub fn now() -> Self {
        WORK.with_borrow(|work| *work)
    }

    /// The weighted work W (THREATS.md 4.4): each counter times its weight,
    /// summed, in cells; at most [`Work::CEILING`] a call.
    pub fn cost(&self) -> u64 {
        (self.counters().into_iter().zip(Self::WEIGHTS))
            .map(|((_, n), (_, weight))| n.saturating_mul(weight))
            .fold(0, u64::saturating_add)
    }
}
