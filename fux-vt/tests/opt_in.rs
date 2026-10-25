//! Opt-in outputs: `Feature::Events` (OSC 0/1/2/52 and BEL),
//! `Feature::ExtendedReplies` (DECRQM, DECXCPR, secondary DA) and
//! `Feature::ModeReports` (DECRQM alone). The default must
//! stay fux's policy: no events and the original reply set.

use fux_vt::{
    Attributes, CellRef, Cells, Color, Event, Feature, Mode, OSC_PAYLOAD_LIMIT, Options, Parser,
    Sink, Size, Stop,
};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

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
            Event::Notification(_) => "unknown".to_owned(),
            Event::CommandEnded(status) => format!("ended:{status:?}"),
        });
    }
}

const EVENTS: Options = Options::new().with(Feature::Events);
const REPLIES: Options = Options::new().with(Feature::ExtendedReplies);

fn run(options: Options, input: &[u8]) -> Result<Record> {
    let mut parser = Parser::with_options(Size::new(24, 80)?, 0, options)?;
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
    let p = Parser::new(Size::new(2, 2)?, 0)?;
    assert_eq!(p.options(), Options::default());
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
        let mut parser = Parser::with_options(Size::new(24, 80)?, 0, EVENTS)?;
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
    let mut parser = Parser::with_options(Size::new(4, 20)?, 0, EVENTS)?;
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

/// ECMA-48 8.3.89 and 5.6: an OSC's command string holds 00/08 to 00/13
/// (BS to CR) and 02/00 to 07/14. The other controls, 00/00 to 00/06,
/// 00/14 to 01/15 but CAN, SUB and ESC, and DEL, are left out of the
/// payload, as Williams's DEC parser model leaves them (`osc_string`
/// ignores C0); BEL ends the string. Read at once and a byte at a time.
#[test]
fn controls_a_command_string_cannot_hold_are_left_out_of_an_osc() -> Result {
    let input = b"\x1b]2;a\x00b\x01c\x06d\x0e\x1f\x7fe\x08\t\n\x0b\x0c\rf\x07";
    let mut split = Parser::with_options(Size::new(4, 20)?, 0, EVENTS)?;
    let mut record = Record::default();
    for byte in input {
        split.process_with(&[*byte], &mut record)?;
    }
    for events in [run(EVENTS, input)?.events, record.events] {
        assert_eq!(events, ["title:abcde\x08\t\n\x0b\x0c\rf"]);
    }
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
        // IRM and LNM, the ANSI modes fux-vt keeps; SRM, set for good
        // (ECMA-48 7.2.15: fux-vt never images what is typed).
        b"\x1b[4;2$y",
        b"\x1b[4;1$y",
        b"\x1b[20;2$y",
        b"\x1b[12;3$y",
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

/// The DSRs of DEC's other devices (`CSI ? Ps n`), which fux-vt has none
/// of: the printer (DEC STD 070-7: ?13, none), UDKs (DEC STD 070-11's
/// `REPORT_UDK_STATUS`: ?23, no UDK extension), the keyboard (the VT520
/// manual, DSR keyboard report: language unknown, as a VT220 reports it,
/// the level DA2 gives; status and type are level 4's),
/// the locator (DEC STD 070-13: ?53, none; ?57;0, unknown), macros (DECMSR:
/// no space; DECCKSR of none: 0000, under the request's label), data
/// integrity (DEC STD 070, DSR: ?73 first after power-up or RIS, then ?70)
/// and sessions (?83, not configured for them).
#[test]
fn extended_replies_answer_the_device_status_reports() -> Result {
    let record = run(
        REPLIES,
        b"\x1b[?15n\x1b[?25n\x1b[?26n\x1b[?55n\x1b[?56n\x1b[?62n\x1b[?63;7n\x1b[?75n\x1b[?75n\
          \x1b[?85n\x1bc\x1b[?75n\x1b[?99n",
    )?;
    let expected: [&[u8]; 11] = [
        b"\x1b[?13n",
        b"\x1b[?23n",
        b"\x1b[?27;0n",
        b"\x1b[?53n",
        b"\x1b[?57;0n",
        b"\x1b[0*{",
        b"\x1bP7!~0000\x1b\\",
        b"\x1b[?73n",
        b"\x1b[?70n",
        b"\x1b[?83n",
        b"\x1b[?73n",
    ];
    assert_eq!(record.replies, expected.map(<[u8]>::to_vec));
    assert!(
        run(Options::new(), b"\x1b[?15n\x1b[?75n")?
            .replies
            .is_empty()
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
    let mut p = Parser::new(Size::new(4, 10)?, 0)?;
    assert!(!p.screen().mode(Mode::SynchronizedOutput));
    p.process(b"\x1b[?2026h")?;
    assert!(p.screen().mode(Mode::SynchronizedOutput));
    p.process(b"\x1b[?2026l")?;
    assert!(!p.screen().mode(Mode::SynchronizedOutput));
    for (then, what) in [(&b"\x1bc"[..], "RIS"), (b"\x1b[!p", "DECSTR")] {
        p.process(b"\x1b[?2026h")?;
        p.process(then)?;
        assert!(!p.screen().mode(Mode::SynchronizedOutput), "{what}");
    }
    p.process(b"\x1b[?2026h")?;
    p.resize(Size::new(4, 10)?)?;
    assert!(
        !p.screen().mode(Mode::SynchronizedOutput),
        "a resize to the same size"
    );
    p.process(b"\x1b[?2026h")?;
    p.resize(Size::new(5, 12)?)?;
    assert!(!p.screen().mode(Mode::SynchronizedOutput), "a resize");
    Ok(())
}

/// `Parser::process_until_stop` stops right after the sequence that sets
/// synchronized output, wherever it is: alone, beside other modes, or split
/// across calls; and reads everything when none does.
#[test]
fn process_until_stop_stops_after_the_sequence_that_sets_2026() -> Result {
    let mut p = Parser::new(Size::new(2, 20)?, 0)?;
    let mut sink = Record::default();
    let begun = |n| Some((n, Stop::Begun));
    assert_eq!(
        p.process_until_stop(b"ab\x1b[?2026hcd", &mut sink)?,
        begun(10)
    );
    p.process(b"\x1b[?2026l")?;
    assert_eq!(
        p.process_until_stop(b"\x1b[?25;2026h!", &mut sink)?,
        begun(11)
    );
    p.process(b"\x1b[?2026l")?;
    assert_eq!(p.process_until_stop(b"x\x1b[?20", &mut sink)?, None);
    assert_eq!(p.process_until_stop(b"26hy", &mut sink)?, begun(3));
    // Set again while set, the mode does not change: the frame being drawn
    // goes on, and none begins (the mode 2026 spec: while it is set, the
    // terminal keeps rendering the last state; setting it again is no new
    // state). XTRESTORE of it saved set, while set, begins none either.
    assert_eq!(p.process_until_stop(b"\x1b[?2026h", &mut sink)?, None);
    p.process(b"\x1b[?2026s")?;
    assert_eq!(p.process_until_stop(b"\x1b[?2026r", &mut sink)?, None);
    p.process(b"\x1b[?2026l")?;
    assert_eq!(p.process_until_stop(b"plain text\r\n", &mut sink)?, None);
    Ok(())
}

/// `Parser::process_until_stop` stops right after whatever resets
/// synchronized output while it is set (ESU, beside other modes or split
/// across calls; XTRESTORE of it saved reset; DECSTR; RIS), so a host that
/// reads output as it comes knows the frame is whole; a reset while it is
/// not set ends nothing.
#[test]
fn process_until_stop_stops_after_the_sequence_that_ends_a_frame() -> Result {
    let mut p = Parser::new(Size::new(2, 20)?, 0)?;
    let mut sink = Record::default();
    let ended = |n| Some((n, Stop::Ended));
    p.process(b"\x1b[?2026s")?;
    for (end, what) in [
        (&b"\x1b[?2026lz"[..], "ESU"),
        (b"\x1b[?25;2026lz", "ESU beside another mode"),
        (b"\x1b[?2026rz", "XTRESTORE"),
        (b"\x1b[!pz", "DECSTR"),
        (b"\x1bcz", "RIS"),
    ] {
        p.process(b"\x1b[?2026h")?;
        let taken = end.len().saturating_sub(1);
        assert_eq!(
            p.process_until_stop(end, &mut sink)?,
            ended(taken),
            "{what}"
        );
    }
    p.process(b"\x1b[?2026h")?;
    assert_eq!(p.process_until_stop(b"ab\x1b[?20", &mut sink)?, None);
    assert_eq!(p.process_until_stop(b"26l", &mut sink)?, ended(3));
    assert_eq!(p.process_until_stop(b"\x1b[?2026l\x1bc", &mut sink)?, None);
    Ok(())
}

/// A frame begun by XTRESTORE (`CSI ? 2026 r`, the mode saved set) stops
/// `process_until_stop` right after it, as one begun by BSU does; and an
/// `h` read afterwards, inside a string or not, stops nothing.
#[test]
fn process_until_stop_stops_after_an_xtrestore_that_begins_a_frame() -> Result {
    let mut p = Parser::new(Size::new(2, 20)?, 0)?;
    let mut sink = Record::default();
    p.process(b"\x1b[?2026h\x1b[?2026s\x1b[?2026l")?;
    assert_eq!(
        p.process_until_stop(b"\x1b[?2026r\x1b]2;hello\x07", &mut sink)?,
        Some((8, Stop::Begun))
    );
    assert!(p.screen().mode(Mode::SynchronizedOutput));
    // An `h` in a string, or printed, after the frame's sequence is read.
    p.process(b"\x1b[?2026l")?;
    assert_eq!(
        p.process_until_stop(b"\x1b[?1h\x1b]2;hhh\x07hh\x1bh", &mut sink)?,
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
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, options)?;
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
    p.resize(Size::new(30, 100)?)?;
    assert_eq!(
        p.resize_report().as_deref(),
        Some(&b"\x1b[48;30;100;0;0t"[..])
    );
    p.process(b"\x1b[?2048l")?;
    assert_eq!(p.resize_report(), None, "reset");
    p.process(b"\x1b[?2048h\x1bc")?;
    assert_eq!(p.resize_report(), None, "RIS");
    // Without the option: not recognized, no report.
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, Feature::ModeReports.into())?;
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
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, options)?;
    let mut record = Record::default();
    p.process_with(b"\x1b[18t\x1b[14t", &mut record)?;
    p.resize(Size::new(30, 100)?)?;
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
    let ask = |input: &str| -> Result<Vec<String>> {
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
    // A cursor shape a program set, 0 as the blinking block it is (1);
    // none for the terminal's own, before any and after DECSCUSR 7.
    assert_eq!(ask("\x1b[5 q\x1bP$q q\x1b\\")?, ["\x1bP1$r5 q\x1b\\"]);
    assert_eq!(ask("\x1b[0 q\x1bP$q q\x1b\\")?, ["\x1bP1$r1 q\x1b\\"]);
    assert!(ask("\x1bP$q q\x1b\\")?.is_empty());
    assert!(ask("\x1b[5 q\x1b[7 q\x1bP$q q\x1b\\")?.is_empty());
    // The left and right margins, one-based, the screen's edges without
    // DECLRMM (DEC STD 070, DECRQSS: DECSLRM's `s`).
    assert_eq!(ask("\x1bP$qs\x1b\\")?, ["\x1bP1$r1;80s\x1b\\"]);
    assert_eq!(
        ask("\x1b[?69h\x1b[3;4s\x1bP$qs\x1b\\")?,
        ["\x1bP1$r3;4s\x1b\\"]
    );
    // DECSCA's selective erase attribute: 1 while DECSCA 1 protects, 0
    // otherwise, SPA's ISO protection being no DECSCA.
    for (setup, ps) in [
        ("", 0),
        ("\x1b[1\"q", 1),
        ("\x1b[1\"q\x1b[0\"q", 0),
        ("\x1bV", 0),
    ] {
        let reply = format!("\x1bP1$r{ps}\"q\x1b\\");
        assert_eq!(
            ask(&format!("{setup}\x1bP$q\"q\x1b\\"))?,
            [reply],
            "{setup:?}"
        );
    }
    // DECSCL's level and C1 replies, 7-bit (1), as fux-vt sends them
    // whatever Pc asks (DEC STD 070, DECSCL; ECMA-35, 15.4).
    for (setup, level) in [
        ("", 62),
        ("\x1b[61\"p", 61),
        ("\x1b[62;0\"p", 62),
        ("\x1b[61\"p\x1b[65;1\"p", 61),
    ] {
        let reply = format!("\x1bP1$r{level};1\"p\x1b\\");
        assert_eq!(
            ask(&format!("{setup}\x1bP$q\"p\x1b\\"))?,
            [reply],
            "{setup:?}"
        );
    }
    // DECSACE's Ps as it was set, 0 until set (DEC STD 070, DECRQSS; esctest
    // sets 0 and expects `0*x`); one it does not take changes nothing.
    for (setup, ps) in [
        ("", 0),
        ("\x1b[2*x", 2),
        ("\x1b[1*x", 1),
        ("\x1b[2*x\x1b[3*x", 2),
    ] {
        let reply = format!("\x1bP1$r{ps}*x\x1b\\");
        assert_eq!(
            ask(&format!("{setup}\x1bP$q*x\x1b\\"))?,
            [reply],
            "{setup:?}"
        );
    }
    // Requests fux-vt does not know, a long one among them, are invalid.
    for request in ["t", "$|", "mm", "a long request"] {
        assert_eq!(
            ask(&format!("\x1bP$q{request}\x1b\\"))?,
            ["\x1bP0$r\x1b\\"],
            "{request:?}"
        );
    }
    // Not DECRQSS: parameters, another intermediate, a string cancelled.
    for other in [
        "\x1bP1$qm\x1b\\",
        "\x1bP$pm\x1b\\",
        "\x1bP$qm\x18",
        "\x1bPqm\x1b\\",
    ] {
        assert!(ask(other)?.is_empty(), "{other:?}");
    }
    // The pen the reply reports is the one when the string ends, and
    // printing goes on as before after it.
    let mut p = Parser::with_options(Size::new(1, 10)?, 0, SETTINGS)?;
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

/// DEC STD 070, 3.5.4.4 (control strings): "It is recommended that all
/// other control codes not used within a command string be ignored by
/// hardware". So C0 controls inside a DECRQSS request are no part of it:
/// `DCS $ q NUL m ST` asks for the pen, as `DCS $ q m ST` does, and a line
/// break inside it changes nothing.
#[test]
fn c0_controls_inside_a_setting_request_are_ignored() -> Result {
    for request in ["\x00m", "\x01m", "m\x0d\x0a", "\x07m"] {
        let replies = run(SETTINGS, format!("\x1bP$q{request}\x1b\\").as_bytes())?.replies;
        let expected: &[u8] = b"\x1bP1$r0m\x1b\\";
        assert_eq!(replies, [expected.to_vec()], "{request:?}");
    }
    Ok(())
}

/// XTGETTCAP, `DCS + q Pt ST` (xterm's ctlseqs): "xterm responds with DCS
/// 1 + r Pt ST for valid requests ... or DCS 0 + r ST for invalid
/// requests". fux-vt knows no capability names, so with
/// `Feature::SettingReports` every request is answered invalid, its names
/// not echoed, a request of any length once; one cancelled, or with
/// parameters, is not answered, nor any without the option.
#[test]
fn xtgettcap_is_answered_as_an_invalid_request() -> Result {
    let ask = |options: Options, input: &[u8]| -> Result<Vec<Vec<u8>>> {
        Ok(run(options, input)?.replies)
    };
    let invalid: &[u8] = b"\x1bP0+r\x1b\\";
    for request in [&b"544e"[..], b"536d756c78;544e", b"", b"not hex"] {
        let input = [b"\x1bP+q", request, b"\x1b\\"].concat();
        assert_eq!(ask(SETTINGS, &input)?, [invalid.to_vec()], "{request:?}");
    }
    let long = [&b"\x1bP+q"[..], &vec![b'4'; 100 << 10], b"\x1b\\"].concat();
    assert_eq!(ask(SETTINGS, &long)?, [invalid.to_vec()]);
    for other in [&b"\x1bP+q544e\x18"[..], b"\x1bP1+q544e\x1b\\"] {
        assert!(ask(SETTINGS, other)?.is_empty(), "{other:?}");
    }
    assert!(ask(Options::new(), b"\x1bP+q544e\x1b\\")?.is_empty());
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
        let mut parser = Parser::with_options(Size::new(24, 80)?, 0, SETTINGS)?;
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
/// kind, its number read as a number (`010` is 10). Setting a colour and
/// other OSC numbers are no query; nor is anything without
/// `Feature::Events`.
#[test]
fn colour_queries_are_events() -> Result {
    let input = b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]10;?;?\x07\x1b]12;red;?\x07\
        \x1b]10;#ffffff\x07\x1b]4;1;?\x07\x1b]19;?;?\x07\x1b]110;?\x07\x1b]010;?\x07";
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
            "color:10:bel",
        ]
    );
    assert!(record.replies.is_empty(), "the host answers");
    for size in [1, 2, 5] {
        let mut parser = Parser::with_options(Size::new(24, 80)?, 0, EVENTS)?;
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
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, options)?;
    let mut said = Said::default();
    p.process_with(b"\x1b[?2031$p\x1b[?2031h\x1b[?2031$p", &mut said)?;
    assert!(p.screen().mode(Mode::ColorSchemeUpdates));
    assert_eq!(said.0, ["\x1b[?2031;2$y", "\x1b[?2031;1$y"]);
    p.process(b"\x1b[?2031l")?;
    assert!(!p.screen().mode(Mode::ColorSchemeUpdates));
    p.process(b"\x1b[?2031h\x1bc")?;
    assert!(!p.screen().mode(Mode::ColorSchemeUpdates), "RIS");
    let mut said = Said::default();
    p.process_with(b"\x1b[?996n", &mut said)?;
    assert_eq!(said.0.len(), 1);
    assert!(said.0.iter().all(|s| s.starts_with("unhandled Csi")));
    let mut p = Parser::with_options(Size::new(24, 80)?, 0, Feature::ModeReports.into())?;
    let mut said = Said::default();
    p.process_with(b"\x1b[?2031h\x1b[?2031$p", &mut said)?;
    assert!(!p.screen().mode(Mode::ColorSchemeUpdates));
    assert_eq!(said.0, ["\x1b[?2031;0$y"]);
    Ok(())
}

