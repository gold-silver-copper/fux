//! A TUI's frame replayed, and assertions about it checked.
use vt_tui_oracle::{Replay, parse};

/// What a small TUI writes: the alternate screen, a title, a red header
/// with a wide glyph, a status line in direct colour, the cursor hidden
/// then placed. Cut at an awkward place, in a synchronized frame.
const FRAME: &[u8] = b"\x1b[?1049h\x1b]2;my app\x07\x1b[?2026h\x1b[H\x1b[2J\
\x1b[31mHello \xe4\xb8\x96\xe7\x95\x8c!\x1b[0m\x1b[3;1H\x1b[38;2;16;32;48mstatus: ok  \x1b[m\
\x1b[?25l\x1b[2;5H\x1b[?2026l";

#[test]
fn a_frame_meets_its_assertions_and_a_wrong_one_says_what_is_there()
-> Result<(), Box<dyn std::error::Error>> {
    let replay = Replay::new(FRAME, fux_vt::Size::new(5, 20)?)?;
    let lines = parse(
        "# header\n\
         cell 0 0 Hello 世界!\n\
         cell 0 6 世界\n\
         cell 2 0 \"status: ok  \"\n\
         cursor 1 4\n\
         mode ShowCursor off\n\
         mode ?1049 on\n\
         mode synchronized-output off\n\
         title my app\n\
         fg 0 0 red\n\
         fg 0 6 1\n\
         fg 2 3 #102030\n\
         fg 0 9 default\n",
    )?;
    for line in &lines {
        assert_eq!(replay.check(&line.assertion), Ok(()), "{}\n{}", line.source, replay.dump());
    }
    let wrong = parse("cell 0 0 Help\ncursor 0 0\nfg 2 0 blue\nmode 4 on\ntitle other\ncell 0 7 x\n")?;
    let got: Vec<_> = wrong.iter().map(|l| replay.check(&l.assertion)).collect();
    assert_eq!(
        got,
        [
            Err("\"Hell\"".into()),
            Err("cursor 1 4".into()),
            Err("fg #102030".into()),
            Err("Insert is off".into()),
            Err("title \"my app\"".into()),
            Err("column 7 is the second half of a wide glyph".into()),
        ]
    );
    Ok(())
}

#[test]
fn a_bad_line_is_refused_with_its_number() {
    for (text, says) in [
        ("cursor 1\n", "line 1: column \"\" is not a number"),
        ("\n# ok\nmode NoSuchMode on\n", "line 3: no mode \"NoSuchMode\""),
        ("fg 0 0 purple\n", "line 1: no colour \"purple\""),
        ("cell 0 0 \"open\n", "line 1: no closing quote"),
        ("blink 0 0\n", "line 1: no assertion \"blink\""),
    ] {
        let error = parse(text).err().unwrap_or_default();
        assert!(error.starts_with(says), "{text:?}: {error}");
    }
}
