//! The work one call may do before it stops (THREATS.md 4).
//!
//! A few bytes of input can ask for work in proportion to the screen, to
//! history or to a picture (a rectangle edit, a scroll, REP, a sweep). fux
//! reads every pane on one thread, so all of fux-vt's work is spent here as
//! it is done, one unit an element visited (a cell, a row, a row slot, a
//! text byte, a pixel, a style or link entry), and
//! [`Parser::process_until_stop`](crate::Parser::process_until_stop) stops
//! at the next place it looks once [`LIMIT`] is spent
//! ([`Stop::Budget`](crate::Stop::Budget)). Spending is kept per thread,
//! and a call compares before and after, so several parsers on one thread
//! don't mix.

use std::cell::Cell;

thread_local! {
    /// What this thread's calls into fux-vt have spent.
    static SPENT: Cell<u64> = const { Cell::new(0) };
}

/// The most a call of `process_until_stop` spends before it stops, over
/// as many calls as it takes: past it, by at most the one indivisible
/// operation that crossed it (THREATS.md 4).
pub(crate) const LIMIT: u64 = 1_000_000;

/// Spends `n` units; past `u64`, as many as there are.
pub(crate) fn spend(n: impl TryInto<u64>) {
    let n = n.try_into().unwrap_or(u64::MAX);
    SPENT.with(|spent| spent.set(spent.get().saturating_add(n)));
}

pub(crate) fn spent() -> u64 {
    SPENT.with(Cell::get)
}

/// Sets what this thread has spent back to `n`: for the checker, whose work
/// is no call's (`check::unseen`).
#[cfg(feature = "checks")]
pub(crate) fn rewind(n: u64) {
    SPENT.with(|spent| spent.set(n));
}
