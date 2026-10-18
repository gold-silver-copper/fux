//! The work one call may do before it stops (threats.md P24, decision 6).
//!
//! A few bytes of input can ask for work in proportion to the screen, or
//! to history: a rectangle edit, a column shift, a scroll, REP, a search of
//! history's rows. At fux's largest sizes a 4 KiB read of such sequences
//! would do far more than the frozen per-call ceiling U (threats.md 4.4),
//! and fux reads every pane on one thread. So the work of those kinds is
//! spent here, weighed by threats.md's frozen weights, and
//! [`Parser::process_until_stop`](crate::Parser::process_until_stop) stops
//! at the first byte boundary once [`LIMIT`] is spent since its last such
//! stop, for the host to go on from there later
//! ([`Stop::Budget`](crate::Stop::Budget)).
//!
//! What is spent is the work one sequence can multiply, charged where the
//! sequence is carried out, from what it covers: ED's screen, a rectangle's
//! fill or erase, the region's rows a column edit or a scroll of more than
//! a line moves, REP's copies; and, where it is done, by the `spend!`
//! sites: DECCRA's copy, scrolls inside margins, the alignment pattern,
//! checksums, a screen made anew, the searches and sweeps of history's
//! rows. The work at most a row's a sequence, or a few units a byte, is
//! not, as the call's length bounds it (threats.md 4.2's rows that hold):
//! examining bytes, printing, joining a cluster, a line feed, EL, ICH and
//! DCH, recording prints, finding a style, decoding sixel. The spending is
//! kept per thread, as the `checks` counters are, so the grid and history
//! spend without being handed anything; a call compares what was spent
//! before and after, so calls of several parsers on one thread don't mix.
//! The parser looks at it after each escape sequence or string it carries
//! out (`Parser::run`), not after text or a C0 control, which spend
//! nothing.

use std::cell::Cell;

thread_local! {
    /// What this thread's calls into fux-vt have spent, in twentieths of a
    /// unit.
    static SPENT: Cell<u64> = const { Cell::new(0) };
}

/// The most a call of `process_until_stop` spends before it stops, in
/// twentieths of a unit: half the ceiling U (threats.md 4.4), so that the
/// sequence that crosses it, and the work no site spends, fit in the other
/// half.
pub(crate) const LIMIT: u64 = 25_000_000 * 20;

/// Each counter's weight, in twentieths of a unit: threats.md 4.4's frozen
/// weights (P23), in ns per unit for this machine, times 20. Named as the
/// counters are ([`crate::Work`]'s fields), for `spend!` to name them; the
/// kinds no site spends are kept for the `checks` counters alone.
#[expect(
    non_upper_case_globals,
    reason = "named as the counters are, for spend! to name them"
)]
pub(crate) mod weight {
    /// An input byte the parser examined.
    #[cfg(feature = "checks")]
    pub(crate) const bytes: u64 = 20;
    /// A byte the frame scan examined.
    #[cfg(feature = "checks")]
    pub(crate) const scanned: u64 = 10;
    /// A grid cell written, cleared, moved or read.
    pub(crate) const cells: u64 = 5;
    /// A row slot moved.
    pub(crate) const slots: u64 = 20;
    /// A row pushed, popped, read, searched or walked.
    pub(crate) const rows: u64 = 100;
    /// A row text byte copied or rescanned.
    pub(crate) const text: u64 = 1;
    /// A sixel pixel painted, filled, copied or zeroed.
    #[cfg(feature = "checks")]
    pub(crate) const pixels: u64 = 5;
    /// A cell a style sweep visited.
    pub(crate) const styles: u64 = 10;
    /// A new style put in a table.
    #[cfg(feature = "checks")]
    pub(crate) const inserts: u64 = 600;
    /// A style-table find.
    #[cfg(feature = "checks")]
    pub(crate) const lookups: u64 = 300;
    /// A step of the link sweep.
    pub(crate) const links: u64 = 100;
    /// A byte recorded into `Prints`.
    #[cfg(feature = "checks")]
    pub(crate) const prints: u64 = 2;
    /// A byte the key decoder scanned or copied.
    #[cfg(feature = "checks")]
    pub(crate) const keys: u64 = 20;
}

/// `n` as units, the most there are if it does not fit.
pub(crate) fn units(n: impl TryInto<u64>) -> u64 {
    n.try_into().unwrap_or(u64::MAX)
}

/// Spends `n` units of a counter weighing `weight`.
pub(crate) fn spend(n: u64, weight: u64) {
    SPENT.with(|spent| spent.set(spent.get().saturating_add(n.saturating_mul(weight))));
}

/// What this thread has spent so far.
pub(crate) fn spent() -> u64 {
    SPENT.with(Cell::get)
}
