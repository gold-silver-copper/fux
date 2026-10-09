//! Opt-in outputs: `Feature::Events` (OSC 0/1/2/52 and BEL),
//! `Feature::ExtendedReplies` (DECRQM, DECXCPR, secondary DA) and
//! `Feature::ModeReports` (DECRQM alone). The default must
//! stay fux's policy: no events and the original reply set.

use fux_vt::{
    Attributes, CellRef, Cells, Color, Event, Feature, OSC_PAYLOAD_LIMIT, Options, Parser, Sink,
};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Default, PartialEq, Eq)]
struct Record {
    replies: Vec<Vec<u8>>,
    events: Vec<String>,
}
impl Sink for Record {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.push(bytes.to_vec());
    }
    fn event(&mut self, event: Event<'_>) {
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        self.events.push(match event {
            Event::Title(t) => format!("title:{}", text(t)),
            Event::IconName(t) => format!("icon:{}", text(t)),
            Event::Bell => "bell".to_owned(),
            Event::Clipboard { selection, data } => {
                format!("clipboard:{}:{}", text(selection), text(data))
            }
            Event::ColorQuery { number, bel } => {
                format!("color:{number}:{}", if bel { "bel" } else { "st" })
            }
            _ => "unknown".to_owned(),
        });
    }
}

const EVENTS: Options = Options::new().with(Feature::Events);
const REPLIES: Options = Options::new().with(Feature::ExtendedReplies);

fn run(options: Options, input: &[u8]) -> std::result::Result<Record, fux_vt::Error> {
    let mut parser = Parser::with_options(24, 80, 0, options)?;
    let mut record = Record::default();
    parser.process_with(input, &mut record)?;
    Ok(record)
}

const SAMPLE: &[u8] = b"\x1b]0;both\x07\x1b]1;icon\x1b\\\x1b]2;title\x07\x07\
    \x1b]52;c;aGk=\x07\x1b]52;c;?\x07\x1b]7;file://host/\x07\x1b[?1$p\x1b[>c\x1b[?6n";

#[test]
fn defaults_deliver_no_events_and_only_the_original_replies() -> Result {
    let record = run(Options::default(), SAMPLE)?;
    assert_eq!(record, Record::default());
    // `Parser::new` is exactly the default options.
    assert_eq!(Parser::new(2, 2, 0)?.options(), Options::default());
    // The original reply set is unchanged by default.
    let record = run(Options::default(), b"\x1b[5n\x1b[6n\x1b[c")?;
    assert_eq!(
        record.replies,
        [&b"\x1b[0n"[..], b"\x1b[1;1R", b"\x1b[?1;2c"].map(<[u8]>::to_vec)
    );
    Ok(())
}

#[test]
fn events_report_titles_icons_bells_and_clipboard_sets() -> Result {
    let record = run(EVENTS, SAMPLE)?;
    assert_eq!(
        record.events,
        [
            "icon:both",
            "title:both",
            "icon:icon",
            "title:title",
            "bell",
            "clipboard:c:aGk="
        ]
    );
    // Replies stay the default set: the extended queries above are not answered.
    assert!(record.replies.is_empty());
    // BEL executes inside CSI (C0 in a sequence) and ESC-state; not inside OSC.
    let record = run(EVENTS, b"\x1b[\x07m\x1b\x07")?;
    assert_eq!(record.events, ["bell", "bell"]);
    Ok(())
}

#[test]
fn events_are_chunk_invariant() -> Result {
    let whole = run(EVENTS, SAMPLE)?;
    for size in [1, 2, 3, 7] {
        let mut parser = Parser::with_options(24, 80, 0, EVENTS)?;
        let mut record = Record::default();
        for chunk in pieces::pieces(SAMPLE, size) {
            parser.process_with(chunk, &mut record)?;
        }
        assert_eq!(record, whole, "chunk size {size}");
    }
    Ok(())
}

