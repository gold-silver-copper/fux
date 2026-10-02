//! Behaviour settled by the specifications in `references/` and by xterm,
//! each test citing the section that sets its expected values. Where xterm
//! departs from the specification, the test says so and follows xterm.

use fux_vt::{CellRef, Color, Parser};
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

fn run(rows: u16, cols: u16, bytes: &[u8]) -> std::result::Result<Parser, fux_vt::Error> {
    let mut parser = Parser::new(rows, cols, 0)?;
    parser.process(bytes)?;
    Ok(parser)
}

/// DEC STD 070, Appendix D.6.1 (Auto Wrap Mode) and "Insert or Replace
/// Graphic Character" (p. 5-139): a glyph in the last column leaves the
/// cursor there with the Last Column Flag set, and the next glyph wraps
/// first. Each control the appendix lists clears the flag, so the cursor
/// acts from the last column itself. Expected values are xterm's (XTerm
/// 411, `fux-vt-compare cases '*-while-wrap-pending'`).
#[test]
fn a_pending_wrap_is_a_flag_on_the_last_column() -> Result {
    let p = run(1, 5, b"abcde")?;
    assert_eq!(p.screen().cursor_position(), (0, 4));
    assert!(p.screen().pending_wrap());

    // BS and CUB move back from the last column.
    assert_eq!(lines(&run(1, 5, b"abcde\x08X")?), ["abcXe"]);
    assert_eq!(lines(&run(1, 5, b"abcde\x1b[DX")?), ["abcXe"]);
    // EL, ECH, ICH and ED act on the last column.
    for edit in [&b"\x1b[K"[..], b"\x1b[X", b"\x1b[@", b"\x1b[J"] {
        let p = run(1, 5, &[&b"abcde"[..], edit].concat())?;
        assert_eq!(lines(&p), ["abcd"], "{edit:?}");
        assert_eq!(p.screen().cursor_position(), (0, 4), "{edit:?}");
        assert!(!p.screen().pending_wrap(), "{edit:?}");
    }
    // LF keeps the column; CUU and RI move up from it, wrapping nothing.
    let p = run(2, 5, b"abcde\nX")?;
    assert_eq!(lines(&p), ["abcde", "    X"]);
    assert!(!p.screen().row_wrapped(0));
    for up in [&b"\x1b[A"[..], b"\x1bM"] {
        let p = run(2, 5, &[&b"zzzzz\r\nabcde"[..], up, b"X"].concat())?;
        assert_eq!(lines(&p), ["zzzzX", "abcde"], "{up:?}");
        assert!(!p.screen().row_wrapped(0), "{up:?}");
    }
    // A glyph printed in the last column after a tab waits there, so BS
    // puts the next one before it.
    assert_eq!(lines(&run(1, 5, b"\tZ\x08Y")?), ["   YZ"]);
    Ok(())
}

/// DEC STD 070, Appendix D.6.1: Save Cursor saves the Last Column Flag
/// and Restore Cursor restores it; xterm does the same for DECSC and
/// SCOSC. What the appendix does not list leaves the flag set: SGR, and
/// SU, as in xterm. HT is in the list, but xterm, and every engine
/// `fux-vt-compare` runs but avt and vt100, leave the flag set at the
/// last column (`replay --engines all --size 2x5 'abcde\tX'`): fux-vt
/// follows xterm.
#[test]
fn a_pending_wrap_is_saved_with_the_cursor_and_kept_by_what_does_not_move_it() -> Result {
    for (save, restore) in [(&b"\x1b7"[..], &b"\x1b8"[..]), (b"\x1b[s", b"\x1b[u")] {
        let p = run(
            2,
            5,
            &[&b"abcde"[..], save, b"\x1b[2;3H", restore, b"X"].concat(),
        )?;
        assert_eq!(lines(&p), ["abcde", "X"], "{save:?}");
        assert!(p.screen().row_wrapped(0), "{save:?}");
    }
    for keep in [&b"\t"[..], b"\x1b[1m", b"\x1b[?25l"] {
        let p = run(2, 5, &[&b"abcde"[..], keep, b"X"].concat())?;
        assert_eq!(lines(&p), ["abcde", "X"], "{keep:?}");
        assert_eq!(p.screen().cursor_position(), (1, 1), "{keep:?}");
    }
    let p = run(2, 5, b"abcde\x1b[SX")?;
    assert_eq!(lines(&p), ["", "X"]);
    Ok(())
}

