//! Left and right margins: DECLRMM (`CSI ? 69 h`) and DECSLRM (`CSI Pl ;
//! Pr s`), and every operation they bound, as DEC STD 070 (5.4.3, "Margins
//! And Scrolling"; DECSLRM, DECLRMM), the VT510 manual and xterm's ctlseqs
//! have them. Expected values are xterm's (XTerm 411): each case was
//! replayed in xterm beside fux-vt (`fux-vt-compare replay --engines
//! xterm --size RxC 'BYTES'`, the bytes as the test gives them) and the two
//! agree, cell for cell, cursor and reports. Where xterm departs from DEC
//! STD 070, the test says so and follows xterm.

use fux_vt::{Color, Options, Parser};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

fn run(rows: u16, cols: u16, bytes: &[u8]) -> std::result::Result<Parser, fux_vt::Error> {
    let mut parser = Parser::new(rows, cols, 0)?;
    parser.process(bytes)?;
    Ok(parser)
}

/// The replies to `bytes`, with DECRQM and DECXCPR answered.
fn replies(rows: u16, cols: u16, bytes: &[u8]) -> std::result::Result<Vec<String>, fux_vt::Error> {
    let options = Options::new().with_extended_replies(true);
    let mut parser = Parser::with_options(rows, cols, 0, options)?;
    let mut replies = Vec::new();
    parser.process_with_replies(bytes, |r| {
        replies.push(String::from_utf8_lossy(r).into_owned());
    })?;
    Ok(replies)
}

/// Six rows of twelve, five of them full, the margins those of most cases
/// here: columns 3 to 8, lines 2 to 5 (`CSI 3 ; 8 s`, `CSI 2 ; 5 r`).
const FULL: &str = "ABCDEFGHIJKL\r\nMNOPQRSTUVWX\r\nabcdefghijkl\r\nmnopqrstuvwx\r\n012345678901";
const MARGINS: &str = "\x1b[?69h\x1b[3;8s\x1b[2;5r";

fn framed(then: &str) -> std::result::Result<Parser, fux_vt::Error> {
    run(6, 12, format!("{FULL}{MARGINS}{then}").as_bytes())
}

/// DECSLRM is recognized only while DECLRMM is set (DEC STD 070, DECSLRM,
/// note 4): reset, `CSI s` is SCOSC, saving the cursor, as it always was.
/// Set, margins with the left one left of the right are set and the cursor
/// goes home, obeying DECOM; others are ignored (note 2). A right margin
/// past the screen is its last column, as xterm reads it, where DEC STD
/// 070 ignores the sequence (note 3), as fux-vt reads DECSTBM. Resetting
/// DECLRMM puts the margins back at the screen's edges, and DECRQM reports
/// the mode (esctest's DECSET_DECLRMM, DECRQM_DEC_DECLRMM).
#[test]
fn declrmm_lets_decslrm_set_the_margins() -> Result {
    // Without DECLRMM, CSI s is SCOSC.
    let p = run(3, 8, b"\x1b[2;3H\x1b[s\x1b[1;1H\x1b[uX")?;
    assert_eq!(lines(&p), ["", "  X", ""]);
    // With it, the margins: printing wraps at the right one to the left one.
    let p = run(3, 8, b"\x1b[?69h\x1b[2;4sabcdefgh\x1b[?69l")?;
    assert_eq!(lines(&p), ["abcd", " efg", " h"]);
    // Reset, the margins are the screen's edges again.
    let p = run(3, 8, b"\x1b[?69h\x1b[2;4s\x1b[?69l\x1b[1;1HABCDEFGH")?;
    assert_eq!(lines(&p), ["ABCDEFGH", "", ""]);
    // Home, and from the left margin in origin mode.
    let p = run(
        3,
        8,
        b"\x1b[?69h\x1b[2;3HX\x1b[3;5sY\x1b[?6h\x1b[2;5s\x1b[1;1HZ",
    )?;
    assert_eq!(lines(&p), ["YZ", "  X", ""]);
    // Left not left of right: ignored, the cursor staying.
    let p = run(3, 8, b"\x1b[?69h\x1b[2;3H\x1b[4;4sX\x1b[5;2sY")?;
    assert_eq!(lines(&p), ["", "  XY", ""]);
    // A right margin past the screen is its last column.
    let p = run(3, 8, b"\x1b[?69h\x1b[3;99sabcdefghij")?;
    assert_eq!(lines(&p), ["abcdefgh", "  ij", ""]);
    // DECRQM.
    let report = b"\x1b[?69$p\x1b[?69h\x1b[?69$p";
    assert_eq!(replies(3, 8, report)?, ["\x1b[?69;2$y", "\x1b[?69;1$y"]);
    Ok(())
}