#[test]
fn osc_payloads_are_bounded_and_cancellable() -> Result {
    let mut parser = Parser::with_options(4, 20, 0, EVENTS)?;
    let mut record = Record::default();
    // An over-limit title is consumed without an event.
    parser.process_with(b"\x1b]2;", &mut record)?;
    for _ in 0..OSC_PAYLOAD_LIMIT / 1024 + 2 {
        parser.process_with(&[b'x'; 1024], &mut record)?;
    }
    parser.process_with(b"\x07", &mut record)?;
    assert!(record.events.is_empty());
    // Exactly at the limit is delivered.
    let mut exact = b"\x1b]2;".to_vec();
    exact.resize(4 + OSC_PAYLOAD_LIMIT - 2, b'y');
    exact.push(7);
    parser.process_with(&exact, &mut record)?;
    assert_eq!(record.events.len(), 1);
    // CAN cancels without an event, and the next string starts clean.
    record.events.clear();
    parser.process_with(b"\x1b]2;gone\x18\x1b]2;kept\x07ok", &mut record)?;
    assert_eq!(record.events, ["title:kept"]);
    assert_eq!(
        parser.screen().cell(0, 0).map(|c| c.contents()),
        Some("o"),
        "OSC payload never reaches the grid"
    );
    Ok(())
}

#[test]
fn extended_replies_answer_decrqm_decxcpr_and_secondary_da() -> Result {
    let record = run(
        REPLIES,
        b"\x1b[3;4H\x1b[?6n\x1b[>c\x1b[>0c\x1b[?1$p\x1b[?1h\x1b[?1$p\x1b[?2004h\x1b[?2004$p\
          \x1b[?25$p\x1b[?1000h\x1b[?1000$p\x1b[?1002$p\x1b[?4242$p\x1b[4$p\x1b[4h\x1b[4$p\x1b[20$p\
          \x1b[12$p",
    )?;
    let expected: [&[u8]; 14] = [
        b"\x1b[?3;4R",
        b"\x1b[>1;10;0c",
        b"\x1b[>1;10;0c",
        b"\x1b[?1;2$y",
        b"\x1b[?1;1$y",
        b"\x1b[?2004;1$y",
        b"\x1b[?25;1$y",
        b"\x1b[?1000;1$y",
        b"\x1b[?1002;2$y",
        b"\x1b[?4242;0$y",
        // IRM and LNM, the ANSI modes fux-vt keeps; SRM it does not.
        b"\x1b[4;2$y",
        b"\x1b[4;1$y",
        b"\x1b[20;2$y",
        b"\x1b[12;0$y",
    ];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    assert!(record.events.is_empty(), "replies do not imply events");
    // The primary DA and DSR answers are the same as without the option.
    let record = run(REPLIES, b"\x1b[c\x1b[5n")?;
    assert_eq!(
        record.replies,
        [&b"\x1b[?1;2c"[..], b"\x1b[0n"].map(<[u8]>::to_vec)
    );
    Ok(())
}

/// `Feature::ModeReports` answers DECRQM, and nothing else the extended
/// replies would: no DA2, no DECXCPR. Synchronized output (2026) is how
/// programs use it: they ask whether the terminal knows the mode
/// (`references/modern/mode_2026_synchronized_output.md`, "Feature
/// detection") before wrapping frames in it.
#[test]
fn mode_reports_answer_decrqm_alone() -> Result {
    const MODES: Options = Options::new().with(Feature::ModeReports);
    let record = run(
        MODES,
        b"\x1b[?2026$p\x1b[?2026h\x1b[?2026$p\x1b[?2026l\x1b[?2026$p\x1b[4$p\x1b[>c\x1b[?6n",
    )?;
    let expected: [&[u8]; 4] = [
        b"\x1b[?2026;2$y",
        b"\x1b[?2026;1$y",
        b"\x1b[?2026;2$y",
        b"\x1b[4;2$y",
    ];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    // Without it, no DECRQM answer.
    assert!(
        run(Options::new(), b"\x1b[?2026$p\x1b[4$p")?
            .replies
            .is_empty()
    );
    Ok(())
}

