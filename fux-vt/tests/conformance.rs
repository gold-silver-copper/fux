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

/// Whether every cell of row `y` from `cols` is blank in the pen's
/// colours alone: green on red, no bold or underline.
fn blank_in_colours(p: &Parser, y: u16, cols: std::ops::Range<u16>) -> bool {
    cols.into_iter().all(|x| {
        p.screen().cell(y, x).is_some_and(|c| {
            !c.has_contents()
                && c.fgcolor() == Color::Idx(2)
                && c.bgcolor() == Color::Idx(1)
                && !c.bold()
                && !c.underline()
        })
    })
}

/// Background colour erase: `TERM=xterm-256color` advertises `bce`
/// (terminfo.src), so the blanks an erase, a scroll or an insertion brings
/// take the pen's colours, as DEC STD 070, ch. 5, erases and scrolls with
/// the current rendition. xterm fills them with the foreground and
/// background colours and no other attribute (`fux-vt-compare replay
/// --engines all --size 2x5 '\e[1;4;32;41mab\r\n\n\e[m\e[2;5HZ'`),
/// and so does fux-vt: LF at the bottom (with history or none), RI, SU, SD,
/// IL, DL, ICH, DCH, and ED, EL and ECH.
#[test]
fn blanks_brought_in_take_the_pens_colours() -> Result {
    let pen = "\x1b[1;4;32;41m";
    for history in [0, 10] {
        let mut p = Parser::new(2, 5, history)?;
        p.process(format!("{pen}ab\r\n\n").as_bytes())?;
        assert!(blank_in_colours(&p, 1, 0..5), "LF, history {history}");
    }
    for (then, row) in [("\x1bM", 0), ("\x1b[S", 1), ("\x1b[T", 0), ("\x1b[L", 0)] {
        let p = run(2, 5, format!("{pen}{then}").as_bytes())?;
        assert!(blank_in_colours(&p, row, 0..5), "{then:?}");
    }
    let p = run(2, 5, format!("{pen}\x1b[M").as_bytes())?;
    assert!(blank_in_colours(&p, 1, 0..5), "DL");
    let edit = |then: &str| run(1, 5, format!("abcde\x1b[1G{pen}{then}").as_bytes());
    assert!(blank_in_colours(&edit("\x1b[2@")?, 0, 0..2), "ICH");
    assert!(blank_in_colours(&edit("\x1b[2P")?, 0, 3..5), "DCH");
    assert!(blank_in_colours(&edit("\x1b[2X")?, 0, 0..2), "ECH");
    assert!(blank_in_colours(&edit("\x1b[K")?, 0, 0..5), "EL");
    assert!(blank_in_colours(&edit("\x1b[2J")?, 0, 0..5), "ED");
    Ok(())
}

/// The first row's text, `bytes` printed on a screen one row high.
fn row(cols: u16, bytes: &[u8]) -> std::result::Result<String, Box<dyn std::error::Error>> {
    let p = run(1, cols, bytes)?;
    Ok(lines(&p).into_iter().next().unwrap_or_default())
}