/// DEC STD 070, 5.4.3: the right margin bounds printing while the cursor
/// is not past it; a glyph past it wraps to the left margin of the next
/// line, which is marked soft-wrapped, as xterm marks it (Ghostty does
/// not). From the cursor right of the right margin, the line ends at the
/// screen's edge, and the next one starts at the left margin; on the bottom
/// margin there, the line feed moves nothing (xterm's `dotext` and
/// `WrapLine`). With DECAWM off, glyphs overwrite the right margin's cell
/// (esctest's DECSET_DECAWM_OffRespectsLeftRightMargin).
#[test]
fn printing_wraps_at_the_right_margin() -> Result {
    let p = run(4, 10, b"\x1b[?69h\x1b[2;5s\x1b[3;1Hxxxxxxxxxxxxxxxxxxxx")?;
    assert_eq!(lines(&p), [" xxxx", " xxxx", "xxxxx", " xxx"]);
    assert!(p.screen().row_wrapped(2));
    // Right of the right margin, at the bottom: the next line is the same.
    let p = run(4, 10, b"\x1b[?69h\x1b[2;5s\x1b[4;9Hxyzwuv")?;
    assert_eq!(lines(&p), ["", "", "", " zwuv   xy"]);
    // The bottom margin above the last row: the region scrolls, and the
    // row at the margin is soft-wrapped, keeping its flag as it stays.
    let p = run(5, 10, b"\x1b[?69h\x1b[2;5s\x1b[2;4r\x1b[4;3Hxxxxxxxxxxxxxx")?;
    assert_eq!(lines(&p), ["", " xxxx", " xxxx", " xxx", ""]);
    assert!(p.screen().row_wrapped(3) && !p.screen().row_wrapped(2));
    // DECAWM off.
    let p = run(6, 12, b"\x1b[?69h\x1b[5;9s\x1b[5;9r\x1b[5;8H\x1b[?7labcdef")?;
    assert_eq!(lines(&p).get(4).map(String::as_str), Some("       af"));
    assert_eq!(p.screen().cursor_position(), (4, 8));
    Ok(())
}

/// IND, LF, VT, FF, NEL and RI (DEC STD 070, 5.4.3, the table of controls
/// the margins affect): at the bottom (top) margin, with the cursor between
/// the left and right margins, the region between all four margins
/// scrolls; outside them the cursor stays and nothing scrolls. NEL's
/// carriage return goes to the left margin, as CR's does.
#[test]
fn index_scrolls_between_the_margins() -> Result {
    let p = framed("\x1b[5;4H\n")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MNcdefghUVWX",
            "abopqrstijkl",
            "mn234567uvwx",
            "01      8901",
            "",
        ]
    );
    let p = framed("\x1b[2;5H\x1bM")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MN      UVWX",
            "abOPQRSTijkl",
            "mncdefghuvwx",
            "01opqrst8901",
            "",
        ]
    );
    // Outside the left and right margins: no scroll.
    let p = framed("\x1b[2;10H\x1bMX")?;
    assert_eq!(lines(&p).get(1).map(String::as_str), Some("MNOPQRSTUXWX"));
    let p = framed("\x1b[5;10H\nX\x1bDY\x1bEZ")?;
    assert_eq!(lines(&p).get(4).map(String::as_str), Some("01Z345678XY1"));
    for feed in ["\x0b", "\x0c"] {
        let p = framed(&format!("\x1b[5;10H{feed}X"))?;
        assert_eq!(lines(&p).get(4).map(String::as_str), Some("012345678X01"));
    }
    Ok(())
}

