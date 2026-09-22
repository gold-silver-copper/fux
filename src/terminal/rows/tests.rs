use super::*;
use crate::testing::*;
use fux_vt::Parser;

#[test]
#[ignore = "explicit release-mode performance measurement"]
fn measure_alternating_width_and_history_row_reuse() -> Outcome {
    let mut parser = Parser::new(24, 80, 100)?;
    parser.process(
        &b"\x1b[31;1mstyled wide: \xe7\x95\x8c text\x1b[0m and default text\r\n".repeat(200),
    )?;
    let mut windows = Rows::default();
    let mut reused = Rows::default();
    let mut rebuilt = Rows::default();
    let views = [(0, 80), (50, 40), (0, 40), (50, 80)];
    for (offset, width) in views {
        windows.snapshot(parser.screen(), offset, 24, width);
        reused.snapshot(parser.screen(), offset, 24, width);
    }
    let extractions = reused.extractions;
    let mut window_us = Vec::new();
    let mut reused_us = Vec::new();
    let mut rebuilt_us = Vec::new();
    for run in 0..6 {
        // Unchanged windows: served from the bounded window index.
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            for (offset, width) in views {
                std::hint::black_box(windows.snapshot(parser.screen(), offset, 24, width));
            }
        }
        let w = started.elapsed().as_micros();
        // Window index cleared: every row looked up by ID/version, none rebuilt.
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            for (offset, width) in views {
                reused.windows.clear();
                std::hint::black_box(reused.snapshot(parser.screen(), offset, 24, width));
            }
        }
        let a = started.elapsed().as_micros();
        // Everything cleared: full extraction of every row.
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            for (offset, width) in views {
                rebuilt.windows.clear();
                rebuilt.entries.clear();
                rebuilt.bytes = 0;
                std::hint::black_box(rebuilt.snapshot(parser.screen(), offset, 24, width));
            }
        }
        let b = started.elapsed().as_micros();
        if run > 0 {
            window_us.push(w);
            reused_us.push(a);
            rebuilt_us.push(b);
        }
    }
    assert_eq!(reused.extractions, extractions);
    assert_eq!(windows.extractions, extractions);
    for (offset, width) in views {
        assert_eq!(
            reused.snapshot(parser.screen(), offset, 24, width),
            rebuilt.snapshot(parser.screen(), offset, 24, width)
        );
    }
    println!(
        "ROW-REUSE-TIMES {{\"frames_per_run\":4000,\"warmups\":1,\"window_us\":{window_us:?},\"reused_us\":{reused_us:?},\"rebuilt_us\":{rebuilt_us:?},\"reused_extractions_after_warmup\":0,\"retained_rows\":{},\"retained_bytes\":{},\"forced_extractions\":{}}}",
        reused.entries.len(),
        reused.bytes,
        rebuilt.extractions
    );
    Ok(())
}