/// Synchronized output is state the host reads: set and reset by the
/// program, and ended by RIS, by DECSTR (so that `tput init` and
/// `tput reset` never leave a frame waiting) and by any resize, as in
/// Ghostty.
#[test]
fn synchronized_output_is_tracked_and_ended() -> Result {
    let mut p = Parser::new(4, 10, 0)?;
    assert!(!p.screen().synchronized_output());
    p.process(b"\x1b[?2026h")?;
    assert!(p.screen().synchronized_output());
    p.process(b"\x1b[?2026l")?;
    assert!(!p.screen().synchronized_output());
    for (then, what) in [(&b"\x1bc"[..], "RIS"), (b"\x1b[!p", "DECSTR")] {
        p.process(b"\x1b[?2026h")?;
        p.process(then)?;
        assert!(!p.screen().synchronized_output(), "{what}");
    }
    p.process(b"\x1b[?2026h")?;
    p.resize(4, 10)?;
    assert!(
        !p.screen().synchronized_output(),
        "a resize to the same size"
    );
    p.process(b"\x1b[?2026h")?;
    p.resize(5, 12)?;
    assert!(!p.screen().synchronized_output(), "a resize");
    Ok(())
}

/// `Parser::process_until_frame` stops right after the sequence that sets
/// synchronized output, wherever it is: alone, beside other modes, or split
/// across calls; and reads everything when none does.
#[test]
fn process_until_frame_stops_after_the_sequence_that_sets_2026() -> Result {
    let mut p = Parser::new(2, 20, 0)?;
    let mut sink = Record::default();
    assert_eq!(
        p.process_until_frame(b"ab\x1b[?2026hcd", &mut sink)?,
        Some(10)
    );
    assert_eq!(p.process_until_frame(b"\x1b[?2026l", &mut sink)?, None);
    assert_eq!(
        p.process_until_frame(b"\x1b[?25;2026h!", &mut sink)?,
        Some(11)
    );
    p.process(b"\x1b[?2026l")?;
    assert_eq!(p.process_until_frame(b"x\x1b[?20", &mut sink)?, None);
    assert_eq!(p.process_until_frame(b"26hy", &mut sink)?, Some(3));
    // Set again while set: each set begins a frame.
    assert_eq!(p.process_until_frame(b"\x1b[?2026h", &mut sink)?, Some(8));
    assert_eq!(p.process_until_frame(b"plain text\r\n", &mut sink)?, None);
    Ok(())
}

/// A frame begun by XTRESTORE (`CSI ? 2026 r`, the mode saved set) stops
/// `process_until_frame` right after it, as one begun by BSU does; and an
/// `h` read afterwards, inside a string or not, stops nothing.
#[test]
fn process_until_frame_stops_after_an_xtrestore_that_begins_a_frame() -> Result {
    let mut p = Parser::new(2, 20, 0)?;
    let mut sink = Record::default();
    p.process(b"\x1b[?2026h\x1b[?2026s\x1b[?2026l")?;
    assert_eq!(
        p.process_until_frame(b"\x1b[?2026r\x1b]2;hello\x07", &mut sink)?,
        Some(8)
    );
    assert!(p.screen().synchronized_output());
    // An `h` in a string, or printed, after the frame's sequence is read.
    p.process(b"\x1b[?2026l")?;
    assert_eq!(
        p.process_until_frame(b"\x1b[?1h\x1b]2;hhh\x07hh\x1bh", &mut sink)?,
        None
    );
    Ok(())
}

/// In-band resize (`references/modern/mode_2048_in_band_resize.md`): with
/// `Feature::InBandResize`, setting mode 2048 reports the size at once,
/// every time it is set; DECRQM reports the mode; `Parser::resize_report`
/// gives the report for the new size after a resize while it is set; RIS
/// ends it. Without the option the mode is not recognized, as DECRQM says.
#[test]
fn in_band_resize_reports_the_size() -> Result {
    let options = Options::new()
        .with(Feature::ModeReports)
        .with(Feature::InBandResize);
    let mut p = Parser::with_options(24, 80, 0, options)?;
    let mut record = Record::default();
    p.process_with(
        b"\x1b[?2048$p\x1b[?2048h\x1b[?2048$p\x1b[?2048h\x1b[?2048;25h",
        &mut record,
    )?;
    let expected: [&[u8]; 5] = [
        b"\x1b[?2048;2$y",
        b"\x1b[48;24;80;0;0t",
        b"\x1b[?2048;1$y",
        b"\x1b[48;24;80;0;0t",
        b"\x1b[48;24;80;0;0t",
    ];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    p.resize(30, 100)?;
    assert_eq!(
        p.resize_report().as_deref(),
        Some(&b"\x1b[48;30;100;0;0t"[..])
    );
    p.process(b"\x1b[?2048l")?;
    assert_eq!(p.resize_report(), None, "reset");
    p.process(b"\x1b[?2048h\x1bc")?;
    assert_eq!(p.resize_report(), None, "RIS");
    // Without the option: not recognized, no report.
    let mut p = Parser::with_options(24, 80, 0, Options::new().with(Feature::ModeReports))?;
    let mut record = Record::default();
    p.process_with(b"\x1b[?2048h\x1b[?2048$p", &mut record)?;
    assert_eq!(record.replies, [b"\x1b[?2048;0$y".to_vec()]);
    assert_eq!(p.resize_report(), None);
    Ok(())
}

