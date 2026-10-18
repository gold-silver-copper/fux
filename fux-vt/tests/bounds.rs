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

#[path = "bounds/probe.rs"]
mod probe;
#[path = "bounds/rows.rs"]
mod rows;

use probe::Scale;

/// The rows known to fail at BASE (`bounds/known-failing.txt`): a line
/// each, the row and the scale (`B19 10x`), then why. A fix takes its
/// rows off; a row on the list that passes fails, so that it does.
const KNOWN_FAILING: &str = include_str!("bounds/known-failing.txt");

/// Whether row `row` at `scale` is on the known-failing list.
fn known_failing(row: &str, scale: Scale) -> bool {
    (KNOWN_FAILING.lines())
        .filter(|line| !line.starts_with('#'))
        .any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some(row) && words.next() == Some(scale.name())
        })
}

/// Runs row `row` at `scale` at every chunking: it passes within its
/// bound, or fails as the known-failing list says it does.
fn judge(row: &str, scale: Scale) -> Result {
    let _one = one_at_a_time();
    let probe = (rows::ROWS.iter())
        .find(|(name, _)| *name == row)
        .map(|(_, probe)| *probe)
        .ok_or(format!("no row {row}"))?;
    match (rows::run(probe, scale), known_failing(row, scale)) {
        (Ok(()), false) | (Err(_), true) => Ok(()),
        (Err(found), false) => Err(format!("{row} at {}: {found}", scale.name()).into()),
        (Ok(()), true) => Err(format!(
            "{row} at {} is within its bound now: take it off tests/bounds/known-failing.txt",
            scale.name()
        )
        .into()),
    }
}

/// The list names rows that exist, at scales that exist.
#[test]
fn the_known_failing_list_names_rows_and_scales() {
    for line in KNOWN_FAILING
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut words = line.split_whitespace();
        let (row, scale) = (words.next(), words.next());
        assert!(
            rows::ROWS.iter().any(|(name, _)| Some(*name) == row),
            "{line}"
        );
        assert!(matches!(scale, Some("1x" | "10x" | "100x")), "{line}");
    }
}

/// The tests of one bound row: `bound!(b01, "B01", b01_1x, b01_10x,
/// b01_100x)` expands to three tests, `b01_1x` and `b01_10x`, which
/// `judge("B01", Scale::Once)` and `Scale::Ten`, and `b01_100x`, at
/// `Scale::Hundred`, ignored but at release.
macro_rules! bound {
    ($row:literal, $once:ident, $ten:ident, $hundred:ident) => {
        #[test]
        fn $once() -> Result {
            judge($row, Scale::Once)
        }
        #[test]
        fn $ten() -> Result {
            judge($row, Scale::Ten)
        }
        #[test]
        #[ignore = "100x: at release, under a 2 GiB memory cap"]
        fn $hundred() -> Result {
            judge($row, Scale::Hundred)
        }
    };
}

bound!("B01", b01_1x, b01_10x, b01_100x);
bound!("B02", b02_1x, b02_10x, b02_100x);
bound!("B03", b03_1x, b03_10x, b03_100x);
bound!("B04", b04_1x, b04_10x, b04_100x);
bound!("B05", b05_1x, b05_10x, b05_100x);
bound!("B06", b06_1x, b06_10x, b06_100x);
bound!("B07", b07_1x, b07_10x, b07_100x);
bound!("B08", b08_1x, b08_10x, b08_100x);
bound!("B09", b09_1x, b09_10x, b09_100x);
bound!("B10", b10_1x, b10_10x, b10_100x);
bound!("B11", b11_1x, b11_10x, b11_100x);
bound!("B12", b12_1x, b12_10x, b12_100x);
bound!("B13", b13_1x, b13_10x, b13_100x);
bound!("B14", b14_1x, b14_10x, b14_100x);
bound!("B15", b15_1x, b15_10x, b15_100x);
bound!("B16", b16_1x, b16_10x, b16_100x);
bound!("B17", b17_1x, b17_10x, b17_100x);
bound!("B18", b18_1x, b18_10x, b18_100x);
bound!("B19", b19_1x, b19_10x, b19_100x);
bound!("B20", b20_1x, b20_10x, b20_100x);
bound!("B21", b21_1x, b21_10x, b21_100x);
bound!("B22", b22_1x, b22_10x, b22_100x);
bound!("B23", b23_1x, b23_10x, b23_100x);
bound!("B24", b24_1x, b24_10x, b24_100x);
bound!("B25", b25_1x, b25_10x, b25_100x);
bound!("B26", b26_1x, b26_10x, b26_100x);
bound!("B27", b27_1x, b27_10x, b27_100x);
bound!("B28", b28_1x, b28_10x, b28_100x);
bound!("B29", b29_1x, b29_10x, b29_100x);
bound!("B30", b30_1x, b30_10x, b30_100x);
bound!("B31", b31_1x, b31_10x, b31_100x);
bound!("B32", b32_1x, b32_10x, b32_100x);
bound!("B33", b33_1x, b33_10x, b33_100x);
bound!("B34", b34_1x, b34_10x, b34_100x);
bound!("B34a", b34a_1x, b34a_10x, b34a_100x);
bound!("B35", b35_1x, b35_10x, b35_100x);
bound!("B36", b36_1x, b36_10x, b36_100x);
bound!("B37", b37_1x, b37_10x, b37_100x);
bound!("B38", b38_1x, b38_10x, b38_100x);
bound!("B39", b39_1x, b39_10x, b39_100x);
bound!("B40", b40_1x, b40_10x, b40_100x);
bound!("B41", b41_1x, b41_10x, b41_100x);
bound!("B42", b42_1x, b42_10x, b42_100x);
bound!("B43", b43_1x, b43_10x, b43_100x);
bound!("B44", b44_1x, b44_10x, b44_100x);
bound!("B45", b45_1x, b45_10x, b45_100x);
