use super::*;

#[test]
fn sgr_colour_parameters_name_the_sixteen_palette_colours() {
    for (first, index) in [(30, 0), (40, 0), (90, 8), (100, 8)] {
        for offset in 0..8u8 {
            let n = first + u16::from(offset);
            assert_eq!(palette(n), Some(Color::Idx(index + offset)), "{n}");
        }
    }
    for n in [0, 29, 38, 39, 48, 49, 89, 98, 99, 108, u16::MAX] {
        assert_eq!(palette(n), None, "{n}");
    }
}

#[test]
fn partially_completed_scroll_error_still_invalidates_every_window() -> Result<(), Error> {
    let mut s = Screen::new(2, 1, 2)?;
    s.begin()?;
    s.print('A')?;
    s.control(10)?;
    s.control(13)?;
    s.print('B')?;
    let mark = s.mark();
    s.next_id = u64::MAX - 1;
    s.begin()?;
    assert_eq!(s.scroll(0, 1, 2, true, true), Err(Error::IdentityExhausted));
    assert_eq!(s.history_len(), 1); // First row moved; second allocation failed.
    assert_eq!(s.cell(0, 0).ok_or(Error::InvalidRange)?.contents(), "B");
    assert!(s.full_refresh_since(mark));
    assert_eq!(s.dirty_rows_since(mark).count(), 3);
    assert_eq!(s.dirty_rows_since(mark).count(), 3);
    Ok(())
}

#[test]
fn identity_and_mark_exhaustion_never_alias_old_rows() -> Result<(), Error> {
    let mut s = Screen::new(1, 1, 0)?;
    let id = s.row_from_bottom(0).ok_or(Error::InvalidRange)?.id;
    s.next_id = u64::MAX;
    assert_eq!(s.linefeed(), Err(Error::IdentityExhausted));
    assert_eq!(s.row_from_bottom(0).ok_or(Error::InvalidRange)?.id, id);
    assert_eq!(s.escape(&[], b'c'), Err(Error::IdentityExhausted));
    assert!(s.row_by_id(id).is_some());
    assert!(s.full_refresh_since(Mark(u64::MAX)));
    s.version = u64::MAX;
    assert_eq!(s.begin(), Err(Error::IdentityExhausted));
    assert_eq!(s.mark(), Mark(u64::MAX));
    assert_eq!(
        Error::ZeroSize.to_string(),
        "terminal dimensions must be nonzero"
    );
    for error in [
        Error::Capacity,
        Error::IdentityExhausted,
        Error::CopyLimit,
        Error::InvalidRange,
    ] {
        assert!(!error.to_string().is_empty());
    }
    Ok(())
}

/// Recycling a slot clears only the cells before its `used` mark, so every
/// edit must keep the cells past it blank: printing, wide glyphs and
/// clusters, erasing in the pen's colours or not, inserting and deleting,
/// scrolling in and out of regions, resizing.
#[test]
fn cells_past_a_rows_used_mark_stay_blank() -> Result<(), Error> {
    let pieces: [&[u8]; 24] = [
        b"hello",
        b"\r\n",
        b"\n",
        "\u{754c}x".as_bytes(),
        "e\u{301}".as_bytes(),
        "\u{1f44d}\u{1f3fd}".as_bytes(),
        b"\x1b[41m",
        b"\x1b[m",
        b"\x1b[K",
        b"\x1b[1K",
        b"\x1b[2J",
        b"\x1b[3X",
        b"\x1b[2@",
        b"\x1b[2P",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[2;4r",
        b"\x1b[r",
        b"\x1bM",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[4h",
        b"\x1b[4l",
        b"\x1b[9;3H",
    ];
    let mut state = 0x5eed_u64;
    for reflow in [false, true] {
        let options = crate::Options::new().with_reflow(reflow);
        let mut p = crate::Parser::with_options(6, 10, 4, options)?;
        for step in 0..3_000u32 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let pick = usize::try_from(state >> 59).unwrap_or(0);
            if step % 500 == 499 {
                let cols = if step % 1000 == 999 { 10 } else { 7 };
                p.resize(6, cols)?;
            } else if let Some(piece) = pieces.get(pick % pieces.len()) {
                p.process(piece)?;
            }
            let s = p.screen();
            assert!(s.primary.blank_past_used(), "step {step}");
            assert!(s.alternate.blank_past_used(), "step {step}");
        }
    }
    Ok(())
}

/// The links each grid counts are those its rows have, whatever the rows
/// went through: printing over links, inserting and deleting characters,
/// erasing, scrolling into and out of history, both screens, resizes with
/// and without reflow, and making room when the links fill their bounds;
/// and no row has the number of a link the grid let go.
#[test]
fn link_counts_follow_the_rows() -> Result<(), Error> {
    let check = |p: &crate::Parser, step: &str| {
        let s = p.screen();
        for grid in [&s.primary, &s.alternate] {
            let (kept, counted, held) = grid.link_counts();
            assert_eq!(kept, counted, "after {step}");
            assert!(held, "after {step}");
        }
    };
    let long: String = std::iter::repeat_n('u', 2000).collect();
    for reflow in [false, true] {
        let options = crate::Options::new()
            .with_hyperlinks(true)
            .with_reflow(reflow);
        let mut p = crate::Parser::with_options(4, 10, 6, options)?;
        let steps: [&[u8]; 12] = [
            b"\x1b]8;;a\x07abc\x1b]8;id=x;b\x07de\x1b]8;;\x07f\r\n",
            b"\x1b]8;;c\x07ghij\x1b[1;2H\x1b[2@\x1b[3P",
            b"\x1b[1;1Hx\x1b]8;;d\x07yz\x1b[K",
            b"\x1b[4h\x1b]8;id=x;b\x07ins\x1b[4l\r\n",
            b"\x1b[2J\x1b[H\x1b]8;;e\x07one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix",
            b"\x1b]8;;f\x07\x1b[?1049hmore\x1b[?1049l",
            b"\x1b[2;3r\x1b[2;1H\x1b[L\x1b[M\x1b[r",
            "界e\u{301}\x1b[C\u{301}".as_bytes(),
            b"\x1b]8;;\x07plain\r\n\r\n\r\n\r\n\r\n\r\n\r\n",
            b"\x1b]8;;g\x07wrapping past the edge\r\n",
            b"\x1b[3;1H\x1b[J",
            b"\x1bc\x1b]8;;h\x07after",
        ];
        for (i, step) in steps.iter().enumerate() {
            p.process(step)?;
            check(&p, &format!("step {i}"));
            p.resize(3, 7)?;
            check(&p, &format!("step {i}, narrower"));
            p.resize(4, 10)?;
            check(&p, &format!("step {i}, back"));
        }
        // More links than fit: room is made, history first.
        for n in 0..2200 {
            p.process(format!("\x1b]8;;{long}{n}\x07x\x1b]8;;\x07\r\n").as_bytes())?;
        }
        check(&p, "filling the links");
        assert!(p.screen().primary.links.len() < 2200);
    }
    Ok(())
}
