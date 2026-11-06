//! What vt-esc-lint promises: each sequence explained, malformed and
//! unknown ones flagged, and what the stream leaves set reported.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "a test")]
use vt_esc_lint::{Flag, lint};

fn size() -> fux_vt::Size {
    fux_vt::Size::new(24, 80).unwrap()
}

#[test]
fn sequences_are_explained_and_the_state_left_is_reported() {
    let input = b"\x1b]2;build\x07\x1b[?1049h\x1b[1;31mred\x1b]8;;https://e.x\x1b\\x\x1b(0q";
    let report = lint(input, size()).unwrap();
    let meanings: Vec<&str> = report.findings.iter().filter(|f| !f.text).map(|f| f.meaning.as_str()).collect();
    assert_eq!(
        meanings,
        [
            "OSC 2: set the window title to \"build\"",
            "DECSET: set AlternateScreenSaveCursor (1049)",
            "SGR: bold, foreground red",
            "OSC 8: start a hyperlink to https://e.x",
            "designate G0 as DEC Special Graphics (line drawing)",
        ]
    );
    assert!(report.findings.iter().all(|f| f.flag.is_none()));
    let left = report.left.join("\n");
    for want in [
        "mode ?1049 (AlternateScreenSaveCursor) left set",
        "SGR left: CSI 0;1;31 m",
        "window title \"build\"",
        "hyperlink left open: https://e.x",
        "G0 left as DEC Special Graphics",
    ] {
        assert!(left.contains(want), "{want:?} not in\n{left}");
    }
}

#[test]
fn malformed_and_unknown_sequences_are_flagged() {
    let input = b"\x1b[?9999h\x1b[1;2\x1b[66m\x1b[?996n\x1b]0;never ended";
    let report = lint(input, size()).unwrap();
    let flags: Vec<_> = report.findings.iter().map(|f| f.flag.clone()).collect();
    assert!(matches!(&flags[0], Some(Flag::Unknown(w)) if w.contains("unknown mode 9999")));
    assert!(matches!(&flags[1], Some(Flag::Malformed(w)) if w.contains("broken off by ESC")));
    assert!(matches!(&flags[2], Some(Flag::Unknown(w)) if w.contains("unknown SGR 66")));
    // Explained here, but fux-vt carries none of it out.
    assert!(matches!(&flags[3], Some(Flag::Unknown(w)) if w.contains("fux-vt does not implement")));
    assert!(matches!(&flags[4], Some(Flag::Malformed(w)) if w.contains("never ended")));
    assert_eq!((report.count(true), report.count(false)), (2, 3));
    // Nothing set is left reported but the cursor.
    assert_eq!(report.left, ["cursor at row 1, column 1"]);
}