/// SCS (VT520 manual, Table 5-13 and 5-14; ECMA-35): `ESC ( 0` designates
/// DEC Special Graphics as G0, `ESC ) 0` as G1, `ESC ( B` ASCII; SO puts
/// G1 in GL and SI G0. `TERM=xterm-256color` draws boxes with them
/// (`smacs=\E(0`, `rmacs=\E(B`). The glyphs are xterm's (`fux-vt-compare
/// replay --engines xterm`), 0x5f a blank; DECSC saves the sets and the
/// shift with the cursor (DEC STD 070's cursor save buffer), DECSTR and
/// RIS designate ASCII again, and a set xterm does not draw in UTF-8 (the
/// U.K. set, `A`) is ASCII.
#[test]
fn dec_special_graphics_draw_lines() -> Result {
    assert_eq!(
        row(6, b"\x1b(0lqqk\x1b(Bx")?,
        "\u{250c}\u{2500}\u{2500}\u{2510}x"
    );
    assert_eq!(row(6, b"\x1b)0\x0elqk\x0fq")?, "\u{250c}\u{2500}\u{2510}q");
    let all = row(40, b"x\x1b(0_`abcdefghijklmnopqrstuvwxyz{|}~^AZ")?;
    assert_eq!(
        all,
        "x \u{25c6}\u{2592}\u{2409}\u{240c}\u{240d}\u{240a}\u{b0}\u{b1}\u{2424}\u{240b}\u{2518}\u{2510}\u{250c}\u{2514}\u{253c}\u{23ba}\u{23bb}\u{2500}\u{23bc}\u{23bd}\u{251c}\u{2524}\u{2534}\u{252c}\u{2502}\u{2264}\u{2265}\u{3c0}\u{2260}\u{a3}\u{b7}^AZ"
    );
    // Only printable ASCII is drawn otherwise.
    assert_eq!(row(4, "\x1b(0é".as_bytes())?, "é");
    // SO with G1 still ASCII changes nothing.
    assert_eq!(row(4, b"\x1b(0\x0eq")?, "q");
    // Saved and restored with the cursor, the shift too.
    assert_eq!(row(4, b"\x1b(0\x1b7\x1b(Bq\x1b8q")?, "\u{2500}");
    assert_eq!(row(4, b"\x1b)0\x0e\x1b7\x0f\x1b8q")?, "\u{2500}");
    for reset in [&b"\x1b[!p"[..], b"\x1bc", b"\x1b(A"] {
        assert_eq!(
            row(4, &[&b"\x1b(0"[..], reset, b"q"].concat())?,
            "q",
            "{reset:?}"
        );
    }
    // A long run, whatever the chunks it comes in.
    let mut p = Parser::new(1, 80, 0)?;
    p.process(b"\x1b(0")?;
    for chunk in [&b"qqqq"[..], b"q", b"qqqqqqqqqqqqqqq"] {
        p.process(chunk)?;
    }
    let line: String = std::iter::repeat_n('\u{2500}', 20).collect();
    assert_eq!(lines(&p), [line]);
    Ok(())
}

/// REP (ECMA-48 8.3.103, `CSI Pn b`): the preceding graphic character is
/// printed Pn more times, 0 or none meaning once, wrapping and taking the
/// character set as printing it again would. ECMA-48 leaves REP undefined
/// after a control function; xterm then repeats nothing, having forgotten
/// its last character once a control, a sequence or a string (REP itself
/// included) came after it, and so does fux-vt. After a cluster, xterm
/// repeats the character that took the cell, without its marks, where
/// ECMA-48 would repeat the last mark: fux-vt departs from the standard
/// with xterm (README, "Departures from the references"). Expected values
/// are xterm's (`fux-vt-compare replay --engines all --size 2x8`).
#[test]
fn rep_repeats_the_preceding_graphic_character() -> Result {
    assert_eq!(row(8, b"-\x1b[4b")?, "-----");
    assert_eq!(run(1, 8, b"-\x1b[4b")?.screen().cursor_position(), (0, 5));
    assert_eq!(row(8, b"-\x1b[b")?, "--");
    assert_eq!(row(8, b"-\x1b[0b")?, "--");
    assert_eq!(row(8, "界\x1b[2b".as_bytes())?, "界界界");
    assert_eq!(row(8, "e\u{301}\x1b[2b".as_bytes())?, "e\u{301}ee");
    assert_eq!(row(8, b"\x1b(0q\x1b[2b")?, "\u{2500}\u{2500}\u{2500}");
    assert_eq!(lines(&run(2, 8, b"abcdefg\x1b[3b")?), ["abcdefgg", "gg"]);
    // Nothing to repeat: at the start, or after a control, a sequence or a
    // string, REP's own included.
    assert_eq!(row(8, b"\x1b[2b")?, "");
    for between in [
        &b"\x1b[2b"[..],
        b"\r",
        b"\x1b[1m",
        b"\x1b]2;x\x07",
        b"\x1b7",
    ] {
        let p = run(1, 8, &[&b"-\x1b[2b"[..], between, b"\x1b[2b"].concat())?;
        assert_eq!(lines(&p), ["---"], "{between:?}");
    }
    // However many: 65536 in all fill the screen, and the history, ending
    // in the last column with a wrap pending, as in xterm.
    let mut p = Parser::new(2, 4, 3)?;
    p.process(b"x\x1b[65535b")?;
    assert_eq!(lines(&p), ["xxxx", "xxxx"]);
    assert_eq!(p.screen().cursor_position(), (1, 3));
    assert!(p.screen().pending_wrap());
    assert_eq!(p.screen().history_len(), 3);
    let window = p.screen().window(3, 2, 4);
    assert_eq!(window.text((0, 0), (1, 3), 100, 100)?, "xxxxxxxx");
    let p = run(2, 5, "界\x1b[65533b".as_bytes())?;
    assert_eq!(lines(&p), ["界界", "界界"]);
    assert_eq!(p.screen().cursor_position(), (1, 4));
    Ok(())
}

