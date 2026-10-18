//! Modes and controls fux-vt keeps as xterm keeps them: DECSCLM, DECSCNM,
//! DECARM, DECNKM and DECBKM for DECRQM; reverse wraparound (45) and
//! xterm's extended reverse wraparound (1045); XTSAVE and XTRESTORE; LNM;
//! DECID; DECALN. The expected values are the references' (DEC STD 070,
//! the VT520 manual, xterm's ctlseqs) and, where those leave it to the
//! terminal, xterm 411's, asked the same sequences under Xvfb (80 by 25,
//! a VT420) and read back by DSR and DECRQM.

use fux_vt::{Attributes, Color, Feature, Identity, Mode, Options, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const MODES: Options = Options::new().with(Feature::ModeReports);

/// What `input` makes the parser answer, as text.
fn replies(parser: &mut Parser, input: &[u8]) -> Result<String> {
    let mut out = Vec::new();
    parser.process_with_replies(input, |bytes| out.extend_from_slice(bytes))?;
    Ok(String::from_utf8_lossy(&out).replace('\x1b', "^["))
}

/// The cursor, one-based (row, column), as xterm's DSR reports it, after
/// `input` on a fresh 25 by 80 screen.
fn cursor_after(input: &[u8]) -> Result<(u16, u16)> {
    let mut parser = Parser::new(Size::new(25, 80)?, 0)?;
    parser.process(input)?;
    let (row, col) = parser.screen().cursor_position();
    Ok((row.saturating_add(1), col.saturating_add(1)))
}

/// ECMA-48 (5th edition) 7.1: "some or all of the modes may have one
/// state only", the reset state recommended as the initial one. The ANSI
/// modes fux-vt has no use for, GATM (1), SRTM (5), VEM (7), HEM (10), PUM
/// (11), FEAM (13), FETM (14), MATM (15), TTM (16), SATM (17) and TSM
/// (18), have the reset state only, and so do those that fux-vt performs
/// in their reset state alone (7.2: "the first state is caused by RM"):
/// KAM (2) ENABLED, the keyboard never locked; CRM (3) CONTROL, controls
/// performed; ERM (6) PROTECT, ED, EL and ECH sparing a guarded area; BDSM
/// (8) EXPLICIT, no implicit bidirectional processing; DCSM (9)
/// PRESENTATION, as there is no data component. DECRQM answers them
/// permanently reset (4), set or not. The same numbers as DEC private
/// modes are others. Three have their set state only, which DECRQM
/// answers permanently set (3), reset or not: SRM (12) SIMULTANEOUS
/// (7.2.15; DEC STD 070's SRM: set, local echo off, the default), as
/// fux-vt never images what is typed; GRCM (21) CUMULATIVE (7.2.8), as an
/// SGR changes only the renditions it names; ZDM (22) DEFAULT (F.4.2), as
/// a parameter of 0 is its default. 19, EBM in the 4th edition, which the
/// 5th says "shall not be used" (F.5.1), is permanently reset too: DECRQM
/// is DEC's, and DEC STD 070's VT420 table (DECRQM, p. 5-194) lists "19
/// EBM *", "always Reset ... and can not be changed". 23, no mode, is not
/// recognized (0).
#[test]
fn decrqm_reports_the_ansi_modes_kept_in_one_state() -> Result {
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    for (state, modes) in [
        (
            4,
            &[1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19][..],
        ),
        (3, &[12, 21, 22]),
    ] {
        for n in modes {
            let ask = format!("\x1b[{n}$p");
            for change in ["h", "l"] {
                p.process(format!("\x1b[{n}{change}").as_bytes())?;
                assert_eq!(
                    replies(&mut p, ask.as_bytes())?,
                    format!("^[[{n};{state}$y")
                );
            }
        }
    }
    assert_eq!(
        replies(&mut p, b"\x1b[23$p\x1b[?5$p")?,
        "^[[23;0$y^[[?5;2$y"
    );
    Ok(())
}

/// DECHCCM (`?60`), horizontal cursor coupling, is "always Reset in the
/// VT420 and can not be changed" (DEC STD 070, DECRQM, p. 5-194, the
/// VT420's table): DECRQM answers 4, set or reset, and DECSTR and RIS
/// leave it so.
#[test]
fn decrqm_reports_dechccm_permanently_reset() -> Result {
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    for change in [
        "",
        "\x1b[?60h",
        "\x1b[?60l",
        "\x1b[?60h\x1b[!p",
        "\x1b[?60h\x1bc",
    ] {
        p.process(change.as_bytes())?;
        assert_eq!(
            replies(&mut p, b"\x1b[?60$p")?,
            "^[[?60;4$y",
            "after {change:?}"
        );
    }
    Ok(())
}

/// DECSCLM (4), DECSCNM (5), DECNKM (66) and DECBKM (67) are kept as modes,
/// set and reset, and reported by DECRQM, as xterm 411 keeps them; DECARM
/// (8) is reported permanently reset, as xterm 411 reports it (esctest
/// expects it settable: "xterm always returns 4"). DECNKM is the keypad
/// mode ESC = and ESC > set (ctlseqs; xterm 411). xterm 411: DECSTR keeps
/// 4, 5 and 67 and resets 66; RIS resets them all. DECRQM reads what
/// `Screen::mode` reads, focus reporting (1004) too, as xterm 411 does.
#[test]
fn decrqm_reports_the_modes_xterm_keeps() -> Result {
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    let all = b"\x1b[?4$p\x1b[?5$p\x1b[?8$p\x1b[?66$p\x1b[?67$p";
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;2$y^[[?5;2$y^[[?8;4$y^[[?66;2$y^[[?67;2$y"
    );
    p.process(b"\x1b[?4;5;8;66;67h")?;
    assert!(p.screen().mode(Mode::ApplicationKeypad));
    assert_eq!(
        replies(&mut p, all)?,
        "^[[?4;1$y^[[?5;1$y^[[?8;4$y^[[?66;1$y^[[?67;1$y"
    );
    p.process(b"\x1b>")?;
    assert_eq!(replies(&mut p, b"\x1b[?66$p")?, "^[[?66;2$y");
    p.process(b"\x1b=")?;
    assert_eq!(replies(&mut p, b"\x1b[?66$p")?, "^[[?66;1$y");
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
    let focus = replies(&mut p, b"\x1b[?1004$p\x1b[?1004h\x1b[?1004$p")?;
    assert_eq!(focus, "^[[?1004;2$y^[[?1004;1$y");
    Ok(())
}

/// Grapheme clustering (2027) is permanently set, as kitty and Ghostty
/// report it: DECRQM says 3 however a program sets, resets, saves or
/// restores it, and `Screen::mode` reads it set.
#[test]
fn grapheme_clustering_is_permanently_set() -> Result {
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    assert_eq!(replies(&mut p, b"\x1b[?2027$p")?, "^[[?2027;3$y");
    p.process(b"\x1b[?2027s\x1b[?2027l\x1b[?2027r\x1b[?2027l")?;
    assert_eq!(replies(&mut p, b"\x1b[?2027$p")?, "^[[?2027;3$y");
    assert!(p.screen().mode(Mode::GraphemeClustering));
    // Not an ANSI mode.
    assert_eq!(replies(&mut p, b"\x1b[2027$p")?, "^[[2027;0$y");
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
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
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
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    p.process(b"\x1b[?7h\x1b[?7s\x1b[?7l\x1b[?7r")?;
    assert!(p.screen().mode(Mode::Autowrap));
    p.process(b"\x1b[?7l\x1b[?7s\x1b[?7h\x1b[?7r")?;
    assert!(!p.screen().mode(Mode::Autowrap));
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    p.process(b"\x1b[?25;2004;1;45s\x1b[?25l\x1b[?2004h\x1b[?1h\x1b[?45h\x1b[?25;2004;1;45r")?;
    let s = p.screen();
    assert!(
        s.mode(Mode::ShowCursor)
            && !s.mode(Mode::BracketedPaste)
            && !s.mode(Mode::ApplicationCursor)
    );
    assert_eq!(replies(&mut p, b"\x1b[?45$p")?, "^[[?45;2$y");
    // Never saved: reset.
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    p.process(b"\x1b[?7r")?;
    assert!(!p.screen().mode(Mode::Autowrap));
    // RIS and DECSTR keep what was saved; a later save replaces it.
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    p.process(b"\x1b[?1h\x1b[?1s\x1bc\x1b[?1r")?;
    assert!(p.screen().mode(Mode::ApplicationCursor));
    p.process(b"\x1b[?1l\x1b[!p\x1b[?1r")?;
    assert!(p.screen().mode(Mode::ApplicationCursor));
    p.process(b"\x1b[?2004h\x1b[?2004s\x1b[?2004l\x1b[?2004s\x1b[?2004h\x1b[?2004r")?;
    assert!(!p.screen().mode(Mode::BracketedPaste));
    // The alternate screen, saved off and restored, is left.
    p.process(b"\x1b[?1049s\x1b[?1049h\x1b[?1049r")?;
    assert!(!p.screen().mode(Mode::AlternateScreen));
    // A mode fux-vt does not keep, or one with a colon, changes nothing.
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    p.process(b"\x1b[?7;12;9999s\x1b[?7l\x1b[?12;9999r\x1b[?7:1r")?;
    assert!(!p.screen().mode(Mode::Autowrap));
    Ok(())
}

/// LNM (DEC STD 070, Line Feed/New Line Mode; the VT520 manual; `CSI 20
/// h`): LF, VT and FF return the carriage too; IND does not. DECRQM
/// reports it (`CSI 20 $ p`); RIS resets it and DECSTR keeps it, as in
/// xterm 411. esctest's SM_LNM.
#[test]
fn lnm_makes_a_line_feed_a_new_line() -> Result {
    for control in [b'\n', 0x0b, 0x0c] {
        let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
        p.process(&[b"\x1b[1;5H".as_slice(), &[control]].concat())?;
        assert_eq!(p.screen().cursor_position(), (1, 4));
        p.process(&[b"\x1b[20h\x1b[1;5H".as_slice(), &[control]].concat())?;
        assert_eq!(p.screen().cursor_position(), (1, 0));
    }
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
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
    let mut p = Parser::new(Size::new(25, 80)?, 0)?;
    assert_eq!(replies(&mut p, b"\x1bZ")?, "^[[?1;2c");
    let identity = Identity {
        name: "fux",
        version: "1.2.3",
    };
    let options = Options::new().with_identity(Some(identity));
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, options)?;
    assert_eq!(
        replies(&mut p, b"\x1bZ\x1b[c")?,
        "^[[?62;6;21;22;28c^[[?62;6;21;22;28c"
    );
    Ok(())
}

/// DECALN (`ESC # 8`; DEC STD 070, Appendix D, Screen Alignment; VT520
/// manual 5-17): the screen filled with E, the margins the whole screen,
/// origin mode off, the cursor home with no wrap pending, and the pen's
/// rendition off; as xterm 411 does it, the pen keeps its colours and
/// loses every attribute, and the E's have none of either. Each row's soft
/// wrap ends (Ghostty, alacritty and wezterm; xterm keeps it). esctest's
/// DECALN_FillsScreen and _MovesCursorHome.
#[test]
fn decaln_fills_the_screen_with_e() -> Result {
    let mut p = Parser::new(Size::new(3, 4)?, 0)?;
    p.process(b"abcdef\x1b[31;1;4;7m\x1b[2;3r\x1b[?6h\x1b[2;2H\x1b#8")?;
    let s = p.screen();
    assert_eq!(s.cursor_position(), (0, 0));
    assert!(!s.pending_wrap() && !s.mode(Mode::Origin));
    assert_eq!(s.scroll_region(), (0, 2));
    assert_eq!(
        s.attributes(),
        Attributes::new(Color::Idx(1), Color::Default)
    );
    for row in 0..3 {
        assert!(!s.row_wrapped(row), "row {row}");
        for col in 0..4 {
            let cell = s.cell(row, col).ok_or("a cell")?;
            assert_eq!(cell.contents(), "E");
            assert_eq!(cell.attributes(), Attributes::default());
        }
    }
    // An E printed after it takes the pen's colours.
    p.process(b"X")?;
    let cell = p.screen().cell(0, 0).ok_or("a cell")?;
    assert_eq!((cell.contents(), cell.fgcolor()), ("X", Color::Idx(1)));
    // A prompt mark goes with the row's text, as ED's erase takes it.
    let options = Options::new().with(Feature::PromptMarks);
    let mut p = Parser::with_options(Size::new(2, 3)?, 0, options)?;
    p.process(b"\x1b]133;A\x07$ \x1b#8")?;
    assert!(!p.screen().starts_prompt(0));
    // Rows already filled keep their version.
    let mut p = Parser::new(Size::new(2, 3)?, 0)?;
    p.process(b"\x1b#8")?;
    let mark = p.screen().mark();
    p.process(b"\x1b#8")?;
    assert_eq!(p.screen().dirty_rows_since(mark).count(), 0);
    Ok(())
}

/// Mode 12 (ctlseqs: "Start blinking cursor (AT&T 610)", reset "Stop
/// blinking cursor") is the blink DECSCUSR sets too: xterm 411 keeps one
/// state for both (`cursor_blink_esc`, set by `srm_ATT610_BLINK` and by
/// DECSCUSR's blinking styles, cleared by its steady ones and by a reset).
/// It starts reset; set, the shape keeps its form and blinks, the
/// terminal's own taken as xterm's block; DECSCUSR 7 puts the terminal's
/// own back, reset.
#[test]
fn mode_12_is_the_cursor_shapes_blink() -> Result {
    use fux_vt::CursorShape::*;
    let mut p = Parser::with_options(Size::new(25, 80)?, 0, MODES)?;
    let ask = b"\x1b[?12$p";
    assert_eq!(replies(&mut p, ask)?, "^[[?12;2$y");
    p.process(b"\x1b[?12h")?;
    assert_eq!(p.screen().cursor_shape(), BlinkingBlock);
    assert_eq!(replies(&mut p, ask)?, "^[[?12;1$y");
    for (bytes, shape, reply) in [
        (&b"\x1b[6 q"[..], SteadyBar, "^[[?12;2$y"),
        (b"\x1b[?12h", BlinkingBar, "^[[?12;1$y"),
        (b"\x1b[?12l", SteadyBar, "^[[?12;2$y"),
        (b"\x1b[3 q", BlinkingUnderline, "^[[?12;1$y"),
        (b"\x1b[?12l", SteadyUnderline, "^[[?12;2$y"),
        (b"\x1b[0 q", Zero, "^[[?12;1$y"),
        (b"\x1b[?12l", SteadyBlock, "^[[?12;2$y"),
        (
            b"\x1b[?12h\x1b[?12s\x1b[?12l\x1b[?12r",
            BlinkingBlock,
            "^[[?12;1$y",
        ),
        (b"\x1b[7 q", Initial, "^[[?12;2$y"),
        (b"\x1b[?12l", Initial, "^[[?12;2$y"),
    ] {
        p.process(bytes)?;
        assert_eq!(p.screen().cursor_shape(), shape, "{bytes:?}");
        assert_eq!(replies(&mut p, ask)?, reply, "{bytes:?}");
    }
    Ok(())
}
