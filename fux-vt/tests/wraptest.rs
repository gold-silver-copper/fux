//! wraptest's 25 checks of the Last Column Flag, re-authored: wraptest
//! (Mattias Engdegård, `wraptest.c` and `results.txt` at 5409c251) has no
//! licence, so nothing of it is copied; each check here is the bytes it
//! writes and the answer DEC STD 070 gives, as a cursor position report
//! and the screen, where wraptest prints a yes or a no. The checks keep
//! wraptest's numbers, so that each can be found in its report; their
//! names are this file's own, each saying what DEC STD 070 has happen.
//!
//! The authority is DEC STD 070, Appendix D.6.1, "Auto Wrap Mode (Last
//! Column Flag)", p. D-13 to D-14 (wraptest's `DEC` column): a glyph
//! written into the last column sets the flag instead of moving the
//! cursor, and the next glyph first goes to the next line's first column;
//! "any control which potentially changes the Active Position from the
//! end-of-line position" resets it, among them BS, HT, LF, CR, RI, CUP,
//! CUF, EL, ED, DCH, ICH and ECH; DECSC saves it and DECRC restores it.
//! Real terminals vary ("existing products vary widely", the appendix
//! says): HT keeps the flag on a real VT100, VT420 and VT510, and in
//! xterm, though the appendix lists HORIZONTAL_TAB (check 7).
//!
//! Each check starts as wraptest does, with autowrap set, LNM reset and
//! the screen erased, and on a screen 10 columns wide, where wraptest
//! measures the width it is given (check 0): the last column is 10, and
//! `CSI 1 ; 9 H` is wraptest's `cup(1, width - 1)`.

