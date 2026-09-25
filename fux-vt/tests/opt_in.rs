//! Opt-in outputs: `Options::events` (OSC 0/1/2/52 and BEL) and
//! `Options::extended_replies` (DECRQM, DECXCPR, secondary DA). The default must
//! stay fux's policy: no events and the original reply set.

use fux_vt::{Attributes, Cell, Color, Event, OSC_PAYLOAD_LIMIT, Options, Parser, Sink};
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
            _ => "unknown".to_owned(),
        });
    }
}

const EVENTS: Options = Options {
    events: true,
    extended_replies: false,
};
const REPLIES: Options = Options {
    events: false,
    extended_replies: true,
};

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
        parser.screen().cell(0, 0).map(Cell::contents),
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
          \x1b[?25$p\x1b[?1000h\x1b[?1000$p\x1b[?1002$p\x1b[?4242$p\x1b[4$p",
    )?;
    let expected: [&[u8]; 11] = [
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
        b"\x1b[4;0$y",
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
    parser.process("\x1b[1;4;38;5;208;48;2;1;2;3m界e\u{301}\x1b[m ".as_bytes())?;
    let screen = parser.screen();
    for col in 0..4 {
        let original = screen.cell(0, col).ok_or("cell")?;
        let copy = if original.is_wide_continuation() {
            Cell::wide_continuation()
        } else {
            let a = original.attributes();
            let attributes = Attributes::new(a.foreground, a.background)
                .with_bold(a.bold())
                .with_dim(a.dim())
                .with_italic(a.italic())
                .with_underline(a.underline())
                .with_inverse(a.inverse());
            Cell::new(original.contents(), original.is_wide(), attributes).ok_or("fits")?
        };
        assert_eq!(&copy, original, "col {col}");
    }
    let attributes = Attributes::new(Color::Idx(1), Color::Default);
    let xs = |n: usize| std::iter::repeat_n('x', n).collect::<String>();
    assert!(Cell::new(&xs(Cell::CONTENTS_CAPACITY), false, attributes).is_some());
    assert!(Cell::new(&xs(Cell::CONTENTS_CAPACITY + 1), false, attributes).is_none());
    assert!(!attributes.with_bold(true).with_bold(false).bold());
    Ok(())
}