fn decoded(lines: &[Arc<str>], width: u16) -> Result<Parser, fux_vt::Error> {
    let mut parser = Parser::new(lines.len() as u16, width, 0)?;
    parser.process(b"\x1b[2J")?;
    for (y, line) in lines.iter().enumerate() {
        assert!(!line.contains(['\r', '\n']));
        for sequence in line.split('\x1b').skip(1) {
            let sgr = sequence
                .strip_prefix('[')
                .and_then(|s| s.split_once('m'))
                .map(|(sgr, _)| sgr);
            assert!(sgr.is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit() || b == b';')));
        }
        parser.process(format!("\x1b[{};1H{line}", y + 1).as_bytes())?;
    }
    Ok(parser)
}
#[test]
fn two_widths_offsets_new_viewer_and_cursor_only_updates_reuse_rows() -> Outcome {
    let mut parser = Parser::new(3, 8, 4)?;
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive")?;
    let mut cache = Rows::default();
    let live = cache.snapshot(parser.screen(), 0, 3, 8).to_vec();
    let older = cache.snapshot(parser.screen(), 2, 3, 5).to_vec();
    assert_eq!(decoded(&live, 8)?.screen().contents(), "three\nfour\nfive");
    assert_eq!(decoded(&older, 5)?.screen().contents(), "one\ntwo\nthree");
    let extracted = cache.extractions;
    let lookups = cache.row_lookups;
    for _ in 0..100 {
        let current = cache.snapshot(parser.screen(), 0, 3, 8);
        assert!(current.iter().zip(&live).all(|(a, b)| Arc::ptr_eq(a, b)));
        let current = cache.snapshot(parser.screen(), 2, 3, 5);
        assert!(current.iter().zip(&older).all(|(a, b)| Arc::ptr_eq(a, b)));
    }
    // Two viewers alternating windows of an unchanged pane do no per-row work.
    assert_eq!(cache.extractions, extracted);
    assert_eq!(cache.row_lookups, lookups);
    // A new/shorter viewer still receives complete independent rows.
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 1, 8), 8)?
            .screen()
            .contents(),
        "three"
    );
    parser.process(b"\x1b[2;2H\x1b[?25l")?;
    let lookups = cache.row_lookups;
    cache.snapshot(parser.screen(), 0, 3, 8);
    // A cursor-only change wakes the window but reuses every row.
    assert_eq!(cache.row_lookups, lookups + 3);
    assert_eq!(cache.extractions, extracted);
    parser.process(b"X")?;
    let changed = cache.snapshot(parser.screen(), 0, 3, 8).to_vec();
    assert_eq!(cache.extractions, extracted + 1);
    assert!(Arc::ptr_eq(changed.first().need()?, live.first().need()?));
    assert_eq!(
        decoded(&changed, 8)?.screen().contents(),
        "three\nfXur\nfive"
    );
    Ok(())
}
#[test]
fn window_index_is_bounded_and_falls_back_to_row_reuse() -> Outcome {
    let mut parser = Parser::new(2, 20, 0)?;
    parser.process(b"abcdefghij\r\nklmnopqrst")?;
    let mut cache = Rows::default();
    for width in 1..=MAX_WINDOWS as u16 + 1 {
        cache.snapshot(parser.screen(), 0, 2, width);
    }
    assert_eq!(cache.windows.len(), MAX_WINDOWS);
    let (extracted, lookups) = (cache.extractions, cache.row_lookups);
    // Width 1 was evicted: rows are looked up again but not re-extracted.
    let first = cache.snapshot(parser.screen(), 0, 2, 1).to_vec();
    assert_eq!(cache.row_lookups, lookups + 2);
    assert_eq!(cache.extractions, extracted);
    assert_eq!(decoded(&first, 1)?.screen().contents(), "a\nk");
    assert_eq!(cache.windows.len(), MAX_WINDOWS);
    Ok(())
}

#[test]
fn resize_clipped_wide_cells_buffer_switches_and_reset_never_reuse_stale_rows() -> Outcome {
    let mut parser = Parser::new(2, 8, 2)?;
    parser.process("AB界CD\r\nlast".as_bytes())?;
    let mut cache = Rows::default();
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 2, 3), 3)?
            .screen()
            .contents(),
        "AB\nlas"
    );
    let original = cache.snapshot(parser.screen(), 0, 2, 8).to_vec();
    parser.process(b"\x1b[?47hALT")?;
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 2, 8), 8)?
            .screen()
            .contents(),
        "ALT"
    );
    parser.process(b"\x1b[?47l")?;
    let restored = cache.snapshot(parser.screen(), 0, 2, 8);
    assert!(
        restored
            .iter()
            .zip(&original)
            .all(|(a, b)| Arc::ptr_eq(a, b))
    );
    parser.resize(2, 3)?;
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 2, 8), 3)?
            .screen()
            .contents(),
        "AB\nlas"
    );
    parser.process(b"\x1bcNEW")?;
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 2, 3), 3)?
            .screen()
            .contents(),
        "NEW"
    );
    Ok(())
}
#[test]
fn sustained_output_plateaus_cache_and_grid_storage_after_eviction() -> Outcome {
    let mut parser = Parser::new(3, 8, 4)?;
    let mut cache = Rows::default();
    let mut plateau = None;
    for phase in 0..2 {
        for _ in 0..6000 {
            parser.process(b"\r\nline")?;
            cache.snapshot(parser.screen(), 0, 3, 8);
            assert!(cache.bytes <= MAX_BYTES);
            assert!(cache.entries.len() <= MAX_ROWS);
        }
        let state = (
            cache.entries.len(),
            cache.bytes,
            parser.screen().storage_cells(),
        );
        if phase == 0 {
            plateau = Some(state);
        } else {
            assert_eq!(Some(state), plateau);
        }
    }
    assert_eq!(
        decoded(cache.snapshot(parser.screen(), 0, 3, 8), 8)?
            .screen()
            .contents(),
        "line\nline\nline"
    );
    Ok(())
}
