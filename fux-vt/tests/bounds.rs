//! The bounds suite (`threats.md` sections 2 to 4): each bound row's
//! reproducer fed at 1×, 10× and 100× and at several chunkings, its work
//! counted by fux-vt's counters ([`fux_vt::Work`], the `checks` feature)
//! and its memory by a counting allocator, independent of fux-vt's own
//! figures.
//!
//! The allocator is `fux-vt-peak`'s, this binary's `#[global_allocator]`:
//! it keeps the live bytes and their peak, which a probe resets, for each
//! thread apart, so the test harness's own threads (a "running for over 60
//! seconds" message, a finished test's report) never count in a probe's.
//! It counts a `realloc` as its new block next to the old until the old is
//! freed, so a growth's transient shows. fux-vt has no `unsafe` of its own
//! for it. Probes still run one at a time (`one_at_a_time`), each holding
//! what its row allows, so that the binary's memory stays one probe's.

use std::sync::{Mutex, MutexGuard, PoisonError};

use fux_vt_peak::ThreadPeak;

#[global_allocator]
static ALLOCATOR: ThreadPeak = ThreadPeak;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Held by each probe while it runs, so that one probe holds memory at a
/// time.
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
        ALLOCATOR.reset_peak();
        Self {
            base: ALLOCATOR.live(),
        }
    }
    /// The bytes live now beyond the base.
    fn live(&self) -> usize {
        ALLOCATOR.live().saturating_sub(self.base)
    }
    /// The most bytes live beyond the base at once since `begin`.
    fn peak(&self) -> usize {
        ALLOCATOR.peak().saturating_sub(self.base)
    }
}

const MIB: usize = 1 << 20;

/// What a probe's own thread may allocate besides what it measures, as the
/// test harness's code on it does.
const SLACK: usize = 64 << 10;

/// What the bounds tests rely on the allocator for (`threats.md` 3.5): it
/// counts what is live, a reset peak starts again from what is live, a
/// growth by `realloc` counts the old block and the new while both live,
/// and what another thread holds meanwhile is not the probe's.
#[test]
fn the_allocator_counts_live_bytes_a_reset_peak_and_a_growth_transient() -> Result {
    let _one = one_at_a_time();
    let heap = Heap::begin();
    let block = std::hint::black_box(vec![1u8; MIB]);
    assert!(heap.live() >= MIB - SLACK, "{} bytes live", heap.live());
    let other = std::thread::spawn(|| std::hint::black_box(vec![1u8; 4 * MIB]).len());
    assert_eq!(other.join().ok(), Some(4 * MIB));
    assert!(
        heap.peak() < MIB + SLACK,
        "another thread's 4 MiB counted: a peak of {}",
        heap.peak()
    );
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

#[path = "bounds/colours.rs"]
mod colours;
#[path = "bounds/probe.rs"]
mod probe;
#[path = "bounds/rows.rs"]
mod rows;

use probe::Scale;

/// The rows known to fail at BASE (`bounds/known-failing.txt`): a line
/// each, the row and the scale (`B19 10x`), then why. A fix takes its
/// rows off; a row on the list that passes fails, so that it does.
const KNOWN_FAILING: &str = include_str!("bounds/known-failing.txt");

/// What row `row` at `scale` is known to fail with, if it is on the
/// known-failing list: the part of its error the line quotes in backticks.
fn known_failing(row: &str, scale: Scale) -> Option<&'static str> {
    (KNOWN_FAILING.lines())
        .filter(|line| !line.starts_with('#'))
        .find(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some(row) && words.next() == Some(scale.name())
        })
        .map(|line| line.split('`').nth(1).unwrap_or_default())
}

/// Whether `found`, a row's failures one a chunking, are as `known` says:
/// at every chunking, each saying it.
fn fails_as_known(found: &[String], known: &str) -> bool {
    !known.is_empty()
        && found.len() == probe::CHUNKS.len()
        && found.iter().all(|f| f.contains(known))
}