#[test]
fn consumers_can_reconstruct_cells_exactly() -> Result {
    let mut parser = Parser::new(Size::new(2, 10)?, 0)?;
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let input = format!("\x1b[1;4;38;5;208;48;2;1;2;3m界e\u{301}\x1b[m {family}");
    parser.process(input.as_bytes())?;
    let screen = parser.screen();
    let mut copy = Cells::new(6).unwrap_or_default();
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
    let row = screen.rows().nth_back(1).ok_or("row")?;
    let original = Cells::try_from_iter(row.cells().take(6))?;
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
/// and negated, four upper-case hex digits. A character past Latin-1 is
/// ESC; a DEC Special Graphics glyph, its DEC code
/// (`rectangle_checksums_sum_a_designated_glyph_as_xterm_does`). Two
/// departures from xterm 411 (README, `RectangleChecksums` row): an empty
/// cell is a space, as esctest reads it (xterm: nothing), and a UTF-8 é its
/// Latin-1 code (xterm: 0x69). The page is ignored, as in xterm (README,
/// DECRQCRA row); the rectangle defaults to the whole screen, and counts
/// from the margins in origin mode. An inverted rectangle has "the sequence
/// ... ignored" (DEC STD 070, DECRQCRA): no reply. Without the option,
/// nothing is answered.
#[test]
fn rectangle_checksums_sum_the_cells() -> Result {
    let checksum = |sum: u16| format!("{:04X}", sum.wrapping_neg());
    let options = Options::new().with(Feature::RectangleChecksums);
    let mut p = Parser::with_options(Size::new(3, 4)?, 0, options)?;
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
    let whole: u16 = 0x61 + 0x62 + 0x63 + 0x80 + 0x64 + 0x30 + 0xe9 + 0x1b * 2 + 0x1d + 0x20 * 4;
    let expected = [
        (1, checksum(0x61)),
        (2, checksum(0x61 + 0x62)),
        (3, checksum(0x63 + 0x80)),
        (4, checksum(0x64 + 0x10 + 0x20)),
        (5, checksum(0xe9)),
        (6, checksum(0x1b * 2)),
        (7, checksum(0x1d)),
        (8, checksum(0x20)),
        (9, checksum(whole)),
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

/// DECRQCRA over glyphs of a designated set, with
/// `Feature::RectangleChecksums`: each counts as the DEC code xterm 411
/// sums for it (`xtermCheckRect` by default calls `xtermCharSetDec`, whose
/// `unmap_DEC_Spec_Graphic` and `unmap_DEC_Supp_Graphic` give the code of
/// the cell's byte in its set), not the byte drawn. Each value is xterm
/// 411's own reply, read from it under Xvfb (`decTerminalID: 420`) for the
/// same bytes: Special Graphics' ─ 0x1d, ▒ 0x78, π 0xc6, ≤ 0x85, ° 0xb0,
/// · 0xb7, £ 0xa3, a bold ─ 0x9d; DEC Supplemental's (`ESC ( % 5`) Œ 0x97,
/// Ÿ 0x98, œ 0x99, Á 0xc1, ¤ 0xa4, and an empty position ESC.
#[test]
fn rectangle_checksums_sum_a_designated_glyph_as_xterm_does() -> Result {
    let options = Options::new().with(Feature::RectangleChecksums);
    let mut p = Parser::with_options(Size::new(2, 8)?, 0, options)?;
    let mut record = Record::default();
    p.process(b"\x1b(0qa{yf~}\x1b[1mq\x1b[m\x1b(B\r\n\x1b(%5W]wA($\x1b(B")?;
    let cells = (1..=8u16)
        .map(|col| (1, col))
        .chain((1..=6).map(|col| (2, col)));
    let mut input = String::new();
    for (row, col) in cells.clone() {
        input.push_str(&format!("\x1b[{row}{col};0;{row};{col};{row};{col}*y"));
    }
    p.process_with(input.as_bytes(), &mut record)?;
    let sums: [u16; 14] = [
        0x1d, 0x78, 0xc6, 0x85, 0xb0, 0xb7, 0xa3, 0x9d, // Special Graphics
        0x97, 0x98, 0x99, 0xc1, 0xa4, 0x1b, // DEC Supplemental
    ];
    let expected: Vec<String> = cells
        .zip(sums)
        .map(|((row, col), sum)| format!("\x1bP{row}{col}!~{:04X}\x1b\\", sum.wrapping_neg()))
        .collect();
    let replies: Vec<String> = record
        .replies
        .iter()
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect();
    assert_eq!(replies, expected);
    Ok(())
}
