use super::*;
use crate::test_rng::Rng;

#[test]
fn the_osc_buffer_has_room_for_its_limit_alone() -> Result<(), Error> {
    for (options, room) in [
        (Options::from(Feature::PromptMarks), 65),
        (Options::new(), 1),
    ] {
        let mut p = Parser::with_options(Size::of(2, 10), 0, options)?;
        p.process(b"\x1b]133;")?;
        for _ in 0..40 {
            p.process(b"aa")?;
        }
        assert!(p.osc.capacity() <= room, "{} for {room}", p.osc.capacity());
    }
    Ok(())
}

#[test]
fn text_runs_keep_grapheme_boundaries() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(3, 20), 0)?;
    parser.process("ab1\u{fe0f}\u{20e3}xé\u{301}界👩\u{200d}🔬Z".as_bytes())?;
    for (col, text, wide) in [
        (0, "a", false),
        (1, "b", false),
        (2, "1\u{fe0f}\u{20e3}", true),
        (4, "x", false),
        (5, "é\u{301}", false),
        (6, "界", true),
        (8, "👩\u{200d}🔬", true),
        (10, "Z", false),
    ] {
        let cell = parser.screen().cell(0, col);
        assert_eq!(
            cell.map(|c| (c.contents(), c.is_wide())),
            Some((text, wide)),
            "column {col}"
        );
    }
    assert_eq!(parser.screen().cursor_position(), (0, 11));
    Ok(())
}

/// Each of `rows` rows of `cols` columns with a wide glyph before its last
/// column, then a run of ASCII over the whole row.
fn wide_glyph_rows(rows: u16, cols: u16) -> Vec<u8> {
    let mut text = Vec::new();
    for row in 1..=rows {
        text.extend_from_slice(
            format!("\x1b[{row};{}H\u{4e2d}\r", cols.saturating_sub(1)).as_bytes(),
        );
        text.extend(std::iter::repeat_n(b'a', usize::from(cols)));
    }
    text
}

#[test]
fn a_run_over_a_wide_glyph_costs_its_length() -> Result<(), Error> {
    let mut p = Parser::new(Size::of(40, 8000), 0)?;
    let text = wide_glyph_rows(40, 8000);
    let start = std::time::Instant::now();
    p.process(&text)?;
    let spent = start.elapsed();
    assert!(spent < std::time::Duration::from_secs(1), "{spent:?}");
    let row = p.screen().window().text((39, 0), (39, 7999), 8000, 8000)?;
    assert_eq!(row, std::iter::repeat_n('a', 8000).collect::<String>());
    Ok(())
}

#[test]
fn a_line_feed_stops_at_a_spent_budget() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(2, 4), 8)?;
    for feeds in [b"\n\n\n", b"\x0b\x0b\x0b", b"\x0c\x0c\x0c"] {
        parser.spent = crate::budget::LIMIT;
        let stop = parser.process_until_stop(feeds, &mut Replies(|_: &[u8]| {}))?;
        assert_eq!(stop, Some((1, Stop::Budget)), "{}", feeds.escape_ascii());
    }
    parser.spent = crate::budget::LIMIT;
    assert_eq!(
        parser.process_until_stop(b"\r\r\r", &mut Replies(|_: &[u8]| {}))?,
        None
    );
    Ok(())
}

