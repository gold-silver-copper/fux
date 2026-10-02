//! Behaviour settled by the specifications in `references/` and by xterm,
//! each test citing the section that sets its expected values. Where xterm
//! departs from the specification, the test says so and follows xterm.

use fux_vt::Parser;
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
