//! Modes and controls fux-vt keeps as xterm keeps them: DECSCLM, DECSCNM,
//! DECARM, DECNKM and DECBKM for DECRQM; reverse wraparound (45) and
//! xterm's extended reverse wraparound (1045); XTSAVE and XTRESTORE; LNM;
//! DECID; DECALN. The expected values are the references' (DEC STD 070,
//! the VT520 manual, xterm's ctlseqs) and, where those leave it to the
//! terminal, xterm 411's, asked the same sequences under Xvfb (80 by 25,
//! a VT420) and read back by DSR and DECRQM.

use fux_vt::{Options, Parser};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

const MODES: Options = Options::new().with_mode_reports(true);

/// What `input` makes the parser answer, as text.
fn replies(parser: &mut Parser, input: &[u8]) -> std::result::Result<String, fux_vt::Error> {
    let mut out = Vec::new();
    parser.process_with_replies(input, |bytes| out.extend_from_slice(bytes))?;
    Ok(String::from_utf8_lossy(&out).replace('\x1b', "^["))
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