/// DECSTBM (DEC STD 070, 5-25): margins with the top above the bottom
/// are set and the cursor goes home, obeying DECOM: the first line with
/// DECOM reset, the top margin with it set (note 1); other margins are
/// ignored (note 2). A bottom past the screen is ignored too (note 3),
/// but xterm takes it as the last line, and fux-vt follows xterm
/// (README, "Departures from the references"). Expected values are
/// xterm's (`fux-vt-compare replay --engines all --size 5x5`).
#[test]
fn decstbm_homes_the_cursor_and_ignores_a_region_it_cannot_set() -> Result {
    let p = run(5, 5, b"abc\x1b[3;5rX")?;
    assert_eq!(lines(&p), ["Xbc", "", "", "", ""]);
    assert_eq!(p.screen().scroll_region(), (2, 4));
    let p = run(5, 5, b"\x1b[?6h\x1b[3;5rX")?;
    assert_eq!(lines(&p), ["", "", "X", "", ""]);
    for invalid in [&b"\x1b[4;2r"[..], b"\x1b[3;3r", b"\x1b[9;99r"] {
        let p = run(5, 5, &[&b"\x1b[2;4r\x1b[3;3H"[..], invalid, b"X"].concat())?;
        assert_eq!(lines(&p), ["", "", "  X", "", ""], "{invalid:?}");
        assert_eq!(p.screen().scroll_region(), (1, 3), "{invalid:?}");
    }
    let p = run(5, 5, b"\x1b[3;3H\x1b[2;99rX")?;
    assert_eq!(p.screen().scroll_region(), (1, 4));
    assert_eq!(lines(&p), ["X", "", "", "", ""]);
    // An ignored DECSTBM leaves a pending wrap waiting.
    let p = run(2, 5, b"abcde\x1b[2;2rX")?;
    assert_eq!(lines(&p), ["abcde", "X"]);
    Ok(())
}

/// VPA and VPR (VT520 manual, 5-208) and HPA and HPR (5-181) address the
/// active position as CUP does: under DECOM, from the top margin and
/// never past the margins (DEC STD 070, DECOM: "the Active position
/// cannot be moved above the Top Margin"), as xterm does for all four;
/// otherwise anywhere on the screen, stopping at its last line or column.
/// VPR therefore passes the bottom margin with DECOM reset, where CUD
/// stops. IL and DL leave the cursor in the first column (DEC STD 070,
/// IL and DL, note 2), and do nothing outside the margins. Expected
/// values are xterm's (`fux-vt-compare replay --engines all --size 5x5`,
/// `fux-vt-compare cases vpa-honours-origin-mode il-moves-to-column-zero
/// dl-moves-to-column-zero`).
#[test]
fn line_and_column_addressing_obeys_origin_mode() -> Result {
    let p = run(5, 5, b"\x1b[2;4r\x1b[?6h\x1b[2dX")?;
    assert_eq!(lines(&p), ["", "", "X", "", ""]);
    let p = run(5, 5, b"\x1b[2;4r\x1b[?6h\x1b[9dX")?;
    assert_eq!(lines(&p), ["", "", "", "X", ""]);
    let p = run(5, 5, b"\x1b[2;4r\x1b[2;2H\x1b[5eX")?;
    assert_eq!(lines(&p), ["", "", "", "", " X"]);
    let p = run(5, 5, b"\x1b[2;4r\x1b[?6h\x1b[1;2H\x1b[5eX")?;
    assert_eq!(lines(&p), ["", "", "", " X", ""]);
    assert_eq!(row(8, b"\x1b[3`X\x1b[2aY")?, "  X  Y");
    assert_eq!(row(8, b"\x1b[99`X")?, "       X");
    assert_eq!(row(8, b"\x1b[0`X\x1b[0aY")?, "X Y");
    // Each ends a pending wrap.
    for movement in [&b"\x1b[1`"[..], b"\x1b[a", b"\x1b[1d", b"\x1b[e"] {
        let p = run(2, 5, &[&b"abcde"[..], movement].concat())?;
        assert!(!p.screen().pending_wrap(), "{movement:?}");
    }
    let p = run(3, 5, b"ab\x1b[LX")?;
    assert_eq!(lines(&p), ["X", "ab", ""]);
    let p = run(3, 5, b"ab\r\ncd\x1b[AX\x1b[MY")?;
    assert_eq!(lines(&p), ["Yd", "", ""]);
    let p = run(5, 5, b"\x1b[2;3r\x1b[5;3H\x1b[LX")?;
    assert_eq!(lines(&p), ["", "", "", "", "  X"]);
    Ok(())
}

