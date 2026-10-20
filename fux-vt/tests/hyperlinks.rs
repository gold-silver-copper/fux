//! Hyperlinks (OSC 8, `references/modern/osc8_hyperlinks.md`), with
//! `Feature::Hyperlinks`: each cell printed while a link is open keeps it,
//! through scrolling, erasing, editing and resizing, within bounds.

use fux_vt::{Feature, ID_LIMIT, OSC_PAYLOAD_LIMIT, Options, Parser, Row, Size, URI_LIMIT};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const LINKS: Options = Options::new().with(Feature::Hyperlinks);

fn parser(rows: u16, cols: u16, history: usize, input: &[u8]) -> Result<Parser> {
    let mut parser = Parser::with_options(Size::new(rows, cols)?, history, LINKS)?;
    parser.process(input)?;
    Ok(parser)
}

/// Each cell's link on a row: its URI, or `-` for none.
fn uris(row: Row<'_>) -> Vec<String> {
    (0..row.len())
        .map(|col| row.link(col).map_or("-".to_owned(), |l| l.uri().to_owned()))
        .collect()
}

/// The URIs of the screen's row `y`.
fn uris_at(parser: &Parser, y: u16) -> Vec<String> {
    parser
        .screen()
        .window()
        .row(y)
        .map(uris)
        .unwrap_or_default()
}

fn expected(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| (*c).to_owned()).collect()
}

#[test]
fn a_link_covers_the_cells_printed_while_it_is_open() -> Result {
    let p = parser(
        2,
        6,
        0,
        b"a\x1b]8;;http://x\x1b\\bc\x1b]8;;\x1b\\d\x1b]8;;http://y\x07e\x1b]8;;\x07",
    )?;
    let s = p.screen();
    assert_eq!(
        uris_at(&p, 0),
        expected(&["-", "http://x", "http://x", "-", "http://y", "-"])
    );
    // No id given: none read, and each OSC 8 is a link of its own.
    let (b, c, e) = (s.link(0, 1), s.link(0, 2), s.link(0, 4));
    assert_eq!(b.and_then(|l| l.id()), None);
    assert_eq!(b.map(|l| l.key()), c.map(|l| l.key()));
    assert_ne!(b.map(|l| l.key()), e.map(|l| l.key()));
    assert_eq!(s.hyperlink(), None);
    // Opening one link and then another without closing switches to it.
    let p = parser(1, 4, 0, b"\x1b]8;;http://a\x07a\x1b]8;;http://b\x07b")?;
    assert_eq!(
        uris_at(&p, 0),
        expected(&["http://a", "http://b", "-", "-"])
    );
    assert_eq!(p.screen().hyperlink(), Some(("http://b", None)));
    Ok(())
}

/// `references/modern/osc8_hyperlinks.md`, "The escape sequence": a link
/// opens at an OSC 8 with a URI, "all subsequent cells that are painted"
/// take it, and OSC 8 with no URI closes it. So DECRC, which is no OSC 8,
/// reopens no link that DECSC saw open, and the blank a wide glyph leaves
/// at a row's end when it wraps, which nothing paints, has no link. A URI
/// with a byte outside 32 to 126 is undefined ("Encodings"): it opens no
/// link, as a URI the spec forbids.
#[test]
fn only_osc_8_opens_and_closes_a_link_and_only_painted_cells_take_it() -> Result {
    let p = parser(1, 4, 0, b"\x1b]8;;u\x1b\\a\x1b7\x1b]8;;\x1b\\\x1b8d")?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "-", "-", "-"]));
    let p = parser(2, 3, 0, "\x1b]8;;u\x1b\\ab\u{754c}".as_bytes())?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "u", "-"]));
    assert_eq!(uris_at(&p, 1), expected(&["u", "u", "-"]));
    let p = parser(1, 4, 0, "\x1b]8;;http://\u{e4}.example/\x1b\\ab".as_bytes())?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "-", "-"]));
    Ok(())
}

