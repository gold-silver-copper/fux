//! Modes and controls fux-vt keeps as xterm keeps them: DECSCLM, DECSCNM,
//! DECARM, DECNKM and DECBKM for DECRQM; reverse wraparound (45) and
//! xterm's extended reverse wraparound (1045); XTSAVE and XTRESTORE; LNM;
//! DECID; DECALN. The expected values are the references' (DEC STD 070,
//! the VT520 manual, xterm's ctlseqs) and, where those leave it to the
//! terminal, xterm 411's, asked the same sequences under Xvfb (80 by 25,
//! a VT420) and read back by DSR and DECRQM.

use fux_vt::{Identity, Options, Parser};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

const MODES: Options = Options::new().with_mode_reports(true);

/// What `input` makes the parser answer, as text.
fn replies(parser: &mut Parser, input: &[u8]) -> std::result::Result<String, fux_vt::Error> {
    let mut out = Vec::new();
    parser.process_with_replies(input, |bytes| out.extend_from_slice(bytes))?;
    Ok(String::from_utf8_lossy(&out).replace('\x1b', "^["))
}

/// The cursor, one-based (row, column), as xterm's DSR reports it, after
/// `input` on a fresh 25 by 80 screen.
fn cursor_after(input: &[u8]) -> std::result::Result<(u16, u16), fux_vt::Error> {
    let mut parser = Parser::new(25, 80, 0)?;
    parser.process(input)?;
    let (row, col) = parser.screen().cursor_position();
    Ok((row.saturating_add(1), col.saturating_add(1)))
}