/// IND (`ESC D`; DEC STD 070, xterm's ctlseqs; ECMA-48 withdrew it) is a
/// line feed, scrolling at the bottom margin; NEL (`ESC E`, ECMA-48
/// 8.3.86) is the same to the first column. Both end a pending wrap, as
/// LF does (DEC STD 070, Appendix D.6.1). Expected values are xterm's
/// (`fux-vt-compare cases ind-and-nel`).
#[test]
fn ind_and_nel_feed_a_line() -> Result {
    assert_eq!(lines(&run(3, 5, b"ab\x1bDX\x1bEY")?), ["ab", "  X", "Y"]);
    assert_eq!(lines(&run(2, 3, b"a\r\nb\x1bDc")?), ["b", " c"]);
    let p = run(2, 5, b"abcde\x1bDX")?;
    assert_eq!(lines(&p), ["abcde", "    X"]);
    assert!(!p.screen().row_wrapped(0));
    let p = run(2, 5, b"abcde\x1bEX")?;
    assert_eq!(lines(&p), ["abcde", "X"]);
    assert!(!p.screen().row_wrapped(0));
    Ok(())
}

/// Tab stops (ECMA-48 8.3.62 HTS, 8.3.154 TBC, 8.3.10 CHT, 8.3.7 CBT),
/// one set for both screens, as in xterm: every eight columns at first
/// and after RIS, kept by DECSTR (the VT520 manual's table, p. 5-150,
/// does not list them) and by a resize, as xterm keeps them. HT and CHT
/// stop at the last column when no stop is left, CBT at the first. TBC
/// 0 clears the stop at the cursor and 3 every stop; xterm ignores the
/// others, which ECMA-48 defines for stops kept line by line, and so does
/// fux-vt. With a wrap pending, CBT leaves the next glyph to wrap, as in
/// xterm, where ECMA-48 moves back (README, "Departures from the
/// references"). Expected values are xterm's (`fux-vt-compare replay
/// --engines all --size 1x20`, `cases hts-and-tbc cht-and-cbt`).
#[test]
fn tab_stops_are_set_cleared_and_kept() -> Result {
    assert_eq!(row(20, b"\x1b[3g\x1b[5G\x1bH\rX\tY")?, "X   Y");
    assert_eq!(row(20, b"\x1b[9G\x1b[g\r\tX")?, "                X");
    assert_eq!(row(20, b"\x1b[2IX\x1b[ZY")?, "                Y");
    assert_eq!(row(10, b"\x1b[3g\tX")?, "         X");
    assert_eq!(row(10, b"abc\x1b[5ZX")?, "Xbc");
    assert_eq!(row(20, b"\x1b[3g\x1bc\tX")?, "        X");
    assert_eq!(row(20, b"\x1b[3g\x1b[!p\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[3g\x1b[5G\x1bH\x1b[2g\x1b[5g\r\tX")?, "    X");
    let p = run(1, 20, b"\x1b[3g\x1b[5G\x1bH\x1b[?1049h\r\tX")?;
    assert_eq!(lines(&p), ["    X"]);
    // A resize keeps them, and the columns it adds have a reset's.
    let mut p = Parser::new(1, 10, 0)?;
    p.process(b"\x1b[3g\x1b[5G\x1bH")?;
    p.resize(1, 30)?;
    p.process(b"\r\t\tX")?;
    assert_eq!(p.screen().cursor_position(), (0, 29));
    let mut p = Parser::new(1, 10, 0)?;
    p.process(b"\x1b[5G\x1bH")?;
    p.resize(1, 30)?;
    p.process(b"\r\t\t\tX")?;
    assert_eq!(lines(&p), ["                X"]);
    // CBT with a wrap pending: the next glyph still wraps.
    let p = run(2, 10, b"abcdefghij\x1b[ZX")?;
    assert_eq!(lines(&p), ["abcdefghij", "X"]);
    Ok(())
}

