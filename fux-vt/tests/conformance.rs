//! Behaviour settled by the specifications in `references/` and by xterm,
//! each test citing the section that sets its expected values. Where xterm
//! departs from the specification, the test says so and follows xterm.

use fux_vt::{CellRef, Color, Feature, Identity, Mode, Options, Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

fn run(rows: u16, cols: u16, bytes: &[u8]) -> Result<Parser> {
    let mut parser = Parser::new(Size::new(rows, cols)?, 0)?;
    parser.process(bytes)?;
    Ok(parser)
}

/// DEC STD 070, Appendix D.6.1 (Auto Wrap Mode) and "Insert or Replace
/// Graphic Character" (p. 5-139): a glyph in the last column leaves the
/// cursor there with the Last Column Flag set, and the next glyph wraps
/// first. Each control the appendix lists clears the flag, so the cursor
/// acts from the last column itself. Expected values are xterm's (XTerm
/// 411, `fux-vt-compare cases '*-while-wrap-pending'`), but for HT and
/// RESET_MODE (AUTO_WRAP_MODE), which the appendix lists and xterm 411
/// keeps the flag through, and a glyph in the last column with DECAWM off,
/// which sets no flag ("when Auto Wrap Mode is set"): there the next glyph
/// overwrites the last column (`compare`'s `tab-ends-a-pending-wrap`,
/// `resetting-autowrap-ends-a-pending-wrap` and
/// `no-wrap-is-left-pending-with-autowrap-off`).
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
    // HT and resetting DECAWM clear the flag; DECAWM off sets none.
    assert_eq!(lines(&run(2, 5, b"abcde\tX")?), ["abcdX", ""]);
    assert_eq!(lines(&run(2, 5, b"abcde\x1b[?7l\x1b[?7hX")?), ["abcdX", ""]);
    assert_eq!(lines(&run(2, 2, b"\x1b[?7lca\x1b[?7hX")?), ["cX", ""]);
    Ok(())
}

/// DEC STD 070, Appendix D.6.1: Save Cursor saves the Last Column Flag
/// and Restore Cursor restores it; xterm does the same for DECSC and
/// SCOSC. What the appendix does not list leaves the flag set: SGR, and
/// SU, as in xterm. HT is in the list, so it ends the wrap, and CHT, HT
/// that many times, does too; xterm, and every engine `fux-vt-compare`
/// runs but avt and vt100, leave the flag set at the last column
/// (`replay --engines all --size 2x5 'abcde\tX'`).
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
    for keep in [&b"\x1b[1m"[..], b"\x1b[?25l"] {
        let p = run(2, 5, &[&b"abcde"[..], keep, b"X"].concat())?;
        assert_eq!(lines(&p), ["abcde", "X"], "{keep:?}");
        assert_eq!(p.screen().cursor_position(), (1, 1), "{keep:?}");
    }
    for tab in [&b"\t"[..], b"\x1b[I", b"\x1b[2I"] {
        let p = run(2, 5, &[&b"abcde"[..], tab].concat())?;
        assert_eq!(p.screen().cursor_position(), (0, 4), "{tab:?}");
        assert!(!p.screen().pending_wrap(), "{tab:?}");
        let p = run(2, 5, &[&b"abcde"[..], tab, b"X"].concat())?;
        assert_eq!(lines(&p), ["abcdX", ""], "{tab:?}");
    }
    let p = run(2, 5, b"abcde\x1b[SX")?;
    assert_eq!(lines(&p), ["", "X"]);
    Ok(())
}

/// DEC STD 070, Appendix D.6.1: a glyph in the last column sets the Last
/// Column Flag "when Auto Wrap Mode is set", and RESET_MODE
/// (AUTO_WRAP_MODE) is among the controls that clear it. So with DECAWM
/// off no wrap is left pending, and the next glyph overwrites the last
/// column, DECAWM set again or not; and resetting DECAWM ends a pending
/// wrap. xterm 411 keeps the flag in both (`fux-vt-compare replay
/// --engines xterm --size 2x5 'abcde\e[?7l\e[?7hX'`).
#[test]
fn the_last_column_flag_is_set_with_autowrap_alone() -> Result {
    for text in ["abcde", "abcd\u{e9}"] {
        let p = run(2, 5, format!("\x1b[?7l{text}").as_bytes())?;
        assert_eq!(p.screen().cursor_position(), (0, 4), "{text:?}");
        assert!(!p.screen().pending_wrap(), "{text:?}");
        let p = run(2, 5, format!("\x1b[?7l{text}\x1b[?7hX").as_bytes())?;
        assert_eq!(lines(&p), ["abcdX", ""], "{text:?}");
    }
    let p = run(2, 5, b"abcde\x1b[?7l")?;
    assert!(!p.screen().pending_wrap());
    let p = run(2, 5, b"abcde\x1b[?7l\x1b[?7hX")?;
    assert_eq!(lines(&p), ["abcdX", ""]);
    assert!(!p.screen().row_wrapped(0));
    // A flag DECRC restores is the Last Column Flag ("restored when a
    // Restore Cursor operation is performed"), which only resetting
    // DECAWM clears, not setting it: with DECAWM set again, DECSET or
    // DECSTR, the next glyph wraps; saved again meanwhile, it is kept.
    for then in [&b"\x1b[?7h"[..], b"\x1b[!p", b"\x1b7\x1b8\x1b[?7h"] {
        let p = run(
            2,
            5,
            &[&b"abcde\x1b7\x1b[?7l\x1b8"[..], then, b"X"].concat(),
        )?;
        assert_eq!(lines(&p), ["abcde", "X"], "{then:?}");
    }
    // No flag for DECSC to save, nor for DECSTR, which sets DECAWM, to
    // carry out; a mark still joins the glyph in the last column.
    for then in [&b"\x1b7\x1b[?7h\x1b8"[..], b"\x1b[!p"] {
        let p = run(2, 5, &[&b"\x1b[?7labcde"[..], then, b"X"].concat())?;
        assert_eq!(lines(&p), ["abcdX", ""], "{then:?}");
    }
    let p = run(1, 5, "\x1b[?7labcde\u{301}".as_bytes())?;
    assert_eq!(lines(&p), ["abcde\u{301}"]);
    // A wide glyph that fits is printed: once DECLRMM is reset, the line
    // runs past the old right margin, where the glyph in it set no flag.
    let p = run(
        1,
        10,
        "\x1b[?69h\x1b[1;5s\x1b[?7l\x1b[1;5Ha\x1b[?69l\u{754c}".as_bytes(),
    )?;
    assert_eq!(lines(&p), ["    \u{754c}"]);
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
    let window = p.screen().window();
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
    let red = Color::Rgb([255, 0, 0].into());
    styled("\x1b[38:2::255:0:0m", |c| assert_eq!(c.fgcolor(), red))?;
    styled("\x1b[48:2::0:255:0m", |c| {
        assert_eq!(c.bgcolor(), Color::Rgb([0, 255, 0].into()));
    })?;
    styled("\x1b[58:2::9:8:7m", |c| {
        assert_eq!(c.underline_color(), Color::Rgb([9, 8, 7].into()));
    })?;
    // The colour space is ignored, whatever it is; so is all after blue.
    styled("\x1b[38:2:9:1:2:3m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb([1, 2, 3].into()));
    })?;
    styled("\x1b[38:2:1:2:3:4:5:6m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb([2, 3, 4].into()));
    })?;
    styled("\x1b[38:2:1:2:3m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb([1, 2, 3].into()));
    })?;
    // Too few elements: no colour.
    styled("\x1b[38:2:1:2m", |c| {
        assert_eq!(c.fgcolor(), Color::Default)
    })?;
    styled("\x1b[38:5m", |c| assert_eq!(c.fgcolor(), Color::Default))?;
    styled("\x1b[1;38:2::10:20:30;4m", |c| {
        assert_eq!(c.fgcolor(), Color::Rgb([10, 20, 30].into()));
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
        assert_eq!(c.fgcolor(), Color::Rgb([1, 2, 0].into()));
    })?;
    styled("\x1b[48;5m", |c| assert_eq!(c.bgcolor(), Color::Idx(0)))?;
    Ok(())
}

