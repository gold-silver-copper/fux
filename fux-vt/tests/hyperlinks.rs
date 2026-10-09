//! Hyperlinks (OSC 8, `references/modern/osc8_hyperlinks.md`), with
//! `Feature::Hyperlinks`: each cell printed while a link is open keeps it,
//! through scrolling, erasing, editing and resizing, within bounds.

use fux_vt::{Feature, ID_LIMIT, OSC_PAYLOAD_LIMIT, Options, Parser, Row, URI_LIMIT};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

const LINKS: Options = Options::new().with(Feature::Hyperlinks);

fn parser(
    rows: u16,
    cols: u16,
    history: usize,
    input: &[u8],
) -> std::result::Result<Parser, fux_vt::Error> {
    let mut parser = Parser::with_options(rows, cols, history, LINKS)?;
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
    let screen = parser.screen();
    let rows = usize::from(screen.size().0);
    // Row `y` of the screen, counted from the bottom.
    let offset = rows.saturating_sub(usize::from(y) + 1);
    screen.row_from_bottom(offset).map(uris).unwrap_or_default()
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
    let mut p = Parser::new(1, 4, 0)?;
    p.process(b"\x1b]8;;http://x\x07ab")?;
    let s = p.screen();
    assert_eq!(s.link(0, 0), None);
    assert_eq!(s.hyperlink(), None);
    assert!(s.row_from_bottom(0).is_some_and(|r| !r.has_links()));
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
    let p = parser(
        2,
        3,
        2,
        b"\x1b]8;;one\x07a\x1b]8;;\x07\r\nb\r\n\x1b]8;;two\x07c\r\nd",
    )?;
    let s = p.screen();
    assert_eq!(s.history_len(), 2);
    let uri = |offset: usize| {
        s.row_from_bottom(offset)
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
    assert!((0..4).all(|o| s.row_from_bottom(o).is_some_and(|r| r.link(0).is_none())));
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
            .row_from_bottom(2)
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
    p.resize(2, 4)?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "u", "u"]));
    let mut p = Parser::with_options(2, 8, 4, LINKS.with(Feature::Reflow))?;
    p.process(text)?;
    p.resize(2, 4)?;
    assert_eq!(uris_at(&p, 0), expected(&["-", "-", "u", "u"]));
    assert_eq!(uris_at(&p, 1), expected(&["u", "u", "-", "-"]));
    p.resize(2, 8)?;
    assert_eq!(
        uris_at(&p, 0),
        expected(&["-", "-", "u", "u", "u", "u", "-", "-"])
    );
    // The link still open goes on in the reflowed grid.
    let mut p = Parser::with_options(2, 4, 4, LINKS.with(Feature::Reflow))?;
    p.process(b"\x1b]8;;w\x07ab")?;
    p.resize(2, 6)?;
    p.process(b"c")?;
    assert_eq!(uris_at(&p, 0), expected(&["w", "w", "w", "-", "-", "-"]));
    Ok(())
}

/// A link is a cell's change: a row redrawn with a link it did not have
/// takes a new version, and one redrawn as it was keeps its own.
#[test]
fn a_new_link_is_a_new_version() -> Result {
    let mut p = parser(1, 4, 0, b"ab")?;
    let version = |p: &Parser| p.screen().row_from_bottom(0).map(|r| r.version());
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
        ("id=\x01", "x", false),
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
    let mut p = Parser::with_options(4, 4, 4000, LINKS)?;
    let long: String = std::iter::repeat_n('u', 2000).collect();
    // About 2 KiB each: 3000 are past the 4 MiB a screen holds.
    for n in 0..3000 {
        p.process(format!("\x1b]8;;{long}{n}\x07x\x1b]8;;\x07\r\n").as_bytes())?;
    }
    let s = p.screen();
    let linked = |offset: usize| s.row_from_bottom(offset).and_then(|r| r.link(0)).is_some();
    assert!(linked(1), "the newest link");
    assert!(!linked(2999), "the oldest link");
    let kept = (1..3001).filter(|o| linked(*o)).count();
    assert!((1000..3000).contains(&kept), "{kept} links kept");
    // The newest is the one printed last.
    let newest = s
        .row_from_bottom(1)
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
