//! The bounds suite (`THREATS.md` sections 2 to 4): each bound row's
//! reproducer fed at 1×, 10× and (with `FUX_VT_BOUND_STRESS=1`) 100× and
//! at several chunkings, its work counted by fux-vt's one counter
//! ([`fux_vt::work_spent`], the `checks` feature) and its memory by a
//! counting allocator, independent of fux-vt's own figures.
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

/// What the bounds tests rely on the allocator for (`THREATS.md` 3.4): it
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

#[path = "bounds/probe.rs"]
mod probe;
#[path = "bounds/rows.rs"]
mod rows;

use probe::Scale;

/// Runs row `row` at every scale but 100×, unless `FUX_VT_BOUND_STRESS`
/// is 1, and at every chunking: it passes within its bound; every
/// violation fails it.
fn judge(row: &str) -> Result {
    let _one = one_at_a_time();
    let probe = (rows::ROWS.iter())
        .find(|(name, _)| *name == row)
        .map(|(_, probe)| *probe)
        .ok_or(format!("no row {row}"))?;
    let stress = std::env::var_os("FUX_VT_BOUND_STRESS").is_some_and(|v| v == "1");
    for scale in [Scale::Once, Scale::Ten, Scale::Hundred] {
        if scale == Scale::Hundred && !stress {
            continue;
        }
        rows::run(probe, scale)
            .map_err(|found| format!("{row} at {}: {}", scale.name(), found.join("; ")))?;
    }
    Ok(())
}

/// `THREATS.md`'s table of bounds (section 2) lists exactly the suite's
/// rows, in its order.
#[test]
fn the_threats_table_lists_the_suites_rows() -> Result {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/THREATS.md"))?;
    let table: Vec<&str> = (text.lines())
        .skip_while(|l| !l.starts_with("## 2 "))
        .take_while(|l| !l.starts_with("## 3 "))
        .filter_map(|l| l.strip_prefix("| B")?.split(' ').next())
        .collect();
    let suite: Vec<&str> = (rows::ROWS.iter())
        .filter_map(|(name, _)| name.strip_prefix('B'))
        .collect();
    assert_eq!(table, suite);
    Ok(())
}

/// The largest indivisible operation (`THREATS.md` 4.2) is the formula's
/// at fux's largest pane and history, and `THREATS.md` names it.
#[test]
fn the_largest_operation_is_the_formula_s_at_the_largest_pane() -> Result {
    assert_eq!(probe::most_at(4096, 4096, 1 << 30), probe::LARGEST);
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/THREATS.md"))?;
    assert!(text.contains("1,878,829,362"), "THREATS.md names another");
    Ok(())
}

/// The test of one bound row, named for its property: `bound!(name,
/// "B01")` is a test `name` that runs `judge("B01")`.
macro_rules! bound {
    ($name:ident, $row:literal) => {
        #[test]
        fn $name() -> Result {
            judge($row)
        }
    };
}

bound!(pictures_fit_the_memory_cap, "B01");
bound!(placements_fit_the_count_cap, "B02");
bound!(sixel_growth_has_bounded_work_and_memory, "B03");
bound!(sixel_aspects_have_bounded_work, "B04");
bound!(raster_extents_and_overlaps_remain_bounded, "B05");
bound!(cloning_a_picture_has_bounded_peak_memory, "B06");
bound!(pictures_validate_their_pixel_count, "B07");
bound!(sixel_encoding_has_bounded_work_and_output, "B08");
bound!(picture_lookup_has_bounded_work, "B09");
bound!(styles_fit_the_history_allowance, "B11");
bound!(chosen_style_collisions_have_bounded_work, "B12");
bound!(style_sweeps_have_bounded_work, "B13");
bound!(links_fit_the_grid_allowance, "B14");
bound!(history_accounts_for_link_runs, "B15");
bound!(link_sweeps_have_bounded_work_and_peak, "B16");
bound!(history_fits_its_memory_allowance, "B17");
bound!(line_feeds_have_bounded_work, "B18");
bound!(frame_scanning_has_linear_work, "B19");
bound!(osc_storage_is_bounded, "B20");
bound!(discarded_strings_keep_no_storage, "B21");
bound!(parameters_have_a_fixed_capacity, "B22");
bound!(pastes_fit_their_storage_allowance, "B23");
bound!(unfinished_keys_keep_a_bounded_prefix, "B24");
bound!(oversized_answers_are_dropped_whole, "B25");
bound!(input_events_are_bounded_by_input_bytes, "B26");
bound!(keyboard_stacks_have_a_fixed_capacity, "B27");
bound!(replies_are_bounded_by_request_bytes, "B28");
bound!(clusters_have_bounded_text_and_join_work, "B30");
bound!(zero_width_suffixes_have_bounded_join_work, "B31");
bound!(glyphs_take_at_most_two_cells, "B32");
bound!(ascii_repetition_has_bounded_work, "B33");
bound!(non_ascii_repetition_has_bounded_work, "B34");
bound!(self_joining_repetition_has_bounded_work, "B34a");
bound!(screen_operations_have_bounded_work, "B35");
bound!(region_scrolls_have_bounded_work, "B36");
bound!(resize_has_bounded_work_and_peak_memory, "B37");
bound!(screens_refuse_excess_cells, "B38");
bound!(cell_vectors_refuse_excess_cells, "B39");
bound!(row_lookup_has_bounded_work, "B40");
bound!(queue_append_cannot_remove_existing_bytes, "B41");
bound!(tab_stops_have_bounded_storage, "B42");
bound!(events_are_delivered_without_retention, "B43");
bound!(allocation_refusal_preserves_parser_recovery, "B44");
bound!(encoded_keys_and_mouse_fit_the_output_cap, "B45");
bound!(picture_covering_has_bounded_work_and_memory, "B46");