/// Modes 1047 and 1048 (xterm's ctlseqs): 1047 switches to the alternate
/// screen and back, clearing it on the way back; 1048 saves and restores
/// the cursor as DECSC and DECRC do. Expected values are xterm's
/// (`fux-vt-compare cases mode-1047-and-1048`).
#[test]
fn modes_1047_and_1048() -> Result {
    let mut p = run(2, 5, b"ab\x1b[?1048h\x1b[?1047hX")?;
    assert!(p.screen().alternate_screen());
    p.process(b"\x1b[?1047l\x1b[?1048lY")?;
    assert!(!p.screen().alternate_screen());
    assert_eq!(lines(&p), ["abY", ""]);
    p.process(b"\x1b[?1047h")?;
    assert_eq!(lines(&p), ["", ""]);
    Ok(())
}

/// IRM (ECMA-48 7.2.10, `CSI 4 h`; `TERM=xterm-256color`'s `smir`): a
/// glyph printed moves what is at and after the cursor right, as ICH
/// does, what passes the last column being lost; RIS and DECSTR end it
/// (VT520 manual, p. 5-150). The insertion keeps the row's soft wrap, as
/// ICH does in xterm, where DCH ends it. Expected values are xterm's
/// (`fux-vt-compare cases insert-mode`, `replay --engines all --size 2x5
/// 'abcdefgh\e[1;1H\e[@'`).
#[test]
fn insert_mode_moves_what_is_there() -> Result {
    assert_eq!(row(6, b"abc\r\x1b[4hX\x1b[4lY")?, "XYbc");
    assert_eq!(row(5, b"abcde\r\x1b[4hXY")?, "XYabc");
    assert_eq!(row(5, "ab\r\x1b[4h界".as_bytes())?, "界ab");
    let p = run(2, 5, b"abcdefgh\x1b[4h\x1b[1;1Hx")?;
    assert_eq!(lines(&p), ["xabcd", "fgh"]);
    assert!(p.screen().row_wrapped(0));
    assert!(p.screen().insert_mode());
    for reset in [&b"\x1b[!p"[..], b"\x1bc"] {
        let p = run(1, 5, &[&b"\x1b[4h"[..], reset].concat())?;
        assert!(!p.screen().insert_mode(), "{reset:?}");
    }
    assert!(
        run(2, 5, b"abcdefgh\x1b[1;1H\x1b[@")?
            .screen()
            .row_wrapped(0)
    );
    assert!(
        !run(2, 5, b"abcdefgh\x1b[1;1H\x1b[P")?
            .screen()
            .row_wrapped(0)
    );
    Ok(())
}

/// Modes 47, 1047 and 1049 (xterm's ctlseqs) switch screens and leave
/// the cursor where it is: xterm keeps one cursor, with its pending wrap,
/// origin mode and margins, for both screens, and each screen its own
/// saved cursor. 1049 saves the cursor as DECSC does before switching,
/// and restores it as DECRC does after switching back; its clear, like
/// 1047's on leaving, ends a pending wrap as ED does (DEC STD 070,
/// Appendix D.6.1) and takes the pen's colours (`bce`). Expected values
/// are xterm's (`fux-vt-compare cases mode-47-keeps-the-cursor
/// mode-1049-keeps-the-cursor`, `replay --engines all --size 2x3`).
#[test]
fn switching_screens_keeps_the_cursor() -> Result {
    for mode in ["47", "1047", "1049"] {
        let p = run(2, 5, format!("ab\x1b[?{mode}hX").as_bytes())?;
        assert_eq!(lines(&p), ["  X", ""], "{mode}");
        assert_eq!(p.screen().cursor_position(), (0, 3), "{mode}");
    }
    assert_eq!(lines(&run(2, 5, b"ab\x1b[?47hX\x1b[?47lY")?), ["ab Y", ""]);
    assert_eq!(
        lines(&run(2, 5, b"ab\x1b[?1049hX\x1b[?1049lY")?),
        ["abY", ""]
    );
    // A pending wrap goes along, but 1049's and 1047's clears end it,
    // and 1049 restores the one it saved.
    let p = run(2, 3, b"abc\x1b[?47hc")?;
    assert_eq!(lines(&p), ["", "c"]);
    assert_eq!(lines(&run(2, 3, b"abc\x1b[?1049hc")?), ["  c", ""]);
    assert_eq!(
        lines(&run(2, 3, b"abc\x1b[?1047h\x1b[?1047lX")?),
        ["abX", ""]
    );
    assert_eq!(
        lines(&run(2, 3, b"abc\x1b[?1049h\x1b[?1049lX")?),
        ["abc", "X"]
    );
    // Origin mode and the margins go along.
    let p = run(4, 5, b"\x1b[2;3r\x1b[?6h\x1b[?47h\x1b[1;1HX")?;
    assert_eq!(lines(&p), ["", "X", "", ""]);
    assert!(p.screen().origin_mode());
    assert_eq!(p.screen().scroll_region(), (1, 2));
    // The cleared screen takes the pen's colours.
    let p = run(2, 3, b"\x1b[41m\x1b[?1049h")?;
    let cell = p.screen().cell(1, 2).ok_or("no cell")?;
    assert_eq!(cell.bgcolor(), Color::Idx(1));
    // Each screen has its own saved cursor, which a clear keeps.
    let p = run(1, 2, b"x\x1b[?1049h\x1b[?1049h\x1b[?1048l")?;
    assert_eq!(p.screen().cursor_position(), (0, 1));
    Ok(())
}