/// SU and SD scroll the region between all four margins, wherever the
/// cursor is; IL and DL only with the cursor between the margins, which
/// they leave at the left margin (DEC STD 070, IL and DL; xterm's
/// `InsertLine`, `DeleteLine`). The lines brought in take the pen's
/// colours (`bce`), between the margins alone. Nothing goes into history.
#[test]
fn scrolling_is_bounded_by_the_margins() -> Result {
    let p = framed("\x1b[3;4H\x1b[2L")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MNOPQRSTUVWX",
            "ab      ijkl",
            "mn      uvwx",
            "01cdefgh8901",
            "",
        ]
    );
    assert_eq!(p.screen().cursor_position(), (2, 2));
    let p = framed("\x1b[3;4H\x1b[M")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MNOPQRSTUVWX",
            "abopqrstijkl",
            "mn234567uvwx",
            "01      8901",
            "",
        ]
    );
    // Outside the left and right margins: nothing.
    let p = framed("\x1b[3;10H\x1b[M")?;
    assert_eq!(lines(&p).get(2).map(String::as_str), Some("abcdefghijkl"));
    assert_eq!(p.screen().cursor_position(), (2, 9));
    let p = framed("\x1b[2S")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MNopqrstUVWX",
            "ab234567ijkl",
            "mn      uvwx",
            "01      8901",
            "",
        ]
    );
    let p = framed("\x1b[44m\x1b[T")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MN      UVWX",
            "abOPQRSTijkl",
            "mncdefghuvwx",
            "01opqrst8901",
            "",
        ]
    );
    let blue = |x: u16| p.screen().cell(1, x).map(|c| c.bgcolor());
    assert!((2..8).all(|x| blue(x) == Some(Color::Idx(4))));
    assert_eq!(
        (blue(1), blue(8)),
        (Some(Color::Default), Some(Color::Default))
    );
    // A full-height region with margins keeps no history.
    let mut p = Parser::new(3, 8, 100)?;
    p.process(b"\x1b[?69h\x1b[2;4s\x1b[1;2Ha\r\nb\r\nc\r\nd\r\ne")?;
    assert_eq!(p.screen().history_len(), 0);
    assert_eq!(lines(&p), [" c", " d", " e"]);
    Ok(())
}

/// ICH, DCH and IRM edit up to the right margin, and outside the margins
/// do nothing (DEC STD 070, 5.4.3; xterm's `InsertChar` and `DeleteChar`,
/// and esctest's SM_IRM_TruncatesAtRightMargin): what passes the margin is
/// lost, and the cells past it stay.
#[test]
fn edits_stop_at_the_right_margin() -> Result {
    let p = run(
        6,
        12,
        format!("{FULL}\x1b[?69h\x1b[3;8s\x1b[2;4H\x1b[2@\x1b[3;4H\x1b[2P\x1b[4;10H\x1b[@\x1b[4;2H\x1b[P").as_bytes(),
    )?;
    assert_eq!(
        lines(&p).get(1..4),
        Some(
            &[
                "MNO  PQRUVWX".to_owned(),
                "abcfgh  ijkl".to_owned(),
                "mnopqrstuvwx".to_owned()
            ][..]
        )
    );
    let p = run(
        6,
        12,
        format!("{FULL}\x1b[?69h\x1b[3;8s\x1b[4h\x1b[2;4Hxy\x1b[3;9Hzw").as_bytes(),
    )?;
    assert_eq!(
        lines(&p).get(1..3),
        Some(&["MNOxyPQRUVWX".to_owned(), "abcdefghzwkl".to_owned()][..])
    );
    // Outside the margins ICH keeps a pending wrap, as xterm returns
    // before it ends one.
    let p = run(1, 12, b"\x1b[?69h\x1b[3;8s\x1b[1;11Hab\x1b[@X")?;
    assert_eq!(lines(&p), ["  X       ab"]);
    Ok(())
}