/// The spec's `id`: cells of the same id and URI are one link, wherever
/// and whenever they were printed; another URI, or no id, is another link.
#[test]
fn an_id_joins_the_cells_of_one_link() -> Result {
    let p = parser(
        2,
        8,
        0,
        b"\x1b]8;id=1;http://a\x07ab\x1b]8;;\x07  \x1b]8;id=1;http://a\x07cd\x1b]8;id=1;http://b\x07e\r\n\
          \x1b]8;foo=bar:id=1:baz;http://a\x07f\x1b]8;;http://a\x07g\x1b]8;id=;http://a\x07h",
    )?;
    let s = p.screen();
    let key = |row, col| s.link(row, col).map(|l| l.key());
    assert_eq!(s.link(0, 0).and_then(|l| l.id()), Some("1"));
    assert_eq!(key(0, 0), key(0, 4));
    assert_eq!(key(0, 0), key(1, 0), "id among other parameters");
    assert_ne!(key(0, 0), key(0, 6), "the same id, another URI");
    assert_ne!(key(0, 0), key(1, 1), "no id");
    assert_ne!(key(1, 1), key(1, 2), "an empty id is none");
    assert_eq!(s.link(1, 2).and_then(|l| l.id()), None);
    Ok(())
}

/// Without the option, OSC 8 is ignored, as it always was.
#[test]
fn without_the_option_links_are_ignored() -> Result {
    let mut p = Parser::new(Size::new(1, 4)?, 0)?;
    p.process(b"\x1b]8;;http://x\x07ab")?;
    let s = p.screen();
    assert_eq!(s.link(0, 0), None);
    assert_eq!(s.hyperlink(), None);
    assert!(s.rows().nth_back(0).is_some_and(|r| !r.has_links()));
    assert_eq!(s.cell(0, 0).map(|c| c.contents()), Some("a"));
    Ok(())
}

/// A wide glyph's second half has the link of its first; a blank cell has
/// none, even inside an open link; a mark after a blank cell gives it a
/// space and the link.
#[test]
fn wide_glyphs_blanks_and_marks() -> Result {
    let p = parser(
        1,
        8,
        0,
        "\x1b]8;;u\x07界\x1b[C\x1b[1X\x1b[2Ce\u{301}\x1b[C\u{301}".as_bytes(),
    )?;
    assert_eq!(
        uris_at(&p, 0),
        expected(&["u", "u", "-", "-", "-", "u", "u", "-"])
    );
    Ok(())
}

/// Links go into history with their rows, and leave with them.
#[test]
fn links_scroll_with_their_rows() -> Result {
    // Two rows of one cell, 24 bytes a row and 8 a cell, and the links'
    // map, about 38 bytes a row it has room for (THREATS.md B17): room for
    // these, not for the rows below as well.
    let p = parser(
        2,
        3,
        256,
        b"\x1b]8;;one\x07a\x1b]8;;\x07\r\nb\r\n\x1b]8;;two\x07c\r\nd",
    )?;
    let s = p.screen();
    assert_eq!(s.history_len(), 2);
    let uri = |offset: usize| {
        s.rows()
            .nth_back(offset)
            .and_then(|r| r.link(0))
            .map(|l| l.uri().to_owned())
    };
    assert_eq!(uri(3).as_deref(), Some("one"));
    assert_eq!(uri(2), None);
    assert_eq!(uri(1).as_deref(), Some("two"));
    // The link is still open on the last row.
    assert_eq!(uri(0).as_deref(), Some("two"));
    // Scrolled past the history, the rows are dropped, and their links.
    let mut p = p;
    p.process(b"\x1b]8;;\x07\r\ne\r\nf\r\ng\r\nh")?;
    let s = p.screen();
    assert!((0..4).all(|o| s.rows().nth_back(o).is_some_and(|r| r.link(0).is_none())));
    // Scrolled within a region (IL, DL, SU, SD), links move with rows too.
    let p = parser(
        3,
        2,
        0,
        b"\x1b]8;;a\x07a\r\n\x1b]8;;b\x07b\r\n\x1b]8;;\x07c\x1b[1;1H\x1b[L",
    )?;
    let firsts: Vec<_> = (0..3)
        .map(|y| uris_at(&p, y).first().cloned().unwrap_or_default())
        .collect();
    assert_eq!(firsts, ["-", "a", "b"]);
    Ok(())
}

/// Erasing blanks cells, which have no link; what is printed over a link
/// takes the link open then, or none.
#[test]
fn erasing_and_overwriting_end_links() -> Result {
    let open = b"\x1b]8;;u\x07abcdef\x1b]8;;\x07";
    let mut p = parser(3, 6, 0, open)?;
    p.process(b"\x1b[1;2H\x1b[2X\x1b[1;5H\x1b[K")?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "-", "-", "u", "-", "-"]));
    p.process(b"\x1b[1;1Hx\x1b]8;;v\x07\x1b[1;4Hy")?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "-", "v", "-", "-"]));
    // ED erases rows whole.
    let mut p = parser(3, 6, 0, open)?;
    p.process(b"\x1b[2J")?;
    assert_eq!(uris_at(&p, 0), expected(&["-"; 6]));
    assert!(
        p.screen()
            .rows()
            .nth_back(2)
            .is_some_and(|r| !r.has_links())
    );
    Ok(())
}