/// `Feature::SizeReports` answers xterm's text-area size query, `CSI 18 t`,
/// with the screen's size in characters; not the pixel query, `CSI 14 t`;
/// and nothing without the option.
#[test]
fn size_reports_answer_the_text_area_in_characters() -> Result {
    let options = Options::new().with(Feature::SizeReports);
    let mut p = Parser::with_options(24, 80, 0, options)?;
    let mut record = Record::default();
    p.process_with(b"\x1b[18t\x1b[14t", &mut record)?;
    p.resize(30, 100)?;
    p.process_with(b"\x1b[18t", &mut record)?;
    let expected: [&[u8]; 2] = [b"\x1b[8;24;80t", b"\x1b[8;30;100t"];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    assert!(run(Options::new(), b"\x1b[18t")?.replies.is_empty());
    Ok(())
}

const SETTINGS: Options = Options::new().with(Feature::SettingReports);

/// DECRQSS, `DCS $ q Pt ST` (xterm's ctlseqs, "Device-Control functions";
/// DECRPSS in the VT510 manual): `DCS 1 $ r Pt ST`, `Pt` the control
/// function that sets the setting now, or `DCS 0 $ r ST` for a request
/// the terminal does not know. The pen is in xterm's form and order
/// (`xtermFormatSGR`, xterm 411 misc.c): 0, then bold, underline, blink,
/// inverse, hidden, faint, italic, strikeout and double underline, then
/// the colours, 16 in their short forms and the rest with colons. An
/// underline style is kitty's `4:n`, in the underline's place, and the
/// underline colour, which xterm lacks, comes last in 38's form. neovim
/// sets `0` then `4:3` and asks: it draws curly diagnostics only on
/// `1$r0;4:3m` or `1$r4:3m` (neovim 0.12.5, src/nvim/tui/input.c,
/// `handle_term_response`).
#[test]
fn setting_reports_answer_the_pen_the_cursor_shape_and_the_margins() -> Result {
    let ask = |input: &str| -> std::result::Result<Vec<String>, fux_vt::Error> {
        Ok(run(SETTINGS, input.as_bytes())?
            .replies
            .iter()
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect())
    };
    let pen = |sgr: &str| ask(&format!("{sgr}\x1bP$qm\x1b\\"));
    assert_eq!(pen("\x1b[0m\x1b[4:3m")?, ["\x1bP1$r0;4:3m\x1b\\"]);
    assert_eq!(pen("")?, ["\x1bP1$r0m\x1b\\"]);
    assert_eq!(pen("\x1b[4m")?, ["\x1bP1$r0;4m\x1b\\"]);
    assert_eq!(pen("\x1b[21m")?, ["\x1bP1$r0;21m\x1b\\"]);
    assert_eq!(pen("\x1b[4:5m")?, ["\x1bP1$r0;4:5m\x1b\\"]);
    assert_eq!(
        pen("\x1b[9;3;2;8;7;6;4:4;1m")?,
        ["\x1bP1$r0;1;4:4;6;7;8;2;3;9m\x1b\\"]
    );
    assert_eq!(pen("\x1b[5m")?, ["\x1bP1$r0;5m\x1b\\"]);
    assert_eq!(pen("\x1b[31;102m")?, ["\x1bP1$r0;31;102m\x1b\\"]);
    assert_eq!(pen("\x1b[38;5;9;48;5;0m")?, ["\x1bP1$r0;91;40m\x1b\\"]);
    assert_eq!(
        pen("\x1b[38;5;208;48;2;1;2;3;58:2::4:5:6m")?,
        ["\x1bP1$r0;38:5:208;48:2::1:2:3;58:2::4:5:6m\x1b\\"]
    );
    // neovim's truecolor check (runtime/lua/vim/_core/defaults.lua) wants
    // its colour back: `48:2` and its three numbers.
    assert_eq!(
        pen("\x1b[0m\x1b[48;2;1;2;3m")?,
        ["\x1bP1$r0;48:2::1:2:3m\x1b\\"]
    );
    assert_eq!(pen("\x1b[4;58;5;1m")?, ["\x1bP1$r0;4;58:5:1m\x1b\\"]);
    // The margins, one-based.
    assert_eq!(ask("\x1bP$qr\x1b\\")?, ["\x1bP1$r1;24r\x1b\\"]);
    assert_eq!(ask("\x1b[3;10r\x1bP$qr\x1b\\")?, ["\x1bP1$r3;10r\x1b\\"]);
    // A cursor shape a program set; none for the terminal's own.
    assert_eq!(ask("\x1b[5 q\x1bP$q q\x1b\\")?, ["\x1bP1$r5 q\x1b\\"]);
    assert!(ask("\x1bP$q q\x1b\\")?.is_empty());
    assert!(ask("\x1b[0 q\x1bP$q q\x1b\\")?.is_empty());
    // Requests fux-vt does not know, a long one among them, are invalid.
    for request in ["s", "\"q", "t", "$|", "mm", "a long request"] {
        assert_eq!(
            ask(&format!("\x1bP$q{request}\x1b\\"))?,
            ["\x1bP0$r\x1b\\"],
            "{request:?}"
        );
    }
    // Not DECRQSS: parameters, another intermediate, XTGETTCAP, a string
    // cancelled.
    for other in [
        "\x1bP1$qm\x1b\\",
        "\x1bP+q536d756c78\x1b\\",
        "\x1bP$pm\x1b\\",
        "\x1bP$qm\x18",
        "\x1bPqm\x1b\\",
    ] {
        assert!(ask(other)?.is_empty(), "{other:?}");
    }
    // The pen the reply reports is the one when the string ends, and
    // printing goes on as before after it.
    let mut p = Parser::with_options(1, 10, 0, SETTINGS)?;
    let mut record = Record::default();
    p.process_with(b"a\x1bP$qm\x1b\\b\x1b[1m\x1bP$qm\x1b\\c", &mut record)?;
    let expected: [&[u8]; 2] = [b"\x1bP1$r0m\x1b\\", b"\x1bP1$r0;1m\x1b\\"];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    let text: String = (0..3)
        .filter_map(|x| p.screen().cell(0, x))
        .map(|c| c.contents().to_owned())
        .collect();
    assert_eq!(text, "abc");
    Ok(())
}

