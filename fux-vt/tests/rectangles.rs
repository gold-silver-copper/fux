//! The rectangular area operations of DEC STD 070, 5.12.1 (VT420, level
//! 4): DECFRA (`CSI Pch ; Pt ; Pl ; Pb ; Pr $ x`), DECERA (`$ z`), DECSERA
//! (`$ {`), DECCRA (`$ v`), DECCARA (`$ r`), DECRARA (`$ t`) and DECSACE
//! (`* x`). Each expected screen is worked out by hand from the
//! standard's text, which each test quotes.

use fux_vt::{Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

/// Four rows of `abcdef`, `ghijkl`, `mnopqr` and `stuvwx`, then `bytes`,
/// on a screen of four rows of six.
fn after(bytes: &str) -> Result<Parser> {
    let mut parser = Parser::new(Size::new(4, 6)?, 0)?;
    parser.process(b"abcdef\r\nghijkl\r\nmnopqr\r\nstuvwx\x1b[2;3H")?;
    parser.process(bytes.as_bytes())?;
    Ok(parser)
}

fn rows(bytes: &str) -> Result<Vec<String>> {
    Ok(lines(&after(bytes)?))
}

/// DEC STD 070, 5.12.1, DECFRA: the rectangle `Pt ; Pl ; Pb ; Pr` is
/// filled with the character Pch names in GL (32 to 126) or GR (160 to
/// 255) in the pen's rendition; another Pch, or a top below the bottom or
/// a left past the right, has the control ignored. Defaults are Pch 32
/// and the whole page; a value past the page is its edge. The cursor
/// stays, and the margins bound nothing unless origin mode is set, when
/// the coordinates count from them.
#[test]
fn decfra_fills_a_rectangle() -> Result {
    assert_eq!(
        rows("\x1b[37;2;2;3;4$x")?,
        ["abcdef", "g%%%kl", "m%%%qr", "stuvwx"]
    );
    // Invalid: inverted, or a Pch out of range.
    let whole = ["abcdef", "ghijkl", "mnopqr", "stuvwx"];
    assert_eq!(rows("\x1b[37;3;2;2;4$x")?, whole);
    assert_eq!(rows("\x1b[37;2;4;3;2$x")?, whole);
    assert_eq!(rows("\x1b[127;1;1;1;1$x")?, whole);
    assert_eq!(rows("\x1b[31;1;1;1;1$x")?, whole);
    // Defaults: a space over the page; past the page is its edge.
    assert_eq!(rows("\x1b[$x")?, ["", "", "", ""]);
    assert_eq!(
        rows("\x1b[88;3;5;99;99$x")?,
        ["abcdef", "ghijkl", "mnopXX", "stuvXX"]
    );
    // GR's 233 is é; GL's 113 in DEC Special Graphics a line.
    assert_eq!(
        rows("\x1b[233;1;1;1;1$x")?,
        ["ébcdef", "ghijkl", "mnopqr", "stuvwx"]
    );
    assert_eq!(
        rows("\x1b(0\x1b[113;1;1;1;2$x")?,
        ["──cdef", "ghijkl", "mnopqr", "stuvwx"]
    );
    // The pen's rendition, and the cursor stays.
    let parser = after("\x1b[1m\x1b[88;1;1;1;1$x")?;
    let screen = parser.screen();
    assert!(screen.cell(0, 0).is_some_and(|c| c.attributes().bold()));
    assert_eq!(screen.cursor_position(), (1, 2));
    // Margins bound nothing; with origin mode the rectangle counts from
    // them and is clamped to them.
    let margins = "\x1b[2;3r\x1b[?69h\x1b[2;4s";
    assert_eq!(
        rows(&format!("{margins}\x1b[88;1;1;1;1$x"))?,
        ["Xbcdef", "ghijkl", "mnopqr", "stuvwx"]
    );
    assert_eq!(
        rows(&format!("{margins}\x1b[?6h\x1b[88;1;1;9;9$x"))?,
        ["abcdef", "gXXXkl", "mXXXqr", "stuvwx"]
    );
    Ok(())
}

/// DEC STD 070, 5.12.1, DECERA erases the rectangle, every character
/// replaced with a space; DECSERA erases only the characters DECSCA does
/// not protect, the others staying, and "does not change or clear any
/// video character attributes". SPA's guard is no DECSCA protection, so
/// DECSERA erases it (as DECSED does, 5.11.1.2).
#[test]
fn decera_and_decsera_erase_a_rectangle() -> Result {
    assert_eq!(
        rows("\x1b[2;2;3;4$z")?,
        ["abcdef", "g   kl", "m   qr", "stuvwx"]
    );
    let protected = "\x1b[1;1H\x1b[1\"qab\x1b[0\"qc\x1bVd\x1bWe";
    assert_eq!(
        rows(&format!("{protected}\x1b[1;1;1;6${{"))?,
        ["ab", "ghijkl", "mnopqr", "stuvwx"]
    );
    assert_eq!(
        rows(&format!("{protected}\x1b[1;1;1;6$z"))?,
        ["", "ghijkl", "mnopqr", "stuvwx"]
    );
    assert_eq!(
        rows("\x1b[1;5;2;6${")?,
        ["abcd", "ghij", "mnopqr", "stuvwx"]
    );
    // The erased keep their attributes; empty positions stay empty.
    let parser = after("\x1b[1;1H\x1b[1mab\x1b[m\x1b[2;4H\x1b[K\x1b[1;1;2;6${")?;
    let screen = parser.screen();
    let erased = screen.cell(0, 0);
    assert!(erased.is_some_and(|c| c.contents() == " " && c.attributes().bold()));
    assert!(screen.cell(1, 4).is_some_and(|c| !c.has_contents()));
    // DECERA "clearing all character attributes": each character becomes
    // a space, an occupied position, and "any unoccupied character
    // positions remain unoccupied".
    let parser = after("\x1b[1;1H\x1b[1mab\x1b[m\x1b[2;4H\x1b[K\x1b[1;1;2;6$z")?;
    let screen = parser.screen();
    let space = |c: &fux_vt::CellRef| c.contents() == " " && !c.attributes().bold();
    assert!(screen.cell(0, 0).is_some_and(|c| space(&c)));
    assert!(screen.cell(1, 2).is_some_and(|c| space(&c)));
    assert!(screen.cell(1, 4).is_some_and(|c| !c.has_contents()));
    Ok(())
}

/// DEC STD 070, 5.12.1: the operations change no line, so a row they reach
/// the last column of stays soft-wrapped, as xterm keeps it (`replay
/// --engines xterm --size 2x3 'abcdef\e[1;1;1;3$z'`).
#[test]
fn rectangles_keep_soft_wraps() -> Result {
    for op in ["\x1b[88;1;1;1;3$x", "\x1b[1;1;1;3$z", "\x1b[1;1;1;3${"] {
        let mut parser = Parser::new(Size::new(2, 3)?, 0)?;
        parser.process(format!("abcdef{op}").as_bytes())?;
        let rows: Vec<bool> = parser.screen().rows().map(|r| r.wrapped()).collect();
        assert_eq!(rows, [true, false], "{op:?}");
    }
    Ok(())
}

/// DEC STD 070, 5.12.1, DECCRA copies the characters and attributes of
/// the rectangle `Pts ; Pls ; Pbs ; Prs` to `Ptd ; Pld` (the pages are
/// one here), clipped where the destination leaves the page; the cursor
/// stays. The source is read whole first, so the two may overlap.
#[test]
fn deccra_copies_a_rectangle() -> Result {
    assert_eq!(
        rows("\x1b[1;1;2;2;1;3;4;1$v")?,
        ["abcdef", "ghijkl", "mnoabr", "stughx"]
    );
    // Overlapping, down and right by one.
    assert_eq!(
        rows("\x1b[1;1;2;3;1;2;2;1$v")?,
        ["abcdef", "gabckl", "mghiqr", "stuvwx"]
    );
    // Clipped at the page's corner; an inverted source does nothing.
    assert_eq!(
        rows("\x1b[1;1;2;2;1;4;6;1$v")?,
        ["abcdef", "ghijkl", "mnopqr", "stuvwa"]
    );
    assert_eq!(
        rows("\x1b[2;1;1;2;1;3;3;1$v")?,
        ["abcdef", "ghijkl", "mnopqr", "stuvwx"]
    );
    // Attributes go with the characters; the cursor stays.
    let parser = after("\x1b[1;1H\x1b[7mA\x1b[1;1;1;1;1;4;1;1$v")?;
    let screen = parser.screen();
    let cell = screen.cell(3, 0);
    assert!(cell.is_some_and(|c| c.contents() == "A" && c.attributes().inverse()));
    assert_eq!(screen.cursor_position(), (0, 1));
    Ok(())
}

/// DECCRA blanks a wide glyph the rectangle cuts, at the source's edges
/// as at the destination's: half a glyph copied is no glyph, so it never
/// joins a half that was there (row 1's `世`) nor spills past the
/// rectangle into one (row 1's `界`).
#[test]
fn deccra_blanks_a_wide_glyph_it_cuts() -> Result {
    let mut parser = Parser::new(Size::new(2, 4)?, 0)?;
    parser.process("\u{4e16}\r\nx\u{4e2d}\x1b[2;3;2;3;1;1;2;1$v".as_bytes())?;
    assert_eq!(lines(&parser), ["", "x\u{4e2d}"]);
    let mut parser = Parser::new(Size::new(2, 4)?, 0)?;
    parser.process("ab\u{754c}\r\n\u{4e2d}\x1b[2;1;2;1;1;1;3;1$v".as_bytes())?;
    assert_eq!(lines(&parser), ["ab", "\u{4e2d}"]);
    // `界`'s first half over `語`'s, whose second half stays.
    let mut parser = Parser::new(Size::new(2, 4)?, 0)?;
    parser.process("\u{754c}\r\n\u{8a9e}\x1b[1;1;1;1;1;2;1;1$v".as_bytes())?;
    assert_eq!(lines(&parser), ["\u{754c}", ""]);
    Ok(())
}

/// The renditions of row `y`, columns `cols`, as `(bold, underline,
/// blink, inverse)`, `None` for an unoccupied position.
fn renditions(p: &Parser, y: u16, cols: std::ops::Range<u16>) -> Vec<Option<[bool; 4]>> {
    cols.map(|x| {
        p.screen().cell(y, x).filter(|c| c.has_contents()).map(|c| {
            let a = c.attributes();
            [
                a.bold(),
                a.underline(),
                a.blink() != fux_vt::Blink::None,
                a.inverse(),
            ]
        })
    })
    .collect()
}

const PLAIN: Option<[bool; 4]> = Some([false; 4]);
const BOLD: Option<[bool; 4]> = Some([true, false, false, false]);

/// DEC STD 070, 5.12.1, DECCARA (`CSI Pt ; Pl ; Pb ; Pr ; Ps … $ r`): the
/// renditions Ps names (0 all off, 1 bold, 4 underscore, 5 blink, 7
/// negative, 22, 24, 25 and 27 each off, and the Color Text Extension's
/// colours, 5.6.3) set in each position of the area, "without altering
/// any characters"; several are "cumulative, as if several DECCARA control
/// functions were received and executed in sequence"; "if no valid
/// subsequent parameter is received", 0; other values are ignored. The
/// cursor and the pen stay. Checked against a second reader given only the
/// standard's pages.
#[test]
fn deccara_changes_the_renditions_of_an_area() -> Result {
    let p = after("\x1b[1;2;1;4;1$r")?;
    assert_eq!(renditions(&p, 0, 0..5), [PLAIN, BOLD, BOLD, BOLD, PLAIN]);
    assert_eq!(lines(&p).first().map(String::as_str), Some("abcdef"));
    assert_eq!(p.screen().cursor_position(), (1, 2));
    let p = after("\x1b[1;2;1;4;1;4;7$r\x1b[1;3;1;3;27$r")?;
    let all = Some([true, true, false, true]);
    assert_eq!(
        renditions(&p, 0, 1..4),
        [all, Some([true, true, false, false]), all]
    );
    // None, or none valid, is 0.
    assert_eq!(
        renditions(&after("\x1b[1;1;1;2;1$r\x1b[1;1;1;2$r")?, 0, 0..2),
        [PLAIN; 2]
    );
    assert_eq!(
        renditions(&after("\x1b[1;1;1;2;1$r\x1b[1;1;1;2;9$r")?, 0, 0..2),
        [PLAIN; 2]
    );
    // The pen is not changed: text printed after it is plain.
    let p = after("\x1b[1;1;1;1;1$rX")?;
    assert_eq!(renditions(&p, 1, 2..3), [PLAIN]);
    // Colours.
    let p = after("\x1b[1;1;1;1;31;44$r")?;
    let a = p.screen().cell(0, 0).map(|c| c.attributes());
    assert_eq!(
        a.map(|a| (a.foreground(), a.background())),
        Some((fux_vt::Color::Idx(1), fux_vt::Color::Idx(4)))
    );
    // An area whose left is past its right is ignored.
    assert_eq!(renditions(&after("\x1b[1;5;2;2;1$r")?, 0, 0..6), [PLAIN; 6]);
    Ok(())
}

/// DEC STD 070, DECSACE (`CSI Ps * x`): with 0 or 1, the default, DECCARA
/// and DECRARA change "the stream of character positions beginning with
/// the first character position specified in the command, and ending with
/// the second", whole lines between, and its unoccupied positions "are
/// not affected"; with 2, the rectangle, whose unoccupied positions "are
/// changed to blanks and are affected".
#[test]
fn decsace_selects_a_stream_or_a_rectangle() -> Result {
    let stream = after("\x1b[1;2;2;3;1$r")?;
    assert_eq!(
        renditions(&stream, 0, 0..6),
        [PLAIN, BOLD, BOLD, BOLD, BOLD, BOLD]
    );
    assert_eq!(renditions(&stream, 1, 0..4), [BOLD, BOLD, BOLD, PLAIN]);
    let rectangle = after("\x1b[2*x\x1b[1;2;2;3;1$r")?;
    assert_eq!(renditions(&rectangle, 0, 0..4), [PLAIN, BOLD, BOLD, PLAIN]);
    assert_eq!(renditions(&rectangle, 1, 0..4), [PLAIN, BOLD, BOLD, PLAIN]);
    // Unoccupied positions.
    let mut p = Parser::new(Size::new(3, 6)?, 0)?;
    p.process(b"AB\x1b[1;1;2;3;4$r")?;
    let under = Some([false, true, false, false]);
    assert_eq!(renditions(&p, 0, 0..3), [under, under, None]);
    assert_eq!(renditions(&p, 1, 0..1), [None]);
    let mut p = Parser::new(Size::new(3, 6)?, 0)?;
    p.process(b"\x1b[2*xAB\x1b[1;1;2;3;4$r")?;
    assert_eq!(renditions(&p, 0, 0..4), [under, under, under, None]);
    assert_eq!(renditions(&p, 1, 0..3), [under; 3]);
    assert_eq!(
        lines(&p).into_iter().take(2).collect::<Vec<_>>(),
        ["AB", ""]
    );
    // DECSACE 3 is none of them: the rectangle stays.
    let p = after("\x1b[2*x\x1b[3*x\x1b[1;2;2;3;1$r")?;
    assert_eq!(renditions(&p, 0, 0..4), [PLAIN, BOLD, BOLD, PLAIN]);
    Ok(())
}

/// DEC STD 070, 5.12.1, DECRARA (`CSI Pt ; Pl ; Pb ; Pr ; Ps … $ t`): the
/// renditions Ps names reversed (0 all four, 1 bold, 4 underscore, 5
/// blink, 7 negative), cumulatively; "if no valid subsequent parameter is
/// received, the command is ignored", and colours, whose reverse "is not
/// defined", are no valid parameter.
#[test]
fn decrara_reverses_the_renditions_of_an_area() -> Result {
    let p = after("\x1b[1;1;1;1;4$r\x1b[1;1;1;2;0;1$t")?;
    assert_eq!(
        renditions(&p, 0, 0..2),
        [
            Some([false, false, true, true]),
            Some([false, true, true, true])
        ]
    );
    assert_eq!(renditions(&after("\x1b[1;1;1;2$t")?, 0, 0..2), [PLAIN; 2]);
    assert_eq!(
        renditions(&after("\x1b[1;1;1;2;31$t")?, 0, 0..2),
        [PLAIN; 2]
    );
    assert_eq!(
        renditions(&after("\x1b[1;1;1;2;1;1$t")?, 0, 0..2),
        [PLAIN; 2]
    );
    Ok(())
}
