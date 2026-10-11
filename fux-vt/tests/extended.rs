//! Behaviour beyond the vt100 baseline: blink, hidden, strikeout and
//! underline colour; HVP and SCOSC/SCORC; grapheme clusters; sequences
//! reported as unhandled; and the opt-in kitty keyboard protocol, identity
//! replies and reflow on resize.

use fux_vt::{
    Blink, CLUSTER_CAPACITY, CellRef, Color, Error, Feature, Identity, Options, Parser, Sink, Size,
    Unhandled,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[path = "corpus/lines.rs"]
mod lines;
use lines::lines;

/// The rows of the window `offset` rows back into history.
fn window_lines(parser: &Parser, offset: usize) -> Vec<String> {
    let window = parser.screen().window();
    let window = window.row(0).map_or(window, |top| top.up(offset).window());
    (0..window.rows())
        .map(|y| {
            (0..window.cols())
                .filter_map(|x| window.cell(y, x))
                .filter(|c| !c.is_wide_continuation())
                .map(|c| if c.has_contents() { c.contents() } else { " " })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn cell(parser: &Parser, row: u16, col: u16) -> std::result::Result<CellRef<'_>, Error> {
    parser.screen().cell(row, col).ok_or(Error::InvalidRange)
}

/// Replies and unhandled sequences, in order, as text.
#[derive(Debug, Default)]
struct Record {
    replies: Vec<String>,
    unhandled: Vec<String>,
}
impl Sink for Record {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies
            .push(String::from_utf8_lossy(bytes).into_owned());
    }
    fn unhandled(&mut self, sequence: Unhandled<'_>) {
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        self.unhandled.push(match sequence {
            Unhandled::Csi {
                params,
                intermediates,
                action,
            } => {
                let params: Vec<String> = params
                    .groups()
                    .map(|g| g.iter().map(u16::to_string).collect::<Vec<_>>().join(":"))
                    .collect();
                format!(
                    "CSI {}{}{}",
                    text(intermediates),
                    params.join(";"),
                    char::from(action)
                )
            }
            Unhandled::Escape {
                intermediates,
                action,
            } => format!("ESC {}{}", text(intermediates), char::from(action)),
            _ => "?".to_owned(),
        });
    }
}

fn run(parser: &mut Parser, input: &[u8]) -> std::result::Result<Record, Error> {
    let mut record = Record::default();
    parser.process_with(input, &mut record)?;
    Ok(record)
}

const KEYBOARD: Options = Options::new().with(Feature::KittyKeyboard);
const REFLOW: Options = Options::new().with(Feature::Reflow);
const RATTY: Identity = Identity {
    name: "ratty",
    version: "0.5.0",
};

// SGR beyond the baseline.

#[test]
fn blink_hidden_strikeout_and_underline_colour_are_stored_on_cells() -> Result {
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process(b"a\x1b[5mb\x1b[6mc\x1b[25md\x1b[5m\x1b[me")?;
    let blink = |p: &Parser, col| cell(p, 0, col).map(|c| c.blink());
    assert_eq!(blink(&p, 0)?, Blink::None);
    assert_eq!(blink(&p, 1)?, Blink::Slow);
    assert_eq!(blink(&p, 2)?, Blink::Rapid, "rapid replaces slow");
    assert_eq!(blink(&p, 3)?, Blink::None);
    assert_eq!(blink(&p, 4)?, Blink::None, "SGR 0 resets blink");

    let mut p = Parser::new(Size::new(2, 12)?, 0)?;
    p.process(b"a\x1b[8mb\x1b[28m\x1b[9mc\x1b[29m\x1b[58;2;1;2;3md\x1b[58;5;9me\x1b[59mf\x1b[8;9;58:2:4:5:6mg\x1b[mh")?;
    assert!(!cell(&p, 0, 0)?.hidden() && !cell(&p, 0, 0)?.strikeout());
    assert!(cell(&p, 0, 1)?.hidden());
    assert!(!cell(&p, 0, 2)?.hidden() && cell(&p, 0, 2)?.strikeout());
    assert!(!cell(&p, 0, 3)?.strikeout());
    assert_eq!(
        cell(&p, 0, 3)?.underline_color(),
        Color::Rgb([1, 2, 3].into())
    );
    assert_eq!(cell(&p, 0, 4)?.underline_color(), Color::Idx(9));
    assert_eq!(cell(&p, 0, 5)?.underline_color(), Color::Default);
    assert!(cell(&p, 0, 6)?.hidden() && cell(&p, 0, 6)?.strikeout());
    assert_eq!(
        cell(&p, 0, 6)?.underline_color(),
        Color::Rgb([4, 5, 6].into())
    );
    assert!(
        !cell(&p, 0, 7)?.hidden() && !cell(&p, 0, 7)?.strikeout(),
        "SGR 0 resets"
    );
    assert_eq!(cell(&p, 0, 7)?.underline_color(), Color::Default);
    assert_eq!(p.screen().attributes(), fux_vt::Attributes::default());
    Ok(())
}

// Cursor sequences beyond the baseline.

#[test]
fn hvp_positions_the_cursor_like_cup() -> Result {
    let mut p = Parser::new(Size::new(5, 10)?, 0)?;
    p.process(b"\x1b[3;4fX")?;
    assert_eq!(cell(&p, 2, 3)?.contents(), "X");
    assert_eq!(p.screen().cursor_position(), (2, 4));
    p.process(b"\x1b[fY")?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "Y");
    // Origin mode applies to HVP exactly as it does to CUP.
    p.process(b"\x1b[2;4r\x1b[?6h\x1b[1;1fZ")?;
    assert_eq!(cell(&p, 1, 0)?.contents(), "Z");
    Ok(())
}

#[test]
fn scosc_and_scorc_save_and_restore_position_and_attributes() -> Result {
    let mut p = Parser::new(Size::new(5, 20)?, 0)?;
    p.process(b"\x1b[2;3H\x1b[s\x1b[1;38;2;1;2;3mtext\x1b[4;10Hmore\x1b[u")?;
    assert_eq!(p.screen().cursor_position(), (1, 2));
    // Like DECRC, SCORC restores the attributes SCOSC saved: an image
    // placeholder row wrapped in `CSI s` ... `CSI u` leaves the pen as it was.
    assert!(!p.screen().attributes().bold());
    assert_eq!(p.screen().attributes().foreground(), Color::Default);
    p.process(b"X")?;
    assert_eq!(cell(&p, 1, 2)?.contents(), "X");
    Ok(())
}

// Grapheme clusters.

#[test]
fn grapheme_clusters_share_one_cell_and_take_their_string_width() -> Result {
    // Emoji presentation, keycap, ZWJ sequences, flag: one wide cell each.
    for cluster in [
        "\u{2764}\u{fe0f}",
        "1\u{fe0f}\u{20e3}",
        "\u{1f469}\u{200d}\u{1f52c}",
        "\u{1f1ef}\u{1f1f5}",
        "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
        // A family: 25 bytes, exactly a cell's capacity.
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
    ] {
        let mut p = Parser::new(Size::new(2, 10)?, 0)?;
        p.process(format!("{cluster}x").as_bytes())?;
        let first = cell(&p, 0, 0)?;
        assert_eq!(first.contents(), cluster);
        assert!(first.is_wide(), "{cluster:?} must be wide");
        assert!(cell(&p, 0, 1)?.is_wide_continuation());
        assert_eq!(cell(&p, 0, 2)?.contents(), "x", "{cluster:?}");
        assert_eq!(p.screen().cursor_position(), (0, 3));
    }
    // A spacing vowel sign joins its consonant, two columns wide.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("\u{928}\u{93f}x".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "\u{928}\u{93f}");
    assert!(cell(&p, 0, 0)?.is_wide());
    assert_eq!(cell(&p, 0, 2)?.contents(), "x");
    // A non-spacing mark still yields a narrow cell.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("e\u{301}x".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "e\u{301}");
    assert!(!cell(&p, 0, 0)?.is_wide());
    assert_eq!(cell(&p, 0, 1)?.contents(), "x");
    Ok(())
}

#[test]
fn a_cluster_joins_across_process_calls_but_not_across_cursor_moves() -> Result {
    let mut whole = Parser::new(Size::new(2, 10)?, 0)?;
    whole.process("\u{1f1ef}\u{1f1f5}".as_bytes())?;
    let mut split = Parser::new(Size::new(2, 10)?, 0)?;
    for byte in "\u{1f1ef}\u{1f1f5}".as_bytes() {
        split.process(&[*byte])?;
    }
    assert_eq!(cell(&whole, 0, 0)?, cell(&split, 0, 0)?);
    assert_eq!(cell(&split, 0, 0)?.contents(), "\u{1f1ef}\u{1f1f5}");

    // A cursor move between the two keeps them apart.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("\u{928}\x1b[2G\u{93f}".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "\u{928}");
    assert!(!cell(&p, 0, 0)?.is_wide());
    assert_eq!(cell(&p, 0, 1)?.contents(), "\u{93f}");
    // SGR is not a move: the flag still joins.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("\u{1f1ef}\x1b[1m\u{1f1f5}".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "\u{1f1ef}\u{1f1f5}");
    // Unrelated characters never join, nor anything after a Prepend.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("ab\u{600}c".as_bytes())?;
    assert_eq!(cell(&p, 0, 1)?.contents(), "b");
    assert_eq!(cell(&p, 0, 3)?.contents(), "c");
    Ok(())
}

#[test]
fn widening_needs_room_and_clears_what_it_covers() -> Result {
    // In the last column the cell stays narrow and nothing wraps.
    let mut p = Parser::new(Size::new(2, 3)?, 0)?;
    p.process("ab\u{2764}\u{fe0f}".as_bytes())?;
    assert_eq!(cell(&p, 0, 2)?.contents(), "\u{2764}\u{fe0f}");
    assert!(!cell(&p, 0, 2)?.is_wide());
    assert_eq!(p.screen().cursor_position(), (0, 2));
    assert!(p.screen().pending_wrap());
    // Widening over the first half of a wide glyph blanks its second half.
    let mut p = Parser::new(Size::new(2, 6)?, 0)?;
    p.process("a\u{4f60}\x1b[1G\u{2764}".as_bytes())?;
    p.process("\u{fe0f}".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), "\u{2764}\u{fe0f}");
    assert!(cell(&p, 0, 1)?.is_wide_continuation());
    assert!(!cell(&p, 0, 2)?.is_wide_continuation() && !cell(&p, 0, 2)?.has_contents());
    // Overwriting a clustered wide cell clears its second half.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("\u{2764}\u{fe0f}\r  ".as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), " ");
    assert!(!cell(&p, 0, 1)?.is_wide_continuation());
    Ok(())
}

