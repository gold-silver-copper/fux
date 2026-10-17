//! The rectangular area operations of DEC STD 070, 5.12.1 (VT420, level
//! 4): DECFRA (`CSI Pch ; Pt ; Pl ; Pb ; Pr $ x`), DECERA (`$ z`), DECSERA
//! (`$ {`), DECCARA (`$ r`), DECRARA (`$ t`), DECCRA (`$ v`) and DECSACE
//! (`CSI Ps * x`). Each expected screen is worked out by hand from the
//! standard's text, which each test quotes.

use fux_vt::{Feature, Options, Parser, Size};
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

/// DEC STD 070, 5.12.1, DECCARA sets the renditions SGR's 1, 4, 5 and 7
/// name over the area, and 22, 24, 25, 27 or 0 (the default) end them;
/// DECRARA reverses those 1, 4, 5 and 7 name, or all four for 0. The
/// characters stay. With DECSACE 0 or 1 (the default) the area is the
/// stream of positions from the first to the last, its unoccupied
/// positions unaffected; with 2 the rectangle, whose unoccupied positions
/// become blanks and are affected.
#[test]
fn deccara_and_decrara_change_renditions() -> Result {
    let bold = |p: &Parser| -> Vec<String> {
        let screen = p.screen();
        (0..4)
            .map(|y| {
                (0..6)
                    .map(|x| match screen.cell(y, x) {
                        Some(c) if c.attributes().bold() => 'B',
                        Some(c) if c.has_contents() => '.',
                        _ => '_',
                    })
                    .collect()
            })
            .collect()
    };
    // A stream from (1, 2) to (3, 5), and a rectangle; in either, a left
    // past the right is ignored.
    let short = "\x1b[2;4H\x1b[K";
    let stream = after(&format!("{short}\x1b[1;2;3;5;1$r"))?;
    assert_eq!(bold(&stream), [".BBBBB", "BBB___", "BBBBB.", "......"]);
    let inverted = after(&format!("{short}\x1b[1;5;3;2;1$r"))?;
    assert_eq!(bold(&inverted), ["......", "...___", "......", "......"]);
    let rectangle = after(&format!("{short}\x1b[2*x\x1b[1;2;3;5;1$r"))?;
    assert_eq!(bold(&rectangle), [".BBBB.", ".BBBB_", ".BBBB.", "......"]);
    assert_eq!(lines(&rectangle), ["abcdef", "ghi", "mnopqr", "stuvwx"]);
    // DECCARA 0, its default, ends what 1 set; DECRARA reverses.
    let ended = after("\x1b[2*x\x1b[1;1;2;6;1$r\x1b[1;2;1;3$r")?;
    assert_eq!(bold(&ended), ["B..BBB", "BBBBBB", "......", "......"]);
    let reversed = after("\x1b[2*x\x1b[1;1;1;3;1$r\x1b[1;2;1;4;1$t")?;
    assert_eq!(bold(&reversed), ["B..B..", "......", "......", "......"]);
    let parser = after("\x1b[2*x\x1b[1;1;1;1;4;7$r\x1b[1;1;1;2;0$t")?;
    let screen = parser.screen();
    let a = |x| screen.cell(0, x).map(|c| c.attributes());
    assert!(a(0).is_some_and(|a| !a.underline() && !a.inverse() && a.bold()));
    assert!(a(1).is_some_and(|a| a.underline() && a.inverse() && a.bold()));
    // DECRARA with no rendition does nothing; colours are none.
    assert_eq!(
        bold(&after("\x1b[1;1;4;6$t\x1b[1;1;4;6;31$r")?),
        bold(&after("")?)
    );
    Ok(())
}

/// DEC STD 070, 5.12.1, DECSACE: 0 and 1 select the stream, 2 the
/// rectangle; DECRQSS reports it.
#[test]
fn decsace_is_reported() -> Result {
    let options = Options::new().with(Feature::SettingReports);
    let mut parser = Parser::with_options(Size::new(2, 4)?, 0, options)?;
    let mut replies = Vec::new();
    let ask = b"\x1bP$q*x\x1b\\";
    for set in [&b"\x1b[2*x"[..], b"\x1b[1*x", b"\x1b[2*x\x1b[7*x"] {
        parser.process_with_replies(set, |_| {})?;
        parser.process_with_replies(ask, |r| replies.push(r.to_vec()))?;
    }
    let reply = |n: u8| format!("\x1bP1$r{}*x\x1b\\", char::from(n)).into_bytes();
    assert_eq!(replies, [reply(b'2'), reply(b'0'), reply(b'2')]);
    Ok(())
}