/// A row known to fail fails so at every chunking, each on its own: one
/// chunking's error saying it four times, the others failing otherwise,
/// or passing, is not as known (FA-RE-B-10).
#[test]
fn a_known_failure_is_matched_at_each_chunking() {
    let known = "past its bound";
    let each: Vec<String> = probe::CHUNKS
        .iter()
        .map(|c| format!("at {c}: {known} 9"))
        .collect();
    assert!(fails_as_known(&each, known));
    let once = [format!("at 1: {known} {known} {known} {known}")];
    assert!(!fails_as_known(&once, known));
    let mut other = each.clone();
    if let Some(last) = other.last_mut() {
        *last = "whole: a heap peak of 9".to_string();
    }
    assert!(!fails_as_known(&other, known));
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
        (Ok(()), None) => Ok(()),
        (Err(found), Some(known)) if fails_as_known(&found, known) => Ok(()),
        (Err(found), Some(known)) => Err(format!(
            "{row} at {} fails otherwise than known (`{known}`): {}",
            scale.name(),
            found.join("; ")
        )
        .into()),
        (Err(found), None) => {
            Err(format!("{row} at {}: {}", scale.name(), found.join("; ")).into())
        }
        (Ok(()), Some(_)) => Err(format!(
            "{row} at {} is within its bound now: take it off tests/bounds/known-failing.txt",
            scale.name()
        )
        .into()),
    }
}