/// DEC STD 070, Appendix D.6.1: with autowrap on, a glyph that does not fit
/// in what is left of the line moves to the start of the next one, so the
/// line goes on there: the row it leaves is soft-wrapped, however its last
/// column ends, as xterm marks it (`fux-vt-compare cases
/// wide-glyph-wrapping-marks-the-row wide-glyph-wrapping-at-four-columns`),
/// and a copy joins the rows.
#[test]
fn a_glyph_that_wraps_marks_its_row_soft_wrapped() -> Result {
    let p = run(2, 5, "あいう".as_bytes())?;
    assert_eq!(lines(&p), ["あい", "う"]);
    assert!(p.screen().row_wrapped(0));
    let window = p.screen().window(0, 2, 5);
    assert_eq!(window.text((0, 0), (1, 1), 100, 100)?, "あいう");
    let p = run(2, 4, "abc界x".as_bytes())?;
    assert_eq!(lines(&p), ["abc", "界x"]);
    assert!(p.screen().row_wrapped(0));
    // A wrap left pending past an SU, which blanks the row: xterm marks it
    // all the same (`replay --engines all --size 2x5 'abcde\e[SX'`).
    let p = run(2, 5, b"abcde\x1b[SX")?;
    assert_eq!(lines(&p), ["", "X"]);
    assert!(p.screen().row_wrapped(0));
    Ok(())
}

/// The attributes of the glyph `sgr` then X print in the first cell.
fn styled(sgr: &str, check: impl FnOnce(CellRef<'_>)) -> Result {
    let p = run(1, 3, format!("{sgr}X").as_bytes())?;
    let cell = p.screen().cell(0, 0).ok_or("no cell")?;
    check(cell);
    Ok(())
}

/// ITU-T T.416, 13.1.8: 38 and 48 take a substring of colon-separated
/// elements, `2:space:r:g:b` for direct colour, the second a colour space
/// identifier, empty when defaulted; xterm's ctlseqs ignores the colour
/// space, and also takes `2:r:g:b`; 58 (underline colour) has the same
/// forms. Expected values are xterm's (`fux-vt-compare replay --engines
/// all`), but for 58, which xterm does not show: Ghostty's, alacritty's,
/// wezterm's and tmux's.
#[test]
fn sgr_colours_take_the_colon_forms_with_a_colour_space() -> Result {
    let red = Color::Rgb(255, 0, 0);
    styled("\x1b[38:2::255:0:0m", |c| assert_eq!(c.fgcolor(), red))?;
    styled("\x1b[48:2::0:255:0m", |c| {
        assert_eq!(c.bgcolor(), Color::Rgb(0, 255, 0));
    })?;
    styled("\x1b[58:2::9:8:7m", |c| {
        assert_eq!(c.underline_color(), Color::Rgb(9, 8, 7));
    })?;
    // The colour space is ignored, whatever it is; so is all after blue.
    styled("\x1b[38:2:9:1:2:3m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb(1, 2, 3));
    })?;
    styled("\x1b[38:2:1:2:3:4:5:6m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb(2, 3, 4));
    })?;
    styled("\x1b[38:2:1:2:3m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb(1, 2, 3));
    })?;
    // Too few elements: no colour.
    styled("\x1b[38:2:1:2m", |c| {
        assert_eq!(c.fgcolor(), Color::Default)
    })?;
    styled("\x1b[38:5m", |c| assert_eq!(c.fgcolor(), Color::Default))?;
    styled("\x1b[1;38:2::10:20:30;4m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb(10, 20, 30));
        assert!(c.bold() && c.underline());
    })?;
    Ok(())
}

/// ECMA-48 8.3.117 and xterm: SGR's parameters apply one after another,
/// so an invalid colour is skipped and the rest still applies. As xterm
/// reads the semicolon form: an index or component past 255 is no
/// colour, though its parameters are taken; another kind than 2 or 5
/// takes only itself; values the list ends before are 0.
#[test]
fn an_invalid_sgr_colour_skips_only_itself() -> Result {
    styled("\x1b[38;5;300;1m", |c| {
        assert_eq!(c.fgcolor(), Color::Default);
        assert!(c.bold());
    })?;
    styled("\x1b[38:5:300;1m", |c| {
        assert_eq!(c.fgcolor(), Color::Default);
        assert!(c.bold());
    })?;
    styled("\x1b[38;2;256;0;0;3m", |c| {
        assert_eq!(c.fgcolor(), Color::Default);
        assert!(c.italic());
    })?;
    styled("\x1b[38;9;1m", |c| assert!(c.bold() && !c.strikeout()))?;
    styled("\x1b[38;2;1;2m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb(1, 2, 0));
    })?;
    styled("\x1b[48;5m", |c| assert_eq!(c.bgcolor(), Color::Idx(0)))?;
    Ok(())
}

