//! Tests that kill the frozen guard mutants no other test kills
//! (`threats.md` 5.2, `fux-vt/compare/mutants-guards.txt`): each names
//! the guard's row and the mutant, and holds the guard's limit at the
//! input where the mutant differs from it.

use std::ops::Range;
use std::time::Instant;

use fux_vt::keys::decode::{Decoder, Input};
use fux_vt::{
    Attributes, CLUSTER_CAPACITY, CellRef, Cells, Feature, Graphics, Options, PRINT_RUNS, Parser,
    RowId, Size, Stop, URI_LIMIT,
};

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

/// B10, `Prints::over_cap`'s text `>` (lib.rs, `>=`): text that
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

/// B10, `Prints::over_cap`'s runs `>` (lib.rs, `>=`): the run
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

/// B10, `Prints::add`'s `runs.len() != runs.capacity()` (lib.rs,
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

/// B10, `Prints::heap` (lib.rs, with 0 and with 1): it is the
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

/// B35, the work budget (budget.rs, `CEILING`'s `50_000_000 * 20` with
/// `+`): a call stops once it has spent half the ceiling, and not before.
/// What fux-vt's counters charge a DECALN of the whole 50x200 screen
/// (`Work::cost`) is at least what the budget spends on it, so the stop
/// comes after ceil(`CEILING` / 2 / that) of them, or later.
/// The frozen ceiling U (threats.md 4.4, P14), in twentieths of a unit.
#[cfg(feature = "checks")]
const CEILING: u64 = 50_000_000 * 20;

#[cfg(feature = "checks")]
#[test]
fn a_call_stops_at_half_the_ceiling_and_not_before() -> Result {
    let align = b"\x1b#8";
    let size = Size::new(50, 200)?;
    let mut p = Parser::new(size, 0)?;
    let before = fux_vt::Work::now();
    p.process(align)?;
    let cost = fux_vt::Work::now().since(before).cost().max(1);
    // U, the frozen ceiling (threats.md 4.4, P14): 50 M units, in twentieths.
    let least = (CEILING / 2).div_ceil(cost);
    let input = times_bytes(align, usize::try_from(least.saturating_mul(4))?);
    let mut p = Parser::new(size, 0)?;
    match p.process_until_stop(&input, &mut Deaf)? {
        Some((taken, Stop::Budget)) => {
            let done = u64::try_from(taken.checked_div(align.len()).unwrap_or(0))?;
            assert!(done >= least, "stopped after {done} of at least {least}");
        }
        other => return Err(format!("stopped as {other:?}").into()),
    }
    Ok(())
}

/// A cell holding `text`, narrow, in the default style.
fn cell(text: &str) -> CellRef<'_> {
    CellRef::new(text, false, Attributes::default())
}

/// B39, `Cells::room_for`'s `len > MAX_CELLS` (cell.rs, `>` to `>=`): exactly 64 Mi cells are made.
/// Allocates 64 Mi cells of 24 bytes, about 1.5 GiB, for a moment: run
/// by CI's step of its own and by the nightly mutation job.
#[test]
#[ignore = "makes 64 Mi cells, about 1.5 GiB: CI runs it in a step of its own"]
fn cells_at_the_most_are_made() -> Result {
    let most = 64 << 20;
    let made = Cells::new(most)?;
    assert_eq!(made.len(), most);
    Ok(())
}

/// B30, `store_long`'s `long + text > limit` (compact.rs, `>` to `>=`): text that ends exactly at the limit is stored without compacting.
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

/// B17, `History::trim_over`'s `&&` (history.rs, `||`),
/// `make_room`'s row cap (history.rs, `<`) and `Ring::advance`
/// (ring.rs: its body, `>=`, `&&`, `!`): history over its budget
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

/// B33, `Screen::repeat`'s ASCII run loop (screen.rs, `>=`): REP
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

/// The bounds tests' counting allocator: the live bytes of each thread apart.
#[global_allocator]
static ALLOCATOR: fux_vt_peak::ThreadPeak = fux_vt_peak::ThreadPeak;

/// Types `row` on a one-row screen and ends its line with history's next
/// allocation refused (`fux_vt::refuse_next`): whether the line feed needed
/// room for the row's cells (a block made or grown), which the refusal then
/// cost the row, as if there were no history. The row is then typed and
/// ended again, with room.
#[cfg(feature = "checks")]
fn needs_room(p: &mut Parser, row: &str) -> Result<bool> {
    p.process(row.as_bytes())?;
    let (held, refused) = (p.screen().history_len(), p.refused());
    fux_vt::refuse_next(1);
    let failed = p.process(b"\r\n").is_err();
    fux_vt::refuse_next(0);
    let needed = p.refused() > refused;
    let kept = p.screen().history_len() > held;
    if failed != needed || kept == needed {
        return Err(format!("failed {failed}, refused {needed}, kept {kept}").into());
    }
    if needed {
        p.process(row.as_bytes())?;
        p.process(b"\r\n")?;
    }
    Ok(needed)
}