#[test]
fn a_screen_wide_sequence_past_the_ceiling_waits_for_the_next_call() -> Result<(), Error> {
    let size = Size::of(1000, 1000);
    assert!(crate::budget::sequence_most(size) > crate::budget::CEILING);
    let first = b"a\x1b[2Jb\x1b[2J";
    for following in ["", "c"] {
        let mut parser = Parser::new(size, 0)?;
        let stop = parser.process_until_stop(first, &mut Replies(|_: &[u8]| {}))?;
        assert_eq!(stop, Some((first.len(), Stop::Budget)));
        assert!(parser.has_work_left());
        let shown = |p: &Parser, col: u16| p.screen().cell(0, col).map(|c| c.contents().to_owned());
        assert_eq!(shown(&parser, 1).as_deref(), Some("b"));
        assert_eq!(
            parser.process_until_stop(following.as_bytes(), &mut Replies(|_: &[u8]| {}))?,
            None
        );
        assert!(!parser.has_work_left());
        for (col, text) in [(0, ""), (1, ""), (2, following), (3, "")] {
            assert_eq!(shown(&parser, col).as_deref(), Some(text), "column {col}");
        }
        assert_eq!(
            parser.screen().cursor_position(),
            (0, if following.is_empty() { 2 } else { 3 })
        );
        assert_eq!(
            parser.process_until_stop(&[], &mut Replies(|_: &[u8]| {}))?,
            None
        );
        assert!(!parser.has_work_left());
    }
    Ok(())
}

#[test]
fn the_alternate_screen_holds_no_cells_until_shown() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(50, 200), 0)?;
    assert_eq!(parser.screen().storage_cells(), 50 * 200);
    parser.resize(Size::of(40, 120))?;
    assert_eq!(parser.screen().storage_cells(), 40 * 120);
    parser.process(b"\x1bc\x1b[!p\x1b[?47l\x1b[?1047l\x1b[?1049l\x1b[?1049s\x1b[?1049r")?;
    parser.resize(Size::of(30, 100))?;
    parser.process(b"\x1bc")?;
    assert_eq!(parser.screen().storage_cells(), 30 * 100);
    parser.process(b"\x1b[?1049h")?;
    assert_eq!(parser.screen().storage_cells(), 2 * 30 * 100);
    // RIS with history makes both grids afresh, the alternate unmade: a
    // history the style table leaves room in for rows.
    let mut parser = Parser::new(Size::of(5, 10), 64 << 10)?;
    parser.process(b"\x1b[?1049h\x1b[?1049l")?;
    let lines: Vec<u8> = std::iter::repeat_n(*b"x\n", 20).flatten().collect();
    parser.process(&lines)?;
    parser.process(b"\x1bc")?;
    assert_eq!(parser.screen().storage_cells(), 5 * 10);
    Ok(())
}

/// The printable run, as the general path counts it: a byte at a time.
fn printable_slowly(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|b| (0x20..=0x7e).contains(*b))
        .count()
}

#[test]
fn printable_runs_are_found_a_word_at_a_time_as_a_byte_at_a_time() {
    for len in 1..=24usize {
        for at in 1..len {
            for byte in 0..=u8::MAX {
                for fill in *b" a~" {
                    let mut bytes = vec![fill; len];
                    if let Some(slot) = bytes.get_mut(at) {
                        *slot = byte;
                    }
                    assert_eq!(
                        printable(&bytes),
                        printable_slowly(&bytes),
                        "{len} bytes, {byte:#x} at {at}"
                    );
                }
            }
        }
    }
    let mut r = Rng(7);
    for _ in 0..20_000 {
        let len = r.below(40).saturating_add(1);
        let mut bytes: Vec<u8> = (0..len)
            .map(|_| {
                if r.chance(95) {
                    r.byte_in(0x20, 0x7e)
                } else {
                    r.byte_in(0, 0xff)
                }
            })
            .collect();
        if let Some(first) = bytes.first_mut() {
            *first = r.byte_in(0x20, 0x7e);
        }
        assert_eq!(printable(&bytes), printable_slowly(&bytes), "{bytes:?}");
    }
}