/// Clusters longer than a cell holds inline are kept whole, in the row's
/// text, one cell each: the cursor lands where unicode-width lays them out.
#[test]
fn long_clusters_keep_one_cell_and_all_their_text() -> Result {
    for cluster in [
        // A family, 25 bytes; with skin tones, 41; a kiss with skin tones,
        // 35; a subdivision flag, 28.
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        "\u{1F468}\u{1F3FB}\u{200D}\u{1F469}\u{1F3FB}\u{200D}\u{1F467}\u{1F3FB}\u{200D}\u{1F466}\u{1F3FB}",
        "\u{1F469}\u{1F3FD}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F468}\u{1F3FB}",
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
    ] {
        let mut whole = Parser::new(Size::new(2, 10)?, 0)?;
        whole.process(format!("{cluster}|").as_bytes())?;
        // In pieces, a byte at a time, the same.
        let mut split = Parser::new(Size::new(2, 10)?, 0)?;
        for byte in format!("{cluster}|").as_bytes() {
            split.process(std::slice::from_ref(byte))?;
        }
        for p in [&whole, &split] {
            assert_eq!(cell(p, 0, 0)?.contents(), cluster);
            assert!(cell(p, 0, 0)?.is_wide() && cell(p, 0, 1)?.is_wide_continuation());
            assert_eq!(cell(p, 0, 2)?.contents(), "|", "{cluster:?}");
            assert_eq!(p.screen().cursor_position(), (0, 3));
        }
    }
    // Zalgo: 'e' and 15 marks, 31 bytes, in one narrow cell.
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    let zalgo: String = std::iter::once("e")
        .chain(std::iter::repeat_n("\u{301}\u{316}\u{330}", 5))
        .collect();
    p.process(format!("{zalgo}|").as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents(), zalgo);
    assert_eq!(cell(&p, 0, 1)?.contents(), "|");
    Ok(())
}