/// B17, `History::fits` (history.rs `||` to `&&`, :542:25 `<=` to `>`), `block_cells` (:337:9 with 0 and with 1) and `make_room`'s half block (:589:40 `>` to `>=`, :589:48 `/` to `%`): the newest block doubles up to a block, a row half a block long starts a whole block, and a row that fits needs no room.
#[cfg(feature = "checks")]
#[test]
fn history_blocks_double_to_a_block_and_rows_fill_their_room() -> Result {
    // 1 MiB of history: a block is 4,096 cells. One-cell rows: the first
    // block is the first row's 1 cell, then it doubles (2, 4, 8) as rows
    // overflow it; rows 4, 6, 7 and 8 fit in the room it has.
    let mut p = Parser::new(Size::new(1, 2048)?, 1 << 20)?;
    let made = (0..9)
        .map(|_| needs_room(&mut p, "a"))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        made,
        [true, true, true, false, true, false, false, false, true]
    );
    // Rows of 2,048 cells, half a block: the first block is the first
    // row's, doubled to a block for the second; the third starts a block of
    // a block's cells (it is not longer than half a block), which the
    // fourth fits in; and so on.
    let mut p = Parser::new(Size::new(1, 2048)?, 1 << 20)?;
    let half = times("x", 2048);
    let made = (0..6)
        .map(|_| needs_room(&mut p, &half))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(made, [true, true, true, false, true, false]);
    Ok(())
}

/// B17/B38, `make_room`'s row cap `rows.len() > most` (history.rs, `>` to `>=` and to `==`): a history whose budget holds them keeps exactly `MAX_ROWS` (1,048,576) rows.
#[test]
fn history_keeps_the_most_rows_a_grid_holds() -> Result {
    // Empty rows cost 24 bytes each in the ring and nothing else: 2^20 of
    // them take 24 MiB, within 32 MiB. The ring's chunks are 512 rows; the
    // cap lets rows go only when a chunk fills with more than MAX_ROWS less
    // a chunk kept, which first happens at the row after the 2^20th.
    let mut p = Parser::new(Size::new(1, 10)?, 32 << 20)?;
    p.process(times("\n", 1 << 20).as_bytes())?;
    assert_eq!(p.screen().history_len(), 1 << 20);
    Ok(())
}

