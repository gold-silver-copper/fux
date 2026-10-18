//! The bounds suite (`threats.md` sections 2 to 4): each bound row's
//! reproducer fed at 1×, 10× and 100× and at several chunkings, its work
//! counted by fux-vt's counters ([`fux_vt::Work`], the `checks` feature)
//! and its memory by a counting allocator, independent of fux-vt's own
//! figures.
//!
//! The allocator is `peak_alloc`'s, this binary's `#[global_allocator]`: it
//! keeps the live bytes and their peak, which a probe resets, and counts a
//! `realloc` as its new block next to the old until the old is freed, so a
//! growth's transient shows. fux-vt has no `unsafe` of its own for it. Its
//! counts are the process's, so probes run one at a time (`one_at_a_time`).

use std::sync::{Mutex, MutexGuard, PoisonError};

use peak_alloc::PeakAlloc;

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Held by each probe while it runs: the allocator counts the process.
static ONE: Mutex<()> = Mutex::new(());

/// Waits for the other probes, and holds them off until dropped.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The heap a probe measures: the live bytes when it began, and its peak
/// since, which beginning reset.
struct Heap {
    base: usize,
}

impl Heap {
    /// Begins measuring: the bytes live now are the base, and the peak
    /// starts again from them.
    fn begin() -> Self {
        ALLOCATOR.reset_peak_usage();
        Self {
            base: ALLOCATOR.current_usage(),
        }
    }
    /// The bytes live now beyond the base.
    fn live(&self) -> usize {
        ALLOCATOR.current_usage().saturating_sub(self.base)
    }
    /// The most bytes live beyond the base at once since `begin`.
    fn peak(&self) -> usize {
        ALLOCATOR.peak_usage().saturating_sub(self.base)
    }
}

const MIB: usize = 1 << 20;

/// What the test harness's own threads may allocate or free while a probe
/// measures: the counts are the process's.
const SLACK: usize = 64 << 10;

/// What the bounds tests rely on the allocator for (`threats.md` 3.5): it
/// counts what is live, a reset peak starts again from what is live, and a
/// growth by `realloc` counts the old block and the new while both live.
#[test]
fn the_allocator_counts_live_bytes_a_reset_peak_and_a_growth_transient() -> Result {
    let _one = one_at_a_time();
    let heap = Heap::begin();
    let block = std::hint::black_box(vec![1u8; MIB]);
    assert!(heap.live() >= MIB - SLACK, "{} bytes live", heap.live());
    drop(block);
    assert!(
        heap.live() < SLACK,
        "{} bytes live after a free",
        heap.live()
    );
    assert!(heap.peak() >= MIB - SLACK, "a peak of {}", heap.peak());
    // A reset forgets the peak: it starts again from what is live.
    let heap = Heap::begin();
    assert!(
        heap.peak() < SLACK,
        "a peak of {} after a reset",
        heap.peak()
    );
    // 1 MiB grown to 3 MiB: both live at once, 4 MiB, until the copy ends.
    let mut grown = std::hint::black_box(vec![1u8; MIB]);
    grown.reserve_exact(2 * MIB);
    assert!(grown.capacity() >= 3 * MIB);
    assert!(
        heap.peak() >= 4 * MIB - SLACK,
        "a growth's peak of {}",
        heap.peak()
    );
    let live = heap.live();
    assert!(live < 3 * MIB + SLACK, "{live} live after the growth");
    drop(grown);
    Ok(())
}

/// A parser's own memory shows: one with 1 MiB of history, fed past it,
/// holds about that much: more than half of it, less than twice. Its
/// bound is B17's row's.
#[test]
fn a_parser_s_history_shows_in_the_counts() -> Result {
    let _one = one_at_a_time();
    let heap = Heap::begin();
    let mut parser = fux_vt::Parser::new(fux_vt::Size::new(24, 80)?, MIB)?;
    let line = [b'x'; 80];
    for _ in 0..20_000 {
        parser.process(&line)?;
        parser.process(b"\r\n")?;
    }
    let held = heap.live();
    assert!(held >= MIB / 2, "{held} bytes held for 1 MiB of history");
    assert!(heap.peak() <= 2 * MIB, "a peak of {}", heap.peak());
    drop(parser);
    Ok(())
}