/// ICH and DCH, and printing in insert mode, move links with their cells.
#[test]
fn inserting_and_deleting_move_links() -> Result {
    let mut p = parser(1, 6, 0, b"a\x1b]8;;u\x07bc\x1b]8;;\x07d")?;
    p.process(b"\x1b[1;1H\x1b[2@")?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "-", "u", "u", "-"]));
    p.process(b"\x1b[3P")?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "u", "-", "-", "-", "-"]));
    p.process(b"\x1b[4h\x1b]8;;v\x07x")?;
    assert_eq!(uris_at(&p, 0), expected(&["v", "u", "u", "-", "-", "-"]));
    Ok(())
}

/// A resize keeps the links of the cells it keeps; a reflow moves them
/// with their cells, and back.
#[test]
fn resizing_and_reflowing_keep_links() -> Result {
    let text = b"ab\x1b]8;;u\x07cdef\x1b]8;;\x07gh";
    let mut p = parser(2, 8, 4, text)?;
    p.resize(Size::new(2, 4)?)?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "u", "u"]));
    let mut p = Parser::with_options(Size::new(2, 8)?, 4, LINKS.with(Feature::Reflow))?;
    p.process(text)?;
    p.resize(Size::new(2, 4)?)?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "u", "u"]));
    assert_eq!(uris_at(&p, 1), expected(&["u", "u", "-", "-"]));
    p.resize(Size::new(2, 8)?)?;
    assert_eq!(
        uris_at(&p, 0),
        expected(&["-", "-", "u", "u", "u", "u", "-", "-"])
    );
    // The link still open goes on in the reflowed grid.
    let mut p = Parser::with_options(Size::new(2, 4)?, 4, LINKS.with(Feature::Reflow))?;
    p.process(b"\x1b]8;;w\x07ab")?;
    p.resize(Size::new(2, 6)?)?;
    p.process(b"c")?;
    assert_eq!(uris_at(&p, 0), expected(&["w", "w", "w", "-", "-", "-"]));
    Ok(())
}

/// A link is a cell's change: a row redrawn with a link it did not have
/// takes a new version, and one redrawn as it was keeps its own.
#[test]
fn a_new_link_is_a_new_version() -> Result {
    let mut p = parser(1, 4, 0, b"ab")?;
    let version = |p: &Parser| p.screen().rows().nth_back(0).map(|r| r.version());
    let before = version(&p);
    p.process(b"\r\x1b]8;;u\x07ab")?;
    let linked = version(&p);
    assert_ne!(before, linked);
    p.process(b"\rab")?;
    assert_eq!(version(&p), linked, "the same text, the same link");
    p.process(b"\x1b]8;;\x07\rab")?;
    assert_ne!(version(&p), linked);
    Ok(())
}

/// The spec's limits: a URI past 2083 bytes, or an id past 250, or either
/// with a byte that is not printable ASCII, opens no link, and closes the
/// one open; so does an OSC string past the payload limit.
#[test]
fn links_past_their_limits_are_not_opened() -> Result {
    let uri: String = std::iter::repeat_n('u', URI_LIMIT).collect();
    let id: String = std::iter::repeat_n('i', ID_LIMIT).collect();
    let open = |params: &str, uri: &str| format!("\x1b]8;;x\x07a\x1b]8;{params};{uri}\x07b");
    for (params, uri, kept) in [
        ("", uri.as_str(), true),
        ("", &format!("{uri}u"), false),
        (&format!("id={id}"), "x", true),
        (&format!("id={id}i"), "x", false),
        ("", "http://é", false),
        ("id=\t", "x", false),
    ] {
        let p = parser(1, 4, 0, open(params, uri).as_bytes())?;
        assert_eq!(p.screen().link(0, 0).map(|l| l.uri()), Some("x"));
        assert_eq!(
            p.screen().link(0, 1).is_some(),
            kept,
            "{params:?} {}",
            uri.len()
        );
    }
    let mut p = parser(1, 4, 0, b"\x1b]8;;x\x07a\x1b]8;;")?;
    for _ in 0..OSC_PAYLOAD_LIMIT / 1024 + 1 {
        p.process(&[b'u'; 1024])?;
    }
    p.process(b"\x07b")?;
    assert_eq!(p.screen().link(0, 1), None);
    Ok(())
}