/// B17/B39, `make_room`'s cell cap `!rows.is_empty() && (… || room_cells > most)` (history.rs delete `!`, :573:62 `||` to `&&`, :573:81 `>` to `==` and to `>=`): rows of 8,192 cells, each a block of its own, are kept to exactly `MAX_CELLS` (64 Mi cells), 8,192 rows. Holds 512 MiB.
#[test]
fn history_keeps_the_most_cells_a_grid_holds() -> Result {
    let held = in_time(|| {
        let size = Size::new(1, 8192).map_err(|e| e.to_string())?;
        let mut p = Parser::new(size, 1 << 30).map_err(|e| e.to_string())?;
        // A red background: each line feed brings in a row of 8,192 red
        // blanks (bce), all of which history keeps. The first row, made
        // before, is empty; 8,201 red rows follow it.
        p.process(b"\x1b[41m").map_err(|e| e.to_string())?;
        p.process(times("\n", 8202).as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(p.screen().history_len())
    })?;
    assert_eq!(held, 8192);
    Ok(())
}

/// B17, `History::trim_over`'s `rows.is_empty() && over()` (history.rs, `&&` to `||`): history over its budget only by a row's text lets the row go and keeps the storage it made, which then counts against the rows to come.
#[test]
fn history_over_its_budget_by_a_row_s_text_keeps_its_storage() -> Result {
    // 512 bytes: the ring's chunks are a row each (24 bytes). A row of 8
    // clusters of 101 bytes each: its 8 cells make a block of 8 (64 bytes),
    // its 808 bytes of text go in the texts map (3 buckets of 66 bytes,
    // 198); far over, so the row goes, and with its text gone the 286
    // bytes left fit.
    let mut p = Parser::new(Size::new(1, 40)?, 512)?;
    let cluster = format!("e{}", times("\u{301}", 50));
    p.process(times(&cluster, 8).as_bytes())?;
    p.process(b"\r\n")?;
    assert_eq!(p.screen().history_len(), 0);
    // Empty rows then: 24 bytes each beside the block's 64 and the map's
    // 198, so 10 fit in 512, not the 21 that 512 holds with nothing else.
    p.process(times("\n", 100).as_bytes())?;
    assert_eq!(p.screen().history_len(), 10);
    Ok(())
}

/// B17, `maps_changed`'s `capacity > most_room(len)` (history.rs, `>` to `>=`, `==` and `<`) and `maps_bytes` (:329:9, with 0): a map's room counts in the budget, and one holding exactly 4 × its entries and 64 keeps it.
#[test]
fn history_counts_a_map_s_room_until_it_is_past_four_times_its_entries_and_64() -> Result {
    // 4 KiB: the ring's chunks are 8 rows (192 bytes). 40 double-width
    // empty rows: their renditions take room for 64 (16 bytes each, 1,024).
    // As the rows go, room for 64 is no more than 4 × 0 + 64, so it stays:
    // empty rows after them fit 24 bytes each in the 3,072 left, at most
    // 128 rows, at least a chunk less one (121), not the 161 to 168 that
    // 4 KiB holds with the map's room given back or not counted.
    let mut p = Parser::new(Size::new(1, 10)?, 4096)?;
    p.process(times("\x1b#6\n", 40).as_bytes())?;
    p.process(times("\n", 1000).as_bytes())?;
    let held = p.screen().history_len();
    assert!((121..=128).contains(&held), "{held} rows held");
    Ok(())
}

/// B17, `History::maps_changed` (history.rs, with `()`; and :794:61 `>` to `==`): a map holding room past 4 × its entries and 64 gives it back as its rows go.
#[cfg(feature = "checks")]
#[test]
fn history_gives_back_a_map_s_room_as_its_rows_go() -> Result {
    // 300 double-width rows: their renditions hold room for 512. Then
    // empty rows, past the 64 KiB budget's 2,730 or so: every double one
    // goes, and the room for 512 with none kept is past 4 × 0 + 64.
    let mut p = Parser::new(Size::new(1, 10)?, 64 << 10)?;
    p.process(times("\x1b#6\n", 300).as_bytes())?;
    p.process(times("\n", 5000).as_bytes())?;
    assert!(p.screen().history_len() < 5000);
    let report = p.check();
    assert!(
        report.faults_of("history.capacities").next().is_none(),
        "{report}"
    );
    Ok(())
}

/// B17, `Ring::let_first_go`'s `start * 2 >= full.len()` (history/ring.rs, `>=` to `<`): the chunks a ring lets go are dropped, so a history held at its size holds no more memory however many rows pass through it.
#[test]
fn history_s_ring_holds_no_more_as_rows_pass_through_it() -> Result {
    // 48 bytes: two empty rows of a chunk each; every line feed lets the
    // oldest chunk go.
    let mut p = Parser::new(Size::new(1, 10)?, 48)?;
    let (few, many) = (times("\n", 1000), times("\n", 100_000));
    p.process(few.as_bytes())?;
    let before = ALLOCATOR.live();
    p.process(many.as_bytes())?;
    let grown = ALLOCATOR.live().saturating_sub(before);
    assert_eq!(p.screen().history_len(), 2);
    assert!(grown < 4096, "{grown} bytes more held after 100,000 rows");
    Ok(())
}

/// `styles` new direct backgrounds, `red` and the number's two low bytes,
/// each on an `x` over the cell under the cursor: each a new style of the
/// grid's table, of which only the last stays in a cell.
fn backgrounds(red: u8, styles: Range<u32>) -> Vec<u8> {
    let mut out = Vec::new();
    for i in styles {
        let [blue, green, _, _] = i.to_le_bytes();
        out.extend_from_slice(format!("\x1b[48;2;{red};{green};{blue}mx\x1b[D").as_bytes());
    }
    out
}

/// The cells style sweeps visited (`Work`'s `styles`) since `before`.
#[cfg(feature = "checks")]
fn swept_since(before: fux_vt::Work) -> u64 {
    let done = fux_vt::Work::now().since(before);
    (done.counters().into_iter())
        .find(|(name, _)| *name == "styles")
        .map_or(0, |(_, n)| n)
}

/// The cells style sweeps visited while `p` read `input`.
#[cfg(feature = "checks")]
fn sweeping(p: &mut Parser, input: &[u8]) -> Result<u64> {
    let before = fux_vt::Work::now();
    p.process(input)?;
    Ok(swept_since(before))
}

/// B11, `Styles::bytes` (style.rs, with 0 and with 1), `Styles::full` (style.rs, with false; :164:51, `||` to `&&`) and `table_bytes`'s buckets (style.rs, `/` to `%` and to `*`): a table grown 86,016 bytes past the screen's share takes that much of history's room.
#[test]
fn a_style_table_past_the_screens_share_takes_history_room() -> Result {
    const BUDGET: usize = 100_000;
    const CHARGED: usize = 86_016;
    let mut p = Parser::new(Size::new(10, 80)?, BUDGET)?;
    p.process(&backgrounds(0, 0..3600))?;
    p.process(b"\x1b[0m")?;
    p.process(times(&format!("{}\r\n", times("x", 80)), 200).as_bytes())?;
    // A row of 80 cells takes at least its 24-byte entry and 8 bytes a
    // cell, and the rows fit in what the table leaves of the budget.
    let rows = p.screen().history_len();
    assert!(
        rows.saturating_mul(24 + 80 * 8) <= BUDGET - CHARGED,
        "{rows} rows kept"
    );
    Ok(())
}

/// B11, `Grid::make_style_room`'s `after > budget` (grid.rs, `>` to `>=`) and `Styles::full` (style.rs, with true; :164:26 and :164:72, `==` to `!=`): a table that grows to exactly history's budget past the share grows without a sweep, and a new style it has room for sweeps nothing, even with the budget lowered under what the table takes.
#[cfg(feature = "checks")]
#[test]
fn a_style_table_that_fits_the_budget_exactly_grows_unswept() -> Result {
    let mut p = Parser::new(Size::new(10, 80)?, 86_016)?;
    assert_eq!(sweeping(&mut p, &backgrounds(0, 0..3600))?, 0);
    p.set_history_bytes(86_015);
    assert_eq!(sweeping(&mut p, &backgrounds(0, 3600..3601))?, 0);
    Ok(())
}

/// B11, `Styles::bytes_after_insert`'s `len < room` (style.rs, `<` to `==` and to `<=`) and `screen_share`'s `* 7 / 8` (style.rs, `/` to `%`): a table whose growth history can't hold (86,016 past the share, of 70,000) is swept before it grows, never before, and history stays within its budget.
#[cfg(feature = "checks")]
#[test]
fn a_style_table_history_cannot_hold_is_swept_before_it_grows() -> Result {
    let mut p = Parser::new(Size::new(10, 80)?, 70_000)?;
    assert_eq!(sweeping(&mut p, &backgrounds(0, 0..3584))?, 0);
    assert!(sweeping(&mut p, &backgrounds(0, 3584..3600))? > 0);
    let report = p.check();
    assert!(
        report.faults_of("history.budget").next().is_none(),
        "{report}"
    );
    Ok(())
}

/// B11, `Grid::find_table_style`'s `grows || swept` (grid.rs, `||` to `&&`) and `Styles::retain`'s `places.capacity() > room` (style.rs, `>` to `==`): a sweep that keeps one style of a table grown to room 14,336 makes its map anew at room 7,168, and charges history for that (28,728 past the share), not for the table before it (315,392).
#[cfg(feature = "checks")]
#[test]
fn a_sweep_charges_history_for_the_table_it_leaves() -> Result {
    let mut p = Parser::new(Size::new(10, 80)?, 2 << 20)?;
    // History's cells raise the floor past 8,000, so 7,300 styles come
    // with no sweep, and the map grows past 7,168 to room 14,336.
    p.process(times(&format!("{}\r\n", times("x", 80)), 810).as_bytes())?;
    assert!(p.screen().history_len() >= 800);
    p.process(b"\x1b[H")?;
    assert_eq!(sweeping(&mut p, &backgrounds(0, 0..7300))?, 0);
    // A budget under what the table takes lets history's rows go, and the
    // floor with them: the next new style sweeps.
    let before = fux_vt::Work::now();
    p.set_history_bytes(100_000);
    p.process(&backgrounds(1, 0..1))?;
    assert!(swept_since(before) > 0);
    let report = p.check();
    assert!(
        report.faults_of("history.budget").next().is_none(),
        "{report}"
    );
    Ok(())
}

/// B11 and B13, `Screen::change_attributes`' memo reset on a sweep (screen.rs, `!=` to `==`): a style DECCARA mapped before a sweep in its midst is mapped again after it, by its new number.
#[test]
fn deccara_maps_a_style_again_after_a_sweep_in_its_midst() -> Result {
    let mut p = Parser::new(Size::new(4, 10)?, 1 << 20)?;
    // A and B are the table's first styles; 4,093 more on row 2 make it
    // 4,095. DECCARA's bold A is the 4,096th, and its bold B sweeps first.
    p.process(b"\x1b[48;2;0;0;1mA\x1b[48;2;0;0;2mB\x1b[48;2;0;0;1mA\x1b[2;1H")?;
    p.process(&backgrounds(1, 0..4093))?;
    p.process(b"\x1b[1;1;1;3;1$r")?;
    let s = p.screen();
    let at = |col: u16| s.cell(0, col).map(|c| c.attributes());
    for col in 0..3u16 {
        assert!(at(col).is_some_and(|a| a.bold()), "column {col}");
    }
    assert_eq!(at(0), at(2));
    assert_ne!(at(0), at(1));
    #[cfg(feature = "checks")]
    {
        let report = p.check();
        assert!(
            report.faults_of("grid.style_numbers").next().is_none(),
            "{report}"
        );
    }
    Ok(())
}

/// B11 and B13, `Screen::change_attributes`' memo slot `kept % 16` (screen.rs, `%` to `/`): DECCARA over three styles, the last the default, makes each bold.
#[test]
fn deccara_maps_each_of_three_styles() -> Result {
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process(b"\x1b[31ma\x1b[32mb\x1b[0mc\x1b[1;1;1;3;1$r")?;
    for col in 0..3u16 {
        let bold = p
            .screen()
            .cell(0, col)
            .is_some_and(|c| c.attributes().bold());
        assert!(bold, "column {col}");
    }
    Ok(())
}

/// How many more bytes this thread holds after `f` runs than before.
fn grown(f: impl FnOnce() -> Result) -> Result<usize> {
    let before = ALLOCATOR.live();
    f()?;
    Ok(ALLOCATOR.live().saturating_sub(before))
}

/// B05, `cell_too_small`'s `cell.1 < 6` (parser.rs, `==` and `>`): a host cell wide enough but under 6 pixels high is taken as 6 high.
#[test]
fn a_host_cell_under_a_band_high_is_taken_as_a_band_high() -> Result {
    let options = Options::new().with(Feature::Pictures);
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, options)?;
    p.set_host_graphics(Graphics {
        cell: Some((3, 5)),
        sixel: true,
    });
    // 6 pixels wide and 12 high, two bands.
    p.process(b"\x1bPq\"1;1#1;2;100;0;0!6~-!6~\x1b\\")?;
    let cells: Vec<_> = p.screen().pictures().map(|s| s.cell).collect();
    assert_eq!(cells, [(3, 6)]);
    Ok(())
}

