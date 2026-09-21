use fux_vt::{Cell, Color, Error, MouseProtocolEncoding, MouseProtocolMode, Parser};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn lines(parser: &Parser) -> Vec<String> {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|y| {
            (0..cols)
                .filter_map(|x| screen.cell(y, x))
                .filter(|c| !c.is_wide_continuation())
                .map(|c| if c.has_contents() { c.contents() } else { " " })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
fn cell(parser: &Parser, row: u16, col: u16) -> std::result::Result<&Cell, Error> {
    parser.screen().cell(row, col).ok_or(Error::InvalidRange)
}

#[test]
fn text_controls_cursor_and_pending_wrap() -> Result {
    let mut p = Parser::new(3, 5, 2)?;
    p.process(b"abcde")?;
    assert_eq!(p.screen().cursor_position(), (0, 5));
    assert!(!p.screen().row_wrapped(0));
    p.process(b"fgh\r\nijk")?;
    assert_eq!(lines(&p), ["abcde", "fgh", "ijk"]);
    assert!(p.screen().row_wrapped(0));
    assert_eq!(p.screen().cursor_position(), (2, 3));
    p.process(b"\x1b[H\tZ\x08Y")?;
    assert_eq!(lines(&p), ["abcdY", "fgh", "ijk"]);
    p.process(b"\x1b[999;999H")?;
    assert_eq!(p.screen().cursor_position(), (2, 4));
    p.process(b"\x1b[2A\x1b[2D")?;
    assert_eq!(p.screen().cursor_position(), (0, 2));
    p.process(b"\x1b[2E\x1b[F\x1b[3G")?;
    assert_eq!(p.screen().cursor_position(), (1, 2));
    p.process(b"\x1b[2C\x1b[1d")?;
    assert_eq!(p.screen().cursor_position(), (0, 4));
    Ok(())
}

#[test]
fn tiny_grids_wrap_and_drop_wide_glyphs_without_underflow() -> Result {
    for (rows, cols) in [(1, 1), (1, 2), (1, 8), (8, 1)] {
        let mut p = Parser::new(rows, cols, 2)?;
        p.process("界".as_bytes())?;
        if cols == 1 {
            assert_eq!(p.screen().cursor_position(), (0, 0));
            assert_eq!(cell(&p, 0, 0)?.contents(), "");
        }
        p.process(b"\x1bc")?;
        p.process(&vec![b'x'; usize::from(rows) * usize::from(cols) + 1])?;
        assert_eq!(p.screen().cursor_position(), (rows - 1, 1));
        assert_eq!(p.screen().history_len(), 1);
        p.process(b"\x1b[999S\x1b[999T\x1b[999L\x1b[999M")?;
    }
    Ok(())
}

#[test]
fn autowrap_disabled_overwrites_without_scrolling() -> Result {
    let mut p = Parser::new(2, 3, 2)?;
    p.process(b"\x1b[?7labcdef")?;
    assert_eq!(lines(&p), ["abf", ""]);
    assert_eq!(p.screen().history_len(), 0);
    assert!(!p.screen().autowrap());
    p.process(b"\x1b[?7hG")?;
    assert_eq!(lines(&p), ["abf", "G"]);
    assert!(p.screen().autowrap());
    Ok(())
}

#[test]
fn unicode_combining_and_clipping_are_cell_aware() -> Result {
    let mut p = Parser::new(3, 12, 0)?;
    p.process("A界e\u{301}Z".as_bytes())?;
    assert!(cell(&p, 0, 1)?.is_wide());
    assert!(cell(&p, 0, 2)?.is_wide_continuation());
    assert_eq!(cell(&p, 0, 3)?.contents(), "e\u{301}");
    let w = p.screen().window(0, 3, 12);
    assert_eq!(w.text((0, 2), (0, 3), 100, 100)?, "界e\u{301}");
    assert_eq!(w.text((0, 3), (0, 2), 100, 100)?, "界e\u{301}");
    assert_eq!(w.text((0, 1), (0, 1), 100, 100)?, "界");
    let clipped = p.screen().window(0, 1, 2);
    assert!(clipped.cell(0, 1).is_none());
    assert_eq!(clipped.text((0, 0), (0, 1), 100, 100)?, "A");
    assert!(clipped.cell(1, 0).is_none());
    Ok(())
}

#[test]
fn erase_insert_delete_and_wide_halves_are_repaired() -> Result {
    let mut p = Parser::new(2, 6, 0)?;
    p.process(b"abcdef\r\x1b[2@")?;
    assert_eq!(lines(&p), ["  abcd", ""]);
    p.process(b"\x1b[3P")?;
    assert_eq!(lines(&p), ["bcd", ""]);
    p.process(b"\x1b[31;44m\x1b[2X")?;
    assert_eq!(lines(&p), ["  d", ""]);
    assert_eq!(cell(&p, 0, 0)?.bgcolor(), Color::Idx(4));
    p.process(b"\x1b[2J\x1b[H")?;
    assert_eq!(lines(&p), ["", ""]);
    p.process("界界界\r\x1b[2Gx".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "");
    assert_eq!(cell(&p, 0, 1)?.contents(), "x");
    p.process(b"\x1b[3G\x1b[X")?;
    assert!(!cell(&p, 0, 3)?.is_wide_continuation());
    p.resize(2, 5)?;
    assert!(!cell(&p, 0, 4)?.is_wide());
    Ok(())
}

#[test]
fn sgr_defaults_resets_and_colour_parameter_forms() -> Result {
    let mut p = Parser::new(2, 10, 0)?;
    p.process(b"\x1b[1;3;4;7;91;104mA\x1b[2mB\x1b[22;23;24;27;39;49mC\x1b[38:2:1:2:3;48:5:200mD\x1b[38;5;255;48;2;5;6;7mE\x1b[mF")?;
    let a = cell(&p, 0, 0)?;
    assert!(a.bold() && a.italic() && a.underline() && a.inverse());
    assert_eq!(a.fgcolor(), Color::Idx(9));
    assert_eq!(a.bgcolor(), Color::Idx(12));
    assert!(cell(&p, 0, 1)?.dim() && !cell(&p, 0, 1)?.bold());
    assert_eq!(cell(&p, 0, 2)?.attributes(), Default::default());
    assert_eq!(cell(&p, 0, 3)?.fgcolor(), Color::Rgb(1, 2, 3));
    assert_eq!(cell(&p, 0, 3)?.bgcolor(), Color::Idx(200));
    assert_eq!(cell(&p, 0, 4)?.fgcolor(), Color::Idx(255));
    assert_eq!(cell(&p, 0, 4)?.bgcolor(), Color::Rgb(5, 6, 7));
    assert_eq!(cell(&p, 0, 5)?.attributes(), Default::default());
    Ok(())
}

#[test]
fn history_ids_survive_scrolling_and_recycled_slots_do_not_alias() -> Result {
    let mut p = Parser::new(3, 8, 2)?;
    p.process(b"one\r\ntwo\r\nthree")?;
    let id = p
        .screen()
        .window(0, 3, 8)
        .row(0)
        .ok_or(Error::InvalidRange)?
        .id;
    let mark = p.screen().mark();
    p.process(b"\r\nfour\r\nfive")?;
    assert_eq!(p.screen().history_len(), 2);
    assert_eq!(p.screen().offset_for_row(id), Some(2));
    let w = p.screen().window(usize::MAX, 3, 8);
    assert_eq!(w.offset, 2);
    assert_eq!(w.text((0, 0), (2, 7), 100, 100)?, "one\ntwo\nthree");
    assert_eq!(
        p.screen()
            .row_by_id(id)
            .ok_or(Error::InvalidRange)?
            .cells
            .first()
            .ok_or(Error::InvalidRange)?
            .contents(),
        "o"
    );
    assert!(p.screen().full_refresh_since(mark));
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 5);
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 5);
    assert_eq!(
        p.screen().row_from_bottom(4).ok_or(Error::InvalidRange)?.id,
        id
    );
    assert!(p.screen().row_from_bottom(usize::MAX).is_none());
    p.process(b"\r\nsix")?;
    assert!(p.screen().row_by_id(id).is_none());
    let allocation = p.screen().storage_cells();
    for _ in 0..10000 {
        p.process(b"line\r\n")?;
    }
    assert_eq!(p.screen().storage_cells(), allocation);
    assert_eq!(p.screen().history_len(), 2);
    Ok(())
}

