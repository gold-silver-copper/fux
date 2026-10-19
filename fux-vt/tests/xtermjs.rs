//! xterm.js's escape-sequence fixtures (`tests/data/xtermjs`, xterm.js
//! 991857a1 `test/fixtures/escape_sequence_files`, MIT, its LICENSE beside
//! them): 76 pairs of the bytes a program wrote (`.in`) and the screen real
//! xterm showed for them at 25 rows of 80 (`.text`, copied from xterm's
//! window). Each is fed as the fixtures' `run_tests.py` writes it: after
//! RIS and CUP, through a terminal's output processing, which turns each
//! LF into CR LF (POSIX `ONLCR`); and the screen's text, each row's blank
//! tail trimmed, is compared with the `.text`'s first 25 lines.
//!
//! A fixture fux-vt does not match is listed in `DIFFERENT`, each checked
//! against the standard or real xterm and given its reason, and the test
//! fails when a listed fixture starts to match, as when one not listed
//! fails. Each listed fixture's screen as its authority has it is kept
//! beside it (`fux-vt/NAME.text`), and asserted: xterm's `.text` but for
//! the rows the reason names, each such row checked by hand against the
//! section the reason cites (for HT, HTS, CHT and DECSTBM, replaying the
//! bytes with xterm's rule gives xterm's `.text` again).

use fux_vt::{Parser, Size};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

const ROWS: u16 = 25;
const COLS: u16 = 80;

/// The fixtures fux-vt's screen differs from, each with its verdict.
const DIFFERENT: &[(&str, &str)] = &[
    (
        "t0015-SUB",
        "SUB shows the error character, U+2426 (DEC STD 070, SUBSTITUTE, p. 5-132); \
         the fixture shows nothing in its place, and real xterm 411 does not match it either",
    ),
    (
        "t0032-VPB",
        "CSI k is VPB, up n lines (ECMA-48 8.3.159); xterm ignores it, and the fixture is xterm's",
    ),
    (
        "t0033-VPB_scroll",
        "CSI 36 k is VPB, which stops at the top line (ECMA-48 8.3.159); xterm ignores it, \
         printing on the last line instead",
    ),
    (
        "t0060-DECSC",
        "DECSC saves the Last Column Flag and DECRC restores it, so the glyph after DECRC wraps \
         (DEC STD 070, Appendix D.6.1); real xterm 411 does so too and does not match the fixture",
    ),
    (
        "t0061-CSI_s",
        "SCOSC and SCORC save and restore the Last Column Flag as DECSC and DECRC do \
         (DEC STD 070, Appendix D.6.1); real xterm 411 does so too and does not match the fixture",
    ),
    (
        "t0077-DECSTBM_quirks",
        "DECSTBM with its bottom past the screen (CSI 23 ; 28 r) changes nothing \
         (DEC STD 070, DECSTBM, p. 5-25, notes 2 and 3); xterm takes the last line instead",
    ),
    (
        "t0080-HT",
        "HT ends a pending wrap, so the glyph after it overwrites the last column \
         (DEC STD 070, Appendix D.6.1, HORIZONTAL_TAB); xterm keeps the wrap",
    ),
    (
        "t0082-HTS",
        "HT ends a pending wrap (DEC STD 070, Appendix D.6.1, HORIZONTAL_TAB); xterm keeps the wrap",
    ),
    (
        "t0083-CHT",
        "CHT ends a pending wrap as HT does (DEC STD 070, Appendix D.6.1: any control that may \
         move the Active Position from the end of the line); xterm keeps the wrap",
    ),
    (
        "t0084-CBT",
        "CBT moves back a stop and ends a pending wrap (ECMA-48 8.3.7; DEC STD 070, Appendix \
         D.6.1); xterm moves back and keeps the wrap, so its next glyph goes to the next line; \
         xterm.js skips this fixture",
    ),
    (
        "t0103-reverse_wrap",
        "reverse wraparound (CSI ? 45 h) is xterm's and the fixture's expectation is its own: \
         real xterm 411 does not match it, and xterm.js skips it",
    ),
];

/// The screen fux-vt shows for one fixture's bytes.
fn screen(input: &[u8]) -> Result<Vec<String>> {
    let mut bytes = b"\x1bc\x1b[H".to_vec();
    for &byte in input {
        if byte == b'\n' {
            bytes.push(b'\r');
        }
        bytes.push(byte);
    }
    let mut parser = Parser::new(Size::new(ROWS, COLS)?, 0)?;
    parser.process(&bytes)?;
    Ok(lines(&parser))
}

/// The screen a fixture's `.text` gives: its first 25 lines, trimmed.
fn expected(text: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(text);
    let mut rows: Vec<String> = text
        .split('\n')
        .take(usize::from(ROWS))
        .map(|line| line.trim_end().to_owned())
        .collect();
    rows.resize(usize::from(ROWS), String::new());
    rows
}

#[test]
fn xterm_js_escape_sequence_fixtures() -> Result {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/xtermjs");
    let mut names: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".in").map(str::to_owned))
        .collect();
    names.sort();
    assert_eq!(names.len(), 76, "the fixtures");
    let mut wrong = Vec::new();
    for name in &names {
        let got = screen(&std::fs::read(dir.join(format!("{name}.in")))?)?;
        let want = expected(&std::fs::read(dir.join(format!("{name}.text")))?);
        let listed = DIFFERENT.iter().any(|(listed, _)| listed == name);
        if listed {
            let authority = dir.join(format!("fux-vt/{name}.text"));
            let authority = expected(&std::fs::read(authority)?);
            if got != authority {
                let rows = got.iter().zip(&authority).enumerate();
                let diff: Vec<String> = rows
                    .filter(|(_, (g, w))| g != w)
                    .map(|(y, (g, w))| format!("  row {y}: got {g:?}\n  row {y}: kept {w:?}"))
                    .collect();
                wrong.push(format!(
                    "{name}, not as its authority has it:\n{}",
                    diff.join("\n")
                ));
            }
        }
        if (got == want) == listed {
            wrong.push(if listed {
                format!("{name}: listed as different, and matches")
            } else {
                let rows = got.iter().zip(&want).enumerate();
                let diff: Vec<String> = rows
                    .filter(|(_, (g, w))| g != w)
                    .map(|(y, (g, w))| format!("  row {y}: got {g:?}\n  row {y}: xterm {w:?}"))
                    .collect();
                format!("{name}:\n{}", diff.join("\n"))
            });
        }
    }
    assert!(
        DIFFERENT
            .iter()
            .all(|(listed, _)| names.contains(&listed.to_string()))
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
