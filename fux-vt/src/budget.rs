//! The work one call may do before it stops (THREATS.md 4.4).
//!
//! A few bytes of input can ask for work in proportion to the screen or to
//! history (a rectangle edit, a scroll, REP, a history search). fux reads
//! every pane on one thread, so such work is spent here as it is done (the
//! `spend!` sites), and
//! [`Parser::process_until_stop`](crate::Parser::process_until_stop) stops
//! at the next byte boundary once [`LIMIT`] is spent
//! ([`Stop::Budget`](crate::Stop::Budget)). Work of at most a row a
//! sequence, or a few cells a byte, is not spent: the call's length bounds
//! it (THREATS.md 4.2). Spending is kept per thread, and a call compares
//! before and after, so several parsers on one thread don't mix.
//!
//! Work is counted in cells: a unit of each kind costs `weight` cells.

use std::cell::Cell;

thread_local! {
    /// What this thread's calls into fux-vt have spent, in cells.
    static SPENT: Cell<u64> = const { Cell::new(0) };
}

/// The most work one call may do, U (THREATS.md 4.4), in cells:
/// `Work::CEILING` under `checks`.
pub(crate) const CEILING: u64 = 32_000_000;

/// The most a call of `process_until_stop` spends before it stops: half
/// the ceiling U, so that the sequence that crosses it, and the work no
/// site spends, fit in the other half.
pub(crate) const LIMIT: u64 = CEILING / 2;

/// What a unit of each kind of work costs, in cells, named as
/// [`crate::Work`]'s fields; kinds no site spends exist for `checks` only.
#[expect(
    non_upper_case_globals,
    reason = "named as the counters are, for spend! to name them"
)]
pub(crate) mod weight {
    /// An input byte the parser examined.
    #[cfg(feature = "checks")]
    pub(crate) const bytes: u64 = 1;
    /// A byte the frame scan examined.
    #[cfg(feature = "checks")]
    pub(crate) const scanned: u64 = 1;
    /// A grid cell written, cleared, moved or read.
    pub(crate) const cells: u64 = 1;
    /// A row slot moved.
    pub(crate) const slots: u64 = 3;
    /// A row pushed, popped, read, searched or walked.
    pub(crate) const rows: u64 = 4;
    /// A row text byte copied or rescanned.
    pub(crate) const text: u64 = 1;
    /// A sixel pixel painted, filled, copied or zeroed.
    #[cfg(feature = "checks")]
    pub(crate) const pixels: u64 = 1;
    /// A cell a style sweep visited.
    pub(crate) const styles: u64 = 1;
    /// A new style put in a table.
    #[cfg(feature = "checks")]
    pub(crate) const inserts: u64 = 20;
    /// A style-table find.
    #[cfg(feature = "checks")]
    pub(crate) const lookups: u64 = 10;
    /// A step of the link sweep.
    pub(crate) const links: u64 = 4;
    /// A byte recorded into `Prints`.
    #[cfg(feature = "checks")]
    pub(crate) const prints: u64 = 1;
    /// A byte the key decoder scanned or copied.
    #[cfg(feature = "checks")]
    pub(crate) const keys: u64 = 1;
    /// A cell a checksum read, past its cell and its text.
    pub(crate) const sums: u64 = 9;
    /// A row a margin scroll copied a span of, past its cells and text.
    pub(crate) const spans: u64 = 81;
    /// A cell a rectangle edit rewrote, past its cells.
    pub(crate) const edits: u64 = 8;
}

/// The most one screen-wide sequence may spend at a screen of `size`, in
/// cells: each cell read and written, rewritten, summed and its text
/// copied (a cell's text is at most 49 bytes, THREATS.md B35), and each
/// row's slot turned, walked and its span copied. A sequence that would
/// take a call that already spent past U is left for the next call
/// (`Screen::holds_back`), so a call of it alone does at most this.
pub(crate) fn sequence_most(size: crate::Size) -> u64 {
    let (rows, cells) = (units(size.rows()), units(size.cells()));
    let cell = weight::cells
        .saturating_mul(2)
        .saturating_add(weight::edits)
        .saturating_add(weight::sums)
        .saturating_add(weight::text.saturating_mul(49));
    let row = weight::slots
        .saturating_add(weight::rows)
        .saturating_add(weight::spans);
    cells
        .saturating_mul(cell)
        .saturating_add(rows.saturating_mul(row))
}

/// `n` as units, the most there are if it does not fit.
pub(crate) fn units(n: impl TryInto<u64>) -> u64 {
    n.try_into().unwrap_or(u64::MAX)
}

pub(crate) fn spend(n: u64, weight: u64) {
    SPENT.with(|spent| spent.set(spent.get().saturating_add(n.saturating_mul(weight))));
}

pub(crate) fn spent() -> u64 {
    SPENT.with(Cell::get)
}