/// B19, `FrameScan::sequence`'s `b'[' if plain` (parser.rs, `true`): `ESC ( [` ends at its `[`, so the `?2026l` after it is text, no DECRST, and the frame goes on.
#[test]
fn an_escape_with_an_intermediate_before_a_bracket_ends_no_frame() -> Result {
    let mut p = Parser::new(Size::new(4, 10)?, 0)?;
    assert_eq!(
        p.process_until_stop(b"\x1b[?2026h", &mut Deaf)?,
        Some((8, Stop::Begun))
    );
    let held = b"\x1b([?2026l";
    assert_eq!(p.frame_scan().end(held), None);
    assert_eq!(p.frame_scan().end(b"\x1b[?2026l"), Some(8));
    // The parser reads it so too: no frame ends in it.
    assert_eq!(p.process_until_stop(held, &mut Deaf)?, None);
    Ok(())
}

/// An OSC string of `len` bytes, its number and `;` among them, that no
/// feature reads, ended by BEL.
fn osc_of(len: usize) -> Vec<u8> {
    let mut osc = b"\x1b]7777;".to_vec();
    osc.extend(std::iter::repeat_n(b'a', len.saturating_sub(5)));
    osc.push(0x07);
    osc
}

/// B20, `osc_room`'s `need > capacity` (parser.rs, `>=`) and `grow_osc`'s doubling to 16 KiB (parser.rs, `<<` to `>>`; parser.rs, `>` to `<` and to `>=`): the OSC buffer takes the room a string needs, doubling up to 16 KiB, and none for a string that fits.
#[test]
fn the_osc_buffer_takes_the_room_a_string_needs_up_to_16_kib() -> Result {
    let options = Options::new().with(Feature::Events);
    let (small, sixteen) = (osc_of(4096), osc_of(16 << 10));
    let mut p = Parser::with_options(Size::new(2, 10)?, 0, options)?;
    p.process(b"x\r")?;
    let first = grown(|| Ok(p.process(&small)?))?;
    assert!(
        (4096..8192).contains(&first),
        "{first} bytes kept for a 4 KiB string"
    );
    let again = grown(|| Ok(p.process(&small)?))?;
    assert!(again < 1024, "{again} bytes more for a string that fits");
    let mut q = Parser::with_options(Size::new(2, 10)?, 0, options)?;
    q.process(b"x\r")?;
    let most = grown(|| Ok(q.process(&sixteen)?))?;
    assert!(
        ((16 << 10)..(32 << 10)).contains(&most),
        "{most} bytes kept for a 16 KiB string"
    );
    Ok(())
}