/// DECIC and DECDC (`CSI Pn ' }`, `CSI Pn ' ~`; DEC STD 070, 5.4.3; the
/// VT510 manual): Pn columns inserted or deleted at the cursor's, in every
/// line of the scrolling region, between the left and right margins, the
/// cursor staying. Outside the margins nothing.
#[test]
fn decic_and_decdc_insert_and_delete_columns() -> Result {
    let p = run(
        6,
        12,
        format!("{FULL}\x1b[2;5H\x1b[2'}}\x1b[4;5H\x1b[3'~").as_bytes(),
    )?;
    assert_eq!(
        lines(&p),
        [
            "ABCDFGHIJ",
            "MNOPRSTUV",
            "abcdfghij",
            "mnoprstuv",
            "012356789",
            ""
        ]
    );
    assert_eq!(p.screen().cursor_position(), (3, 4));
    let p = framed("\x1b[3;5H\x1b[2'}\x1b[4;5H\x1b['~")?;
    assert_eq!(
        lines(&p),
        [
            "ABCDEFGHIJKL",
            "MNOP QR UVWX",
            "abcd ef ijkl",
            "mnop qr uvwx",
            "0123 45 8901",
            "",
        ]
    );
    // Outside the region, or the margins: nothing.
    let before = lines(&framed("")?);
    for at in ["\x1b[6;5H", "\x1b[3;10H", "\x1b[3;2H"] {
        let p = framed(&format!("{at}\x1b[2'}}\x1b[3'~"))?;
        assert_eq!(lines(&p), before, "{at:?}");
    }
    Ok(())
}

/// CR goes to the left margin, or to the first column from left of it
/// outside origin mode (xterm's `CarriageReturn`); BS and CUB stop at the
/// left margin, unless the cursor is already left of it (`CursorBack`);
/// CUF stops at the right margin, unless already past it
/// (`CursorForward`); CNL and CPL end where CR goes (esctest's CR, CUB,
/// CUF, CNL and CPL tests).
#[test]
fn cursor_movements_stop_at_the_margins() -> Result {
    let p = run(
        6,
        12,
        b"ABCDEFGHIJKL\x1b[?69h\x1b[3;8s\x1b[2;5r\x1b[3;6H\r1\x1b[3;2H\r2\x1b[?6h\x1b[1;1H3\x1b[2;1H\x08\x084\x1b[3;3H\x1b[9D5\x1b[4;2H\x1b[9C6",
    )?;
    assert_eq!(
        lines(&p),
        ["ABCDEFGHIJKL", "  3", "2 4", "  5", "       6", ""]
    );
    assert_eq!(p.screen().cursor_position(), (4, 7));
    // Right of the right margin, CUF goes on to the last column; left of
    // the left margin, BS and CUB go on to the first.
    let p = run(1, 12, b"\x1b[?69h\x1b[3;8s\x1b[1;10H\x1b[9CX\x1b[1;2H\x08Y")?;
    assert_eq!(lines(&p), ["Y          X"]);
    for (step, at) in [("\x1b[E", (3, 2)), ("\x1b[F", (1, 2))] {
        let p = framed(&format!("\x1b[3;7H{step}"))?;
        assert_eq!(p.screen().cursor_position(), at, "{step:?}");
    }
    let p = framed("\x1b[3;1H\x1b[E")?;
    assert_eq!(p.screen().cursor_position(), (3, 0));
    Ok(())
}