#[test]
fn a_frame_scan_stops_at_the_first_synchronized_output_reset() -> Result<(), Error> {
    for saved in [false, true] {
        for (end, ends) in [
            (&b"\x1b[?2026l"[..], true),
            (b"\x1b[?25;2026l", true),
            (b"\x1b[?20\r26l", true),
            (b"\x1bc", true),
            (b"\x1b[!p", true),
            (b"\x1b[61;1\"p", true),
            (b"\x1b[62\"p", true),
            (b"\x1b[63\"p", false),
            (b"\x1b]2;\x1b[?2026l", true),
            (b"\x1b[?2026r", !saved),
            (b"\x1b[?20\x1826l", false),
            (b"\x1b[?2026:1l", false),
            (b"\x1b[?2026h", false),
            (b"\x1b]2;hello h\x07", false),
            (b"\x1b[?2026", false),
        ] {
            let mut parser = Parser::new(Size::of(4, 10), 8)?;
            if saved {
                parser.process(b"\x1b[?2026h")?;
            }
            parser.process(b"\x1b[?2026s\x1b[?2026h")?;
            let input = [b"ab".as_slice(), end].concat();
            let expected = ends.then_some(input.len());
            for chunk in [1, 2, input.len()] {
                let mut scan = parser.frame_scan();
                let mut held = 1;
                while held < input.len() {
                    assert_eq!(scan.end(input.get(..held).unwrap_or_default()), None);
                    held += chunk;
                }
                assert_eq!(
                    scan.end(&input),
                    expected,
                    "{end:?}, saved {saved}, chunk {chunk}"
                );
                let mut streamed = parser.clone();
                let mut offset = 0;
                let mut stopped = None;
                let mut rest = input.as_slice();
                while !rest.is_empty() {
                    let (piece, tail) = rest.split_at_checked(chunk).unwrap_or((rest, &[]));
                    rest = tail;
                    if let Some((taken, stop)) =
                        streamed.process_until_stop(piece, &mut Replies(|_: &[u8]| {}))?
                    {
                        stopped = Some((offset + taken, stop));
                    }
                    offset += piece.len();
                }
                assert_eq!(
                    stopped,
                    expected.map(|at| (at, Stop::Ended)),
                    "{end:?}, saved {saved}, chunk {chunk}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn esc_in_finds_the_first_esc() {
    for len in 0..20 {
        for at in 0..len {
            let mut bytes = vec![b'x'; len];
            for (i, b) in bytes.iter_mut().enumerate() {
                if i == at || i == at.saturating_add(3) {
                    *b = 0x1b;
                } else if i % 3 == 0 {
                    *b = 0x9b;
                }
            }
            assert_eq!(esc_in(&bytes), Some(at), "{len} {at}");
        }
        assert_eq!(esc_in(&vec![0x1a; len]), None);
    }
}

#[test]
fn the_string_scans_find_what_a_byte_at_a_time_finds() {
    let mut r = Rng(7);
    for _ in 0..20_000 {
        let bytes: Vec<u8> = (0..r.below(40))
            .map(|_| match r.below(6) {
                0 => r.byte_in(0, 0x1f),
                1 => 0x7f,
                _ => r.byte_in(0, 0xff),
            })
            .collect();
        let control = bytes.iter().position(|&b| b < 0x20 || b == 0x7f);
        let end = bytes.iter().position(|&b| matches!(b, 0x18 | 0x1a | 0x1b));
        assert_eq!(control_in(&bytes), control, "{bytes:?}");
        assert_eq!(string_end(&bytes), end, "{bytes:?}");
    }
}

#[test]
fn a_region_scroll_spends_its_slots_once() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(4095, 1), 0)?;
    let bytes: Vec<u8> = std::iter::repeat_n(&b"\x1b[4095L"[..], 100)
        .flatten()
        .copied()
        .collect();
    let start = crate::budget::spent();
    assert_eq!(
        parser.process_until_stop(&bytes, &mut Replies(|_: &[u8]| {}))?,
        None
    );
    let spent = crate::budget::spent().saturating_sub(start);
    let each = 8_190 * (crate::budget::weight::cells + crate::budget::weight::slots);
    assert!(spent <= 100 * each, "{spent}");
    Ok(())
}