/// B23, `PASTE_LIMIT`'s `64 * 1024` (decode.rs, `+`): a paste of 64 KiB is delivered whole, and one a byte longer is too long.
#[test]
fn a_paste_of_64_kib_is_kept_and_one_byte_more_is_not() {
    let pasted = |n: usize| {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', n));
        bytes.extend_from_slice(b"\x1b[201~");
        let mut out = Vec::new();
        Decoder::default().bytes(&bytes, &mut out);
        out
    };
    assert!(matches!(
        pasted(64 << 10).as_slice(),
        [Input::Paste(text)] if text.len() == 64 << 10
    ));
    assert_eq!(pasted((64 << 10) + 1), [Input::PasteTooLong]);
}

/// B24, `HELD`'s `DCS_LIMIT + 2` (decode.rs, `-` and `*`), `held_first`'s `bytes.len() > HELD` (decode.rs, `>=`) and its `&&` (decode.rs, `||`): a long read gives what is held the 259 bytes that decide it, reads the rest where it lies, and returns.
#[cfg(feature = "checks")]
#[test]
fn a_long_read_gives_what_is_held_259_bytes_and_returns() -> Result {
    let (held, idle) = in_time(|| {
        let mut out = Vec::new();
        // An Escape held, then a read of 1,000 `a`s.
        let mut d = Decoder::default();
        d.bytes(b"\x1b", &mut out);
        let before = fux_vt::Work::now();
        d.bytes(&[b'a'; 1000], &mut out);
        let held = fux_vt::Work::now().since(before).keys;
        // Nothing held, then 516 `a`s and an Escape: 258 bytes are left
        // after the first 259, and they leave the Escape held.
        let mut d = Decoder::default();
        let mut read = vec![b'a'; 516];
        read.push(0x1b);
        let before = fux_vt::Work::now();
        d.bytes(&read, &mut out);
        let idle = fux_vt::Work::now().since(before).keys;
        Ok((held, idle))
    })?;
    // Each byte decoded once, after the 259 copied once behind the Escape.
    assert_eq!(held, 1001 + 259);
    // Each byte decoded once, and the Escape left held copied once.
    assert_eq!(idle, 517 + 1);
    Ok(())
}

