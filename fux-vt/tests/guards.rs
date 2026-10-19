//! Tests that kill the frozen guard mutants no other test kills
//! (`threats.md` 5.2, `fux-vt/compare/mutants-guards.txt`): each names
//! the guard's row and the mutant, and holds the guard's limit at the
//! input where the mutant differs from it.

use std::ops::Range;

use fux_vt::{Attributes, CLUSTER_CAPACITY, CellRef, Cells, PRINT_RUNS, Parser, RowId, Size, Stop};

#[path = "corpus/lines.rs"]
mod lines;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A host that hears nothing.
struct Deaf;
impl fux_vt::Sink for Deaf {}

/// `count` prints of `width` bytes on a 24×80 screen, each at its own
/// spot a column apart from the next, so that each is a run of its own:
/// the spots row by row, then again, `a`s and `b`s by turns so that each
/// changes its cells; then `last` at the next spot.
fn spots(count: usize, width: usize, last: &str) -> Vec<u8> {
    let step = width.saturating_add(1);
    let per_row = 80usize.checked_div(step).unwrap_or(1);
    let screen = per_row.saturating_mul(24);
    let at = |i: usize| {
        let spot = i.checked_rem(screen).unwrap_or(0);
        let row = spot.checked_div(per_row).unwrap_or(0).saturating_add(1);
        let col = (spot.checked_rem(per_row).unwrap_or(0))
            .saturating_mul(step)
            .saturating_add(1);
        format!("\x1b[{row};{col}H")
    };
    let mut input = Vec::new();
    for i in 0..count {
        let round = i.checked_div(screen).unwrap_or(0);
        let letter = if round.is_multiple_of(2) { "a" } else { "b" };
        input.extend_from_slice(at(i).as_bytes());
        input.extend_from_slice(times(letter, width).as_bytes());
    }
    input.extend_from_slice(at(count).as_bytes());
    input.extend_from_slice(last.as_bytes());
    input
}

/// `text` `n` times over.
fn times(text: &str, n: usize) -> String {
    std::iter::repeat_n(text, n).collect()
}

/// The text bytes `prints` holds.
fn text_bytes(prints: &fux_vt::Prints) -> usize {
    prints.iter().map(|p| p.text.len()).sum()
}

/// B10, `Prints::over_cap`'s text `>` (lib.rs:556:46, `>=`): text that
/// ends exactly at the cap is kept, not lost.
#[test]
fn prints_keep_text_that_ends_exactly_at_the_cap() -> Result {
    let mut p = Parser::new(Size::new(41, 100)?, 0)?;
    p.record_prints(true);
    let mut input = Vec::new();
    for letter in ["a", "b", "c", "d"] {
        input.extend_from_slice(b"\x1b[H");
        input.extend_from_slice(times(letter, 41 * 100).as_bytes());
    }
    p.process(&input)?;
    let prints = p.take_prints();
    assert_eq!(prints.cap(), 4 * 41 * 100);
    assert!(!prints.lost());
    assert_eq!(
        (prints.iter().count(), text_bytes(&prints)),
        (4 * 41, 16_400)
    );
    Ok(())
}

/// B10, `Prints::over_cap`'s runs `>` (lib.rs:556:76, `>=`): the run
/// that makes `PRINT_RUNS` is kept, though the text must grow for it.
#[test]
fn prints_keep_the_last_run_under_the_cap_when_their_text_grows() -> Result {
    let mut p = Parser::new(Size::new(24, 80)?, 0)?;
    p.record_prints(true);
    p.process(&spots(PRINT_RUNS - 1, 2, "xxx"))?;
    let prints = p.take_prints();
    assert!(!prints.lost());
    assert_eq!(
        (prints.iter().count(), text_bytes(&prints)),
        (PRINT_RUNS, 2 * (PRINT_RUNS - 1) + 3)
    );
    Ok(())
}

/// B10, `Prints::add`'s `runs.len() != runs.capacity()` (lib.rs:622:32,
/// `==`): a run past `PRINT_RUNS` is lost, though its text has room.
#[test]
fn prints_lose_a_run_past_the_cap_though_their_text_has_room() -> Result {
    let mut p = Parser::new(Size::new(24, 80)?, 0)?;
    p.record_prints(true);
    p.process(&spots(PRINT_RUNS, 3, "xxx"))?;
    #[cfg(feature = "checks")]
    {
        let report = p.check();
        assert!(report.faults_of("prints.cap").next().is_none(), "{report}");
    }
    let prints = p.take_prints();
    assert!(prints.lost());
    assert_eq!(
        (prints.iter().count(), text_bytes(&prints)),
        (PRINT_RUNS, 3 * PRINT_RUNS)
    );
    Ok(())
}

/// B10, `Prints::heap` (lib.rs:592:9, with 0 and with 1): it is the
/// room its text and runs took, and none before anything is printed.
#[test]
fn prints_heap_is_the_room_of_their_text_and_runs() -> Result {
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.record_prints(true);
    p.process(b"hello")?;
    let prints = p.take_prints();
    let run = size_of::<(RowId, Range<u16>, Range<usize>)>();
    assert_eq!(prints.heap(), 256 + 16 * run);
    assert_eq!(p.take_prints().heap(), 0);
    Ok(())
}

