//! Line renditions: DECSWL (`ESC # 5`), DECDWL (`ESC # 6`) and DECDHL's
//! top and bottom (`ESC # 3`, `ESC # 4`), as DEC STD 070 has them (5.6.1,
//! "a conforming device shall support", at every level). Each expected
//! value comes from the standard's text: the cases were derived from it
//! alone, by hand and by a second reader who saw nothing but its pages.
//! Columns and lines here count from 0, the standard's from 1.

use fux_vt::{Feature, LineRendition, Options, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

fn run(rows: u16, cols: u16, bytes: &[u8]) -> Result<Parser> {
    let mut parser = Parser::new(Size::new(rows, cols)?, 0)?;
    parser.process(bytes)?;
    Ok(parser)
}

/// Row `y` of the screen, as `lines` reads it.
fn line(p: &Parser, y: usize) -> String {
    lines(p).get(y).cloned().unwrap_or_default()
}

/// The first `n` rows of the screen, as `lines` reads them.
fn first(p: &Parser, n: usize) -> Vec<String> {
    lines(p).into_iter().take(n).collect()
}

/// Row `y`'s rendition.
fn rendition(p: &Parser, y: u16) -> Option<LineRendition> {
    p.screen().window().row(y).map(|r| r.rendition())
}

/// Each row's rendition, top to bottom.
fn renditions(p: &Parser) -> Vec<LineRendition> {
    let window = p.screen().window();
    (0..window.rows())
        .filter_map(|y| window.row(y).map(|r| r.rendition()))
        .collect()
}

use LineRendition::{DoubleHeightBottom, DoubleHeightTop, DoubleWidth, Single};

/// DEC STD 070, 5.6.1: each of the four renditions is set on the Active
/// Line alone, one at a time. DECDWL and DECDHL halve the line's length:
/// "character positions cannot be addressed which are more than half the
/// current value of the right edge of the display" (5.6.1), and data
/// beyond the line's new end is lost (DECDWL, notes 1 and 2; DECDHL, notes
/// 2 and 3; their algorithms). DECSWL doubles the length again, and loses
/// nothing (DECSWL, notes 1 and 2), nor brings anything back. Each of the
/// four ends a pending wrap (Appendix D.6.1). The line's end is half the
/// columns, rounded down (5.14.1, `end_of_line`).
#[test]
fn line_renditions_halve_the_line_and_lose_what_is_past_its_end() -> Result {
    let p = run(4, 10, b"abcdefghij\r\x1b#6")?;
    assert_eq!(line(&p, 0), "abcde");
    assert_eq!(renditions(&p), [DoubleWidth, Single, Single, Single]);
    assert_eq!(p.screen().cursor_position(), (0, 0));
    let p = run(4, 10, b"abcdefghij\r\x1b#6\x1b#5")?;
    assert_eq!(line(&p, 0), "abcde");
    assert_eq!(rendition(&p, 0), Some(Single));
    // Every rendition is the line's own.
    let p = run(4, 10, b"\x1b#3\n\x1b#4\n\x1b#6\n\x1b#5")?;
    assert_eq!(
        renditions(&p),
        [DoubleHeightTop, DoubleHeightBottom, DoubleWidth, Single]
    );
    // CUP past the end stops at the end (CUP's algorithm), where a glyph
    // printed waits to wrap.
    let p = run(4, 10, b"\x1b#6\x1b[1;9HZ")?;
    assert_eq!(line(&p, 0), "    Z");
    assert_eq!(p.screen().cursor_position(), (0, 4));
    assert!(p.screen().pending_wrap());
    // Text wraps at the line's end.
    let p = run(4, 10, b"\x1b#6abcdefgh")?;
    assert_eq!(first(&p, 2), ["abcde", "fgh"]);
    assert!(p.screen().row_wrapped(0));
    assert_eq!(p.screen().cursor_position(), (1, 3));
    // A cursor past the new end goes to it (DECDHL's algorithm).
    let p = run(4, 10, b"\x1b[2;8H\x1b#3X")?;
    assert_eq!(line(&p, 1), "    X");
    // The flag j set ends, and the cursor is at the line's new end: X
    // overwrites e there (Appendix D.6.1; DECDWL's algorithm).
    let p = run(4, 10, b"abcdefghij\x1b#6X")?;
    assert_eq!(first(&p, 2), ["abcdX", ""]);
    assert_eq!(p.screen().cursor_position(), (0, 4));
    // An odd width rounds down.
    let p = run(4, 9, b"\x1b#6\x1b[1;9HX")?;
    assert_eq!(line(&p, 0), "   X");
    Ok(())
}

/// DEC STD 070: a cursor that moves onto a line, or along it, stops at
/// the line's end: IND, LF, CUD and CUU "adjust column" to the new line's
/// end_of_line (their algorithms; "new line may have a different length");
/// CUF stops at it; HT goes to the next stop or the line's end, whichever
/// comes first (HT, note 1, and its algorithm).
#[test]
fn the_cursor_stops_at_the_end_of_a_halved_line() -> Result {
    let p = run(4, 10, b"\x1b[3;1H\x1b#6\x1b[2;8H\x1b[BX")?;
    assert_eq!(line(&p, 2), "    X");
    let p = run(4, 10, b"\x1b[3;1H\x1b#6\x1b[4;8H\x1b[AX")?;
    assert_eq!(line(&p, 2), "    X");
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[1;8H\nX")?;
    assert_eq!(line(&p, 1), "    X");
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[1;8H\x1bDX")?;
    assert_eq!(line(&p, 1), "    X");
    let p = run(4, 10, b"\x1b#6\x1b[20CX")?;
    assert_eq!(line(&p, 0), "    X");
    let p = run(4, 10, b"\x1b#6\t\tX")?;
    assert_eq!(line(&p, 0), "    X");
    Ok(())
}

/// DEC STD 070: ICH and DCH move the cells up to the line's end, and what
/// ICH pushes past it is lost (ICH and DCH, their algorithms: the margin
/// is end_of_line when the right margin is past it); DECSWL brings none
/// of it back.
#[test]
fn edits_on_a_halved_line_end_at_its_end() -> Result {
    let p = run(4, 10, b"\x1b#6abcde\x1b[1;1H\x1b[2@\x1b#5")?;
    assert_eq!(line(&p, 0), "  abc");
    let p = run(4, 10, b"\x1b#6abcd\x1b[1;3H\x1b[P\x1b#5")?;
    assert_eq!(line(&p, 0), "abd");
    Ok(())
}

/// DEC STD 070: ED makes a line it erases whole single width (ED, note 1,
/// and its algorithm: ED 0 from the line's first column, ED 1 to its end,
/// and ED 2); EL leaves the rendition. DECSTR leaves it ("Line
/// Attributes", p. 4-37), and DECALN makes every line single width (its
/// algorithm, D-20).
#[test]
fn erasing_a_whole_line_makes_it_single_width() -> Result {
    let p = run(4, 10, b"\x1b#6abc\x1b[2J\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "       X");
    assert_eq!(rendition(&p, 0), Some(Single));
    let p = run(4, 10, b"\x1b#6abc\x1b[1;1H\x1b[J\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "       X");
    let p = run(4, 10, b"\x1b#6abc\x1b[1;2H\x1b[J\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "a   X");
    assert_eq!(rendition(&p, 0), Some(DoubleWidth));
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[3;1H\x1b[1J\x1b[2;8HX")?;
    assert_eq!(line(&p, 1), "       X");
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[2;5H\x1b[1J\x1b[2;8HX")?;
    assert_eq!(rendition(&p, 1), Some(Single));
    let p = run(4, 10, b"\x1b#6abc\x1b[2K\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "    X");
    assert_eq!(rendition(&p, 0), Some(DoubleWidth));
    let p = run(4, 10, b"\x1b#6\x1b[!p\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "    X");
    let p = run(4, 10, b"\x1b#6\x1b#8\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "EEEEEEEXEE");
    assert_eq!(rendition(&p, 0), Some(Single));
    Ok(())
}

/// DEC STD 070, DECLRMM (5-29): setting it makes every line single width,
/// and while it is set DECDWL and DECDHL are ignored (DECDWL, note 3;
/// DECDHL, note 4). Scrolls carry a line's rendition with it, and the line
/// a scroll brings in is single width (5.14.2, `scroll_up`).
#[test]
fn margins_and_scrolls_keep_renditions_as_dec_says() -> Result {
    let p = run(4, 10, b"\x1b#6\x1b[?69h\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "       X");
    assert_eq!(rendition(&p, 0), Some(Single));
    let p = run(4, 10, b"\x1b[?69h\x1b#6\x1b#3\x1b[1;8HX")?;
    assert_eq!(line(&p, 0), "       X");
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[1;1H\x1bM\x1b[3;8HX")?;
    assert_eq!(renditions(&p), [Single, Single, DoubleWidth, Single]);
    assert_eq!(line(&p, 2), "    X");
    let p = run(4, 10, b"\x1b[2;1H\x1b#6\x1b[4;1H\n")?;
    assert_eq!(renditions(&p), [DoubleWidth, Single, Single, Single]);
    // Into history and back, as a resize brings it (the row keeps what it
    // keeps, at any width: its cells past the new end are lost).
    let mut p = Parser::new(Size::new(4, 10)?, 4096)?;
    p.process(b"\x1b#6abcde\r\n\x1b#3top\n\n\n\n")?;
    let kept: Vec<_> = p.screen().rows().take(2).map(|r| r.rendition()).collect();
    assert_eq!(kept, [DoubleWidth, DoubleHeightTop]);
    p.resize(Size::new(6, 6)?)?;
    assert_eq!(
        renditions(&p).into_iter().take(2).collect::<Vec<_>>(),
        [DoubleWidth, DoubleHeightTop]
    );
    assert_eq!(first(&p, 2), ["abc", "top"]);
    Ok(())
}

/// Reflow never joins a line that is not single width to the rows around
/// it, nor splits it: it is a line of its own, which keeps its rendition
/// and loses what no longer fits in half the width, as DECDWL loses it
/// (DECDWL, notes 1 and 2). The rows before it reflow as their own line.
#[test]
fn reflow_keeps_a_halved_line_whole_and_alone() -> Result {
    let options = Options::new().with(Feature::Reflow);
    let mut p = Parser::with_options(Size::new(4, 10)?, 4096, options)?;
    p.process(b"0123456789a\x1b#6bc\r\n\x1b#3abcde")?;
    assert!(p.screen().row_wrapped(0));
    p.resize(Size::new(5, 6)?)?;
    assert_eq!(first(&p, 4), ["012345", "6789", "abc", "abc"]);
    assert_eq!(
        renditions(&p).into_iter().take(4).collect::<Vec<_>>(),
        [Single, Single, DoubleWidth, DoubleHeightTop]
    );
    assert!(p.screen().row_wrapped(0));
    assert!(!p.screen().row_wrapped(1));
    Ok(())
}

/// DECSED erases no line's rendition (DEC STD 070, DECSED, note 3, p.
/// 5-162: "Line attributes are not affected"), nor does an ED that spares
/// SPA's guarded glyphs, which erases no line whole; ED with nothing
/// protected makes each line it erases whole single width (ED, note 1).
#[test]
fn decsed_and_an_ed_sparing_guarded_glyphs_keep_the_renditions() -> Result {
    let p = run(2, 10, b"\x1b#6\x1b[?2J")?;
    assert_eq!(rendition(&p, 0), Some(DoubleWidth), "DECSED");
    let p = run(2, 10, b"\x1b#6\x1bVx\x1bW\x1b[2J")?;
    assert_eq!(rendition(&p, 0), Some(DoubleWidth), "ED around SPA's glyph");
    let p = run(2, 10, b"\x1b#6\x1b[2J")?;
    assert_eq!(rendition(&p, 0), Some(Single), "ED");
    // ED once protection was used, sparing nothing on the line: single
    // width (FA-RE-B-2).
    let p = run(2, 10, b"\x1b#6\x1b[1\"q\x1b[2J")?;
    assert_eq!(rendition(&p, 0), Some(Single), "ED after DECSCA 1");
    let p = run(2, 10, b"\x1bVx\x1bW\r\n\x1b#6\x1b[2J")?;
    assert_eq!(
        rendition(&p, 1),
        Some(Single),
        "ED of a line with no guarded glyph"
    );
    Ok(())
}

/// The rectangular area operations stop at a double-width line's end (DEC
/// STD 070, 5.6.1, and DECDWL, note 2: a line of double-width characters
/// holds half the columns): DECFRA over the whole width of a doubled line
/// fills its 5 columns of 10, as DECSWL then shows; DECRQCRA sums those 5.
#[test]
fn rectangles_stop_at_a_double_width_lines_end() -> Result {
    let p = run(4, 10, b"\x1b#6\x1b[88;1;1;1;10$x\x1b#5")?;
    assert_eq!(line(&p, 0), "XXXXX");
    let p = run(4, 10, b"\x1b#6\x1b[88;1;1;2;10$x\x1b#5")?;
    assert_eq!(first(&p, 2), ["XXXXX", "XXXXXXXXXX"]);
    // Five X's, 0x58 each: the sum's negation, 0x10000 - 5 x 0x58.
    let options = Options::default().set(Feature::RectangleChecksums, true);
    let mut p = Parser::with_options(Size::new(4, 10)?, 0, options)?;
    let mut replies = Vec::new();
    p.process_with_replies(b"\x1b#6XXXXX\x1b[1;1;1;1;1;10*y", |r| {
        replies.extend_from_slice(r)
    })?;
    assert_eq!(replies, b"\x1bP1!~FE48\x1b\\");
    Ok(())
}

/// The cursor stays on its line's columns when the lines move under it:
/// SU brings a double-width line to the cursor's row, and leaving the
/// alternate screen comes back to one, and the cursor goes back to its
/// last column, as setting the rendition takes it there (DECDWL, note 2).
#[test]
fn the_cursor_stays_on_a_double_width_line_brought_under_it() -> Result {
    let p = run(4, 80, b"\x1b[2H\x1b#6\x1b[1;71H\x1b[S")?;
    assert_eq!(rendition(&p, 0), Some(DoubleWidth));
    assert_eq!(p.screen().cursor_position(), (0, 39), "SU");
    let p = run(4, 80, b"\x1b#6\x1b[?47h\x1b[1;71H\x1b[?47l")?;
    assert_eq!(p.screen().cursor_position(), (0, 39), "the primary screen");
    Ok(())
}