/// In origin mode CUP, HVP, CHA and HPA address columns from the left
/// margin and no further than the right one, as lines from the top margin
/// (DEC STD 070, DECOM: "relative to the origin of the current scrolling
/// region (the Top and Left Margin)"); HPR and VPR are positions, as CUP
/// sets them, so HPR stops at the right margin in origin mode and passes
/// it outside it, and VPA and VPR keep the column (xterm's `CASE_HPR`,
/// `CASE_VPA`; esctest's CHA, CUP, HVP, HPR and VPR RespectsOriginMode
/// tests). CPR counts the column from the left margin too.
#[test]
fn origin_mode_addresses_columns_from_the_left_margin() -> Result {
    let p = run(
        6,
        12,
        b"ABCDEFGHIJKL\x1b[?69h\x1b[3;8s\x1b[2;5r\x1b[?6h\x1b[2;2H\x1b[5G1\x1b[3a2\x1b[3;1H\x1b[9a3\x1b[2e4\x1b[4d5",
    )?;
    assert_eq!(
        lines(&p),
        ["ABCDEFGHIJKL", "", "      12", "       3", "       5", ""]
    );
    // HPR passes the right margin outside origin mode.
    let p = run(1, 12, b"\x1b[?69h\x1b[3;8s\x1b[1;4H\x1b[6aX")?;
    assert_eq!(lines(&p), ["         X"]);
    let report = b"\x1b[?69h\x1b[3;8s\x1b[2;5r\x1b[?6h\x1b[2;3H\x1b[6n\x1b[?6n";
    assert_eq!(replies(6, 12, report)?, ["\x1b[2;3R", "\x1b[?2;3R"]);
    // DECOM's home is the top and left margins.
    let p = framed("\x1b[?6hX")?;
    assert_eq!(lines(&p).get(1).map(String::as_str), Some("MNXPQRSTUVWX"));
    Ok(())
}

/// HT and CHT stop at the right margin with DECLRMM set, wherever the
/// cursor is, as xterm's `TabToNextStop` has it; DEC STD 070 (HT, note 1)
/// stops there only from inside the scrolling region, going on to the
/// next stop or the last column from right of the margin (see the README's
/// departures). CBT stops at the left margin in origin mode
/// (`TabToPrevStop`; esctest's CHT_IgnoresScrollingRegion and
/// DECSET_DECAWM_NoLineWrapOnTabWithLeftRightMargin).
#[test]
fn tabs_stop_at_the_right_margin() -> Result {
    let p = run(
        6,
        12,
        b"\x1b[?69h\x1b[3;8s\x1b[2;10H\t1\x1b[2;1H\t2\x1b[3;5H\x1b[2I3\x1b[4;8H\x1b[Z4\x1b[?6h\x1b[3;5H\x1b[3Z5",
    )?;
    assert_eq!(lines(&p), ["", "       2", "  5    3", "4", "", ""]);
    let mut p = Parser::new(3, 80, 0)?;
    p.process(b"\x1b[?69h\x1b[10;20s")?;
    for col in [8, 16, 19, 19] {
        p.process(b"\t")?;
        assert_eq!(p.screen().cursor_position(), (0, col));
    }
    Ok(())
}

/// REP prints as printing the character again would: it wraps at the
/// right margin and scrolls the region between the margins. The copies
/// past those that fill the region are skipped a line's worth at a time,
/// a line being the margins' width, which leaves all as printing them
/// would: REP and printing the same copies agree.
#[test]
fn rep_wraps_and_scrolls_between_the_margins() -> Result {
    let p = run(6, 12, b"\x1b[?69h\x1b[3;8s\x1b[2;5r\x1b[4;5Hx\x1b[20b")?;
    assert_eq!(
        lines(&p),
        ["", "    xxxx", "  xxxxxx", "  xxxxxx", "  xxxxx", ""]
    );
    assert_eq!(p.screen().cursor_position(), (4, 7));
    for setup in [
        "\x1b[?69h\x1b[2;5s\x1b[2;4r\x1b[4;3H",
        "\x1b[?69h\x1b[2;5s\x1b[2;4r\x1b[4;9H",
        "\x1b[?69h\x1b[2;6s\x1b[1;3H",
        "\x1b[?69h\x1b[3;5s\x1b[3;3H\x1b[?7l",
    ] {
        for (glyph, count) in [("q", 1000usize), ("\u{4e00}", 333), ("q", 37)] {
            let rep = run(4, 10, format!("{setup}{glyph}\x1b[{count}b").as_bytes())?;
            let printed = run(
                4,
                10,
                format!(
                    "{setup}{}",
                    std::iter::repeat_n(glyph, count.saturating_add(1)).collect::<String>()
                )
                .as_bytes(),
            )?;
            assert_eq!(lines(&rep), lines(&printed), "{setup:?} {glyph} {count}");
            let at = |p: &Parser| (p.screen().cursor_position(), p.screen().pending_wrap());
            assert_eq!(at(&rep), at(&printed), "{setup:?} {glyph} {count}");
        }
    }
    Ok(())
}