/// Without `Feature::SettingReports`, DECRQSS is a DCS like any other:
/// consumed, unanswered.
#[test]
fn without_the_option_decrqss_is_ignored() -> Result {
    let input = b"\x1b[4:3m\x1bP$qm\x1b\\\x1bP$qr\x1b\\\x1b[5 q\x1bP$q q\x1b\\\x1bP$qx\x1b\\";
    let record = run(Options::default(), input)?;
    assert_eq!(record, Record::default());
    let all = Options::new()
        .with(Feature::Events)
        .with(Feature::ExtendedReplies)
        .with(Feature::ModeReports);
    assert_eq!(run(all, input)?, Record::default());
    Ok(())
}

/// A DECRQSS answers the same however its bytes arrive, a byte at a time
/// included.
#[test]
fn setting_reports_are_the_same_in_any_pieces() -> Result {
    let input =
        b"\x1b[0m\x1b[4:3;38:2::1:2:3m\x1bP$qm\x1b\\x\x1b[2;5r\x1bP$qr\x1b\\\x1bP$qzz\x1b\\";
    let whole = run(SETTINGS, input)?;
    assert_eq!(whole.replies.len(), 3);
    for size in 1..input.len() {
        let mut parser = Parser::with_options(24, 80, 0, SETTINGS)?;
        let mut record = Record::default();
        for piece in pieces::pieces(input, size) {
            parser.process_with(piece, &mut record)?;
        }
        assert_eq!(record, whole, "pieces of {size}");
    }
    Ok(())
}