/// B24, `Decoder::may_hold` (decode.rs, `true`): a string being dropped, with no ESC that may end it, holds nothing, so a long read is not cut up for it: the drop looks at it once, and what follows the string's end is decoded.
#[cfg(feature = "checks")]
#[test]
fn a_long_read_while_a_string_is_dropped_is_read_whole() {
    let mut d = Decoder::default();
    let mut out = Vec::new();
    // An OSC answer past its 128 bytes: dropped to its end as it comes.
    let mut long = b"\x1b]1".to_vec();
    long.extend([b'a'; 200]);
    d.bytes(&long, &mut out);
    let mut read = vec![b'a'; 10];
    read.push(0x07);
    read.extend([b'b'; 289]);
    let before = fux_vt::Work::now();
    d.bytes(&read, &mut out);
    let keys = fux_vt::Work::now().since(before).keys;
    assert_eq!(out.len(), 289);
    assert_eq!(keys, 300 + 289);
}

/// B25, `answer`'s `body.len() > limit` (decode.rs, `>=`): a DCS answer of exactly 256 bytes, its window's inclusive end, decodes alike whole and in a read that ends with its string.
#[test]
fn a_dcs_answer_at_its_limit_decodes_alike_whole_and_split() -> Result {
    let mut payload = b"1+r536d756c78=".to_vec();
    payload.resize(256, b'0');
    let answer = [b"\x1bP".as_slice(), &payload, b"\x1b\\"].concat();
    let decoded = |reads: &[&[u8]]| {
        let mut d = Decoder::default();
        d.expect(Instant::now());
        let mut out = Vec::new();
        for read in reads {
            d.bytes(read, &mut out);
        }
        out
    };
    let (string, st) = answer.split_at_checked(258).ok_or("no ST")?;
    let whole = decoded(&[answer.as_slice()]);
    assert_eq!(decoded(&[string, st]), whole);
    assert!(
        !whole.iter().any(|i| matches!(i, Input::Key(_))),
        "{whole:?}"
    );
    Ok(())
}

/// B25, `long_answer`'s `body.len() > DCS_LIMIT` (decode.rs, `>=`; decode.rs, the guard with `true`): a `CSI ?` answer past its 64-byte window is held up to 256 bytes, then dropped as it comes, to its final byte, nothing typed.
#[test]
fn a_long_csi_answer_is_held_up_to_256_bytes() -> Result {
    for (len, holds) in [(65usize, true), (256, true), (257, false)] {
        let mut read = b"\x1b[?".to_vec();
        read.resize(len.saturating_add(2), b'1');
        let mut d = Decoder::default();
        let mut out = Vec::new();
        let kept = grown(|| {
            d.bytes(&read, &mut out);
            Ok(())
        })?;
        if holds {
            assert!(kept >= read.len(), "{len} bytes: {kept} held");
        } else {
            assert!(kept < 64, "{len} bytes: {kept} held");
        }
        d.bytes(b"cx", &mut out);
        assert_eq!(out.len(), 1, "{len} bytes: {out:?}");
        assert!(matches!(out.first(), Some(Input::Key(_))), "{out:?}");
    }
    Ok(())
}

/// A parser of `rows` by `cols`, with no history, that keeps pictures,
/// drawn in cells of 8 by 16 pixels.
fn picturing(rows: u16, cols: u16) -> Result<Parser> {
    let options = Options::new().with(Feature::Pictures);
    let mut p = Parser::with_options(Size::new(rows, cols)?, 0, options)?;
    p.set_host_graphics(Graphics {
        cell: Some((8, 16)),
        sixel: true,
    });
    Ok(p)
}

/// The width and height of each picture the screen shows.
fn picture_sizes(p: &Parser) -> Vec<(u32, u32)> {
    (p.screen().pictures())
        .map(|shown| (shown.picture.width(), shown.picture.height()))
        .collect()
}

/// B03, `Decoder::sixel`'s "some of it is kept" test (sixel.rs and 361:49, `<` to `<=`): a sixel wholly past the columns, or the rows, a picture may keep grows it by nothing.
#[test]
fn a_sixel_past_what_a_picture_may_keep_grows_it_by_nothing() -> Result {
    // A band one row tall across all 4,096 columns, at 1:1; then a full
    // sixel at column 4,096, past them: the picture stays one row tall.
    let mut p = picturing(24, 80)?;
    p.process(b"\x1bPq\"1;1!4096@~\x1b\\")?;
    assert_eq!(picture_sizes(&p), [(4096, 1)]);
    // 4,096 columns keep at most 8,847,360 / 4,096 = 2,160 rows: a sixel
    // on the band at row 2,160 is past them, and the picture stays one
    // row tall.
    let mut p = picturing(24, 80)?;
    let mut input = b"\x1bPq\"1;1!4096@".to_vec();
    input.extend(times("-", 360).as_bytes());
    input.extend(b"@\x1b\\");
    p.process(&input)?;
    assert_eq!(picture_sizes(&p), [(4096, 1)]);
    Ok(())
}