/// A bound naming a counter there is none of fails, rather than passing
/// for counting nothing (FA-HAR-D11).
#[test]
fn a_bound_naming_no_counter_fails() -> Result {
    let _one = one_at_a_time();
    let setup = probe::Setup::defaults().sized(2, 2).history(0);
    let bound = probe::Bound {
        work: &[("cell", 1, 0, 0)],
        heap: usize::MAX,
    };
    let mut probe = probe::Probe::new(&setup, bound, 1, 1)?;
    let fed = probe.feed(b"x");
    assert!(fed.is_err_and(|e| e.contains("no counter is named cell")));
    Ok(())
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
        assert!(
            line.split('`')
                .nth(1)
                .is_some_and(|known| !known.is_empty()),
            "{line}"
        );
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

/// The per-call work budget (`threats.md` P24, decision 6): at fux's
/// largest pane, 4095×4096, each class of screen-wide function stops a
/// call at the budget, no call does more than its ceiling (U, or one such
/// function alone, `Work::ceiling`, decision 950), and the
/// screen after every call equals that of one call given it all.
mod budget {
    use fux_vt::{Parser, Size, Stop, Work};

    use super::{Result, one_at_a_time};

    /// A host that hears nothing.
    struct Deaf;
    impl fux_vt::Sink for Deaf {}

    /// The screen filled with `X`, then `each` 24 times: each one moves or
    /// fills millions of cells, a few bytes apiece.
    fn input(each: &str) -> Vec<u8> {
        let mut input = b"\x1b[88;1;1;4095;4096$x".to_vec();
        for _ in 0..24 {
            input.extend_from_slice(each.as_bytes());
        }
        input
    }

    /// Feeds `input` whole to one parser, and to another by
    /// `process_until_stop`, given the rest again after each stop: each
    /// call under U, the budget stopping some, and the two screens alike.
    fn budgeted(each: &str) -> Result {
        let _one = one_at_a_time();
        let input = input(each);
        let size = Size::new(4095, 4096)?;
        let mut whole = Parser::new(size, 0)?;
        whole.process(&input)?;
        let mut parser = Parser::new(size, 0)?;
        let (mut rest, mut stops) = (&input[..], 0u32);
        while !rest.is_empty() {
            let before = Work::now();
            let stop = parser.process_until_stop(rest, &mut Deaf)?;
            let done = Work::now().since(before);
            assert!(
                done.cost() <= Work::ceiling(size),
                "{each:?}: a call did {} units, past its ceiling: {done:?}",
                done.cost() / 20
            );
            match stop {
                None => break,
                Some((taken, Stop::Budget)) if taken > 0 => {
                    stops = stops.saturating_add(1);
                    rest = rest.get(taken..).unwrap_or_default();
                }
                Some(other) => return Err(format!("{each:?}: stopped as {other:?}").into()),
            }
        }
        assert!(stops >= 2, "{each:?}: {stops} stops");
        let (a, b) = (whole.screen(), parser.screen());
        assert_eq!(a.cursor_position(), b.cursor_position(), "{each:?}");
        assert!(
            a.rows().zip(b.rows()).all(|(a, b)| a.cells().eq(b.cells())),
            "{each:?}: the screens differ"
        );
        Ok(())
    }

    /// A REP inside left and right margins whose wraps scroll past the
    /// budget stops where the budget is spent, its copies left made by
    /// the next call, and text in DEC Special Graphics after it stops so
    /// too: the screen is that of one call given it all (FA-RE-F-5, F-8,
    /// F-11).
    #[test]
    fn a_rep_inside_margins_stops_at_the_budget_and_goes_on() -> Result {
        let _one = one_at_a_time();
        let mut input = b"\x1b[88;1;1;4095;4096$x\x1b[?69h\x1b[1;4090s\x1b[4095;1H".to_vec();
        for _ in 0..2 {
            input.extend_from_slice(b"x\x1b[65535b\x1b(0");
            input.extend(std::iter::repeat_n(b'q', 20_000));
            input.extend_from_slice(b"\x1b(B");
        }
        let size = Size::new(4095, 4096)?;
        let mut whole = Parser::new(size, 0)?;
        whole.process(&input)?;
        let mut parser = Parser::new(size, 0)?;
        let (mut rest, mut stops) = (&input[..], 0u32);
        while !rest.is_empty() {
            let before = Work::now();
            let stop = parser.process_until_stop(rest, &mut Deaf)?;
            let done = Work::now().since(before);
            assert!(
                done.cost() <= Work::ceiling(size),
                "a call did {} units, past its ceiling",
                done.cost() / 20
            );
            match stop {
                None => break,
                Some((taken, Stop::Budget)) => {
                    stops = stops.saturating_add(1);
                    rest = rest.get(taken..).unwrap_or_default();
                }
                Some(other) => return Err(format!("stopped as {other:?}").into()),
            }
        }
        assert!(stops >= 2, "{stops} stops");
        let (a, b) = (whole.screen(), parser.screen());
        assert_eq!(a.cursor_position(), b.cursor_position());
        assert!(
            a.rows().zip(b.rows()).all(|(a, b)| a.cells().eq(b.cells())),
            "the screens differ"
        );
        Ok(())
    }

    /// How many of the REPs of `glyph` in `input` a call carries out
    /// before the budget stops it, on fux's default screen and history,
    /// where none of a REP's copies is skipped: each is printed, and
    /// spent.
    fn reps_before_the_budget(glyph: &str) -> Result<usize> {
        let each = format!("{glyph}\x1b[65535b");
        let input: String = std::iter::repeat_n(each.as_str(), 4000).collect();
        let mut parser = Parser::new(Size::new(50, 200)?, 8 << 20)?;
        match parser.process_until_stop(input.as_bytes(), &mut Deaf)? {
            Some((taken, Stop::Budget)) => Ok(taken.checked_div(each.len()).unwrap_or(0)),
            other => Err(format!("{glyph}: stopped as {other:?}").into()),
        }
    }

    /// A wide glyph's copy writes two cells, and REP spends it as two: the
    /// budget stops a call after half as many REPs of it.
    #[test]
    fn rep_spends_a_copy_by_its_width() -> Result {
        let _one = one_at_a_time();
        let (narrow, wide) = (reps_before_the_budget("é")?, reps_before_the_budget("一")?);
        assert!(
            wide.abs_diff(narrow / 2) <= 1,
            "{wide} REPs of a wide glyph before the budget, {narrow} of a narrow one"
        );
        Ok(())
    }

    #[test]
    fn column_edits_stop_at_the_budget() -> Result {
        for each in ["\x1b[9'}", "\x1b[9'~", "\x1b[9 @", "\x1b[9 A"] {
            budgeted(each)?;
        }
        Ok(())
    }

    #[test]
    fn rectangle_copies_stop_at_the_budget() -> Result {
        budgeted("\x1b[1;1;4095;4090;1;1;7;1$v")
    }

    #[test]
    fn rectangle_fills_and_erases_stop_at_the_budget() -> Result {
        for each in [
            "\x1b[89;1;2;4095;4096$x",
            "\x1b[1;2;4095;4096$z",
            "\x1b[1;2;4095;4096${",
        ] {
            budgeted(each)?;
        }
        Ok(())
    }
}