#[test]
fn a_full_cluster_drops_what_follows_and_is_never_split() -> Result {
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    // 'e' and 63 two-byte marks fill 127 of 128 bytes; the rest is dropped.
    let marks: String = std::iter::repeat_n('\u{301}', 100).collect();
    p.process(format!("e{marks}x").as_bytes())?;
    assert_eq!(cell(&p, 0, 0)?.contents().len(), CLUSTER_CAPACITY - 1);
    assert_eq!(cell(&p, 0, 1)?.contents(), "x");
    // A ZWJ sequence past the capacity still ends in its one cell: the
    // pictographs after it are dropped, not given cells of their own.
    let mut p = Parser::new(Size::new(2, 20)?, 0)?;
    let long: String = std::iter::once("\u{1F468}")
        .chain(std::iter::repeat_n("\u{200D}\u{1F468}", 30))
        .collect();
    p.process(format!("{long}|").as_bytes())?;
    assert!(cell(&p, 0, 0)?.is_wide());
    assert!(cell(&p, 0, 0)?.contents().len() <= CLUSTER_CAPACITY);
    assert!(long.starts_with(cell(&p, 0, 0)?.contents()));
    assert_eq!(cell(&p, 0, 2)?.contents(), "|");
    assert_eq!(p.screen().cursor_position(), (0, 3));
    Ok(())
}

