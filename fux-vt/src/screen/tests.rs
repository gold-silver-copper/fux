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

/// The fast answer never contradicts the tables: for every scalar value
/// after a spread of cluster endings (letters, marks, CJK, Hangul, Indic,
/// emoji, flags, joiners), whenever `starts_cluster` says a new cluster
/// begins, UAX #29 puts a boundary there.
#[test]
fn the_fast_cluster_answer_agrees_with_the_tables() {
    let endings = [
        "a",
        "e\u{301}",
        "\u{4f60}",
        "\u{3042}",
        "\u{ac00}",
        "\u{1100}",
        "\u{1161}",
        "\u{11a8}",
        "\u{915}",
        "\u{915}\u{94d}",
        "\u{928}\u{93f}",
        "\u{2764}",
        "\u{1f600}",
        "\u{1f469}\u{200d}",
        "\u{1f1ef}",
        "\u{1f1ef}\u{1f1f5}",
        "\u{200d}",
        "\u{a9}",
        " ",
        "\u{e33}",
        "\u{16ff0}",
        "1\u{fe0f}",
    ];
    let mut decided = 0u32;
    for previous in endings {
        let Some(last) = previous.chars().next_back() else {
            continue;
        };
        for c in (0..=0x10ffff).filter_map(char::from_u32) {
            if starts_cluster(last, c) {
                assert!(!joins_by_tables(previous, c), "{previous:?} + {c:?}");
                decided = decided.saturating_add(1);
            }
        }
    }
    assert!(decided > 1_000_000, "{decided}");
}