/// B35, `budget::LIMIT`'s `25_000_000 * 20` (budget.rs:43:42, `+`): a
/// call stops at the budget after 25 M units of work, not before.
#[test]
fn a_call_stops_at_the_budget_after_25_million_units() -> Result {
    let pair = "a\x1b[65535b";
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    assert_eq!(
        p.process_until_stop(times(pair, 100).as_bytes(), &mut Deaf)?,
        None
    );
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    match p.process_until_stop(times(pair, 2000).as_bytes(), &mut Deaf)? {
        Some((taken, Stop::Budget)) => assert!(
            (1500 * pair.len()..2000 * pair.len()).contains(&taken),
            "stopped after {taken} bytes"
        ),
        other => return Err(format!("stopped as {other:?}").into()),
    }
    Ok(())
}

/// A cell holding `text`, narrow, in the default style.
fn cell(text: &str) -> CellRef<'_> {
    CellRef::new(text, false, Attributes::default())
}

/// B39, `Cells::room_for`'s `len > MAX_CELLS` (cell.rs:748:16, `>` to `>=`): exactly 64 Mi cells are made.
/// Allocates 64 Mi cells of 24 bytes, about 1.5 GiB, for a moment.
#[test]
fn cells_at_the_most_are_made() -> Result {
    let most = 64 << 20;
    let made = Cells::new(most)?;
    assert_eq!(made.len(), most);
    Ok(())
}

/// B39, `Cells::resize`'s `len < was` (cell.rs:854:16, `<` to `<=`): a resize to the same length stores no text again.
#[test]
fn a_resize_to_the_same_length_keeps_the_overwritten_text() -> Result {
    let (first, second) = (times("a", 20), times("b", 20));
    let mut cells = Cells::new(1)?;
    assert!(cells.set(0, cell(&first)));
    assert!(cells.set(0, cell(&second)));
    assert_eq!(cells.text_len(), 40);
    cells.resize(1, CellRef::default())?;
    assert_eq!(cells.text_len(), 40);
    assert_eq!(cells.get(0).map(|c| c.contents()), Some(second.as_str()));
    Ok(())
}

/// B30, `store_long`'s `long + text > limit` (compact.rs:578:60, `>` to `>=`): text that ends exactly at the limit is stored without compacting.
#[test]
fn a_long_cluster_that_ends_at_the_limit_does_not_compact() -> Result {
    let limit = Cells::text_limit(1);
    assert_eq!(limit, 32 + CLUSTER_CAPACITY);
    let (first, second) = (times("a", 32), times("b", CLUSTER_CAPACITY));
    let mut cells = Cells::new(1)?;
    assert!(cells.set(0, cell(&first)));
    assert!(cells.set(0, cell(&second)));
    assert_eq!(cells.text_len(), limit);
    assert_eq!(cells.get(0).map(|c| c.contents()), Some(second.as_str()));
    Ok(())
}

/// Runs `f` on a thread of its own and fails if it takes past 60 seconds
/// (it takes well under one): a guard whose mutant loops forever then
/// fails a test rather than hanging the run.
fn in_time<T: Send + 'static>(
    f: impl FnOnce() -> std::result::Result<T, String> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(done) => Ok(done?),
        Err(_) => Err("it did not finish in 60 seconds".into()),
    }
}

/// B17, `History::trim_over`'s `&&` (history.rs:366:37, `||`),
/// `make_room`'s row cap (history.rs:560:35, `<`) and `Ring::advance`
/// (ring.rs:115-117: its body, `>=`, `&&`, `!`): history over its budget
/// lets its oldest rows go, a block's at a time, never all it holds, and
/// in time.
#[test]
fn history_over_its_budget_lets_only_its_oldest_rows_go() -> Result {
    let lens = in_time(|| {
        let mut p = Parser::new(Size::new(24, 80).map_err(|e| e.to_string())?, 256 << 10)
            .map_err(|e| e.to_string())?;
        let mut lens = Vec::new();
        for i in 0..20_000 {
            p.process(format!("{i}\r\n").as_bytes())
                .map_err(|e| e.to_string())?;
            lens.push(p.screen().history_len());
        }
        Ok(lens)
    })?;
    // 20,000 rows of up to 5 cells are past 256 KiB at 24 bytes a row and
    // 8 a cell, so rows went; a block is 1/64 of the budget's cells, so a
    // line lets go far fewer than half of the rows held.
    let most = lens.iter().copied().max().unwrap_or(0);
    assert!((1_000..20_000).contains(&most), "at most {most} rows");
    for (line, (before, after)) in lens.iter().zip(lens.iter().skip(1)).enumerate() {
        assert!(
            *after >= before.checked_div(2).unwrap_or(0),
            "line {line}: {before} rows held, then {after}"
        );
    }
    Ok(())
}

/// B33, `Screen::repeat`'s ASCII run loop (screen.rs:2208:25, `>=`): REP
/// makes its copies and returns.
#[test]
fn repeat_makes_its_ascii_copies_and_returns() -> Result {
    let shown = in_time(|| {
        let mut p = Parser::new(Size::new(2, 10).map_err(|e| e.to_string())?, 0)
            .map_err(|e| e.to_string())?;
        p.process(b"a\x1b[5bz").map_err(|e| e.to_string())?;
        Ok(lines::lines(&p))
    })?;
    assert_eq!(shown, ["aaaaaaz", ""]);
    Ok(())
}