// Unhandled sequences.

#[test]
fn sequences_fux_vt_does_not_implement_reach_the_sink() -> Result {
    let mut p = Parser::new(Size::new(5, 20)?, 0)?;
    let record = run(
        &mut p,
        b"\x1b[3J\x1b[>1;2m\x1b[12h\x1b[?1u\x1b*B\x1bn\x1b[2;3:4^\x1b[H\x1b[?25l\x1b[1m\x1b7",
    )?;
    assert_eq!(
        record.unhandled,
        [
            "CSI 3J",
            "CSI >1;2m",
            // SRM: of the ANSI modes, IRM and LNM alone are kept.
            "CSI 12h",
            "CSI ?1u",
            // G2 is not kept: only G0 and G1 are designated.
            "ESC *B",
            "ESC n",
            "CSI 2;3:4^"
        ]
    );
    assert!(record.replies.is_empty());
    // Unknown private modes are consumed quietly; queries still answer.
    let record = run(&mut p, b"\x1b[?12345h\x1b[5n")?;
    assert!(record.unhandled.is_empty());
    assert_eq!(record.replies, ["\x1b[0n"]);
    Ok(())
}

// Identity.

#[test]
fn an_identity_answers_device_attributes_and_version_queries() -> Result {
    let options = Options::new().with_identity(Some(RATTY));
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, options)?;
    let record = run(&mut p, b"\x1b[c\x1b[>c\x1b[>0q\x1b[>q\x1b[5n")?;
    assert_eq!(
        record.replies,
        [
            "\x1b[?62;22c",
            "\x1b[>1;500;0c",
            "\x1bP>|ratty 0.5.0\x1b\\",
            "\x1bP>|ratty 0.5.0\x1b\\",
            "\x1b[0n"
        ]
    );
    // A cursor waiting to wrap is reported at the last column, as xterm does.
    let record = run(&mut p, b"\x1b[H01234567890123456789\x1b[6n")?;
    assert_eq!(record.replies, ["\x1b[1;20R"]);

    // Without one, fux-vt's own answers, and XTVERSION goes unanswered.
    let mut p = Parser::new(Size::new(5, 20)?, 0)?;
    let record = run(&mut p, b"\x1b[c\x1b[>c\x1b[>q01234567890123456789\x1b[6n")?;
    assert_eq!(record.replies, ["\x1b[?1;2c", "\x1b[1;21R"]);
    // A missing parameter is reported as 0.
    assert_eq!(record.unhandled, ["CSI >0c", "CSI >0q"]);
    Ok(())
}

#[test]
fn identity_versions_encode_like_xterm_and_long_names_go_unanswered() -> Result {
    for (version, encoded) in [
        ("1.2.3", 10203),
        ("0.16.0-rc.1", 1600),
        ("7", 7),
        ("x.y", 0),
    ] {
        let options = Options::new().with_identity(Some(Identity { name: "t", version }));
        let mut p = Parser::with_options(Size::new(2, 2)?, 0, options)?;
        let record = run(&mut p, b"\x1b[>c")?;
        assert_eq!(
            record.replies,
            [format!("\x1b[>1;{encoded};0c")],
            "{version}"
        );
    }
    let long: &'static str = "a-terminal-name-that-is-far-too-long-for-a-reply";
    let options = Options::new().with_identity(Some(Identity {
        name: long,
        version: "1.0.0",
    }));
    let mut p = Parser::with_options(Size::new(2, 2)?, 0, options)?;
    let record = run(&mut p, b"\x1b[>q")?;
    assert!(record.replies.is_empty());
    assert_eq!(record.unhandled, ["CSI >0q"]);
    Ok(())
}