/// The work quota (`THREATS.md` 4) at fux's largest pane, 4095×4096,
/// where one screen-wide function moves or fills millions of cells for a
/// few bytes: every call spends at most the quota and one indivisible
/// operation, each kind stops a call at the quota and goes on in the next,
/// and the screen and the replies after the last call are those of one
/// call given it all.
mod quota {
    use fux_vt::{Parser, Size, Stop};

    use super::{Result, one_at_a_time, probe};

    /// A host that keeps its replies, in order.
    #[derive(Default)]
    struct Replies(Vec<Vec<u8>>);
    impl fux_vt::Sink for Replies {
        fn reply(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    /// Feeds `input` whole to one parser, and to another by
    /// `process_until_stop`, given the rest again after each stop and
    /// called again while work is left: between two budget stops, frame
    /// boundaries between, at most the quota and one operation spent, the
    /// quota stopping it at least twice, and the two alike at the end,
    /// screen, cursor and replies.
    fn split_and_whole_agree(input: &[u8]) -> Result {
        let _one = one_at_a_time();
        let size = Size::new(4095, 4096)?;
        let setup = probe::Setup::defaults().sized(4095, 4096).history(0);
        let most = probe::LIMIT.saturating_add(probe::most(&setup));
        let (mut whole, mut alone) = (Parser::new(size, 0)?, Replies::default());
        whole.process_with(input, &mut alone)?;
        let (mut parser, mut replies) = (Parser::new(size, 0)?, Replies::default());
        let (mut rest, mut stops, mut since) = (input, 0u32, 0u64);
        while !rest.is_empty() || parser.has_work_left() {
            let before = fux_vt::work_spent();
            let stop = parser.process_until_stop(rest, &mut replies)?;
            since = since.saturating_add(fux_vt::work_spent().saturating_sub(before));
            assert!(
                since <= most,
                "{since} spent between budget stops, past {most}"
            );
            match stop {
                None => rest = &[],
                Some((taken, stop)) => {
                    if stop == Stop::Budget {
                        (stops, since) = (stops.saturating_add(1), 0);
                    }
                    rest = rest.get(taken..).unwrap_or_default();
                }
            }
        }
        assert!(stops >= 2, "{stops} stops");
        let (a, b) = (whole.screen(), parser.screen());
        assert_eq!(a.cursor_position(), b.cursor_position());
        assert!(
            a.rows().zip(b.rows()).all(|(a, b)| a.cells().eq(b.cells())),
            "the screens differ"
        );
        assert_eq!(alone.0, replies.0, "the replies differ");
        Ok(())
    }

    /// The screen filled with `X`, then `each` 24 times, each followed by
    /// a cursor report, whose replies say where each left the cursor.
    fn each_of(each: &str) -> Vec<u8> {
        let mut input = b"\x1b[88;1;1;4095;4096$x".to_vec();
        for _ in 0..24 {
            input.extend_from_slice(each.as_bytes());
            input.extend_from_slice(b"\x1b[6n");
        }
        input
    }

    #[test]
    fn column_edits_stop_at_the_quota_and_go_on() -> Result {
        for each in ["\x1b[9'}", "\x1b[9'~", "\x1b[9 @", "\x1b[9 A"] {
            split_and_whole_agree(&each_of(each))?;
        }
        Ok(())
    }

    #[test]
    fn rectangle_copies_stop_at_the_quota_and_go_on() -> Result {
        split_and_whole_agree(&each_of("\x1b[1;1;4095;4090;1;1;7;1$v"))
    }

    #[test]
    fn rectangle_fills_and_erases_stop_at_the_quota_and_go_on() -> Result {
        for each in [
            "\x1b[89;1;2;4095;4096$x",
            "\x1b[1;2;4095;4096$z",
            "\x1b[1;2;4095;4096${",
        ] {
            split_and_whole_agree(&each_of(each))?;
        }
        Ok(())
    }

    /// REP of a narrow glyph and of a wide one, and in insert mode, where
    /// each copy moves the row: the quota stops a call between copies.
    #[test]
    fn repetition_stops_at_the_quota_and_goes_on() -> Result {
        for each in [
            "\x1b[Hé\x1b[65535b",
            "\x1b[H一\x1b[65535b",
            "\x1b[4h\x1b[Hx\x1b[65535b\x1b[4l",
        ] {
            split_and_whole_agree(&each_of(each))?;
        }
        Ok(())
    }

    /// Frames begun and ended in the same sequence that clears the
    /// alternate screen, in a background: the clear spends the quota, and
    /// the stop it owes comes in the call after the frame's boundary.
    #[test]
    fn frame_boundaries_stop_at_the_quota_too() -> Result {
        let mut input = b"\x1b[41m".to_vec();
        for _ in 0..12 {
            input.extend_from_slice(b"\x1b[?1049;2026h\x1b[?1049;2026l\x1b[6n");
        }
        split_and_whole_agree(&input)
    }

    /// A REP inside left and right margins whose wraps scroll stops where
    /// the quota is spent, its copies left made by the next call, and text
    /// in DEC Special Graphics after it stops so too.
    #[test]
    fn a_rep_inside_margins_stops_at_the_quota_and_goes_on() -> Result {
        let mut input = b"\x1b[88;1;1;4095;4096$x\x1b[?69h\x1b[1;4090s\x1b[4095;1H".to_vec();
        for _ in 0..2 {
            input.extend_from_slice(b"x\x1b[65535b\x1b(0");
            input.extend(std::iter::repeat_n(b'q', 20_000));
            input.extend_from_slice(b"\x1b(B\x1b[6n");
        }
        split_and_whole_agree(&input)
    }
}