/// Underline styles, `4:n`, are kitty's extension
/// (`references/modern/kitty_underlines.html`), which xterm ignores:
/// `4:0` is no underline, and `4:1` to `4:5` straight, double, curly,
/// dotted and dashed, each replacing the last. 4 is straight and 24 ends
/// any (ECMA-48 8.3.117). Ghostty, alacritty, libvterm, wezterm, xterm.js
/// and tmux read them; fux-vt follows the spec, on purpose departing from
/// xterm, so that neovim's curly diagnostics stay curly.
#[test]
fn underline_styles_are_kept_and_replace_one_another() -> Result {
    use fux_vt::UnderlineStyle::{Curly, Dashed, Dotted, Double, None, Single};
    for (sgr, style) in [
        ("\x1b[4:0m", None),
        ("\x1b[4:1m", Single),
        ("\x1b[4:2m", Double),
        ("\x1b[4:3m", Curly),
        ("\x1b[4:4m", Dotted),
        ("\x1b[4:5m", Dashed),
        ("\x1b[4m", Single),
        ("\x1b[4:3m\x1b[4m", Single),
        ("\x1b[4m\x1b[4:5m", Dashed),
        ("\x1b[4:3m\x1b[4:0m", None),
        ("\x1b[4:3m\x1b[24m", None),
        ("\x1b[4:3;1m", Curly),
        // `4:` is `4:0`; a number past 5 names no style and changes nothing.
        ("\x1b[4:3m\x1b[4:m", None),
        ("\x1b[4:3m\x1b[4:6m", Curly),
        ("\x1b[4:3m\x1b[0m", None),
    ] {
        styled(sgr, |c| {
            assert_eq!(c.underline_style(), style, "{sgr:?}");
            assert_eq!(c.underline(), style != None, "{sgr:?}");
        })?;
    }
    styled("\x1b[1;4:3;9m", |c| assert!(c.bold() && c.strikeout()))?;
    Ok(())
}