#[test]
fn copying_widened_history_joins_original_row_extents_without_padding() -> Result {
    let mut p = Parser::new(2, 5, 2)?;
    p.process(b"abcdefgh\r\nlast")?;
    assert_eq!(p.screen().history_len(), 1);
    p.resize(2, 10)?;
    let window = p.screen().window(1, 2, 10);
    assert!(window.row_wrapped(0));
    assert!(window.cell(0, 5).is_none());
    assert_eq!(window.text((0, 0), (1, 9), 20, 100)?, "abcdefgh");
    Ok(())
}

#[test]
fn copy_soft_wraps_trim_hard_padding_and_enforce_limits() -> Result {
    let mut p = Parser::new(4, 5, 0)?;
    p.process(b"abcdefgh\r\nijk")?;
    let w = p.screen().window(0, 4, 5);
    assert_eq!(w.text((0, 0), (2, 2), 100, 100)?, "abcdefgh\nijk");
    assert_eq!(w.text((1, 0), (1, 4), 100, 100)?, "fgh");
    assert_eq!(w.text((0, 0), (2, 2), 10, 100), Err(Error::CopyLimit));
    assert_eq!(w.text((0, 0), (2, 2), 100, 5), Err(Error::CopyLimit));
    assert_eq!(w.text((0, 0), (4, 0), 100, 100), Err(Error::InvalidRange));
    assert!(!p.screen().window(0, 4, 3).row_wrapped(0));
    Ok(())
}

