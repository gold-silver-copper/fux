//! The work one call may do before it stops (THREATS.md 4.4).
//!
//! A few bytes of input can ask for work in proportion to the screen, or
//! to history: a rectangle edit, a column shift, a scroll, REP, a search of
//! history's rows. At fux's largest sizes a 4 KiB read of such sequences
//! would do far more than the frozen per-call ceiling U (THREATS.md 4.4),
//! and fux reads every pane on one thread. So the work of those kinds is
//! spent here, weighed by its measured weights (`weight`), and
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
//! rows, and a line feed's copy of the row it scrolls into history. The
//! work at most a row's a sequence, or a few units a byte, is not, as the
//! call's length bounds it (THREATS.md 4.2): examining
//! bytes, printing, joining a cluster, EL, ICH and DCH, recording prints,
//! finding a style, decoding sixel. The spending is kept per thread, as
//! the `checks` counters are, so the grid and history spend without being
//! handed anything; a call compares what was spent before and after, so
//! calls of several parsers on one thread don't mix.
//!
//! The parser looks at it after each escape sequence or string it carries
//! out, after each line feed (LF, VT, FF) it carries out, whose scroll can
//! copy a row's text into history for one byte, and after a printed
//! glyph whose wrap scrolled inside left and right margins, which copies
//! the region's span of every row (`Parser::run`). Not after other text
//! or C0 controls: what they spend between two looks is at most one style
//! sweep, for the one new style an SGR before them gave the pen, spent
//! whole where it is done (THREATS.md B13), and a scroll into history of
//! the row a wrap leaves, which the row's own printed cells pay for.
//! One margin scroll can itself cost up to 3 cells and a cell's text for
//! every cell of the region, which at fux's largest pane is past U / 2
//! (THREATS.md B35, 4.4).

use std::cell::Cell;

thread_local! {
    /// What this thread's calls into fux-vt have spent, in twentieths of a
    /// unit.
    static SPENT: Cell<u64> = const { Cell::new(0) };
}

/// The most weighted work one call may do: U, 50,000,000 units (THREATS.md
/// 4.4), in twentieths of a unit; `Work::CEILING` under `checks`.
pub(crate) const CEILING: u64 = 50_000_000 * 20;

/// The most a call of `process_until_stop` spends before it stops, in
/// twentieths of a unit: half the ceiling U, so that the sequence that
/// crosses it, and the work no site spends, fit in the other half.
pub(crate) const LIMIT: u64 = CEILING / 2;

/// Each counter's weight, in twentieths of a unit: an upper bound of its
/// ns a unit on an i5-9600K, times 20, so that U units are at most 50 ms
/// there (THREATS.md 4.4). Each was measured over every bounds probe fed
/// 4 KiB a call in fux's build, from the calls of 5 ms or more whose work
/// is that kind's (each probe's 95th percentile), the kinds before it paid
/// at their weights, times 1.25; the kinds no such call needs more of keep
/// the weights first frozen for them, before any was measured. Named as the
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
    /// A grid cell written, cleared, moved or read: 1.39 ns, DECIC's,
    /// DECDC's, SL's and SR's at 4095x4096 (no more than 1.5, as the
    /// cells B05's placements and B33's and B34's copies count, at 0.4 to
    /// 0.65 ns, keep a call within U at it).
    pub(crate) const cells: u64 = 30;
    /// A row slot moved: 2.9 ns, B36's region scroll (a call of 1.2 ms).
    pub(crate) const slots: u64 = 73;
    /// A row pushed, popped, read, searched or walked.
    pub(crate) const rows: u64 = 100;
    /// A row text byte copied or rescanned: 1.06 ns, a scroll inside left
    /// and right margins over rows of 17-byte clusters at 1000x500, which
    /// spends each byte of the span's text twice, read and written
    /// (2.12 ns a byte read), past its cells and spans, times
    /// 1.2.
    pub(crate) const text: u64 = 26;
    /// A sixel pixel painted, filled, copied or zeroed: 0.96 ns, B04's.
    #[cfg(feature = "checks")]
    pub(crate) const pixels: u64 = 24;
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
    /// A cell a checksum read, past its cell and its text: 10.3 ns,
    /// DECRQCRA's over cells of 31-byte clusters at 4095x4096 (38.15 ns a
    /// cell in all), times 1.25.
    pub(crate) const sums: u64 = 258;
    /// A row a margin scroll copied a span of, past its cells and text:
    /// 96 ns, a wrap's scroll between 2-column margins at 4095x4096.
    pub(crate) const spans: u64 = 2408;
    /// A cell a rectangle edit rewrote, past its cells: 8.5 ns, DECCRA's
    /// at 4095x4096.
    pub(crate) const edits: u64 = 212;
}

/// The most one screen-wide sequence may spend at a screen of `size`, in
/// twentieths of a unit: each cell read and written, rewritten, summed and
/// its text copied (a cell's text is at most 49 bytes: 32 long and 17
/// short, THREATS.md B35), and each row's slot turned, walked and its span
/// copied, at the most each kind costs. The parser looks at it before such
/// a sequence (`Screen::holds_back`): one that would take the call past U, in a
/// call that spent already, is left for the next call, to be carried out
/// alone; a call of it alone does at most this.
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

/// Spends `n` units of a counter weighing `weight`.
pub(crate) fn spend(n: u64, weight: u64) {
    SPENT.with(|spent| spent.set(spent.get().saturating_add(n.saturating_mul(weight))));
}

/// What this thread has spent so far.
pub(crate) fn spent() -> u64 {
    SPENT.with(Cell::get)
}