/// DECSC saves the cursor's column as it is, and DECRC in origin mode puts
/// it back no further right than the right margin, as xterm's
/// `CursorRestore` does; DECALN resets the margins (xterm: DEC STD 070's
/// DECALN predates them), DECLRMM staying set; DECSTR resets DECLRMM and
/// the margins (DEC STD 070, Soft Terminal Reset; the VT520 manual's
/// table), so `CSI s` saves the cursor again.
#[test]
fn the_saved_cursor_and_resets() -> Result {
    let p = run(
        3,
        12,
        b"\x1b[?69h\x1b[3;8s\x1b[?6h\x1b[1;6H\x1b7\x1b[3;5s\x1b8X\x1b[?6l",
    )?;
    assert_eq!(lines(&p), ["    X", "", ""]);
    let p = run(
        6,
        12,
        format!("ABCDEFGHIJKL\r\nMNOPQRSTUVWX{MARGINS}\x1b#8\x1b[1;1Hxyz1234567890\x1b[s\x1b[5;5Hq\x1b[u!").as_bytes(),
    )?;
    assert_eq!(
        lines(&p),
        [
            "!yz123456789",
            "0EEEEEEEEEEE",
            "EEEEEEEEEEEE",
            "EEEEEEEEEEEE",
            "EEEEqEEEEEEE",
            "EEEEEEEEEEEE",
        ]
    );
    let p = run(
        3,
        12,
        b"\x1b[?69h\x1b[3;8s\x1b[!p\x1b[1;1H\x1b[s\x1b[3;3H\x1b[uX",
    )?;
    assert_eq!(lines(&p), ["X", "", ""]);
    assert_eq!(
        replies(3, 12, b"\x1b[?69h\x1b[!p\x1b[?69$p")?,
        ["\x1b[?69;2$y"]
    );
    Ok(())
}

/// A wide glyph across a margin loses both halves when the region scrolls
/// or an edit moves one half and not the other, as xterm clears it in
/// `scrollInMargins`: fux-vt keeps no half of a glyph. xterm leaves half of
/// one where ICH and DCH cut it at the right margin; fux-vt blanks it.
#[test]
fn a_wide_glyph_across_a_margin_goes_whole() -> Result {
    let wide = "\x1b[1;2H\u{4e00}\x1b[1;7H\u{4e00}\x1b[2;2H\u{4e00}\x1b[2;7H\u{4e00}abc";
    let p = run(
        3,
        12,
        format!("{wide}\x1b[?69h\x1b[3;7s\x1b[1;2r\x1b[S").as_bytes(),
    )?;
    assert_eq!(lines(&p), ["", "        abc", ""]);
    let p = run(
        3,
        12,
        format!("{wide}\x1b[?69h\x1b[3;7s\x1b[1;4H\x1b[@\x1b[2;4H\x1b[P").as_bytes(),
    )?;
    assert_eq!(lines(&p), [" \u{4e00}", " \u{4e00}     abc", ""]);
    Ok(())
}

/// DEC STD 070's Last Column Flag is no column: a wrap waiting at the
/// right margin when DECLRMM is reset, or DECSTR resets it, still waits
/// there, and the next glyph wraps from it, as xterm and Ghostty wrap it;
/// with DECAWM off it is written there, as in xterm.
#[test]
fn a_wrap_waiting_at_a_margin_reset_is_carried_out() -> Result {
    for reset in ["\x1b[?69l", "\x1b[!p"] {
        let p = run(
            3,
            10,
            format!("\x1b[?69h\x1b[2;5s\x1b[1;1Habcde{reset}X").as_bytes(),
        )?;
        assert_eq!(lines(&p), ["abcde", "X", ""], "{reset:?}");
        assert_eq!(p.screen().cursor_position(), (1, 1), "{reset:?}");
        assert!(p.screen().row_wrapped(0), "{reset:?}");
    }
    let p = run(
        3,
        10,
        b"\x1b[?69h\x1b[2;5s\x1b[1;1Habcde\x1b[?69l\x1b[?7lXY",
    )?;
    assert_eq!(lines(&p), ["abcdXY", "", ""]);
    Ok(())
}