#[test]
fn marks_observe_cursor_modes_resize_and_invalid_marks_without_consumption() -> Result {
    let mut p = Parser::new(2, 5, 0)?;
    let mark = p.screen().mark();
    p.process(b"\x1b[?25l")?;
    assert!(p.screen().changed_since(mark));
    assert!(!p.screen().full_refresh_since(mark));
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 0);
    let mark = p.screen().mark();
    p.process(b"x")?;
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 1);
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 1);
    let mark = p.screen().mark();
    p.resize(3, 6)?;
    assert!(p.screen().full_refresh_since(mark));
    let mark = p.screen().mark();
    p.process(b"")?;
    assert!(!p.screen().changed_since(mark));
    Ok(())
}

#[test]
fn height_only_resize_clears_live_wrap_metadata() -> Result {
    let mut p = Parser::new(3, 5, 0)?;
    p.process(b"abcdef")?;
    assert!(p.screen().row_wrapped(0));
    p.resize(4, 5)?;
    assert!(!p.screen().row_wrapped(0));
    assert_eq!(lines(&p), ["abcde", "f", "", ""]);
    Ok(())
}

#[test]
fn resize_rejects_bad_capacity_without_mutating_state() -> Result {
    assert_eq!(Parser::new(0, 1, 0).err(), Some(Error::ZeroSize));
    assert_eq!(Parser::new(1, 1, usize::MAX).err(), Some(Error::Capacity));
    let mut p = Parser::new(3, 5, 2)?;
    p.process(b"abcde\r\nfghij\r\nklmno\r\npqrst")?;
    let mark = p.screen().mark();
    assert_eq!(p.resize(0, 4), Err(Error::ZeroSize));
    assert!(!p.screen().changed_since(mark));
    assert_eq!(p.resize(u16::MAX, u16::MAX), Err(Error::Capacity));
    assert_eq!(p.screen().size(), (3, 5));
    p.resize(2, 3)?;
    assert_eq!(lines(&p), ["fgh", "klm"]);
    assert_eq!(p.screen().cursor_position(), (1, 2));
    assert_eq!(
        p.screen().window(1, 2, 3).text((0, 0), (0, 2), 100, 100)?,
        "abc"
    );
    p.resize(3, 5)?;
    assert_eq!(lines(&p), ["fgh", "klm", ""]);
    assert_eq!(
        p.screen().window(1, 3, 5).text((0, 0), (0, 4), 100, 100)?,
        "abcde"
    );
    Ok(())
}

#[test]
fn alternate_mouse_modes_saved_cursor_and_replies() -> Result {
    let mut p = Parser::new(3, 8, 2)?;
    p.process(b"main\x1b[31m\x1b[?1049hALT\x1b[?1h\x1b[?25l\x1b[?2004h\x1b[?1002h\x1b[?1006h")?;
    assert!(
        p.screen().alternate_screen()
            && p.screen().application_cursor()
            && p.screen().hide_cursor()
            && p.screen().bracketed_paste()
    );
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        MouseProtocolMode::ButtonMotion
    );
    assert_eq!(
        p.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Sgr
    );
    assert_eq!(p.screen().history_len(), 0);
    p.process(b"\x1b[?1049lX")?;
    assert_eq!(lines(&p), ["mainX", "", ""]);
    assert_eq!(cell(&p, 0, 4)?.fgcolor(), Color::Idx(1));
    p.process(b"\x1b[?1000l\x1b[?1005l")?;
    assert_eq!(
        p.screen().mouse_protocol_mode(),
        MouseProtocolMode::ButtonMotion
    );
    assert_eq!(
        p.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Sgr
    );
    p.process(b"\x1b[?1002l\x1b[?1006l")?;
    assert_eq!(p.screen().mouse_protocol_mode(), MouseProtocolMode::None);
    assert_eq!(
        p.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Default
    );
    let mut replies = Vec::new();
    p.process_with_replies(b"\x1b[5n\x1b[6n\x1b[c\x1b[?6n", |r| {
        replies.push(r.to_vec())
    })?;
    assert_eq!(
        replies,
        [
            b"\x1b[0n".to_vec(),
            b"\x1b[1;6R".to_vec(),
            b"\x1b[?1;2c".to_vec()
        ]
    );
    p.process(b"\x1b7\x1b[H\x1b8")?;
    assert_eq!(p.screen().cursor_position(), (0, 5));
    Ok(())
}