/// B03, `next_side`'s clamp (sixel.rs, `>` to `==`): a side doubled past `MAX_SIDE` is clamped to it, and the decoder's room stays within what a picture may keep.
#[cfg(feature = "checks")]
#[test]
fn a_doubled_side_past_the_most_is_clamped() -> Result {
    // A side 3,000 wide that must grow by one doubles to 6,000, past 4,096.
    let mut p = picturing(24, 80)?;
    p.process(b"\x1bPq!3000@@")?;
    let report = p.check();
    assert!(
        report.faults_of("parser.sixel").next().is_none(),
        "{report}"
    );
    p.process(b"\x1b\\")?;
    assert_eq!(picture_sizes(&p), [(3001, 2)]);
    Ok(())
}

/// What a grid's links may cost (`link.rs` `LINK_BYTES`), and what a link
/// with no `id` costs beyond its URI: its `Arc<str>`'s two counts.
const LINKS_HELD: usize = 4 << 20;
const LINK_COST: usize = 2 * size_of::<usize>();
/// The cost of a link to the longest URI kept, and how many fit.
const LONGEST: usize = URI_LIMIT + LINK_COST;
const FIT: usize = LINKS_HELD / LONGEST;
/// The URI whose link, after `FIT` of the longest, fills the bound exactly.
const LAST_URI: usize = LINKS_HELD - FIT * LONGEST - LINK_COST;

// The last URI is long enough for its `http://`.
const _: () = assert!(LAST_URI >= 8);

/// `OSC 8` opening a link to a URI `len` bytes long, an `x` printed in it.
fn link_of(len: usize) -> Vec<u8> {
    let mut link = b"\x1b]8;;http://".to_vec();
    link.extend(times("a", len.saturating_sub(7)).as_bytes());
    link.extend(b"\x07x");
    link
}

/// Whether each cell of the screen has a link, row by row.
fn linked_cells(p: &Parser) -> Vec<bool> {
    let size = p.screen().size();
    let mut linked = Vec::new();
    for row in 0..size.rows() {
        for col in 0..size.cols() {
            linked.push(p.screen().link(row, col).is_some());
        }
    }
    linked
}

/// `a` trues, then `b` falses, then `c` trues, then falses to `len`.
fn runs_of(a: usize, b: usize, c: usize, len: usize) -> Vec<bool> {
    let mut cells: Vec<bool> = std::iter::repeat_n(true, a)
        .chain(std::iter::repeat_n(false, b))
        .chain(std::iter::repeat_n(true, c))
        .collect();
    cells.resize(len, false);
    cells
}

/// B14, `Links::insert`'s limit (link.rs, `>` to `>=`), and B16, `Links::may_make_room` (link.rs, with true) and `Links::stall` (link.rs, with ()): links fill 4 MiB exactly, then 256 are refused without making room, and the 257th makes it.
#[test]
fn links_fill_their_bound_exactly_then_stall_for_256() -> Result {
    // `FIT` links to the longest URI and one to `LAST_URI` hold exactly
    // 4 MiB; the next is refused, with every link on the screen, and making room
    // stalls; the screen erased, the next 256 are refused without trying
    // and the 257th makes room.
    let mut p = Parser::with_options(
        Size::new(25, 80)?,
        0,
        Options::new().with(Feature::Hyperlinks),
    )?;
    let mut input = times_bytes(&link_of(URI_LIMIT), FIT);
    input.extend(link_of(LAST_URI));
    input.extend(link_of(URI_LIMIT));
    input.extend(b"\x1b]8;;\x07");
    p.process(&input)?;
    assert_eq!(linked_cells(&p), runs_of(FIT + 1, 0, 0, 2000));
    let mut input = b"\x1b[2J\x1b[H".to_vec();
    input.extend(times_bytes(&link_of(URI_LIMIT), 257));
    input.extend(b"\x1b]8;;\x07");
    p.process(&input)?;
    assert_eq!(linked_cells(&p), runs_of(0, 256, 1, 2000));
    Ok(())
}

/// `bytes` `n` times over.
fn times_bytes(bytes: &[u8], n: usize) -> Vec<u8> {
    std::iter::repeat_n(bytes, n).flatten().copied().collect()
}

/// B29, `Screen::after_prompt` (shell.rs, with None): a late `C` with no line submitted starts the output on the row after its prompt's line, not at the cursor.
#[test]
fn a_late_start_follows_its_prompt_s_line() -> Result {
    let marks = Options::new().with(Feature::PromptMarks);
    let mut p = Parser::with_options(Size::new(8, 20)?, 0, marks)?;
    p.process(b"\x1b]133;A\x07$ oops\r\nnot found\r\n")?;
    p.process(b"\x1b]133;C;late\x07\x1b]133;D;127\x07")?;
    let (start, _) = p.screen().commands().last_output.ok_or("no output")?;
    let row = p.screen().window().row(1).map(|r| r.id());
    assert_eq!((Some(start.row), start.col), (row, 0));
    Ok(())
}