// Kitty keyboard protocol and modifyOtherKeys.

#[test]
fn kitty_keyboard_flags_push_set_pop_and_answer_queries() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    let record = run(
        &mut p,
        b"\x1b[?u\x1b[>5u\x1b[?u\x1b[=2;2u\x1b[?u\x1b[=4;3u\x1b[?u\x1b[=1u\x1b[?u\x1b[<u\x1b[?u",
    )?;
    assert_eq!(
        record.replies,
        [
            "\x1b[?0u", "\x1b[?5u", "\x1b[?7u", "\x1b[?3u", "\x1b[?1u", "\x1b[?0u"
        ]
    );
    assert!(record.unhandled.is_empty());
    // Popping more than there are empties the stack; setting on an empty
    // stack gives it one entry.
    p.process(b"\x1b[>1u\x1b[>2u\x1b[<9u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    p.process(b"\x1b[=3u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 3);
    // Flags too large for a byte saturate.
    p.process(b"\x1b[>999u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 255);
    Ok(())
}

#[test]
fn kitty_keyboard_stacks_are_per_screen_bounded_and_reset() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    p.process(b"\x1b[>1u\x1b[?1049h")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    p.process(b"\x1b[>8u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 8);
    // Leaving without popping leaves the primary screen's flags alone.
    p.process(b"\x1b[?1049l")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 1);
    // A full stack drops its oldest: 33 pushes, 32 pops, then empty.
    for n in 1..=33u8 {
        p.process(format!("\x1b[>{n}u").as_bytes())?;
    }
    p.process(b"\x1b[<31u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 2);
    p.process(b"\x1b[<u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    p.process(b"\x1b[>4u\x1b[>4;2m\x1bc")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    assert_eq!(p.screen().modify_other_keys(), None);
    Ok(())
}

#[test]
fn modify_other_keys_levels_survive_split_sequences() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    assert_eq!(p.screen().modify_other_keys(), None);
    p.process(b"\x1b[>4;2m")?;
    assert_eq!(p.screen().modify_other_keys(), Some(2));
    p.process(b"\x1b[>4;0m")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    for byte in b"\x1b[>4;1m" {
        p.process(&[*byte])?;
    }
    assert_eq!(p.screen().modify_other_keys(), Some(1));
    p.process(b"\x1b[>4m")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    // Other resources are not modifyOtherKeys, and not SGR either.
    let record = run(&mut p, b"\x1b[>1;2mx")?;
    assert_eq!(record.unhandled, ["CSI >1;2m"]);
    assert_eq!(p.screen().attributes(), fux_vt::Attributes::default());
    Ok(())
}

#[test]
fn without_the_option_keyboard_requests_are_ignored_and_unanswered() -> Result {
    let mut p = Parser::new(Size::new(5, 20)?, 0)?;
    let record = run(&mut p, b"\x1b[>5u\x1b[=3u\x1b[?u\x1b[>4;2m")?;
    assert!(record.replies.is_empty());
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    assert_eq!(p.screen().modify_other_keys(), None);
    assert_eq!(
        record.unhandled,
        ["CSI >5u", "CSI =3u", "CSI ?0u", "CSI >4;2m"]
    );
    Ok(())
}

// Reflow.

#[test]
fn narrowing_reflows_a_long_line_and_widening_joins_it() -> Result {
    let mut p = Parser::with_options(Size::new(4, 20)?, 100, REFLOW)?;
    p.process(b"0123456789abcdefghij\r\nnext")?;
    p.resize(Size::new(4, 8)?)?;
    assert_eq!(lines(&p), ["01234567", "89abcdef", "ghij", "next"]);
    assert!(p.screen().row_wrapped(0) && p.screen().row_wrapped(1));
    assert!(!p.screen().row_wrapped(2));
    assert_eq!(p.screen().cursor_position(), (3, 4));
    p.resize(Size::new(4, 30)?)?;
    assert_eq!(lines(&p), ["0123456789abcdefghij", "next", "", ""]);
    assert_eq!(p.screen().cursor_position(), (1, 4));
    // What the program prints next continues the reflowed line.
    p.process(b"!")?;
    assert_eq!(lines(&p).get(1).map(String::as_str), Some("next!"));
    Ok(())
}

#[test]
fn reflow_keeps_the_cursor_on_its_character() -> Result {
    let mut p = Parser::with_options(Size::new(3, 10)?, 100, REFLOW)?;
    p.process(b"abcdefghij\x1b[1;7H")?;
    assert_eq!(p.screen().cursor_position(), (0, 6));
    p.resize(Size::new(3, 4)?)?;
    // "abcd" / "efgh" / "ij": 'g' is row 1, column 2.
    assert_eq!(p.screen().cursor_position(), (1, 2));
    assert_eq!(cell(&p, 1, 2)?.contents(), "g");
    Ok(())
}

/// The saved cursor moves with its character as the cursor does, so a
/// program that saved it (vim's 1049) finds it where it left it: after
/// the prompt, not inside the wrapped text above (`fux-vt-compare cases
/// reflow-moves-the-saved-cursor`: wezterm and xterm.js agree; xterm
/// does not reflow).
#[test]
fn reflow_moves_the_saved_cursor_with_its_character() -> Result {
    let mut p = Parser::with_options(Size::new(8, 20)?, 10000, REFLOW)?;
    p.process(b"aaaaaaaaaaaaaaaaaa\r\nbbbbbbbbbbbbbbbbbb\r\n$ \x1b[?1049h")?;
    p.resize(Size::new(8, 10)?)?;
    p.process(b"\x1b[?1049l")?;
    assert_eq!(p.screen().cursor_position(), (4, 2));
    assert_eq!(lines(&p).get(4).map(String::as_str), Some("$"));
    // So does DECSC's, and one waiting to wrap still waits.
    let mut p = Parser::with_options(Size::new(5, 10)?, 100, REFLOW)?;
    p.process(b"abcdefghij\x1b[1;7H\x1b7\x1b[3;1H")?;
    p.resize(Size::new(5, 4)?)?;
    p.process(b"\x1b8")?;
    assert_eq!(p.screen().cursor_position(), (1, 2));
    assert_eq!(cell(&p, 1, 2)?.contents(), "g");
    let mut p = Parser::with_options(Size::new(3, 10)?, 100, REFLOW)?;
    p.process(b"abcd\x1b7\r\n")?;
    p.resize(Size::new(3, 4)?)?;
    p.process(b"\x1b8X")?;
    assert_eq!(lines(&p), ["abcd", "X", ""]);
    Ok(())
}

/// DL and IL move rows across the region's top: the row above it, which
/// went on into the row that moved, no longer does, so reflow joins it to
/// nothing that follows.
#[test]
fn deleting_or_inserting_lines_ends_the_wrap_above() -> Result {
    let mut p = Parser::with_options(Size::new(4, 6)?, 100, REFLOW)?;
    p.process(b"abcdefgh\r\nxyz\x1b[2;1H\x1b[M")?;
    assert!(!p.screen().row_wrapped(0));
    p.resize(Size::new(4, 12)?)?;
    assert_eq!(lines(&p), ["abcdef", "xyz", "", ""]);
    let mut p = Parser::with_options(Size::new(4, 6)?, 100, REFLOW)?;
    p.process(b"abcdefgh\x1b[2;1H\x1b[L")?;
    assert!(!p.screen().row_wrapped(0));
    Ok(())
}

#[test]
fn reflow_pushes_overflow_into_history_and_pulls_it_back() -> Result {
    let mut p = Parser::with_options(Size::new(3, 12)?, 100, REFLOW)?;
    p.process(b"aaaaaaaaaaaa\r\nbb\r\ncc")?;
    p.resize(Size::new(3, 6)?)?;
    assert_eq!(lines(&p), ["aaaaaa", "bb", "cc"]);
    assert_eq!(window_lines(&p, 1), ["aaaaaa", "aaaaaa", "bb"]);
    p.resize(Size::new(3, 12)?)?;
    assert_eq!(lines(&p), ["aaaaaaaaaaaa", "bb", "cc"]);
    assert_eq!(p.screen().history_len(), 0);
    assert_eq!(p.screen().cursor_position(), (2, 2));
    Ok(())
}

#[test]
fn reflowed_heights_scroll_into_history_and_drop_blank_rows_first() -> Result {
    let mut p = Parser::with_options(Size::new(4, 10)?, 100, REFLOW)?;
    p.process(b"one\r\ntwo\r\nthree\r\nfour")?;
    p.resize(Size::new(2, 10)?)?;
    assert_eq!(lines(&p), ["three", "four"]);
    assert_eq!(p.screen().cursor_position(), (1, 4));
    p.resize(Size::new(5, 10)?)?;
    assert_eq!(lines(&p), ["one", "two", "three", "four", ""]);
    assert_eq!(p.screen().cursor_position(), (3, 4));

    let mut p = Parser::with_options(Size::new(6, 10)?, 100, REFLOW)?;
    p.process(b"top\x1b[2;1Hmid")?;
    p.resize(Size::new(3, 10)?)?;
    assert_eq!(lines(&p), ["top", "mid", ""]);
    assert_eq!(p.screen().cursor_position(), (1, 3));
    assert_eq!(p.screen().history_len(), 0);
    Ok(())
}

/// Rows in history stay there on a resize: the blank rows below the cursor
/// are dropped only as far as the screen's own lines are past the new
/// height, so none is given up to bring a history row back above the
/// cursor. A shrink after a line scrolled into history and the screen was
/// cleared (ED 2) left the cursor on row 1, under a history row brought
/// back.
#[test]
fn a_shrink_takes_no_history_back_onto_the_screen() -> Result {
    let mut p = Parser::with_options(Size::new(4, 10)?, 100, REFLOW)?;
    p.process(b"old\r\none\r\ntwo\r\nthree\r\nfour\x1b[H\x1b[2J")?;
    assert_eq!(p.screen().history_len(), 1);
    p.resize(Size::new(2, 10)?)?;
    assert_eq!(lines(&p), ["", ""]);
    assert_eq!(p.screen().cursor_position(), (0, 0));
    assert_eq!(p.screen().history_len(), 1);

    let mut p = Parser::with_options(Size::new(6, 10)?, 100, REFLOW)?;
    p.process(b"old\r\n\r\n\r\n\r\n\r\n\r\n\x1b[H\x1b[2Jtop\r\nmid")?;
    assert_eq!(p.screen().history_len(), 1);
    p.resize(Size::new(3, 10)?)?;
    assert_eq!(lines(&p), ["top", "mid", ""]);
    assert_eq!(p.screen().cursor_position(), (1, 3));
    assert_eq!(window_lines(&p, 1), ["old", "top", "mid"]);

    // Growing, the blank rows below the cursor are not given up to bring
    // more history back above it: the rows land where a resize without
    // reflow puts them.
    for options in [REFLOW, Options::new()] {
        let mut p = Parser::with_options(Size::new(2, 1)?, 3, options)?;
        p.process(b"b9m\n\n\x1b[?1049l")?;
        p.resize(Size::new(4, 1)?)?;
        assert_eq!(lines(&p), ["9", "m", "", ""]);
        assert_eq!(p.screen().cursor_position(), (2, 0));
        assert_eq!(p.screen().history_len(), 1);
    }
    Ok(())
}

/// One column cannot show a wide glyph: reflowed to it, the line keeps
/// its narrow characters alone, and the cursor stays after them.
#[test]
fn reflow_to_one_column_leaves_wide_glyphs_out() -> Result {
    let mut p = Parser::with_options(Size::new(4, 6)?, 10, REFLOW)?;
    p.process("a\u{754c}b\u{754c}".as_bytes())?;
    p.resize(Size::new(4, 1)?)?;
    assert_eq!(lines(&p), ["a", "b", "", ""]);
    assert_eq!(p.screen().cursor_position(), (1, 0));
    Ok(())
}

#[test]
fn reflow_resets_the_scroll_region_and_keeps_the_history_limit() -> Result {
    let mut p = Parser::with_options(Size::new(10, 20)?, 0, REFLOW)?;
    p.process(b"\x1b[2;5r")?;
    p.resize(Size::new(10, 21)?)?;
    assert_eq!(p.screen().scroll_region(), (0, 9));
    p.process(b"\x1b[10;1Hlast\r\nafter")?;
    assert_eq!(lines(&p).get(8).map(String::as_str), Some("last"));
    assert_eq!(lines(&p).get(9).map(String::as_str), Some("after"));

    let mut p = Parser::with_options(Size::new(2, 10)?, 3, REFLOW)?;
    p.process(b"0123456789\r\n0123456789\r\nabcdefghij\r\nend")?;
    p.resize(Size::new(2, 2)?)?;
    // Seventeen rows: the last two on screen and three of history, the
    // rest dropped oldest first.
    assert_eq!(lines(&p), ["en", "d"]);
    assert_eq!(p.screen().history_len(), 3);
    assert_eq!(window_lines(&p, 3), ["ef", "gh"]);
    Ok(())
}

#[test]
fn reflow_does_not_split_wide_glyphs() -> Result {
    let mut p = Parser::with_options(Size::new(2, 10)?, 100, REFLOW)?;
    p.process("ab\u{4f60}\u{597d}cd".as_bytes())?;
    p.resize(Size::new(4, 3)?)?;
    // "ab" and a blank, "你", "好c", "d".
    assert_eq!(lines(&p), ["ab", "\u{4f60}", "\u{597d}c", "d"]);
    assert!(cell(&p, 1, 0)?.is_wide());
    assert!(cell(&p, 1, 1)?.is_wide_continuation());
    assert_eq!(p.screen().cursor_position(), (3, 1));
    Ok(())
}

#[test]
fn the_alternate_screen_resizes_without_reflow() -> Result {
    let mut p = Parser::with_options(Size::new(3, 10)?, 100, REFLOW)?;
    // 1049 keeps the cursor where it was, as xterm does: home it.
    p.process(b"main line\x1b[?1049h\x1b[H0123456789")?;
    p.resize(Size::new(3, 5)?)?;
    assert_eq!(lines(&p), ["01234", "", ""]);
    p.process(b"\x1b[?1049l")?;
    assert_eq!(lines(&p), ["main", "line", ""]);
    Ok(())
}

#[test]
fn reflowed_rows_keep_their_lines_identities() -> Result {
    let mut p = Parser::with_options(Size::new(3, 10)?, 100, REFLOW)?;
    p.process(b"first\r\nsecond")?;
    let first = p.screen().rows().nth_back(2).map(|r| r.id());
    let second = p.screen().rows().nth_back(1).map(|r| r.id());
    let mark = p.screen().mark();
    p.resize(Size::new(3, 3)?)?;
    // "fir" keeps the first line's identity, "sec" the second's.
    assert_eq!(lines(&p), ["st", "sec", "ond"]);
    assert_eq!(window_lines(&p, 1).first().map(String::as_str), Some("fir"));
    assert_eq!(p.screen().rows().nth_back(3).map(|r| r.id()), first);
    assert_eq!(p.screen().rows().nth_back(1).map(|r| r.id()), second);
    assert!(p.screen().full_refresh_since(mark));
    Ok(())
}

#[test]
fn without_the_option_resizing_does_not_reflow() -> Result {
    let mut p = Parser::new(Size::new(4, 20)?, 100)?;
    p.process(b"0123456789abcdefghij\r\nnext")?;
    p.resize(Size::new(4, 8)?)?;
    assert_eq!(lines(&p), ["01234567", "next", "", ""]);
    Ok(())
}

/// A string ended by ST (`ESC \`) is a sequence fux-vt implements: the
/// `ESC \` that ends it reaches no `Sink::unhandled`, whatever string it
/// ends (an OSC, a DCS, an APC, a PM, an SOS), as with BEL; and a lone
/// `ESC \` is no unhandled sequence either.
#[test]
fn a_string_ended_by_st_leaves_nothing_unhandled() -> Result {
    for input in [
        &b"\x1b]2;title\x1b\\"[..],
        b"\x1b]8;;https://example.com\x1b\\",
        b"\x1bP$qm\x1b\\",
        b"\x1b_apc\x1b\\",
        b"\x1b^pm\x1b\\",
        b"\x1bXsos\x1b\\",
        b"\x1b\\",
    ] {
        let mut p = Parser::new(Size::new(2, 10)?, 0)?;
        let record = run(&mut p, input)?;
        assert!(
            record.unhandled.is_empty(),
            "{}: {:?}",
            String::from_utf8_lossy(input),
            record.unhandled
        );
    }
    Ok(())
}