/// ECMA-48 8.3.117 lists 21 as doubly underlined, and xterm's ctlseqs
/// does too, as xterm, Ghostty, libvterm, wezterm, xterm.js and tmux read
/// it; 24 ends it. Bold (1) and
/// faint (2) are separate renditions, each ended by 22: xterm keeps both
/// (`fux-vt-compare replay --engines all --size 1x3 '\e[1;2mX'`), where
/// the vt100 crate let each replace the other.
#[test]
fn sgr_21_underlines_doubly_and_bold_and_dim_are_kept_apart() -> Result {
    use fux_vt::UnderlineStyle;
    styled("\x1b[21m", |c| {
        assert_eq!(c.underline_style(), UnderlineStyle::Double);
    })?;
    styled("\x1b[21;4m", |c| {
        assert_eq!(c.underline_style(), UnderlineStyle::Single);
    })?;
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
    assert!(p.screen().mode(Mode::Autowrap));
    let p = run(
        3,
        5,
        b"\x1b[?25l\x1b[?6h\x1b[?1h\x1b=\x1b[2;3r\x1b[?2004h\x1b[?1004h\x1b[!p",
    )?;
    let screen = p.screen();
    assert!(screen.mode(Mode::ShowCursor));
    assert!(!screen.mode(Mode::Origin));
    assert!(!screen.mode(Mode::ApplicationCursor));
    assert!(!screen.mode(Mode::ApplicationKeypad));
    assert_eq!(screen.scroll_region(), (0, 2));
    assert!(screen.mode(Mode::BracketedPaste) && screen.mode(Mode::FocusReporting));
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
        let mut p = Parser::new(Size::new(2, 5)?, history)?;
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

/// DEC STD 070, Erase in Line and Erase in Display (and DECSEL, DECSED):
/// their algorithms carry out each selective parameter in turn (revision
/// note 12 of chapter 5, "processing of multiple selective parameters";
/// 3.5.3.3: as many separate sequences, each with one), so `1;0` erases
/// what 1 and then 0 erase, the whole line or display, and an unknown
/// parameter beside them is ignored. xterm 411 reads the first alone.
#[test]
fn ed_and_el_carry_out_each_parameter() -> Result {
    assert_eq!(lines(&run(1, 6, b"abcdef\x1b[1;3H\x1b[1;0K")?), [""]);
    assert_eq!(lines(&run(1, 6, b"abcdef\x1b[1;3H\x1b[9;1K")?), ["   def"]);
    let screen = b"abc\r\ndef\r\nghi\x1b[2;2H";
    assert_eq!(
        lines(&run(3, 3, &[&screen[..], b"\x1b[0;1J"].concat())?),
        ["", "", ""]
    );
    assert_eq!(
        lines(&run(3, 3, &[&screen[..], b"\x1b[1;5J"].concat())?),
        ["", "  f", "ghi"]
    );
    // DECSEL alike, sparing the glyphs DECSCA protects.
    let protected = b"a\x1b[1\"qb\x1b[0\"qcdef\x1b[1;3H\x1b[?1;0K";
    assert_eq!(lines(&run(1, 6, protected)?), [" b"]);
    Ok(())
}

/// The first row's text, `bytes` printed on a screen one row high.
fn row(cols: u16, bytes: &[u8]) -> Result<String> {
    let p = run(1, cols, bytes)?;
    Ok(lines(&p).into_iter().next().unwrap_or_default())
}

/// G2 and G3 (ECMA-35, 6.3 and 9; DEC STD 070, ch. 3): `ESC * F` and
/// `ESC + F` designate them, `0` DEC Special Graphics; LS2 (`ESC n`) and
/// LS3 (`ESC o`) invoke them into GL until another shift, SI or SO among
/// them; SS2 (`ESC N`) and SS3 (`ESC O`) invoke G2 or G3 for the next
/// graphic character alone. DECSC saves them with G0 and G1, and RIS
/// designates ASCII again. A single shift is no state DECSC saves (DEC STD
/// 070, DECSC: the designations and the sets in GL and GR), and any
/// function but a graphic character, SO or SI cancels it (3.8.3.2: "the
/// single shift function is ignored"), DECSC and DECRC among them.
#[test]
fn g2_and_g3_are_shifted_in_locked_or_for_one_character() -> Result {
    assert_eq!(row(6, b"\x1b*0\x1bN\x1b7q\x1b8q")?, "q");
    assert_eq!(row(6, b"\x1b*0\x1b7\x1bN\x1b8q")?, "q");
    assert_eq!(row(6, b"\x1b*0\x1bnqq\x0fq")?, "\u{2500}\u{2500}q");
    assert_eq!(row(6, b"\x1b+0\x1boxx\x0ex")?, "\u{2502}\u{2502}x");
    assert_eq!(row(6, b"\x1b*0\x1bNqq")?, "\u{2500}q");
    assert_eq!(row(6, b"\x1b+0\x1bOxx")?, "\u{2502}x");
    assert_eq!(row(6, b"\x1b(0\x1b*B\x1bNqq")?, "q\u{2500}");
    assert_eq!(row(6, b"\x1b*0\x1bn\x1b7\x0f\x1b8q")?, "\u{2500}");
    assert_eq!(row(6, b"\x1b*0\x1bn\x1bcq")?, "q");
    Ok(())
}

/// A single shift is spent on the next graphic character; another function
/// cancels it, but SO and SI, which "may occur between the Single Shift
/// and the subsequent graphic character" (DEC STD 070, 3.8.3.2): a C0
/// control, an escape or control sequence, a string.
#[test]
fn a_single_shift_is_cancelled_by_any_function_but_so_and_si() -> Result {
    assert_eq!(row(6, b"\x1b*0\x1bN\x0e\x0fq")?, "\u{2500}");
    for between in [
        &b"\x07"[..],
        b"\x00",
        b"\x1b[C",
        b"\x1b=",
        b"\x1b]2;t\x07",
        b"\x1bP$qm\x1b\\",
    ] {
        let bytes = [&b"\x1b*0\x1bN"[..], between, b"q"].concat();
        assert_eq!(row(6, &bytes)?.trim_start(), "q", "{between:?}");
    }
    // SS3 after SS2 is the one pending.
    assert_eq!(row(6, b"\x1b*0\x1b+B\x1bN\x1bOq")?, "q");
    Ok(())
}

/// SCS designates what ctlseqs lists for its final and intermediates (the
/// national sets and the others as ASCII); a combination that names no
/// set is not implemented, ignored "as if [it] were not received" (DEC
/// STD 070, 3.5.1.3), the set designated before staying.
#[test]
fn scs_of_no_set_is_ignored() -> Result {
    assert_eq!(row(6, b"\x1b(0\x1b(Wq")?, "\u{2500}");
    assert_eq!(row(6, b"\x1b(0\x1b(%9q")?, "\u{2500}");
    assert_eq!(row(6, b"\x1b(0\x1b(Aq")?, "q");
    assert_eq!(row(6, b"\x1b(0\x1b(%6q")?, "q");
    Ok(())
}

/// DECSCL (DEC STD 070, DECSCL): `CSI 61 " p` and `CSI 62 " p` select
/// level 1 or 2, each with a soft reset (note 3); a level fux-vt does not
/// support is ignored (note 4). A soft reset at level 1 designates ASCII
/// into all four sets (3.6.4.1), at level 2 DEC Supplemental into G2 and
/// G3 (3.6.4.2); RIS starts level 2 again. The level gates no function.
#[test]
fn decscl_selects_a_level_with_a_soft_reset() -> Result {
    assert_eq!(row(4, b"\x1b[4hab\r\x1b[61\"pX")?, "Xb");
    assert_eq!(row(4, b"\x1b[4hab\r\x1b[62;1\"pX")?, "Xb");
    assert_eq!(row(4, b"\x1b[4hab\r\x1b[64\"pX")?, "Xab");
    assert_eq!(row(4, b"\x1b[61\"p\x1bNA")?, "A");
    assert_eq!(row(4, b"\x1b[61\"p\x1b[!p\x1bNA")?, "A");
    assert_eq!(row(4, b"\x1b[61\"p\x1b[62\"p\x1bNA")?, "\u{c1}");
    assert_eq!(row(4, b"\x1b[61\"p\x1bc\x1bNA")?, "\u{c1}");
    // Level 1 executes what level 2 does: no function is gated.
    assert_eq!(row(4, b"\x1b[61\"pab\x1b[1;1H\x1b[2X")?, "");
    Ok(())
}

/// DEC Supplemental Graphics (DEC STD 070, 5.8.1: required at levels 2 to
/// 4): SCS designates it with `% 5`, and with `<`, the user-preferred
/// supplemental set, which is DEC Supplemental (SCS; 3.6.3). At level 2,
/// G2 and G3 hold it at power-up and after a soft reset (DEC STD 070,
/// 3.6.4.2: "the DEC Supplemental graphics set in G2 and G3"), so SS2 and
/// LS2 reach it with no designation, where xterm has ASCII there. Its characters are the DEC Multinational set's right
/// half (the VT330 and VT340 manual, Figure 2-5): Latin-1's 0x80 above,
/// but ¤ at 0x28, Œ at 0x57, Ÿ at 0x5d, œ at 0x77 and ÿ at 0x7d, and the
/// positions it leaves empty (0x24, 0x26, 0x2c to 0x2f, 0x34, 0x38, 0x3e,
/// 0x50, 0x5e, 0x70 and 0x7e) show U+2426, the reversed question mark.
#[test]
fn dec_supplemental_graphics_is_in_g2_and_g3_at_level_2() -> Result {
    assert_eq!(row(6, b"\x1bNA")?, "\u{c1}");
    assert_eq!(row(6, b"\x1bOa\x1bnbc")?, "\u{e1}\u{e2}\u{e3}");
    assert_eq!(row(6, b"\x1b*B\x1bNA")?, "A");
    assert_eq!(row(6, b"\x1b*B\x1b*<\x1bNA")?, "\u{c1}");
    assert_eq!(row(6, b"\x1b*B\x1b[!p\x1bNA")?, "\u{c1}");
    assert_eq!(row(6, b"\x1b*B\x1bc\x1bNA")?, "\u{c1}");
    assert_eq!(row(4, b"\x1b)%5\x0e\x21\x7e\x0f!")?, "\u{a1}\u{2426}!");
    let all = b"\x1b(%5\x28\x57\x5d\x77\x7d\x5f\x24\x50\x2c\x2f";
    assert_eq!(
        row(12, all)?,
        "\u{a4}\u{152}\u{178}\u{153}\u{ff}\u{df}\u{2426}\u{2426}\u{2426}\u{2426}"
    );
    // Other sets are ASCII, % 6 (Portuguese) among them.
    assert_eq!(row(4, b"\x1b(%6a")?, "a");
    Ok(())
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
    let mut p = Parser::new(Size::new(1, 80)?, 0)?;
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
/// included) came after it, and so does fux-vt. ECMA-48 knows no grapheme
/// cluster: after one, xterm repeats the character that took the cell,
/// without its marks, and so does fux-vt (README, "Where the standards
/// leave the choice"). A wide glyph a one-column screen drops is still the
/// preceding graphic character, so REP repeats it, dropped again. Expected values are xterm's
/// (`fux-vt-compare replay --engines all --size 2x8`; `--size 1x1
/// '#\u{754c}\e[4b'`).
#[test]
fn rep_repeats_the_preceding_graphic_character() -> Result {
    assert_eq!(row(8, b"-\x1b[4b")?, "-----");
    assert_eq!(run(1, 8, b"-\x1b[4b")?.screen().cursor_position(), (0, 5));
    assert_eq!(row(8, b"-\x1b[b")?, "--");
    assert_eq!(row(8, b"-\x1b[0b")?, "--");
    assert_eq!(row(8, "界\x1b[2b".as_bytes())?, "界界界");
    assert_eq!(row(8, "e\u{301}\x1b[2b".as_bytes())?, "e\u{301}ee");
    assert_eq!(row(8, b"\x1b(0q\x1b[2b")?, "\u{2500}\u{2500}\u{2500}");
    // After a single shift, the character it invoked: ECMA-48 (8.3.103)
    // repeats the graphic character "represented by one or more bit
    // combinations", here SS2 and `q` in DEC Special Graphics. xterm prints
    // the byte again in GL (`replay --engines xterm '\e*0\eNq\e[3b'`).
    assert_eq!(row(8, b"\x1b*0\x1bNq\x1b[2b")?, "\u{2500}\u{2500}\u{2500}");
    assert_eq!(lines(&run(2, 8, b"abcdefg\x1b[3b")?), ["abcdefgg", "gg"]);
    assert_eq!(lines(&run(1, 1, "#界\x1b[4b".as_bytes())?), [""]);
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
    // in the last column with a wrap pending, as in xterm. History holds
    // three rows of four cells: 24 bytes a row and 8 a cell.
    let mut p = Parser::new(Size::new(2, 4)?, 3 * 56)?;
    p.process(b"x\x1b[65535b")?;
    assert_eq!(lines(&p), ["xxxx", "xxxx"]);
    assert_eq!(p.screen().cursor_position(), (1, 3));
    assert!(p.screen().pending_wrap());
    assert_eq!(p.screen().history_len(), 3);
    let window = p.screen().rows().next().ok_or("a row")?.window();
    assert_eq!(window.text((0, 0), (1, 3), 100, 100)?, "xxxxxxxx");
    let p = run(2, 5, "界\x1b[65533b".as_bytes())?;
    assert_eq!(lines(&p), ["界界", "界界"]);
    assert_eq!(p.screen().cursor_position(), (1, 4));
    Ok(())
}

/// REP's copies of a glyph write what printing it again writes: over
/// blanks, over the same glyph, and over wide glyphs either half of which a copy lands
/// on, wrapping and scrolling.
#[test]
fn rep_writes_what_printing_again_writes() -> Result {
    fn fed(bytes: &[u8]) -> Result<Parser> {
        let mut parser = Parser::new(Size::new(3, 7)?, 1 << 10)?;
        parser.process(bytes)?;
        Ok(parser)
    }
    for before in [
        "",
        "\x1b[H界界界界界界",
        "\x1b[Ha界b界界界界",
        "\x1b[Héééé",
        "\x1b[H\x1b[1m界界",
        // Inside left and right margins, a scroll region, below it on a
        // halved line, with autowrap off, in insert mode and on a halved
        // line, past the copies that change anything.
        "\x1b[?69h\x1b[2;5s",
        "\x1b[1;2r",
        "\x1b[1;2r\x1b[3;1H\x1b#6",
        "\x1b[?7l",
        "\x1b[Habc\x1b[4h",
        "\x1b[H\x1b#6",
    ] {
        for glyph in ["é", "界", "x"] {
            for at in ["\x1b[H", "\x1b[1;2H", "\x1b[2;7H", "\x1b[3;3H"] {
                for count in [1, 2, 5, 6, 7, 13, 40, 300, 1000] {
                    let start = format!("{before}{before}\x1b[m{at}{glyph}");
                    let rep = fed(format!("{start}\x1b[{count}b").as_bytes())?;
                    let again = {
                        let copies: String = std::iter::repeat_n(glyph, count).collect();
                        fed(format!("{start}{copies}").as_bytes())?
                    };
                    let case = format!("{start:?} REP {count}");
                    assert_eq!(lines(&rep), lines(&again), "{case}");
                    let (a, b) = (rep.screen(), again.screen());
                    assert_eq!(a.cursor_position(), b.cursor_position(), "{case}");
                    assert_eq!(a.pending_wrap(), b.pending_wrap(), "{case}");
                    // The screen's rows: history keeps what its budget lets,
                    // trimmed in steps, which fewer copies may meet apart.
                    let shown = |s: &fux_vt::Screen| -> Vec<Vec<String>> {
                        let rows: Vec<_> = s.rows().collect();
                        let first = rows.len().saturating_sub(3);
                        let rows = rows.get(first..).unwrap_or_default();
                        rows.iter()
                            .map(|r| r.cells().map(|c| format!("{c:?}")).collect())
                            .collect()
                    };
                    assert_eq!(shown(a), shown(b), "{case}: the cells differ");
                }
            }
        }
    }
    Ok(())
}

/// DECSTBM (DEC STD 070, 5-25): margins with the top above the bottom
/// are set and the cursor goes home, obeying DECOM: the first line with
/// DECOM reset, the top margin with it set (note 1); other margins are
/// ignored (note 2). A bottom past the screen is its last line, as xterm
/// takes it (note 3 ignores it). Expected values are xterm's
/// (`fux-vt-compare replay --engines all --size 5x5`).
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
    for past in [&b"\x1b[2;99r"[..], b"\x1b[2;6r"] {
        let p = run(5, 5, &[&b"\x1b[2;4r\x1b[3;3H"[..], past, b"X"].concat())?;
        assert_eq!(p.screen().scroll_region(), (1, 4), "{past:?}");
        assert_eq!(lines(&p), ["X", "", "", "", ""], "{past:?}");
    }
    let p = run(5, 5, b"\x1b[2;4r\x1b[2;5rX")?;
    assert_eq!(p.screen().scroll_region(), (1, 4));
    // An ignored DECSTBM leaves a pending wrap waiting.
    let p = run(2, 5, b"abcde\x1b[2;2rX")?;
    assert_eq!(lines(&p), ["abcde", "X"]);
    Ok(())
}

/// A scroll up of a region at the top of the screen takes the rows it
/// scrolls off into history, as xterm keeps them, and leaves the lines
/// below the region: a status line under a chat's lines (irssi, weechat).
/// A region below the top keeps none, nor does SD.
#[test]
fn a_region_at_the_top_scrolls_its_rows_into_history() -> Result {
    let mut p = Parser::new(Size::new(4, 5)?, 1 << 16)?;
    p.process(b"\x1b[4;1Hstat\x1b[1;3r\x1b[1;1Ha\r\nb\r\nc\r\nd\r\ne")?;
    assert_eq!(lines(&p), ["c", "d", "e", "stat"]);
    let history: Vec<String> = (p.screen().rows())
        .take(p.screen().history_len())
        .map(|r| r.cells_text().trim_end().to_owned())
        .collect();
    assert_eq!(history, ["a", "b"]);
    let mut p = Parser::new(Size::new(4, 5)?, 1 << 16)?;
    p.process(b"\x1b[2;3r\x1b[2;1Ha\r\nb\r\nc\x1b[1;3r\x1b[T")?;
    assert_eq!(lines(&p), ["", "", "b", ""]);
    assert_eq!(p.screen().history_len(), 0);
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

/// The replies to `bytes` on a `rows` by `cols` screen answering
/// DECXCPR, with an identity if `identity`.
fn replies(rows: u16, cols: u16, identity: bool, bytes: &[u8]) -> Result<Vec<String>> {
    let fux = Identity {
        name: "fux",
        version: "1.0.0",
    };
    let options = Options::new()
        .with(Feature::ExtendedReplies)
        .with_identity(identity.then_some(fux));
    let mut parser = Parser::with_options(Size::new(rows, cols)?, 0, options)?;
    let mut replies = Vec::new();
    parser.process_with_replies(bytes, |r| {
        replies.push(String::from_utf8_lossy(r).into_owned());
    })?;
    Ok(replies)
}

/// DECREQTPARM (the VT100 User Guide, chapter 3, DECREQTPARM): the
/// VT100 that DA claims without an identity answers a request, `sol` 0 or
/// none and 1, with DECREPTPARM, a report, `sol` 2 or 3, of xterm's fixed
/// parameters (ctlseqs, `CSI Ps x`: no parity, 8 bits, 128 for each
/// speed, clock multiplier 1, no flags). Another `sol` is no request. With
/// an identity, DA's VT220 does not answer it: xterm answers "if ... xterm
/// is emulating VT100" alone.
#[test]
fn decreqtparm_is_answered_as_a_vt100_answers_it() -> Result {
    assert_eq!(
        replies(2, 8, false, b"\x1b[x\x1b[0x\x1b[1x\x1b[2x\x1b[3x")?,
        [
            "\x1b[2;1;1;128;128;1;0x",
            "\x1b[2;1;1;128;128;1;0x",
            "\x1b[3;1;1;128;128;1;0x"
        ]
    );
    assert!(replies(2, 8, true, b"\x1b[x\x1b[1x")?.is_empty());
    Ok(())
}

/// CPR and DECXCPR count the line as CUP addresses it: from the top
/// margin with DECOM set, "relative to the origin of the current
/// scrolling region (the Top and Left Margin)", from the screen's top
/// with it reset (DEC STD 070, CPR and DECXCPR, note 1 and the
/// algorithms, pages 5-53 to 5-56; the VT100 User Guide, chapter 3, CPR:
/// "The numbering of lines depends on the state of the Origin Mode";
/// xterm's charproc.c, `CASE_CPR`). Without left and right margins the
/// column is the screen's (with them, see `margins.rs`). A cursor waiting
/// to wrap reports its column, the last, with an identity or without:
/// CPR reports "the active presentation position" (ECMA-48 8.3.14), "the
/// Active Line and Active Column" (DEC STD 070, CPR, p. 5-53), and the Last
/// Column Flag is a flag, not a column (Appendix D.6.1). Expected values are xterm's (`fux-vt-compare replay
/// --engines all --size 12x10 '\e[6;11r\e[?6h\e[2d\e[6n'`: Ghostty,
/// WezTerm and avt agree; alacritty, libvterm, xterm.js and tmux report
/// from the screen's top).
#[test]
fn cursor_reports_count_lines_from_the_top_margin_in_origin_mode() -> Result {
    let report = b"\x1b[6n\x1b[?6n";
    for (setup, cpr) in [
        (&b"\x1b[6;11r\x1b[?6h\x1b[2d"[..], "2;1"),
        (b"\x1b[6;11r\x1b[?6h\x1b[3;4H", "3;4"),
        (b"\x1b[6;11r\x1b[?6h\x1b[99;4H", "6;4"),
        (b"\x1b[6;11r\x1b[?6h\x1b[3;4H\x1b[?6l\x1b[3;4H", "3;4"),
        (b"\x1b[6;11r\x1b[8;4H", "8;4"),
        (b"\x1b[6;11r\x1b[2d", "2;1"),
    ] {
        let expected = [format!("\x1b[{cpr}R"), format!("\x1b[?{cpr}R")];
        let both = [setup, &report[..]].concat();
        assert_eq!(replies(24, 80, false, &both)?, expected, "{setup:?}");
        assert_eq!(replies(24, 80, true, &both)?, expected, "{setup:?}");
    }
    // At the last column, waiting to wrap, in the region's second line.
    let wrap = b"\x1b[6;11r\x1b[?6h\x1b[2;1H0123456789\x1b[6n";
    assert_eq!(replies(12, 10, false, wrap)?, ["\x1b[2;10R"]);
    assert_eq!(replies(12, 10, true, wrap)?, ["\x1b[2;10R"]);
    // DECSC kept the cursor above where the margins now begin: its line
    // would be negative. xterm's subtraction wraps (it reports 65532),
    // Ghostty reports the first line, as fux-vt does.
    let above = b"\x1b[?6h\x1b7\x1b[6;11r\x1b8\x1b[6n";
    assert_eq!(replies(24, 80, false, above)?, ["\x1b[1;1R"]);
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

/// Tab stops (ECMA-48 8.3.62 HTS, 8.3.154 TBC, 8.3.10 CHT, 8.3.7 CBT), one
/// set for both screens, as in xterm: every eight columns at first and
/// after RIS, kept by DECSTR (the VT520 manual's table, p. 5-150, does not
/// list them) and by a resize, as xterm keeps them. HT and CHT stop at the
/// last column when no stop is left, CBT at the first. TBC 0 clears the
/// stop at the cursor and 3 every stop, the two DEC STD 070 (TBC) defines;
/// of the others, which ECMA-48 defines (8.3.154), 2 and 5 clear every
/// stop too (`tbc_acts_on_each_of_its_parameters`), where xterm does
/// nothing. With a wrap pending CBT moves back from the last
/// column, the active position (ECMA-48 8.3.7), and so ends the wrap, as
/// DEC STD 070 (Appendix D.6.1) ends it for a control that may move the
/// cursor; xterm moves it but keeps the wrap. Expected values are xterm's
/// (`fux-vt-compare replay --engines all --size 1x20`, `cases hts-and-tbc
/// cht-and-cbt`), but for CBT with a wrap pending.
#[test]
fn tab_stops_are_set_cleared_and_kept() -> Result {
    assert_eq!(row(20, b"\x1b[3g\x1b[5G\x1bH\rX\tY")?, "X   Y");
    assert_eq!(row(20, b"\x1b[9G\x1b[g\r\tX")?, "                X");
    assert_eq!(row(20, b"\x1b[2IX\x1b[ZY")?, "                Y");
    assert_eq!(row(10, b"\x1b[3g\tX")?, "         X");
    assert_eq!(row(10, b"abc\x1b[5ZX")?, "Xbc");
    assert_eq!(row(20, b"\x1b[3g\x1bc\tX")?, "        X");
    assert_eq!(row(20, b"\x1b[3g\x1b[!p\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[3g\x1b[5G\x1bH\x1b[1g\x1b[4g\r\tX")?, "    X");
    let p = run(1, 20, b"\x1b[3g\x1b[5G\x1bH\x1b[?1049h\r\tX")?;
    assert_eq!(lines(&p), ["    X"]);
    // A resize keeps them, and the columns it adds have a reset's.
    let mut p = Parser::new(Size::new(1, 10)?, 0)?;
    p.process(b"\x1b[3g\x1b[5G\x1bH")?;
    p.resize(Size::new(1, 30)?)?;
    p.process(b"\r\t\tX")?;
    assert_eq!(p.screen().cursor_position(), (0, 29));
    let mut p = Parser::new(Size::new(1, 10)?, 0)?;
    p.process(b"\x1b[5G\x1bH")?;
    p.resize(Size::new(1, 30)?)?;
    p.process(b"\r\t\t\tX")?;
    assert_eq!(lines(&p), ["                X"]);
    // CBT with a wrap pending moves back from the last column, and ends
    // the wrap.
    let p = run(2, 10, b"abcdefghij\x1b[ZX")?;
    assert_eq!(lines(&p), ["abcdefghXj", ""]);
    assert!(!p.screen().pending_wrap());
    Ok(())
}

/// TBC with several parameters acts on each in turn (DEC STD 070, TBC,
/// p. 5-66, whose algorithm loops over every parameter, and its revision
/// note 11): `CSI 0 ; 3 g` clears every stop, and in `CSI 1 ; 0 g` the 0
/// still clears the stop at the cursor, though 1 (the line tabulation stop,
/// of which there are none) does nothing. ECMA-48 8.3.154 gives 2 ("all
/// character tabulation stops in the active line", and under TSM's
/// MULTIPLE, which fux-vt keeps for good, every line's) and 5 ("all
/// tabulation stops") too, each clearing every stop.
#[test]
fn tbc_acts_on_each_of_its_parameters() -> Result {
    assert_eq!(row(20, b"\x1b[0;3g\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[9G\x1b[1;0g\r\tX")?, "                X");
    assert_eq!(row(20, b"\x1b[2g\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[5g\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[4g\tX")?, "        X");
    Ok(())
}

/// The cheap functions (ECMA-48): HPB (8.3.58, `CSI Pn j`) moves the cursor
/// back n columns and VPB (8.3.159, `CSI Pn k`) up n lines, each ending a
/// pending wrap (DEC STD 070, D.6.1, cursor movement); CTC (8.3.17, `CSI
/// Ps W`) sets a character tabulation stop at the cursor (0), clears it (2),
/// or clears every one (4, the active line's, under TSM's MULTIPLE every
/// line's; and 5), the line tabulation stops (1, 3, 6) being none; TSR
/// (8.3.156, `CSI Pn SP d`) clears the stop at column n, and with no
/// parameter, which it has no default for, does nothing. DECST8C (ctlseqs,
/// `CSI ? 5 W`) sets a stop every 8 columns again. `CSI` with 5 parameters
/// and `T` is xterm's mouse highlight tracking (ctlseqs, XTHIMOUSE), which
/// fux-vt ignores, not SD.
#[test]
fn hpb_vpb_ctc_tsr_and_decst8c() -> Result {
    assert_eq!(row(10, b"\x1b[5Gab\x1b[2jX")?, "    Xb");
    let p = run(3, 4, b"\x1b[3;2H\x1b[2kX")?;
    assert_eq!(lines(&p), [" X", "", ""]);
    let p = run(2, 3, b"abc\x1b[jX")?;
    assert_eq!(lines(&p), ["aXc", ""]);
    assert_eq!(row(20, b"\x1b[5G\x1b[W\r\tX")?, "    X");
    assert_eq!(row(20, b"\x1b[9G\x1b[2W\r\tX")?, "                X");
    assert_eq!(row(20, b"\x1b[4W\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[5W\tX")?, "                   X");
    assert_eq!(row(20, b"\x1b[1;3;6W\tX")?, "        X");
    assert_eq!(row(20, b"\x1b[9 d\tX")?, "                X");
    assert_eq!(row(20, b"\x1b[ d\tX")?, "        X");
    assert_eq!(row(20, b"\x1b[3g\x1b[?5W\tX")?, "        X");
    let p = run(3, 2, b"a\r\nb\x1b[1;2;3;4;5T")?;
    assert_eq!(lines(&p), ["a", "b", ""]);
    Ok(())
}

/// Mode 41 (ctlseqs, `CSI ? 41 h`, more(1)'s fix): set, HT with a wrap
/// pending carries the wrap out first, then goes to the next stop (xterm's
/// `TabNext` with its `curses` resource), so a line exactly as wide as the
/// screen followed by a tab draws the tab. Reset, as by default, HT ends a
/// pending wrap without carrying it out (DEC STD 070, Appendix D.6.1, which
/// lists HT), where xterm keeps the wrap pending: ctlseqs describes only
/// the set state, and DEC decides HT's.
#[test]
fn mode_41_has_ht_carry_out_a_pending_wrap() -> Result {
    let p = run(2, 10, b"\x1b[?41h0123456789\tX")?;
    assert_eq!(lines(&p), ["0123456789", "        X"]);
    assert!(!p.screen().row_wrapped(0));
    let p = run(2, 10, b"0123456789\tX")?;
    assert_eq!(lines(&p), ["012345678X", ""]);
    let p = run(2, 10, b"\x1b[?41h\x1b[?41l0123456789\tX")?;
    assert_eq!(lines(&p), ["012345678X", ""]);
    Ok(())
}

/// Modes 1047 and 1048 (xterm's ctlseqs): 1047 switches to the alternate
/// screen and back, clearing it on the way back; 1048 saves and restores
/// the cursor as DECSC and DECRC do. Expected values are xterm's
/// (`fux-vt-compare cases mode-1047-and-1048`).
#[test]
fn modes_1047_and_1048() -> Result {
    let mut p = run(2, 5, b"ab\x1b[?1048h\x1b[?1047hX")?;
    assert!(p.screen().mode(Mode::AlternateScreen));
    p.process(b"\x1b[?1047l\x1b[?1048lY")?;
    assert!(!p.screen().mode(Mode::AlternateScreen));
    assert_eq!(lines(&p), ["abY", ""]);
    // What the program drew is kept to read until the screen is shown
    // again, cleared, as 1049's and 47's are.
    let kept: Vec<String> = (p.screen().alternate_rows())
        .map(|r| r.cells_text().trim_end().to_owned())
        .collect();
    assert_eq!(kept, ["  X", ""]);
    p.process(b"\x1b[?47h")?;
    assert_eq!(lines(&p), ["", ""]);
    // Set again while it is shown, nothing is left, so nothing is cleared
    // (xterm's ToAlternate does nothing on the alternate screen).
    let p = run(2, 5, b"\x1b[?1047hX\x1b[?1047h")?;
    assert_eq!(lines(&p), ["X", ""]);
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
    assert!(p.screen().mode(Mode::Insert));
    for reset in [&b"\x1b[!p"[..], b"\x1bc"] {
        let p = run(1, 5, &[&b"\x1b[4h"[..], reset].concat())?;
        assert!(!p.screen().mode(Mode::Insert), "{reset:?}");
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
    assert!(p.screen().mode(Mode::Origin));
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

/// What DECSC saves, the pen and the character sets with the cursor, is
/// the screen's: xterm keeps one saved cursor per screen (`cursor.c`,
/// `CursorSave` into `screen->sc[screen->whichBuf]`), so a DECSC on the
/// alternate screen leaves the main screen's, and DECSTR resets the saved
/// state of the screen shown alone. Expected values are xterm's
/// (`fux-vt-compare replay --engines xterm --size 2x8
/// 'a\e[1m\e[?1049h\e[0m\e7b\e[?1049lX'`, and the same with
/// `'\e(0\e7\e(B\e[?1047h\e7\e[?1047l\e8q'` and
/// `'\e[2;3H\e[1m\e7\e[0m\e[H\e[?1047h\e[!p\e[?1047l\e8X'`).
#[test]
fn each_screen_saves_its_own_pen_and_character_sets() -> Result {
    let p = run(2, 8, b"a\x1b[1m\x1b[?1049h\x1b[0m\x1b7b\x1b[?1049lX")?;
    assert!(p.screen().cell(0, 1).ok_or("no cell")?.bold());
    let p = run(1, 8, b"\x1b(0\x1b7\x1b(B\x1b[?1047h\x1b7\x1b[?1047l\x1b8q")?;
    assert_eq!(lines(&p), ["\u{2500}"]);
    let p = run(
        2,
        8,
        b"\x1b[2;3H\x1b[1m\x1b7\x1b[0m\x1b[H\x1b[?1047h\x1b[!p\x1b[?1047l\x1b8X",
    )?;
    assert_eq!(p.screen().cursor_position(), (1, 3));
    assert!(p.screen().cell(1, 2).ok_or("no cell")?.bold());
    Ok(())
}

/// Invalid UTF-8 prints U+FFFD, one column wide, for each maximal subpart
/// (the Unicode Standard, 3.9, "U+FFFD Substitution of Maximal
/// Subparts"), as xterm prints it for what real output holds: a Latin-1
/// byte among UTF-8, a sequence cut off by a control, ESC or the input
/// that follows (`fux-vt-compare cases invalid-utf8-prints-a-replacement`).
/// A continuation byte alone is a maximal subpart too: U+FFFD, as
/// conformance clause C10 has ill-formed input treated as an error, where
/// xterm reads it as Latin-1 (`fux-vt-compare replay --engines all --size
/// 1x12 'price \xa35'`). U+FFFD itself prints.
#[test]
fn invalid_utf8_prints_a_replacement_character() -> Result {
    assert_eq!(row(12, b"caf\xe9 ok")?, "caf\u{fffd} ok");
    assert_eq!(row(12, b"a\xffb")?, "a\u{fffd}b");
    assert_eq!(row(12, b"a\xe7\x95b")?, "a\u{fffd}b");
    assert_eq!(row(12, b"a\xf0\x9f\rb")?, "b\u{fffd}");
    assert_eq!(row(12, b"a\x80b")?, "a\u{fffd}b");
    assert_eq!(row(12, b"a\x9c\xbfb")?, "a\u{fffd}\u{fffd}b");
    assert_eq!(row(12, b"price \xa35")?, "price \u{fffd}5");
    assert_eq!(row(12, b"a\xe0\x80b")?, "a\u{fffd}\u{fffd}b");
    assert_eq!(row(12, "a\u{fffd}b".as_bytes())?, "a\u{fffd}b");
    // Cut off by ESC, which still begins its sequence.
    let p = run(1, 12, b"a\xe7\x1b[1mb")?;
    assert_eq!(lines(&p), ["a\u{fffd}b"]);
    assert!(p.screen().cell(0, 2).ok_or("no cell")?.bold());
    // Across writes: a sequence completed later is one character, one
    // cut off later is U+FFFD.
    let mut p = Parser::new(Size::new(1, 12)?, 0)?;
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

/// DEC STD 070, 3.5.3.1: "Parameters beyond the maximum number supported
/// may be ignored"; the sequence still runs, with the ones kept. fux-vt
/// keeps 32: an SGR of 33 parameters sets bold and underline (the 31st
/// and the 32nd) and not inverse (the 33rd), and digits and colons past
/// the 32nd change nothing; the same read at once and a byte at a time.
#[test]
fn parameters_past_the_32nd_are_ignored_and_the_sequence_runs() -> Result {
    let zeros: String = std::iter::repeat_n("0;", 30).collect();
    for sgr in [
        format!("\x1b[{zeros}1;4;7mX"),
        format!("\x1b[{zeros}1;4;7;5:3;8mX"),
    ] {
        let at_once = run(1, 4, sgr.as_bytes())?;
        let mut split = Parser::new(Size::new(1, 4)?, 0)?;
        for byte in sgr.bytes() {
            split.process(&[byte])?;
        }
        for p in [at_once, split] {
            let c = p.screen().cell(0, 0).ok_or("no cell")?;
            assert_eq!(c.contents(), "X", "{sgr:?}");
            assert!(c.bold() && c.underline() && !c.inverse(), "{sgr:?}");
            assert!(!c.dim() && !c.hidden(), "{sgr:?}");
        }
    }
    Ok(())
}

/// DECSCUSR (`CSI Ps SP q`; ctlseqs, "Set cursor style", VT520): Ps 0,
/// the default, and 1 are a blinking block, 0 kept as such (`Zero`); 2 to
/// 6 the steady block, the underlines and the bars; 7 xterm's initial
/// resources, the shape before any is set. A Ps ctlseqs lists no style
/// for changes nothing.
#[test]
fn decscusr_sets_the_styles_ctlseqs_lists() -> Result {
    use fux_vt::CursorShape::*;
    let shape = |bytes: &[u8]| -> Result<_> { Ok(run(1, 4, bytes)?.screen().cursor_shape()) };
    assert_eq!(shape(b"")?, Initial);
    for (ps, expected) in [
        ("0", Zero),
        ("", Zero),
        ("1", BlinkingBlock),
        ("2", SteadyBlock),
        ("3", BlinkingUnderline),
        ("4", SteadyUnderline),
        ("5", BlinkingBar),
        ("6", SteadyBar),
        ("7", Initial),
        ("8", SteadyUnderline),
        ("999", SteadyUnderline),
    ] {
        let bytes = format!("\x1b[4 q\x1b[{ps} q");
        assert_eq!(shape(bytes.as_bytes())?, expected, "{ps:?}");
    }
    Ok(())
}

/// UAX #29, GB9b (`Prepend ×`): a Prepend character joins what follows it
/// but a control (GB4, GB5), as mode 2027 reports clustering on. Printed
/// at once, ASCII after it goes the fast path; a byte at a time, the
/// general one: both join. A cluster two columns wide by unicode-width,
/// as U+0600 and a letter are, takes two cells (the README's "Grapheme
/// clusters"). `continues_cluster` says so too.
#[test]
fn a_prepend_character_joins_what_follows_it() -> Result {
    let text = "ab\u{600}c\u{600}\u{915}d";
    let at_once = run(1, 12, text.as_bytes())?;
    let mut split = Parser::new(Size::new(1, 12)?, 0)?;
    for byte in text.bytes() {
        split.process(&[byte])?;
    }
    for p in [at_once, split] {
        let cells: Vec<String> = (0..12)
            .filter_map(|x| p.screen().cell(0, x))
            .filter(|c| !c.is_wide_continuation() && c.has_contents())
            .map(|c| c.contents().to_owned())
            .collect();
        assert_eq!(cells, ["a", "b", "\u{600}c", "\u{600}\u{915}", "d"]);
        let wide = |x| p.screen().cell(0, x).is_some_and(|c| c.is_wide());
        assert_eq!((wide(2), wide(4)), (true, true));
        assert_eq!(p.screen().cursor_position(), (0, 7));
    }
    assert!(fux_vt::continues_cluster("\u{600}", 'c'));
    assert!(fux_vt::continues_cluster("\u{600}", '\u{600}'));
    assert!(fux_vt::continues_cluster("\u{600}", '\u{915}'));
    assert!(!fux_vt::continues_cluster("\u{600}", '\r'));
    assert!(!fux_vt::continues_cluster("a", 'c'));
    Ok(())
}

/// A wide glyph with no room for it, where no standard says what to do
/// (DEC STD 070 and ECMA-48 know no glyph two columns wide), does as
/// xterm 411 does: with autowrap off, one that no longer fits on the row
/// is not printed, and the glyph before it stays (`replay --engines xterm
/// --size 2x4 '\e[?7labc\u{754c}'`, and `'\e[?7labcd\u{754c}'`); on a
/// screen one column wide it fits nowhere, but a wrap pending before it is
/// carried out (`--size 3x1 'a\u{754c}'`).
#[test]
fn a_wide_glyph_with_no_room_is_not_printed() -> Result {
    for (text, row) in [("abc\u{754c}", "abc"), ("abcd\u{754c}", "abcd")] {
        let p = run(2, 4, format!("\x1b[?7l{text}").as_bytes())?;
        assert_eq!(lines(&p), [row, ""], "{text:?}");
        assert_eq!(p.screen().cursor_position(), (0, 3), "{text:?}");
    }
    let p = run(3, 1, "a\u{754c}".as_bytes())?;
    assert_eq!(lines(&p), ["a", "", ""]);
    assert_eq!(p.screen().cursor_position(), (1, 0));
    assert!(!p.screen().pending_wrap());
    assert!(p.screen().row_wrapped(0));
    Ok(())
}

/// DEC STD 070, HT (p. 5-67), note 1: with no stop before the right
/// margin, HT stops at it; from right of the margin, or on a line outside
/// the scrolling region, it goes on to the next stop or the line's last
/// column, whichever comes first. CHT moves as that many HTs.
#[test]
fn a_tab_from_right_of_the_margin_or_outside_the_region_goes_on() -> Result {
    let margins = "\x1b[?69h\x1b[3;8s\x1b[2;3r";
    for (moves, expected) in [
        // Inside: at the right margin, column 8.
        ("\x1b[2;1H\t", (1, 7)),
        // Right of the margin: no stop before the last column.
        ("\x1b[2;10H\t", (1, 11)),
        // Outside the region: the stop at column 9, past the margin.
        ("\x1b[4;1H\t", (3, 8)),
        ("\x1b[4;1H\x1b[2I", (3, 11)),
    ] {
        let p = run(4, 12, format!("{margins}{moves}").as_bytes())?;
        assert_eq!(p.screen().cursor_position(), expected, "{moves:?}");
    }
    Ok(())
}

/// ED 3 (`CSI 3 J`; ctlseqs, "Erase Saved Lines") and
/// `Parser::clear_history` drop the saved lines, which are the main
/// screen's: on the alternate screen too, which has none of its own. Either
/// is a new version, which every reader reads again.
#[test]
fn erasing_saved_lines_on_the_alternate_screen_drops_the_main_screens() -> Result {
    for clear in [&b"\x1b[3J"[..], b""] {
        let mut p = Parser::new(Size::new(2, 5)?, 1 << 16)?;
        p.process(b"a\r\nb\r\nc\r\nd\x1b[?1049h")?;
        let mark = p.screen().mark();
        p.process(clear)?;
        if clear.is_empty() {
            p.clear_history();
        }
        assert!(p.screen().full_refresh_since(mark), "{clear:?}");
        p.process(b"\x1b[?1049l")?;
        assert_eq!(p.screen().history_len(), 0, "{clear:?}");
        assert_eq!(lines(&p), ["c", "d"], "{clear:?}");
    }
    Ok(())
}

/// DEC STD 070, SUBSTITUTE (p. 5-132, levels 1-4): SUB "is processed by
/// placing a reversed question mark in the display at the Active
/// Position", as `insert_or_replace_character` places a glyph; note 2:
/// within an escape or control sequence "the sequence in progress [is]
/// aborted and the Substitute control function ... executed instead". The
/// C0 table (5.9.1, p. 5-129) agrees: "The error character (reverse
/// question mark) is displayed", where CAN displays none. U+2426 is the
/// reversed question mark (SYMBOL FOR SUBSTITUTE FORM TWO), one column, as
/// xterm 411 shows it (`charproc.c`, `CASE_SUB`). fux-vt shows it inside a
/// control string too, which SUB ends (3.5.4.4), as xterm does
/// (README, "SUB shows its error character"). As a control it is no graphic
/// character for REP.
#[test]
fn sub_is_displayed_as_the_error_character() -> Result {
    let sub = "\u{2426}";
    // In ground, inside an escape sequence, a CSI and a control string.
    assert_eq!(lines(&run(1, 6, b"a\x1ab")?), [format!("a{sub}b")]);
    for cut in [
        &b"\x1b("[..],
        b"\x1b[1",
        b"\x1b[?25",
        b"\x1b]0;x",
        b"\x1bPq#1",
        b"\x1b_x",
    ] {
        let bytes = [cut, b"\x1ac"].concat();
        let p = run(1, 6, &bytes)?;
        assert_eq!(lines(&p), [format!("{sub}c")], "{:?}", bytes.escape_ascii());
    }
    // CAN aborts without a glyph.
    assert_eq!(lines(&run(1, 6, b"\x1b[1\x18c")?), ["c"]);
    // A character cut short by SUB is U+FFFD, then SUB's glyph (Unicode
    // 3.9's maximal subpart).
    assert_eq!(
        lines(&run(1, 6, b"\xe2\x82\x1a")?),
        [format!("\u{fffd}{sub}")]
    );
    // It is placed as a glyph is: the cursor moves on, a pending wrap is
    // carried out, IRM inserts it, and no character set maps it.
    let p = run(2, 3, b"abc\x1a")?;
    assert_eq!(lines(&p), ["abc".to_owned(), sub.to_owned()]);
    assert_eq!(p.screen().cursor_position(), (1, 1));
    assert_eq!(lines(&run(1, 4, b"ab\r\x1b[4h\x1a")?), [format!("{sub}ab")]);
    assert_eq!(
        lines(&run(1, 4, b"\x1b(0\x1aq")?),
        [format!("{sub}\u{2500}")]
    );
    // It cancels a single shift, as any control but SO and SI does (DEC
    // STD 070, 3.8.3.2).
    assert_eq!(lines(&run(1, 4, b"\x1b*0\x1bN\x1aq")?), [format!("{sub}q")]);
    // REP after it repeats nothing.
    assert_eq!(lines(&run(1, 6, b"X\x1a\x1b[3b")?), [format!("X{sub}")]);
    Ok(())
}

/// The Unicode Standard 17.0, 3.9, "U+FFFD Substitution of Maximal
/// Subparts", Tables 3-8 to 3-11 (`data/utf8-substitution.txt`, their rows
/// as data): each table's bytes print its "Output" row, a U+FFFD a column,
/// whether they come whole (the parser's run of text), one byte a call (its
/// state between calls) or split in two anywhere.
#[test]
fn utf8_tables_3_8_to_3_11_substitute_maximal_subparts() -> Result {
    let tables = include_str!("data/utf8-substitution.txt");
    let hex = |field: &str| -> Vec<u32> {
        (field.split_whitespace())
            .filter_map(|h| u32::from_str_radix(h, 16).ok())
            .collect()
    };
    let mut checked = 0;
    for line in tables.lines().filter(|l| !l.starts_with('#')) {
        let mut fields = line.split(';');
        let (Some(table), Some(bytes), Some(output)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("a line of three fields: {line}").into());
        };
        let bytes: Vec<u8> = hex(bytes)
            .into_iter()
            .filter_map(|b| u8::try_from(b).ok())
            .collect();
        let want: String = hex(output).into_iter().filter_map(char::from_u32).collect();
        let shown = |p: &Parser| lines(p).concat().trim_end().to_owned();
        let whole = run(1, 20, &bytes)?;
        assert_eq!(shown(&whole), want, "Table {table}, whole");
        let mut p = Parser::new(Size::new(1, 20)?, 0)?;
        for byte in &bytes {
            p.process(std::slice::from_ref(byte))?;
        }
        assert_eq!(shown(&p), want, "Table {table}, a byte a call");
        for at in 1..bytes.len() {
            let (head, tail) = bytes.split_at_checked(at).ok_or("a split past the end")?;
            let mut p = Parser::new(Size::new(1, 20)?, 0)?;
            p.process(head)?;
            p.process(tail)?;
            assert_eq!(shown(&p), want, "Table {table}, split at {at}");
        }
        checked += 1;
    }
    assert_eq!(checked, 4);
    Ok(())
}

/// A cluster that becomes two columns wide (VS16) takes the cell under
/// the cursor as its second half, as a glyph printed wide would: under
/// IRM it is inserted, what is there moving right (ECMA-48 7.2.10, INSERT:
/// "the graphic symbol ... is inserted at the active presentation
/// position"; the README's IRM row); the second half takes the open link
/// (OSC 8); and a wide glyph whose first half it took keeps its second as
/// a blank in the pen's colours, as `print` repairs a glyph it cuts.
#[test]
fn a_cluster_widened_is_placed_as_a_wide_glyph() -> Result {
    let heart = "\u{2764}\u{fe0f}";
    let cells = |p: &Parser| -> Vec<String> {
        (0..p.screen().size().cols())
            .filter_map(|x| p.screen().cell(0, x))
            .map(|c| c.contents().to_owned())
            .collect()
    };
    let p = run(1, 6, format!("\x1b[4hab\r{heart}").as_bytes())?;
    assert_eq!(cells(&p), [heart, "", "a", "b", "", ""]);
    assert!(
        p.screen()
            .cell(0, 1)
            .is_some_and(|c| c.is_wide_continuation())
    );
    let mut p = Parser::with_options(
        Size::new(1, 6)?,
        0,
        Options::new().with(Feature::Hyperlinks),
    )?;
    p.process(format!("\x1b]8;;u\x1b\\{heart}").as_bytes())?;
    let row = p.screen().rows().last().ok_or("a row")?;
    let link = |x: usize| row.link(x).map(|l| l.uri().to_owned());
    assert_eq!(
        (link(0), link(1)),
        (Some("u".to_owned()), Some("u".to_owned()))
    );
    let p = run(1, 6, format!("x\u{4e00}\r\x1b[41m{heart}").as_bytes())?;
    assert_eq!(cells(&p), [heart, "", "", "", "", ""]);
    let orphan = p.screen().cell(0, 2).ok_or("cell 2")?;
    assert_eq!(orphan.bgcolor(), Color::Idx(1));
    Ok(())
}

/// DEC STD 070, 3.5.1.2.1: "the receipt of CAN causes immediate
/// termination, without execution, of any sequence in progress. The CAN
/// character itself receives no further processing. The characters
/// following the CAN are ... interpreted normally." 3.5.1.2.3: "If an
/// Escape (ESC, 1/11) is embedded in an escape or control sequence, the
/// sequence is canceled and a new sequence begins with that ESC." And
/// 3.5.4.4, for control strings: "the control codes CAN (1/8), SUB (1/10),
/// ESC (1/11), or any C1 control will also terminate control strings".
/// So, in every state the parser can be in, CAN then `c` shows `c` alone in
/// the first column, and ESC beginning `CSI 3 C` moves three columns
/// first. (SUB in every state is `sub_is_displayed_as_the_error_character`.)
#[test]
fn can_and_esc_end_a_sequence_in_every_state() -> Result {
    for cut in [
        &b"\x1b"[..],
        b"\x1b(",
        b"\x1b#",
        b"\x1b[",
        b"\x1b[1",
        b"\x1b[?25",
        b"\x1b[1;2$",
        b"\x1b[1<",
        b"\x1bP",
        b"\x1bP1;2",
        b"\x1bP$qm",
        b"\x1bP1<x",
        b"\x1b]0;x",
        b"\x1b]8;;u",
        b"\x1b_x",
        b"\x1b^x",
        b"\x1bXx",
    ] {
        let can = run(1, 6, &[cut, b"\x18c"].concat())?;
        assert_eq!(lines(&can), ["c"], "CAN after {:?}", cut.escape_ascii());
        let esc = run(1, 6, &[cut, b"\x1b[3Cc"].concat())?;
        assert_eq!(lines(&esc), ["   c"], "ESC after {:?}", cut.escape_ascii());
    }
    Ok(())
}