/// B33 and B34, `Screen::repeat`'s `per_row` filter (screen.rs, `>` to `>=`): REP of a glyph wider than the screen makes no copy and spends none of the call's budget.
#[test]
fn a_repeat_with_no_copy_to_a_row_spends_nothing() -> Result {
    // 2,000 REPs of 65,535 copies would spend 655 M twentieths, past the
    // 500 M a call may.
    let mut p = Parser::new(Size::new(2, 1)?, 0)?;
    let input = times("\u{4e00}\x1b[65535b", 2000);
    assert_eq!(p.process_until_stop(input.as_bytes(), &mut Deaf)?, None);
    Ok(())
}

/// B14, `Links::sweep`'s `live <= half` (link.rs, `<=` to `>`): a link past the bound takes the links of the oldest history rows, as many as leave the rest at half the bound, not only the oldest row's.
#[test]
fn a_sweep_takes_the_oldest_links_until_half_the_bound_is_left() -> Result {
    // As many of the longest links as cost at most half the bound.
    const HALF_FIT: usize = LINKS_HELD / 2 / LONGEST;
    let mut p = Parser::with_options(
        Size::new(1, 80)?,
        4 << 20,
        Options::new().with(Feature::Hyperlinks),
    )?;
    // `FIT` links fill the bound, a row of history each; the next one
    // sweeps: the oldest `FIT - HALF_FIT` rows lose theirs, and it fits.
    let line = [link_of(URI_LIMIT).as_slice(), b"\r\n"].concat();
    p.process(&times_bytes(&line, FIT + 1))?;
    let linked: Vec<bool> = p.screen().rows().map(|r| r.link(0).is_some()).collect();
    let rows = FIT + 2;
    assert_eq!(linked, runs_of(0, FIT - HALF_FIT, HALF_FIT + 1, rows));
    Ok(())
}

/// B25, `csi`'s window `window.len() > limit` (decode.rs, `>` to `>=`): a key CSI of 32 parameter bytes is within its window, so at the timeout it is Alt-[ and the 32 bytes typed, not dropped.
#[test]
fn a_key_csi_of_32_parameter_bytes_is_typed_at_the_timeout() {
    let mut read = b"\x1b[".to_vec();
    read.resize(34, b'1');
    let mut d = Decoder::default();
    let mut out = Vec::new();
    d.bytes(&read, &mut out);
    assert!(out.is_empty(), "{out:?}");
    d.timeout(&mut out);
    assert_eq!(out.len(), 33, "{out:?}");
    assert!(out.iter().all(|i| matches!(i, Input::Key(_))), "{out:?}");
}

/// B33 and B34, `Screen::repeat`'s non-ASCII copies loop (screen.rs, `>` to `>=`): REP of a non-ASCII glyph makes its copies and returns.
#[test]
fn repeat_makes_its_non_ascii_copies_and_returns() -> Result {
    let shown = in_time(|| {
        let mut p = Parser::new(Size::new(2, 10).map_err(|e| e.to_string())?, 0)
            .map_err(|e| e.to_string())?;
        p.process("é\x1b[3bz".as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(lines::lines(&p))
    })?;
    assert_eq!(shown, ["ééééz", ""]);
    Ok(())
}

/// B33, `Screen::copies_needed`'s `width > cols` (screen.rs, `>` to `==` and to `>=`): REP on a one-column screen makes every copy, a row each.
#[test]
fn a_repeat_on_a_one_column_screen_makes_every_copy() -> Result {
    let mut p = Parser::new(Size::new(3, 1)?, 0)?;
    // `a` and 2 copies of it: each copy wraps to the next row.
    p.process(b"a\x1b[2b")?;
    assert_eq!(lines::lines(&p), ["a", "a", "a"]);
    Ok(())
}

/// B24, `Decoder::held_first`'s `fed < bytes.len()` (decode.rs, `<` to `<=`): a held read of exactly 259 bytes that ends on the ESC of an answer dropped past its limit returns, and the `\` after it ends the drop.
#[test]
fn a_read_of_259_bytes_that_ends_on_a_dropped_answer_s_escape_returns() -> Result {
    let (dropped, after, typed) = in_time(|| {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b", &mut out);
        // `] 1`, 256 `a`s and an ESC: with the Escape held, an OSC answer
        // whose string and ESC are past its 128 bytes, dropped to its ST.
        let mut read = b"]1".to_vec();
        read.extend([b'a'; 256]);
        read.push(0x1b);
        d.bytes(&read, &mut out);
        let dropped = out.len();
        d.bytes(b"\\x", &mut out);
        let typed = out.last().is_some_and(|i| matches!(i, Input::Key(_)));
        Ok((dropped, out.len(), typed))
    })?;
    assert_eq!((dropped, after, typed), (0, 1, true));
    Ok(())
}
