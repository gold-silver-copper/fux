//! Protected glyphs and selective erase: DECSCA (`CSI Ps " q`), SPA and
//! EPA (`ESC V`, `ESC W`), DECSED (`CSI ? Ps J`) and DECSEL (`CSI ? Ps
//! K`). DEC protection (DEC STD 070, 5.11.1.2, Selectively Erasable
//! Character Attribute) keeps a glyph from DECSED and DECSEL only; ISO
//! protection (ECMA-48, SPA and EPA, with ERM reset) from every erase, ED,
//! EL and ECH too, and, as xterm has it for compatibility, DECSED and
//! DECSEL. Expected values are xterm's (XTerm 411): each case was replayed
//! in xterm beside fux-vt (`fux-vt-compare replay --engines xterm --size
//! 2x10 'BYTES'`) and the two agree.

use fux_vt::{Color, Parser};
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

/// The screen's two rows after `bytes`, on a screen of two rows of ten.
fn after(bytes: &str) -> std::result::Result<Vec<String>, fux_vt::Error> {
    let mut parser = Parser::new(2, 10, 0)?;
    parser.process(bytes.as_bytes())?;
    Ok(lines(&parser))
}

/// DECSCA 1 protects the glyphs printed after it, 0 and 2 (and none) end
/// it, as xterm reads it (`CASE_DECSCA`; other values leave the pen as it
/// was). DECSEL and DECSED leave protected glyphs, in each of their modes;
/// EL, ED and ECH erase them, DEC protection being for selective erase
/// alone (DEC STD 070, 5.11.1.2; the VT510 manual, DECSCA).
#[test]
fn dec_protection_keeps_glyphs_from_selective_erase() -> Result {
    let row = "ab\x1b[1\"qcd\x1b[0\"qef";
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[?K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[?0K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;4H\x1b[?1K"))?, ["  cdef", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;5H\x1b[?2K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[K"))?, ["", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;3H\x1b[2X"))?, ["ab  ef", ""]);
    let screen = format!("{row}\r\nxyz\x1b[2;2H");
    assert_eq!(after(&format!("{screen}\x1b[?J"))?, ["abcdef", "x"]);
    assert_eq!(after(&format!("{screen}\x1b[?1J"))?, ["  cd", "  z"]);
    assert_eq!(after(&format!("{screen}\x1b[?2J"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{screen}\x1b[2J"))?, ["", ""]);
    // DECSCA 2 ends it as 0 does; 5, unknown, leaves it on.
    assert_eq!(
        after("ab\x1b[1\"qcd\x1b[2\"qef\x1b[1;3H\x1b[?0K")?,
        ["abcd", ""]
    );
    assert_eq!(
        after("\x1b[1\"qab\x1b[5\"qcd\x1b[1;1H\x1b[?K")?,
        ["abcd", ""]
    );
    Ok(())
}

/// SPA starts glyphs protected from every erase, EPA ends them (ECMA-48
/// 8.3.140 and 8.3.49, with ERM, 7.2.6, reset): ED, EL and ECH leave them,
/// and so do DECSED and DECSEL, as xterm has it (esctest marks that a
/// known xterm bug, DECSED_doesNotRespectISOProtect). A DECSCA after SPA
/// makes the protection DEC's again: EL erases them then.
#[test]
fn iso_protection_keeps_glyphs_from_every_erase() -> Result {
    let row = "ab\x1bVcd\x1bWef";
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[?K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[4X"))?, ["  cdef", ""]);
    let screen = format!("{row}\r\nxyz\x1b[2;2H");
    assert_eq!(after(&format!("{screen}\x1b[J"))?, ["abcdef", "x"]);
    assert_eq!(after(&format!("{screen}\x1b[1J"))?, ["  cd", "  z"]);
    assert_eq!(after(&format!("{screen}\x1b[2J"))?, ["  cd", ""]);
    assert_eq!(after("\x1bVab\x1b[1\"qcd\x1b[1;1H\x1b[2K")?, ["", ""]);
    Ok(())
}

/// What erasing leaves is the terminal's, set by the last DECSCA (any
/// DECSCA, 0 too) or SPA, and, as in xterm (`do_erase_display`), an ED of
/// the whole screen that finds no protected glyph ends it: glyphs
/// protected after that are erased by every erase, until the next DECSCA
/// or SPA.
#[test]
fn an_erase_that_finds_no_protected_glyph_ends_the_protection() -> Result {
    assert_eq!(after("\x1b[1\"q\x1b[?2Jab\x1b[1;1H\x1b[?2K")?, ["", ""]);
    assert_eq!(
        after("\x1b[1\"q\x1b[?2Jab\x1b[0\"qcd\x1b[1;1H\x1b[?2K")?,
        ["ab", ""]
    );
    assert_eq!(
        after("\x1b[1\"qab\x1b[0\"q\x1b[?2J\x1b[1\"qcd\x1b[0\"q\x1b[1;1H\x1b[?2K")?,
        ["abcd", ""]
    );
    assert_eq!(
        after("\x1bVab\x1bW\x1b[2Jcd\x1b[1;1H\x1bVef\x1bW\x1b[1;1H\x1b[?2K")?,
        ["ef", ""]
    );
    assert_eq!(
        after("\x1bVab\x1bW\x1b[2J\x1bVcd\x1bW\x1b[1;1H\x1b[2K")?,
        ["abcd", ""]
    );
    Ok(())
}

/// DECSC saves whether glyphs are protected, and DECRC restores it (the
/// VT510 manual, DECSC: "selective erase attribute"); DECSTR ends it and
/// the protection, as the VT520 manual's table and xterm have it, the
/// glyphs protected before staying so.
#[test]
fn the_saved_cursor_and_decstr() -> Result {
    assert_eq!(
        after("\x1b[1\"qab\x1b7\x1b[0\"qcd\x1b8ef\x1b[1;1H\x1b[?2K")?,
        ["abef", ""]
    );
    assert_eq!(after("\x1b[1\"qab\x1b[!pcd\x1b[1;1H\x1b[?2K")?, ["", ""]);
    assert_eq!(
        after("\x1b[1\"qab\x1b[!p\x1b[1\"qcd\x1b[1;1H\x1b[?2K")?,
        ["abcd", ""]
    );
    Ok(())
}

/// A protected glyph keeps its protection where it goes: ICH, DCH, IL and
/// IRM move it with it; REP repeats it protected; a glyph printed over it
/// takes the pen's. A wide glyph's second half goes with its first. The
/// blanks an erase leaves take the pen's colours (`bce`), as ED's do.
#[test]
fn protection_goes_with_the_glyph() -> Result {
    assert_eq!(
        after(
            "\x1b[1\"qab\x1b[0\"qcd\x1b[1;1H\x1b[2@\x1b[2;1H\x1b[1\"qxy\x1b[0\"q\x1b[2;1H\x1b[P\x1b[?K"
        )?,
        ["  abcd", "y"]
    );
    let mut p = Parser::new(4, 10, 0)?;
    p.process(b"\x1b[1\"qab\x1b[0\"qcd\x1b[2;1H\x1b[1\"qxy\x1b[0\"q\x1b[1;1H\x1b[L\x1b[?J")?;
    assert_eq!(lines(&p), ["", "ab", "xy", ""]);
    assert_eq!(
        after("\x1b[1\"qa\x1b[3b\x1b[0\"qcd\x1b[1;1H\x1b[?K")?,
        ["aaaa", ""]
    );
    assert_eq!(
        after("\x1b[1\"qab\x1b[0\"qcd\x1b[1;1Hx\x1b[?K")?,
        ["xb", ""]
    );
    assert_eq!(
        after("\x1b[1\"qab\x1b[0\"qcd\x1b[1;1H\x1b[4h\x1b[1\"qxy\x1b[0\"q\x1b[4l\x1b[?K")?,
        ["xyab", ""]
    );
    assert_eq!(
        after("\x1bVa\u{4e00}b\x1bWc\x1b[1;1H\x1b[?2K\x1b[1;1H\x1b[2K")?,
        ["a\u{4e00}b", ""]
    );
    let mut p = Parser::new(2, 10, 0)?;
    p.process(b"\x1b[44m\x1bVab\x1bWcd\x1b[1;1H\x1b[2K")?;
    assert_eq!(lines(&p), ["ab", ""]);
    let blue = |x: u16| {
        p.screen()
            .cell(0, x)
            .map(|c| (c.has_contents(), c.bgcolor()))
    };
    assert_eq!(blue(2), Some((false, Color::Idx(4))));
    Ok(())
}