/// Underline styles, `4:n`, are kitty's extension, which no reference
/// defines and xterm ignores: Ghostty, alacritty, libvterm, wezterm,
/// xterm.js and tmux read `4:0` as no underline and `4:1` to `4:5` as
/// one style or another. fux-vt keeps no style, and follows them, on
/// purpose departing from xterm, so that a program's curly underline
/// stays an underline.
#[test]
fn underline_styles_set_and_end_underline() -> Result {
    styled("\x1b[4m\x1b[4:0m", |c| assert!(!c.underline()))?;
    styled("\x1b[4:3m", |c| assert!(c.underline()))?;
    styled("\x1b[4:1m", |c| assert!(c.underline()))?;
    Ok(())
}

/// ECMA-48 8.3.117 lists 21 as doubly underlined, and xterm's ctlseqs
/// does too; fux-vt keeps no underline style, so it is underline, as in
/// xterm, Ghostty, libvterm, wezterm, xterm.js and tmux. Bold (1) and
/// faint (2) are separate renditions, each ended by 22: xterm keeps both
/// (`fux-vt-compare replay --engines all --size 1x3 '\e[1;2mX'`), where
/// the vt100 crate let each replace the other.
#[test]
fn sgr_21_underlines_and_bold_and_dim_are_kept_apart() -> Result {
    styled("\x1b[21m", |c| assert!(c.underline()))?;
    styled("\x1b[21;24m", |c| assert!(!c.underline()))?;
    styled("\x1b[1;2m", |c| assert!(c.bold() && c.dim()))?;
    styled("\x1b[2;1m", |c| assert!(c.bold() && c.dim()))?;
    styled("\x1b[1;2;22m", |c| assert!(!c.bold() && !c.dim()))?;
    Ok(())
}

/// DECSTR (`CSI ! p`): the VT520 manual's table (p. 5-150) and DEC STD
/// 070's Soft Terminal Reset (p. 4-37) reset the cursor's visibility,
/// DECOM, DECCKM, the keypad, the margins, the rendition and the saved
/// cursor (home, normal rendition), and leave the screen and the cursor.
/// Both set DECAWM to the terminal's setting, which for xterm, and
/// fux-vt, is on. Expected values are xterm's (`fux-vt-compare replay
/// --engines all`): it resets these and keeps bracketed paste and focus
/// reporting.
#[test]
fn a_soft_reset_restores_the_modes_and_keeps_the_screen() -> Result {
    let p = run(2, 5, b"ab\x1b[1m\x1b[!pX")?;
    assert_eq!(lines(&p), ["abX", ""]);
    assert!(!p.screen().cell(0, 2).ok_or("no cell")?.bold());
    // Autowrap is on again.
    let p = run(1, 3, b"\x1b[?7l\x1b[!pabcd")?;
    assert_eq!(lines(&p), ["d"]);
    assert!(p.screen().autowrap());
    let p = run(
        3,
        5,
        b"\x1b[?25l\x1b[?6h\x1b[?1h\x1b=\x1b[2;3r\x1b[?2004h\x1b[?1004h\x1b[!p",
    )?;
    let screen = p.screen();
    assert!(!screen.hide_cursor());
    assert!(!screen.origin_mode());
    assert!(!screen.application_cursor());
    assert!(!screen.application_keypad());
    assert_eq!(screen.scroll_region(), (0, 2));
    assert!(screen.bracketed_paste() && screen.focus_reporting());
    // The saved cursor goes home, with the normal rendition.
    let p = run(3, 5, b"\x1b[2;2H\x1b[1m\x1b7\x1b[m\x1b[!p\x1b[3;3H\x1b8X")?;
    assert_eq!(lines(&p), ["X", "", ""]);
    assert!(!p.screen().cell(0, 0).ok_or("no cell")?.bold());
    // A wrap waiting at the last column still waits.
    let p = run(2, 5, b"abcde\x1b[!pX")?;
    assert_eq!(lines(&p), ["abcde", "X"]);
    Ok(())
}