#[test]
fn line_edits_outside_margins_leave_the_grid_unchanged() -> Result {
    for edit in *b"LM" {
        let mut p = Parser::new(4, 4, 0)?;
        p.process(b"\x1b[2;3r\x1b[4;4HZ")?;
        p.process(&[27, b'[', edit])?;
        assert_eq!(lines(&p), ["", "", "", "   Z"]);
        p.process(b"\x1b[1;1HA")?;
        p.process(&[27, b'[', edit])?;
        assert_eq!(lines(&p), ["A", "", "", "   Z"]);
        assert_eq!(p.screen().history_len(), 0);
    }
    Ok(())
}

#[test]
fn wrapping_below_the_scroll_region_does_not_invent_a_soft_line_join() -> Result {
    let mut p = Parser::new(4, 4, 0)?;
    p.process(b"\x1b[2;3r\x1b[4;4Hab")?;
    assert_eq!(p.screen().cursor_position(), (3, 1));
    assert_eq!(lines(&p), ["", "", "", "b  a"]);
    assert!(!p.screen().row_wrapped(3));
    Ok(())
}

#[test]
fn regions_origin_and_reset_have_explicit_history_semantics() -> Result {
    let mut p = Parser::new(4, 4, 3)?;
    p.process(b"a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[?6h")?;
    assert_eq!(p.screen().scroll_region(), (1, 2));
    assert!(p.screen().origin_mode());
    assert_eq!(p.screen().cursor_position(), (1, 0));
    p.process(b"\x1b[2;1H\n")?;
    assert_eq!(lines(&p), ["a", "c", "", "d"]);
    assert_eq!(p.screen().history_len(), 0);
    p.process(b"\x1b[H\x1bM")?;
    assert_eq!(lines(&p), ["a", "", "c", "d"]);
    let id = p
        .screen()
        .window(0, 4, 4)
        .row(0)
        .ok_or(Error::InvalidRange)?
        .id;
    p.process(b"\x1bc")?;
    assert_eq!(lines(&p), ["", "", "", ""]);
    assert!(!p.screen().origin_mode());
    assert!(p.screen().autowrap());
    assert_eq!(p.screen().scroll_region(), (0, 3));
    assert!(p.screen().row_by_id(id).is_none());
    Ok(())
}

#[test]
fn ignored_sequences_cancel_and_recover_without_payload_leakage() -> Result {
    for ignored in [
        b"\x1b]0;title\x07".as_slice(),
        b"\x1bP1;2qpayload\x1b\\",
        b"\x1b_apc\x1b\\",
        b"\x1b^pm\x1b\\",
        b"\x1bXsos\x1b\\",
        b"\x1b[?1047h",
        b"\x1b[?1048h",
        b"\x1b[999z",
        b"\x1b(0",
    ] {
        let mut p = Parser::new(2, 8, 0)?;
        p.process(b"A")?;
        p.process(ignored)?;
        p.process(b"B")?;
        assert_eq!(lines(&p), ["AB", ""], "{ignored:?}");
    }
    let mut p = Parser::new(2, 8, 0)?;
    p.process(b"\x1b]unfinished\x18A\x1b[12\x1aB\x1b[999999999999999999C!")?;
    assert_eq!(lines(&p), ["AB     !", ""]);
    p.process(b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26;27;28;29;30;31;32;33H\rZ")?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "Z");
    Ok(())
}

#[test]
fn all_chunk_boundaries_preserve_utf8_escapes_and_ascii_fast_path_results() -> Result {
    let bytes = "A界e\u{301}\x1b[31mRED\x1b[m\r\n0123456789\x1b]ignored\x07END".as_bytes();
    let mut whole = Parser::new(4, 8, 3)?;
    whole.process(bytes)?;
    for split in 0..=bytes.len() {
        let mut p = Parser::new(4, 8, 3)?;
        p.process(bytes.get(..split).ok_or(Error::InvalidRange)?)?;
        p.process(bytes.get(split..).ok_or(Error::InvalidRange)?)?;
        assert_eq!(lines(&p), lines(&whole), "split {split}");
        assert_eq!(
            p.screen().cursor_position(),
            whole.screen().cursor_position()
        );
        for y in 0..4 {
            for x in 0..8 {
                assert_eq!(cell(&p, y, x)?, cell(&whole, y, x)?);
            }
        }
    }
    let mut p = Parser::new(4, 8, 3)?;
    for byte in bytes {
        p.process(std::slice::from_ref(byte))?;
    }
    assert_eq!(lines(&p), lines(&whole));
    p.process(&[0xf0, 0x9f, 0x1b, b'[', b'H', b'Z'])?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "Z");
    Ok(())
}