/// Invalid UTF-8 prints U+FFFD, one column wide, for each maximal subpart
/// (the Unicode Standard, 3.9, "U+FFFD Substitution of Maximal
/// Subparts"), as xterm prints it for what real output holds: a Latin-1
/// byte among UTF-8, a sequence cut off by a control, ESC or the input
/// that follows (`fux-vt-compare cases invalid-utf8-prints-a-replacement`).
/// A continuation byte alone xterm reads as Latin-1, and so does fux-vt:
/// a raw C1 control is ignored, and 0xa0 to 0xbf print, so Latin-1 text
/// keeps its `£` (`fux-vt-compare replay --engines all --size 1x12
/// 'price \xa35'`), where the Unicode Standard would print U+FFFD (see
/// the README's "Departures from the references"). U+FFFD itself prints.
#[test]
fn invalid_utf8_prints_a_replacement_character() -> Result {
    assert_eq!(row(12, b"caf\xe9 ok")?, "caf\u{fffd} ok");
    assert_eq!(row(12, b"a\xffb")?, "a\u{fffd}b");
    assert_eq!(row(12, b"a\xe7\x95b")?, "a\u{fffd}b");
    assert_eq!(row(12, b"a\xf0\x9f\rb")?, "b\u{fffd}");
    assert_eq!(row(12, b"a\x80b")?, "ab");
    assert_eq!(row(12, b"price \xa35")?, "price \u{a3}5");
    assert_eq!(row(12, "a\u{fffd}b".as_bytes())?, "a\u{fffd}b");
    // Cut off by ESC, which still begins its sequence.
    let p = run(1, 12, b"a\xe7\x1b[1mb")?;
    assert_eq!(lines(&p), ["a\u{fffd}b"]);
    assert!(p.screen().cell(0, 2).ok_or("no cell")?.bold());
    // Across writes: a sequence completed later is one character, one
    // cut off later is U+FFFD.
    let mut p = Parser::new(1, 12, 0)?;
    p.process(b"a\xc3")?;
    p.process(b"\xa9b\xc3")?;
    p.process(b"c")?;
    assert_eq!(lines(&p), ["a\u{e9}b\u{fffd}c"]);
    Ok(())
}

/// In UTF-8 a control string ends at ST, ESC `\` (ECMA-48 8.3.143; 5.6):
/// the byte 0x9c, ST's 8-bit form, is part of a character there
/// (`\u{271c}` is e2 9c 9c), and ends nothing, as in xterm
/// (`fux-vt-compare cases dcs-payload-holding-0x9c`).
#[test]
fn a_control_string_ends_at_esc_backslash_alone() -> Result {
    for open in ["\x1bP1$r", "\x1b_", "\x1b^", "\x1bX"] {
        let text = format!("{open}\u{271c} leaked\x1b\\ok");
        assert_eq!(row(12, text.as_bytes())?, "ok", "{open:?}");
    }
    Ok(())
}