/// More links than a screen holds: the history rows printed first lose
/// theirs, and the newest still have theirs.
#[test]
fn links_are_bounded_and_history_loses_them_first() -> Result {
    // History for 4,000 rows of one linked cell: a 24-byte record, an
    // 8-byte cell, its 48-byte run and the links' map's room for it, about
    // 38 to 76 bytes (THREATS.md B15, B17), so the links' bound decides.
    let mut p = Parser::with_options(Size::new(4, 4)?, 4000 * 160, LINKS)?;
    let long: String = std::iter::repeat_n('u', 2000).collect();
    // About 2 KiB each: 3000 are past the 4 MiB a screen holds.
    for n in 0..3000 {
        p.process(format!("\x1b]8;;{long}{n}\x07x\x1b]8;;\x07\r\n").as_bytes())?;
    }
    let s = p.screen();
    let linked = |offset: usize| s.rows().nth_back(offset).and_then(|r| r.link(0)).is_some();
    assert!(linked(1), "the newest link");
    assert!(!linked(2999), "the oldest link");
    let kept = (1..3001).filter(|o| linked(*o)).count();
    assert!((1000..3000).contains(&kept), "{kept} links kept");
    // The newest is the one printed last.
    let newest = s
        .rows()
        .nth_back(1)
        .and_then(|r| r.link(0))
        .map(|l| l.uri().to_owned());
    assert_eq!(newest, Some(format!("{long}2999")));
    Ok(())
}

/// Each screen keeps its own cells' links; the open link goes on across a
/// switch, and RIS closes it and forgets every link.
#[test]
fn screens_and_resets() -> Result {
    let mut p = parser(1, 4, 0, b"\x1b]8;;u\x07a\x1b[?1049hb")?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "u", "-", "-"]));
    p.process(b"\x1b[?1049lc")?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "u", "-", "-"]));
    p.process(b"\x1bcd")?;
    assert_eq!(uris_at(&p, 0), expected(&["-"; 4]));
    assert_eq!(p.screen().hyperlink(), None);
    // SGR 0, DECSTR and DECSC/DECRC are no end of a link, nor its start.
    let p = parser(1, 4, 0, b"\x1b7\x1b]8;;u\x07\x1b[0ma\x1b[!pb\x1b8c")?;
    assert_eq!(uris_at(&p, 0), expected(&["u", "u", "-", "-"]));
    assert_eq!(p.screen().cell(0, 0).map(|c| c.contents()), Some("c"));
    assert_eq!(p.screen().link(0, 0).map(|l| l.uri()), Some("u"));
    Ok(())
}

/// A line made double width loses its cells past its new end (DECDWL,
/// note 2), and their links with them: a row linked only past its half
/// has no link left.
#[test]
fn a_line_made_double_width_loses_the_links_past_its_end() -> Result {
    let p = parser(
        2,
        10,
        0,
        b"abcde\x1b]8;;http://x\x07fghij\x1b]8;;\x07\x1b#6",
    )?;
    let row = p.screen().window().row(0).ok_or("no row")?;
    assert!(!row.has_links());
    let p = parser(
        2,
        10,
        0,
        b"\x1b]8;;http://x\x07abcdefghij\x1b]8;;\x07\x1b#6",
    )?;
    let screen = p.screen();
    assert!(screen.link(0, 4).is_some());
    Ok(())
}

/// A row made double width and then halved again by a resize keeps no
/// link past its end, as it keeps no cell there (DEC STD 070,
/// 5.6.1): 1x8, a link over `abcd`, DECDWL, then 1x4.
#[test]
fn a_resize_drops_the_links_past_a_halved_rows_end() -> Result {
    for reflow in [false, true] {
        let options = Options::new()
            .with(Feature::Hyperlinks)
            .set(Feature::Reflow, reflow);
        let mut p = Parser::with_options(Size::new(1, 8)?, 0, options)?;
        p.process(b"\x1b]8;;x\x1b\\abcd\x1b]8;;\x1b\\\x1b#6")?;
        p.resize(Size::new(1, 4)?)?;
        let row = p.screen().rows().last().ok_or("a row")?;
        let linked: Vec<bool> = (0..4).map(|x| row.link(x).is_some()).collect();
        assert_eq!(linked, [true, true, false, false], "reflow {reflow}");
        #[cfg(feature = "checks")]
        {
            let report = p.check();
            assert!(report.is_clean(), "reflow {reflow}: {report}");
        }
    }
    Ok(())
}