/// xterm's dynamic colour queries (ctlseqs, "Operating System Commands",
/// OSC 10 to 19): each `?` asks for the next colour from the one the OSC
/// names, an event each, with how the query ended so the host answers in
/// kind. Setting a colour and other OSC numbers are no query; nor is
/// anything without `Feature::Events`.
#[test]
fn colour_queries_are_events() -> Result {
    let input = b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]10;?;?\x07\x1b]12;red;?\x07\
        \x1b]10;#ffffff\x07\x1b]4;1;?\x07\x1b]19;?;?\x07\x1b]110;?\x07";
    let record = run(EVENTS, input)?;
    assert_eq!(
        record.events,
        [
            "color:11:bel",
            "color:10:st",
            "color:10:bel",
            "color:11:bel",
            "color:13:bel",
            "color:19:bel",
        ]
    );
    assert!(record.replies.is_empty(), "the host answers");
    for size in [1, 2, 5] {
        let mut parser = Parser::with_options(24, 80, 0, EVENTS)?;
        let mut pieces_record = Record::default();
        for chunk in pieces::pieces(input, size) {
            parser.process_with(chunk, &mut pieces_record)?;
        }
        assert_eq!(pieces_record, record, "chunk size {size}");
    }
    assert_eq!(run(Options::default(), input)?, Record::default());
    Ok(())
}

/// Colour-scheme change reports
/// (`references/modern/mode_2031_color_scheme_updates.md`): with
/// `Feature::ColorSchemeUpdates`, mode 2031 is tracked and DECRQM reports
/// it; RIS ends it. `CSI ? 996 n`, the scheme asked for, is the host's to
/// answer. Without the option the mode is not recognized.
#[test]
fn colour_scheme_updates_are_tracked() -> Result {
    #[derive(Default)]
    struct Said(Vec<String>);
    impl Sink for Said {
        fn reply(&mut self, bytes: &[u8]) {
            self.0.push(String::from_utf8_lossy(bytes).into_owned());
        }
        fn unhandled(&mut self, sequence: fux_vt::Unhandled<'_>) {
            self.0.push(format!("unhandled {sequence:?}"));
        }
    }
    let options = Options::new()
        .with(Feature::ModeReports)
        .with(Feature::ColorSchemeUpdates);
    let mut p = Parser::with_options(24, 80, 0, options)?;
    let mut said = Said::default();
    p.process_with(b"\x1b[?2031$p\x1b[?2031h\x1b[?2031$p", &mut said)?;
    assert!(p.screen().color_scheme_updates());
    assert_eq!(said.0, ["\x1b[?2031;2$y", "\x1b[?2031;1$y"]);
    p.process(b"\x1b[?2031l")?;
    assert!(!p.screen().color_scheme_updates());
    p.process(b"\x1b[?2031h\x1bc")?;
    assert!(!p.screen().color_scheme_updates(), "RIS");
    let mut said = Said::default();
    p.process_with(b"\x1b[?996n", &mut said)?;
    assert_eq!(said.0.len(), 1);
    assert!(said.0.iter().all(|s| s.starts_with("unhandled Csi")));
    let mut p = Parser::with_options(24, 80, 0, Options::new().with(Feature::ModeReports))?;
    let mut said = Said::default();
    p.process_with(b"\x1b[?2031h\x1b[?2031$p", &mut said)?;
    assert!(!p.screen().color_scheme_updates());
    assert_eq!(said.0, ["\x1b[?2031;0$y"]);
    Ok(())
}

#[test]
fn keypad_mode_is_tracked_and_reset() -> Result {
    let mut parser = Parser::new(2, 4, 0)?;
    assert!(!parser.screen().application_keypad());
    parser.process(b"\x1b=")?;
    assert!(parser.screen().application_keypad());
    parser.process(b"\x1b>")?;
    assert!(!parser.screen().application_keypad());
    parser.process(b"\x1b=\x1bc")?;
    assert!(!parser.screen().application_keypad(), "RIS resets DECKPAM");
    Ok(())
}