/// DECSCLM (4), DECSCNM (5), DECNKM (66) and DECBKM (67) are kept as modes,
/// set and reset, and reported by DECRQM, as xterm 411 keeps them; DECARM
/// (8) is reported permanently reset, as xterm 411 reports it (esctest
/// expects it settable: "xterm always returns 4"). DECNKM is the keypad
/// mode ESC = and ESC > set (ctlseqs; xterm 411). xterm 411: DECSTR keeps
/// 4, 5 and 67 and resets 66; RIS resets them all.
#[test]
fn decrqm_reports_the_modes_xterm_keeps() -> Result {
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    let all = b"\x1b[?4$p\x1b[?5$p\x1b[?8$p\x1b[?66$p\x1b[?67$p";
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;2$y^[[?5;2$y^[[?8;4$y^[[?66;2$y^[[?67;2$y"
    );
    p.process(b"\x1b[?4;5;8;66;67h")?;
    assert!(p.screen().application_keypad());
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;1$y^[[?5;1$y^[[?8;4$y^[[?66;1$y^[[?67;1$y"
    );
    p.process(b"\x1b>")?;
    assert_eq!(replies(&mut p, b"\x1b[?66$p")?, "^[[?66;2$y");
    p.process(b"\x1b=\x1b[!p")?;
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;1$y^[[?5;1$y^[[?8;4$y^[[?66;2$y^[[?67;1$y"
    );
    p.process(b"\x1bc")?;
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;2$y^[[?5;2$y^[[?8;4$y^[[?66;2$y^[[?67;2$y"
    );
    Ok(())
}

/// Reverse wraparound (ctlseqs: `CSI ? 45 h`, XTREVWRAP, and `CSI ? 1045
/// h`, XTREVWRAP2), with DECAWM: BS and CUB at the first column go on at
/// the end of the line before. 45 goes back only over a line's soft wraps;
/// 1045 over any line, from the top margin to the bottom margin. A cursor
/// waiting to wrap counts as one past the last column. Each expected
/// position is xterm 411's.
#[test]
fn reverse_wraparound_moves_as_xterm_moves() -> Result {
    let long: String = std::iter::repeat_n('x', 81).collect();
    let cases: [(&str, (u16, u16)); 14] = [
        // 45: row 1 is not soft-wrapped, so nothing.
        ("\x1b[?45h\x1b[2;1H\x08", (2, 1)),
        // 45: row 1 is, so back to its end.
        ("\x1b[?45h\x1b[1;1H{long}\x08\x08", (1, 80)),
        // 1045: from the top to the bottom, the margins first.
        ("\x1b[?1045h\x1b[1;1H\x08", (25, 80)),
        ("\x1b[?1045h\x1b[3;5r\x1b[3;1H\x08", (5, 80)),
        ("\x1b[?1045h\x1b[3;5r\x1b[7;1H\x08", (6, 80)),
        ("\x1b[?1045h\x1b[3;5r\x1b[3;1H\x1b[200D", (3, 41)),
        ("\x1b[?1045h\x1b[3;5r\x1b[6;1H{long}\x1b[7;1H\x08", (6, 80)),
        // A wrap pending: the first column back is the one it waits in.
        ("\x1b[?45h\x1b[1;79Hab\x08\x08", (1, 79)),
        ("\x1b[?1045h\x1b[1;80Hx\x08", (1, 80)),
        // Without DECAWM, nothing.
        ("\x1b[?7l\x1b[?45h\x1b[1;1H{long}\x1b[2;1H\x08", (2, 1)),
        ("\x1b[?1045h\x1b[?7l\x1b[2;1H\x08", (2, 1)),
        // 45 alone after 1045 is reset; CUB counts the wrap as a column.
        (
            "\x1b[?1045h\x1b[?45h\x1b[?1045l\x1b[1;1H{long}\x1b[2;1H\x1b[3D",
            (1, 78),
        ),
        // 45 crosses the margins (xterm's, unlike Ghostty's).
        ("\x1b[?45h\x1b[3;5r\x1b[2;1H{long}\x1b[3;1H\x08", (2, 80)),
        ("\x1b[?45h\x1b[1;1H{long}{long}\x1b[3;1H\x1b[3D", (2, 78)),
    ];
    for (sequence, expected) in cases {
        let input = sequence.replace("{long}", &long);
        assert_eq!(cursor_after(input.as_bytes())?, expected, "{input:?}");
    }
    Ok(())
}

/// Where xterm 411 crashes (45 past a soft-wrapped first row) or puts the
/// cursor above the screen (1045 above the top margin), fux-vt stops at the
/// first row, as Ghostty does.
#[test]
fn reverse_wraparound_stops_at_the_first_row() -> Result {
    let long: String = std::iter::repeat_n('x', 81).collect();
    let past = format!("\x1b[?45h\x1b[1;1H{long}\x1b[2;1H\x1b[200D");
    assert_eq!(cursor_after(past.as_bytes())?, (1, 1));
    assert_eq!(
        cursor_after(b"\x1b[?45h\x1b[3;5r\x1b[1;5H\x08\x08\x08\x08\x08\x08")?,
        (1, 1)
    );
    assert_eq!(cursor_after(b"\x1b[?1045h\x1b[3;5r\x1b[1;1H\x08")?, (1, 1));
    Ok(())
}

/// The modes are reported by DECRQM; DECSTR and RIS reset both, as xterm
/// 411 does.
#[test]
fn reverse_wraparound_is_a_mode() -> Result {
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    let both = b"\x1b[?45$p\x1b[?1045$p";
    assert_eq!(replies(&mut p, both)?, "^[[?45;2$y^[[?1045;2$y");
    p.process(b"\x1b[?45;1045h")?;
    assert_eq!(replies(&mut p, both)?, "^[[?45;1$y^[[?1045;1$y");
    p.process(b"\x1b[!p")?;
    assert_eq!(replies(&mut p, both)?, "^[[?45;2$y^[[?1045;2$y");
    p.process(b"\x1b[?45;1045h\x1bc")?;
    assert_eq!(replies(&mut p, both)?, "^[[?45;2$y^[[?1045;2$y");
    Ok(())
}

/// XTSAVE and XTRESTORE (ctlseqs: `CSI ? Pm s`, `CSI ? Pm r`): each mode
/// listed saved, and set back as it was saved; one never saved is restored
/// reset, as in xterm 411 (`CSI ? 7 r` turns autowrap off), and RIS and
/// DECSTR keep what was saved, as xterm 411 does. esctest's
/// XtermSave_SaveSetState and _SaveResetState: autowrap.
#[test]
fn xtsave_and_xtrestore_save_and_restore_modes() -> Result {
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[?7h\x1b[?7s\x1b[?7l\x1b[?7r")?;
    assert!(p.screen().autowrap());
    p.process(b"\x1b[?7l\x1b[?7s\x1b[?7h\x1b[?7r")?;
    assert!(!p.screen().autowrap());
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[?25;2004;1;45s\x1b[?25l\x1b[?2004h\x1b[?1h\x1b[?45h\x1b[?25;2004;1;45r")?;
    let s = p.screen();
    assert!(!s.hide_cursor() && !s.bracketed_paste() && !s.application_cursor());
    assert_eq!(replies(&mut p, b"\x1b[?45$p")?, "^[[?45;2$y");
    // Never saved: reset.
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[?7r")?;
    assert!(!p.screen().autowrap());
    // RIS and DECSTR keep what was saved; a later save replaces it.
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[?1h\x1b[?1s\x1bc\x1b[?1r")?;
    assert!(p.screen().application_cursor());
    p.process(b"\x1b[?1l\x1b[!p\x1b[?1r")?;
    assert!(p.screen().application_cursor());
    p.process(b"\x1b[?2004h\x1b[?2004s\x1b[?2004l\x1b[?2004s\x1b[?2004h\x1b[?2004r")?;
    assert!(!p.screen().bracketed_paste());
    // The alternate screen, saved off and restored, is left.
    p.process(b"\x1b[?1049s\x1b[?1049h\x1b[?1049r")?;
    assert!(!p.screen().alternate_screen());
    // A mode fux-vt does not keep, or one with a colon, changes nothing.
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[?7;12;9999s\x1b[?7l\x1b[?12;9999r\x1b[?7:1r")?;
    assert!(!p.screen().autowrap());
    Ok(())
}

/// LNM (DEC STD 070, Line Feed/New Line Mode; the VT520 manual; `CSI 20
/// h`): LF, VT and FF return the carriage too; IND does not. DECRQM
/// reports it (`CSI 20 $ p`); RIS resets it and DECSTR keeps it, as in
/// xterm 411. esctest's SM_LNM.
#[test]
fn lnm_makes_a_line_feed_a_new_line() -> Result {
    for control in [b'\n', 0x0b, 0x0c] {
        let mut p = Parser::with_options(25, 80, 0, MODES)?;
        p.process(&[b"\x1b[1;5H".as_slice(), &[control]].concat())?;
        assert_eq!(p.screen().cursor_position(), (1, 4));
        p.process(&[b"\x1b[20h\x1b[1;5H".as_slice(), &[control]].concat())?;
        assert_eq!(p.screen().cursor_position(), (1, 0));
    }
    let mut p = Parser::with_options(25, 80, 0, MODES)?;
    p.process(b"\x1b[20h\x1b[1;5H\x1bD")?;
    assert_eq!(p.screen().cursor_position(), (1, 4));
    assert_eq!(replies(&mut p, b"\x1b[20$p")?, "^[[20;1$y");
    p.process(b"\x1b[!p")?;
    assert_eq!(replies(&mut p, b"\x1b[20$p")?, "^[[20;1$y");
    p.process(b"\x1b[20;4l")?;
    assert_eq!(replies(&mut p, b"\x1b[20$p")?, "^[[20;2$y");
    p.process(b"\x1b[20h\x1bc")?;
    assert_eq!(replies(&mut p, b"\x1b[20$p")?, "^[[20;2$y");
    Ok(())
}

/// DECID (`ESC Z`), the VT100's request for its identity, which the VT220
/// replaced by DA (ctlseqs: "Obsolete form of CSI c"), is answered as DA1
/// is. esctest's DECID_Basic.
#[test]
fn decid_is_answered_as_da1() -> Result {
    let mut p = Parser::new(25, 80, 0)?;
    assert_eq!(replies(&mut p, b"\x1bZ")?, "^[[?1;2c");
    let identity = Identity {
        name: "fux",
        version: "1.2.3",
    };
    let options = Options::new().with_identity(Some(identity));
    let mut p = Parser::with_options(25, 80, 0, options)?;
    assert_eq!(replies(&mut p, b"\x1bZ\x1b[c")?, "^[[?62;22c^[[?62;22c");
    Ok(())
}
