//! Protected glyphs and selective erase: DECSCA (`CSI Ps " q`), SPA and
//! EPA (`ESC V`, `ESC W`), DECSED (`CSI ? Ps J`) and DECSEL (`CSI ? Ps
//! K`). DEC protection (DEC STD 070, 5.11.1.2, Selectively Erasable
//! Character Attribute) keeps a glyph from DECSED and DECSEL only; ISO
//! protection (ECMA-48, SPA and EPA, with ERM reset: a guarded area) from
//! ED, EL and ECH. Each glyph keeps which it has, either or both, as each
//! standard knows its own alone. Expected values are xterm's (XTerm 411:
//! `fux-vt-compare replay --engines xterm --size 2x10 'BYTES'`) where the
//! standards agree with it; xterm keeps one kind for the whole terminal,
//! the last DECSCA's or SPA's, and has DECSED and DECSEL spare ISO's too.

use fux_vt::{Color, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

/// The screen's two rows after `bytes`, on a screen of two rows of ten.
fn after(bytes: &str) -> Result<Vec<String>> {
    let mut parser = Parser::new(Size::new(2, 10)?, 0)?;
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

/// SPA starts a guarded area, EPA ends it (ECMA-48 8.3.140 and 8.3.49,
/// with ERM, 7.2.6, reset): ED, EL and ECH leave its glyphs. DECSED and
/// DECSEL know DECSCA's attribute alone (DEC STD 070, 5.11.1.2), and erase
/// them, where xterm leaves them (esctest's DECSED_doesNotRespectISOProtect
/// expects so). A DECSCA inside the area protects its glyphs both ways.
#[test]
fn iso_protection_keeps_glyphs_from_ed_el_and_ech() -> Result {
    let row = "ab\x1bVcd\x1bWef";
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[K"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[?K"))?, ["", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[4X"))?, ["  cdef", ""]);
    let screen = format!("{row}\r\nxyz\x1b[2;2H");
    assert_eq!(after(&format!("{screen}\x1b[J"))?, ["abcdef", "x"]);
    assert_eq!(after(&format!("{screen}\x1b[1J"))?, ["  cd", "  z"]);
    assert_eq!(after(&format!("{screen}\x1b[2J"))?, ["  cd", ""]);
    assert_eq!(after("\x1bVab\x1b[1\"qcd\x1b[1;1H\x1b[2K")?, ["abcd", ""]);
    assert_eq!(after("\x1bVab\x1b[1\"qcd\x1b[1;1H\x1b[?2K")?, ["  cd", ""]);
    Ok(())
}

/// Each glyph keeps its own protection, whichever came last: the ISO
/// glyphs stay through ED and the DEC ones through DECSED, an unprotected
/// one through neither.
#[test]
fn each_glyph_keeps_its_own_protection() -> Result {
    let row = "\x1bVab\x1bW\x1b[1\"qcd\x1b[0\"qef";
    assert_eq!(after(&format!("{row}\x1b[2J"))?, ["ab", ""]);
    assert_eq!(after(&format!("{row}\x1b[?2J"))?, ["  cd", ""]);
    assert_eq!(after(&format!("{row}\x1b[1;1H\x1b[6X"))?, ["ab", ""]);
    Ok(())
}

/// What erasing leaves is the terminal's, set by the last DECSCA (any
/// DECSCA, 0 too) or SPA. An ED of the whole screen that finds no
/// protected glyph changes none of it: DECSCA's attribute stays until
/// another DECSCA, DECSTR or RIS (DEC STD 070, 5.11.1.2, and Soft
/// Terminal Reset), and SPA's guarded area until EPA (ECMA-48 8.3.140),
/// where xterm ends the protection (`do_erase_display`).
#[test]
fn an_erase_that_finds_no_protected_glyph_keeps_the_protection() -> Result {
    assert_eq!(after("\x1b[1\"q\x1b[?2Jab\x1b[1;1H\x1b[?2K")?, ["ab", ""]);
    assert_eq!(after("\x1bV\x1b[2Jab\x1b[1;1H\x1b[2K")?, ["ab", ""]);
    assert_eq!(
        after("\x1b[1\"q\x1b[?2Jab\x1b[0\"qcd\x1b[1;1H\x1b[?2K")?,
        ["ab", ""]
    );
    assert_eq!(
        after("\x1b[1\"qab\x1b[0\"q\x1b[?2J\x1b[1\"qcd\x1b[0\"q\x1b[1;1H\x1b[?2K")?,
        ["abcd", ""]
    );
    assert_eq!(
        after("\x1bVab\x1bW\x1b[2Jcd\x1b[1;1H\x1bVef\x1bW\x1b[1;1H\x1b[2K")?,
        ["ef", ""]
    );
    assert_eq!(
        after("\x1bVab\x1bW\x1b[2J\x1bVcd\x1bW\x1b[1;1H\x1b[2K")?,
        ["abcd", ""]
    );
    Ok(())
}

/// DECSC saves whether glyphs are protected, and DECRC restores it (the
/// VT510 manual, DECSC: "selective erase attribute"); DECSTR ends it, as
/// DEC STD 070's Soft Terminal Reset (`Current Attributes: not selective
/// erasable`) and the VT520 manual's table have it, the glyphs protected
/// before staying so (xterm, keeping one protection for the terminal,
/// erases them).
#[test]
fn the_saved_cursor_and_decstr() -> Result {
    assert_eq!(
        after("\x1b[1\"qab\x1b7\x1b[0\"qcd\x1b8ef\x1b[1;1H\x1b[?2K")?,
        ["abef", ""]
    );
    assert_eq!(after("\x1b[1\"qab\x1b[!pcd\x1b[1;1H\x1b[?2K")?, ["ab", ""]);
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
    let mut p = Parser::new(Size::new(4, 10)?, 0)?;
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
        after("\x1bVa\u{4e00}b\x1bWc\x1b[1;1H\x1b[2K")?,
        ["a\u{4e00}b", ""]
    );
    assert_eq!(
        after("\x1b[1\"qa\u{4e00}b\x1b[0\"qc\x1b[1;1H\x1b[?2K")?,
        ["a\u{4e00}b", ""]
    );
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
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