use fux_vt::{Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

const ROWS: u16 = 4;
const COLS: u16 = 10;
/// A cursor position report's row and column.
type Position = (u16, u16);
/// wraptest's start: DECAWM set, LNM reset, ED 2.
const START: &str = "\x1b[?7h\x1b[20l\x1b[2J";

/// The cursor position reports (row, column) to `bytes` after `START`,
/// and the screen.
fn run(bytes: &str) -> Result<(Vec<Position>, Vec<String>)> {
    let mut parser = Parser::new(Size::new(ROWS, COLS)?, 0)?;
    let mut replies = Vec::new();
    parser.process_with_replies(format!("{START}{bytes}").as_bytes(), |reply| {
        replies.extend_from_slice(reply);
    })?;
    let text = String::from_utf8(replies)?;
    let reports = text
        .split_terminator('R')
        .map(|report| {
            let (row, col) = report
                .strip_prefix("\x1b[")
                .and_then(|r| r.split_once(';'))
                .ok_or_else(|| format!("not a CPR: {report:?}"))?;
            Ok((row.parse()?, col.parse()?))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((reports, lines(&parser)))
}

/// One check: its number in wraptest's report and what it checks, the
/// bytes, the position the last CPR reports, and the screen's first rows.
struct Check {
    number: u8,
    name: &'static str,
    bytes: &'static str,
    cursor: Position,
    screen: &'static [&'static str],
}

/// The 25 checks, each ending with CPR (`CSI 6 n`).
const CHECKS: &[Check] = &[
    // A glyph past the last column wraps to the next line's first column.
    Check {
        number: 1,
        name: "a glyph past the last column goes to the next line",
        bytes: "\x1b[1;9HABC\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    // The glyph in the last column leaves the cursor there, the flag set.
    Check {
        number: 2,
        name: "a glyph in the last column leaves the cursor there",
        bytes: "\x1b[1;9HAB\x1b[6n",
        cursor: (1, 10),
        screen: &["        AB"],
    },
    // CPR reports the Active Position, which "cannot be advanced" past the
    // last column (D.6.1; CPR, p. 5-53): never column 11.
    Check {
        number: 3,
        name: "CPR reports the last column, never past it",
        bytes: "\x1b[1;9HAB\x1b[6n",
        cursor: (1, 10),
        screen: &["        AB"],
    },
    Check {
        number: 4,
        name: "CR from the last column goes to the first",
        bytes: "\x1b[1;9HAB\r\x1b[6n",
        cursor: (1, 1),
        screen: &["        AB"],
    },
    Check {
        number: 5,
        name: "BS from the last column goes back one",
        bytes: "\x1b[1;9HAB\x08\x1b[6n",
        cursor: (1, 9),
        screen: &["        AB"],
    },
    // HT from the last column finds no stop and stays (DEC STD 070, HT).
    Check {
        number: 6,
        name: "HT from the last column stays on its line",
        bytes: "\x1b[1;9HAB\t\x1b[6n",
        cursor: (1, 10),
        screen: &["        AB"],
    },
    // HORIZONTAL_TAB resets the flag; real VT100, VT420, VT510 and xterm
    // keep it, and print C on the next line.
    Check {
        number: 7,
        name: "HT ends the pending wrap",
        bytes: "\x1b[1;9HAB\tC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 8,
        name: "LF ends the pending wrap",
        bytes: "\x1b[1;9HAB\nC\x1b[6n",
        cursor: (2, 10),
        screen: &["        AB", "         C"],
    },
    // NUL and BEL move nothing, so the flag stays and C wraps.
    Check {
        number: 9,
        name: "NUL leaves the pending wrap",
        bytes: "\x1b[1;9HAB\0C\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    Check {
        number: 10,
        name: "BEL leaves the pending wrap",
        bytes: "\x1b[1;9HAB\x07C\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    Check {
        number: 11,
        name: "RI ends the pending wrap",
        bytes: "\x1b[2;9HAB\x1bMC\x1b[6n",
        cursor: (1, 10),
        screen: &["         C", "        AB"],
    },
    // SGR and SM are no cursor controls: the flag stays.
    Check {
        number: 12,
        name: "SGR leaves the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[mC\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    Check {
        number: 13,
        name: "SM leaves the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[hC\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    // CUP to the very same position resets it all the same.
    Check {
        number: 14,
        name: "CUP ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[1;10HC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 15,
        name: "CUF ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[CC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 16,
        name: "EL ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[KC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 17,
        name: "ED ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[JC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 18,
        name: "DCH ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[PC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 19,
        name: "ICH ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[@C\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    Check {
        number: 20,
        name: "ECH ends the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[XC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    // DSR moves nothing: the first report is the last column, and C wraps.
    Check {
        number: 21,
        name: "DSR, answered by CPR, leaves the pending wrap",
        bytes: "\x1b[1;9HAB\x1b[6nC\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    Check {
        number: 22,
        name: "DECSC leaves the pending wrap",
        bytes: "\x1b[1;9HAB\x1b7C\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
    // DECRC brings the flag back with the position, so X wraps.
    Check {
        number: 23,
        name: "DECRC brings the pending wrap back",
        bytes: "\x1b[1;9HAB\x1b7\x1b[3;10HQ\x1b8X\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "X", "         Q"],
    },
    // DECSC does not save DECAWM (DEC STD 070, DECSC, p. 5-187): DECRC
    // leaves it reset, and C overwrites the last column.
    Check {
        number: 24,
        name: "DECRC leaves DECAWM reset",
        bytes: "\x1b[1;1H\x1b7\x1b[?7l\x1b8\x1b[1;9HABC\x1b[6n",
        cursor: (1, 10),
        screen: &["        AC"],
    },
    // Nor does DECRC reset it: C wraps.
    Check {
        number: 25,
        name: "DECRC leaves DECAWM set",
        bytes: "\x1b[1;1H\x1b[?7l\x1b7\x1b[?7h\x1b8\x1b[1;9HABC\x1b[6n",
        cursor: (2, 2),
        screen: &["        AB", "C"],
    },
];

/// wraptest's own first step: CUP past the last column stops in it, and
/// CPR gives the width.
#[test]
fn wraptest_measures_the_width() -> Result {
    let (reports, _) = run("\x1b[1;999H\x1b[6n")?;
    assert_eq!(reports, [(1, COLS)]);
    Ok(())
}

#[test]
fn wraptest_checks_follow_dec_std_070_d_6_1() -> Result {
    assert_eq!(CHECKS.len(), 25);
    let mut wrong = Vec::new();
    for (check, number) in CHECKS.iter().zip(1u8..) {
        assert_eq!(check.number, number, "{}", check.name);
        let (reports, screen) = run(check.bytes)?;
        let mut want: Vec<String> = check.screen.iter().map(|&row| row.to_owned()).collect();
        want.resize(usize::from(ROWS), String::new());
        if reports.last() != Some(&check.cursor) || screen != want {
            wrong.push(format!(
                "{}. {}: CPR {reports:?}, screen {screen:?}; expected {:?}, {want:?}",
                check.number, check.name, check.cursor
            ));
        }
    }
    // Check 21's first report: the last column, before C.
    assert_eq!(
        run(CHECKS.get(20).map_or("", |c| c.bytes))?.0.first(),
        Some(&(1, COLS))
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