#[test]
fn consumers_can_reconstruct_cells_exactly() -> Result {
    let mut parser = Parser::new(2, 10, 0)?;
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let input = format!("\x1b[1;4;38;5;208;48;2;1;2;3m界e\u{301}\x1b[m {family}");
    parser.process(input.as_bytes())?;
    let screen = parser.screen();
    let mut copy = Cells::new(6);
    for col in 0..6 {
        let original = screen.cell(0, col).ok_or("cell")?;
        if original.is_wide_continuation() {
            copy.set(usize::from(col), CellRef::wide_continuation());
            continue;
        }
        let a = original.attributes();
        let attributes = Attributes::new(a.foreground(), a.background())
            .with_bold(a.bold())
            .with_dim(a.dim())
            .with_italic(a.italic())
            .with_underline_style(a.underline_style())
            .with_inverse(a.inverse());
        let text = original.contents();
        assert!(copy.set(
            usize::from(col),
            CellRef::new(text, original.is_wide(), attributes)
        ));
    }
    let row = screen.row_from_bottom(1).ok_or("row")?;
    let original: Cells = row.cells().take(6).collect();
    assert_eq!(copy, original);
    assert_eq!(copy.get(4).map(|c| c.contents()), Some(family));
    let attributes = Attributes::new(Color::Idx(1), Color::Default);
    assert!(!attributes.with_bold(true).with_bold(false).bold());
    Ok(())
}

/// DECRQCRA (VT520 manual, 5-104; DEC STD 070, 5-180), with
/// `Feature::RectangleChecksums`: DECCKSR, `DCS Pi ! ~ xxxx ST`, gives the
/// checksum of the rectangle's cells as xterm and the VT520 sum them
/// (`xtermCheckRect`; ctlseqs, XTCHECKSUM): each character, plus 0x10
/// underlined, 0x20 inverse, 0x40 blinking and 0x80 bold, summed in 16 bits
/// and negated, four upper-case hex digits. An empty cell is a space; a
/// character past Latin-1 is ESC; a DEC Special Graphics glyph, the code it
/// was drawn with. The page is ignored, as in xterm; the rectangle defaults
/// to the whole screen, and is relative to the margins in origin mode.
/// Without the option, nothing is answered.
#[test]
fn rectangle_checksums_sum_the_cells() -> Result {
    let checksum = |sum: u16| format!("{:04X}", sum.wrapping_neg());
    let options = Options::new().with(Feature::RectangleChecksums);
    let mut p = Parser::with_options(3, 4, 0, options)?;
    let mut record = Record::default();
    // Row 1: a, b, bold c, underlined and inverse d. Row 2: é, a wide
    // glyph, a line-drawing glyph.
    p.process("ab\x1b[1mc\x1b[0;4;7md\x1b[m\r\né中\x1b(0q\x1b(B".as_bytes())?;
    let mut input = String::new();
    for (id, rect) in [
        (1, "1;1;1;1"),
        (2, "1;1;1;2"),
        (3, "1;3;1;3"),
        (4, "1;4;1;4"),
        (5, "2;1;2;1"),
        (6, "2;2;2;3"),
        (7, "2;4;2;4"),
        (8, "3;1;3;1"),
        (9, ""),
        (10, "2;2;1;1"),
    ] {
        input.push_str(&format!("\x1b[{id};0;{rect}*y"));
    }
    p.process_with(input.as_bytes(), &mut record)?;
    // Origin mode: row 1 is the top margin, row 3.
    p.process_with(b"\x1b[3;3r\x1b[?6hz\x1b[11;0;1;1;1;1*y", &mut record)?;
    let whole: u16 = 0x61 + 0x62 + 0x63 + 0x80 + 0x64 + 0x30 + 0xe9 + 0x1b * 2 + 0x71 + 0x20 * 4;
    let expected = [
        (1, checksum(0x61)),
        (2, checksum(0x61 + 0x62)),
        (3, checksum(0x63 + 0x80)),
        (4, checksum(0x64 + 0x10 + 0x20)),
        (5, checksum(0xe9)),
        (6, checksum(0x1b * 2)),
        (7, checksum(0x71)),
        (8, checksum(0x20)),
        (9, checksum(whole)),
        (10, "0000".to_owned()),
        (11, checksum(0x7a)),
    ]
    .map(|(id, sum)| format!("\x1bP{id}!~{sum}\x1b\\").into_bytes());
    assert_eq!(record.replies, expected);
    assert!(
        run(Options::new(), b"\x1b[1;0;1;1;1;1*y")?
            .replies
            .is_empty()
    );
    Ok(())
}
