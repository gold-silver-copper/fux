//! Behaviour beyond the vt100 baseline: blink, hidden, strikeout and
//! underline colour; HVP and SCOSC/SCORC; grapheme clusters; sequences
//! reported as unhandled; and the opt-in kitty keyboard protocol, identity
//! replies and reflow on resize.

use fux_vt::keys::encode::OtherKeys;
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
const PICTURES: Options = Options::new().with(Feature::Pictures);
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
    // Unrelated characters never join; a Prepend joins what follows it
    // (UAX #29, GB9b).
    let mut p = Parser::new(Size::new(2, 10)?, 0)?;
    p.process("ab\u{600}c".as_bytes())?;
    assert_eq!(cell(&p, 0, 1)?.contents(), "b");
    assert_eq!(cell(&p, 0, 2)?.contents(), "\u{600}c");
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
        b"\x1b[>1;2m\x1b[12h\x1b[?1u\x1b-A\x1b~\x1b[2;3:4^\x1b[H\x1b[?25l\x1b[1m\x1b7",
    )?;
    assert_eq!(
        record.unhandled,
        [
            "CSI >1;2m",
            // SRM: of the ANSI modes, IRM and LNM alone are kept.
            "CSI 12h",
            "CSI ?1u",
            // The 96-character sets are not designated, nor shifted into
            // GR, which UTF-8 leaves no room for.
            "ESC -A",
            "ESC ~",
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
            "\x1b[?62;6;21;22;28c",
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
    assert_eq!(record.replies, ["\x1b[?1;2c", "\x1b[1;20R"]);
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
    // Only the spec's five flags are kept ("The progressive enhancement
    // flags": 1 to 16); other bits mean nothing.
    p.process(b"\x1b[>999u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 7);
    p.process(b"\x1b[>32u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    p.process(b"\x1b[=63u")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 31);
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

/// Each screen's stack stays with it while its cells are cleared or
/// resized: 1049 clears the alternate screen each time it is shown, and
/// 1047 once it is shown again. RIS empties both.
#[test]
fn kitty_keyboard_stacks_outlive_clears_and_resizes() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    p.process(b"\x1b[>3u\x1b[?1049h\x1b[>6u\x1b[?1049l\x1b[?1049h")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 6);
    p.process(b"\x1b[?1049l\x1b[?1047h\x1b[?1047l\x1b[?1047h")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 6);
    p.resize(Size::new(7, 30)?)?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 6);
    p.process(b"\x1b[?1047l")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 3);
    p.process(b"\x1bc\x1b[?1049h")?;
    assert_eq!(p.screen().kitty_keyboard_flags(), 0);
    Ok(())
}

/// XTQMODKEYS (ctlseqs, `CSI ? Pp m`): "XTerm's response ... is formatted
/// as an XTMODKEYS control, i.e., CSI > Pp m", as xterm answers
/// modifyOtherKeys (4), its value 0 when off. fux-vt keeps no other
/// resource, and answers none of them.
#[test]
fn xtqmodkeys_answers_the_modify_other_keys_level() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    let asked =
        |p: &mut Parser, input: &[u8]| -> Result<Vec<String>> { Ok(run(p, input)?.replies) };
    assert_eq!(asked(&mut p, b"\x1b[?4m")?, ["\x1b[>4;0m"]);
    assert_eq!(asked(&mut p, b"\x1b[>4;2m\x1b[?4m")?, ["\x1b[>4;2m"]);
    assert_eq!(asked(&mut p, b"\x1b[>4;3m\x1b[?4m")?, ["\x1b[>4;3m"]);
    assert!(asked(&mut p, b"\x1b[?1m\x1b[?6m")?.is_empty());
    let mut off = Parser::new(Size::new(5, 20)?, 0)?;
    assert!(asked(&mut off, b"\x1b[?4m")?.is_empty());
    Ok(())
}

#[test]
fn modify_other_keys_levels_survive_split_sequences() -> Result {
    let mut p = Parser::with_options(Size::new(5, 20)?, 0, KEYBOARD)?;
    assert_eq!(p.screen().modify_other_keys(), None);
    p.process(b"\x1b[>4;2m")?;
    assert_eq!(p.screen().modify_other_keys(), Some(OtherKeys::All));
    p.process(b"\x1b[>4;0m")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    for byte in b"\x1b[>4;1m" {
        p.process(&[*byte])?;
    }
    assert_eq!(p.screen().modify_other_keys(), Some(OtherKeys::Usual));
    p.process(b"\x1b[>4m")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    // No parameters reset every resource (ctlseqs, XTMODKEYS: "If no
    // parameters are given, all resources are reset to their initial
    // values"), modifyOtherKeys among them; 0 alone is modifyKeyboard
    // ("If Pp is given but Pv is omitted, that resource is reset"), which
    // leaves modifyOtherKeys, byte by byte too.
    p.process(b"\x1b[>4;2m")?;
    let record = run(&mut p, b"\x1b[>m")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    assert!(record.unhandled.is_empty());
    p.process(b"\x1b[>4;2m\x1b[>0m")?;
    for byte in b"\x1b[>0m" {
        p.process(&[*byte])?;
    }
    assert_eq!(p.screen().modify_other_keys(), Some(OtherKeys::All));
    for byte in b"\x1b[>m" {
        p.process(&[*byte])?;
    }
    assert_eq!(p.screen().modify_other_keys(), None);
    // `CSI > 4 n` disables it (ctlseqs: "Disable key modifier options",
    // the resource value -1), until set again; other resources are not
    // modifyOtherKeys, and `CSI > n` disables modifyFunctionKeys.
    p.process(b"\x1b[>4;2m\x1b[>4n")?;
    assert_eq!(p.screen().modify_other_keys(), None);
    p.process(b"\x1b[>4;1m")?;
    assert_eq!(p.screen().modify_other_keys(), Some(OtherKeys::Usual));
    let record = run(&mut p, b"\x1b[>n\x1b[>2n")?;
    assert_eq!(p.screen().modify_other_keys(), Some(OtherKeys::Usual));
    assert_eq!(record.unhandled, ["CSI >0n", "CSI >2n"]);
    p.process(b"\x1b[>4n")?;
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

/// A cursor in the blanks past a line's text stays on the line's last
/// row, in its last column at most, as resizing without reflow keeps it:
/// it never waits to wrap, so the next glyph leaves no empty soft-wrapped
/// row behind.
#[test]
fn reflow_keeps_a_cursor_past_the_text_on_its_row() -> Result {
    let mut p = Parser::with_options(Size::new(3, 10)?, 100, REFLOW)?;
    p.process(b"\x1b[9C")?;
    p.resize(Size::new(3, 5)?)?;
    assert_eq!(p.screen().cursor_position(), (0, 4));
    assert!(!p.screen().pending_wrap());
    p.process(b"X")?;
    assert_eq!(lines(&p), ["    X", "", ""]);
    assert!(!p.screen().row_wrapped(0));
    Ok(())
}

/// DL and IL move rows across the region's top, and the row above it
/// keeps its soft wrap, as xterm keeps it (`fux-vt-compare replay
/// --engines xterm --size 2x1 '\e[999Cab\e[L'`; soft wrap is xterm's own
/// notion): reflow then joins it to the row that moved up, as Ghostty,
/// alacritty and wezterm join them (`replay --engines all --size 4x6
/// 'abcdefgh\r\nxyz\e[2;1H\e[M' resize:4x12`).
#[test]
fn deleting_or_inserting_lines_keeps_the_wrap_above() -> Result {
    let mut p = Parser::with_options(Size::new(4, 6)?, 100, REFLOW)?;
    p.process(b"abcdefgh\r\nxyz\x1b[2;1H\x1b[M")?;
    assert!(p.screen().row_wrapped(0));
    p.resize(Size::new(4, 12)?)?;
    assert_eq!(lines(&p), ["abcdefxyz", "", "", ""]);
    let mut p = Parser::with_options(Size::new(4, 6)?, 100, REFLOW)?;
    p.process(b"abcdefgh\x1b[2;1H\x1b[L")?;
    assert!(p.screen().row_wrapped(0));
    // Scrolling down, the row moved to the bottom margin keeps its own,
    // as xterm keeps it (`--size 6x1 '~!@#\e[3;5r\e[10H\e[2T'`).
    let mut p = Parser::new(Size::new(6, 1)?, 0)?;
    p.process(b"~!@#\x1b[3;5r\x1b[10H\x1b[2T")?;
    let wrapped: Vec<bool> = (0..6).map(|y| p.screen().row_wrapped(y)).collect();
    assert_eq!(wrapped, [true, true, false, false, true, false]);
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
        let mut p = Parser::with_options(Size::new(2, 1)?, 1 << 10, options)?;
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
fn reflow_resets_the_scroll_region_and_keeps_the_history_budget() -> Result {
    let mut p = Parser::with_options(Size::new(10, 20)?, 0, REFLOW)?;
    p.process(b"\x1b[2;5r")?;
    p.resize(Size::new(10, 21)?)?;
    assert_eq!(p.screen().scroll_region(), (0, 9));
    p.process(b"\x1b[10;1Hlast\r\nafter")?;
    assert_eq!(lines(&p).get(8).map(String::as_str), Some("last"));
    assert_eq!(lines(&p).get(9).map(String::as_str), Some("after"));

    // Three rows of two cells: 24 bytes a row and 8 a cell.
    let mut p = Parser::with_options(Size::new(2, 10)?, 3 * 40, REFLOW)?;
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

/// The history budget changes on a live parser: lowered, the oldest rows
/// go at once and readers read every row again; raised, rows gather again.
/// A row of two cells holds 40 bytes: its 24-byte record and its cells.
#[test]
fn the_history_budget_changes_on_a_live_parser() -> Result {
    let mut p = Parser::new(Size::new(2, 10)?, 1 << 20)?;
    for n in 10..60 {
        p.process(format!("{n}\r\n").as_bytes())?;
    }
    assert_eq!(p.screen().history_len(), 49);
    let mark = p.screen().mark();
    p.set_history_bytes(3 * 40);
    assert!(p.screen().full_refresh_since(mark));
    assert_eq!(window_lines(&p, 3), ["56", "57"], "the newest rows stay");
    assert_eq!(p.screen().history_len(), 3);
    p.process(b"x\r\ny\r\n")?;
    assert_eq!(p.screen().history_len(), 3, "the budget holds");
    p.set_history_bytes(0);
    assert_eq!(p.screen().history_len(), 0);
    p.set_history_bytes(1 << 10);
    p.process(b"a\r\nb\r\n")?;
    assert_eq!(p.screen().history_len(), 2);
    Ok(())
}

/// A sixel picture takes the cells it covers, in the host's cell size, and
/// shows while they are its: text over one of them takes it away, and a
/// scroll carries it up. The cursor ends below it.
#[test]
fn a_sixel_picture_holds_its_cells_until_text_covers_them() -> Result {
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    p.set_host_graphics(fux_vt::Graphics {
        cell: Some((4, 8)),
        sixel: true,
    });
    // 6 pixels wide and 12 high (two bands of square pixels): 2 cells by 2,
    // at row 1 col 3.
    p.process(b"\r\n   \x1bPq\"1;1#1;2;100;0;0!6~-!6~\x1b\\")?;
    let shown: Vec<_> = p
        .screen()
        .pictures()
        .map(|s| (s.row, s.col, s.cols, s.rows))
        .collect();
    assert_eq!(shown, [(1, 3, 2, 2)]);
    assert_eq!(
        p.screen().cursor_position(),
        (3, 3),
        "below it, at its left"
    );
    let covered = format!("{0}{0}", fux_vt::PLACEHOLDER);
    assert_eq!(lines(&p).get(1).map(|l| l.trim()), Some(covered.as_str()));
    p.process(b"\x1b[6;1H\n")?;
    let rows: Vec<_> = p.screen().pictures().map(|s| s.row).collect();
    assert_eq!(rows, [0], "a scroll carries it up");
    p.process(b"\x1b[1;5Hx")?;
    assert_eq!(
        p.screen().pictures().count(),
        0,
        "text over a cell takes it away"
    );
    Ok(())
}

/// A picture shows while every cell it covers, row by row down from its
/// top, holds the placeholder: a region scroll that moves part of it past
/// the rest, or an edit in the call that placed it, takes it away, as
/// text over a cell does, however the screen remembers having found it
/// whole.
#[test]
fn a_picture_split_or_written_over_after_it_was_placed_is_gone() -> Result {
    // 4 pixels by 24 at 4 by 8: one cell by three, at rows 1 to 3, with
    // text in the rows below.
    let place = |p: &mut Parser, after: &[u8]| -> Result<usize> {
        p.set_host_graphics(fux_vt::Graphics {
            cell: Some((4, 8)),
            sixel: true,
        });
        let mut bytes =
            b"\x1b[5;1Hxxx\r\nxxx\x1b[2;1H\x1bPq\"1;1#1;2;100;0;0!4~-!4~-!4~-!4~\x1b\\".to_vec();
        bytes.extend(after);
        p.process(&bytes)?;
        // Another picture placed, apart, checks the first again.
        p.process(b"\x1b[1;9H\x1bPq~\x1b\\")?;
        Ok(p.screen().pictures().filter(|s| s.col == 0).count())
    };
    let fresh = || -> Result<Parser> { Ok(Parser::with_options(Size::new(8, 10)?, 10, PICTURES)?) };
    assert_eq!(place(&mut fresh()?, b"")?, 1, "whole");
    // Rows 1 to 3 scrolled down: its second row goes, its top moves to row
    // 2, and text is where its last row would be.
    assert_eq!(place(&mut fresh()?, b"\x1b[1;3r\x1b[T\x1b[r")?, 0, "SD");
    // Rows 3 to 7 scrolled up: its second row goes, text below its last.
    assert_eq!(place(&mut fresh()?, b"\x1b[3;7r\x1b[S\x1b[r")?, 0, "SU");
    assert_eq!(place(&mut fresh()?, b"\x1b[3;1H\x1b[M")?, 0, "DL");
    assert_eq!(
        place(&mut fresh()?, b"\x1b[3;1Hy")?,
        0,
        "text, in the same call"
    );
    Ok(())
}

/// A picture whose rows scroll into history is shown there, where they
/// are, as history lets its oldest rows go, and still after a resize
/// makes history anew and brings some of them back.
#[test]
fn a_picture_scrolled_into_history_is_shown_where_it_is() -> Result {
    // 420 bytes of history: 24 a row and 8 a cell, so the 17th row the
    // line feeds below take in lets the first go.
    let mut p = Parser::with_options(Size::new(8, 10)?, 420, PICTURES)?;
    p.set_host_graphics(fux_vt::Graphics {
        cell: Some((4, 8)),
        sixel: true,
    });
    // One cell by three at row 1; the cursor ends at row 4.
    p.process(b"\r\n\x1bPq\"1;1#1;2;100;0;0!4~-!4~-!4~-!4~\x1b\\")?;
    let rows = |p: &Parser| p.screen().pictures().map(|s| s.row).collect::<Vec<_>>();
    assert_eq!(rows(&p), [1]);
    // Three line feeds to the bottom row, then 17 scrolls.
    p.process(&[b'\n'; 20])?;
    assert_eq!(rows(&p), [-16]);
    assert_eq!(p.screen().history_len(), 16, "the first row went");
    // Two more rows: two come back from history.
    p.resize(Size::new(10, 10)?)?;
    assert_eq!(rows(&p), [-14]);
    p.process(b"\n")?;
    assert_eq!(rows(&p), [-15]);
    Ok(())
}

/// A copy reads a picture's cells as a reader sees them, as blanks
/// (`CellRef::shown`), not as the placeholder that marks them.
#[test]
fn a_copy_over_a_picture_reads_its_cells_as_blanks() -> Result {
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    p.set_host_graphics(fux_vt::Graphics {
        cell: Some((4, 8)),
        sixel: true,
    });
    // 2 cells by 2 at row 1 col 3, and a `z` after them on its first row.
    p.process(b"\r\n   \x1bPq\"1;1#1;2;100;0;0!6~-!6~\x1b\\\x1b[2;8Hz")?;
    assert_eq!(p.screen().pictures().count(), 1);
    let copied = p.screen().window().text((1, 0), (1, 9), 100, 100)?;
    assert_eq!(copied, "       z");
    Ok(())
}

/// A picture's size, and one of its pixels.
type Drawn = ((u32, u32), Option<[u8; 4]>);

/// The size, and the pixel at `x`, `y`, of the picture `sixel` draws.
fn sixel_pixel(sixel: &str, (x, y): (u32, u32)) -> Result<Drawn> {
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    p.process(format!("\x1bP{sixel}\x1b\\").as_bytes())?;
    let shown = p.screen().pictures().next().ok_or("no picture")?;
    let picture = shown.picture;
    Ok(((picture.width(), picture.height()), picture.pixel(x, y)))
}

/// DEC STD 070, 7.1 and 8.1.2: a pixel is as tall, to its width, as P1's
/// macro says (2:1 by default, 4.5:1 for 2, 1:1 for 9), or as the raster
/// attributes' Pan:Pad say, omitted ones 1; a picture's pixels are square,
/// so a sixel of tall pixels takes that many rows, of wide ones columns.
/// Raster attributes after a sixel or a positioning command are ignored.
#[test]
fn a_sixel_pixel_has_the_aspect_p1_or_the_raster_attributes_give() -> Result {
    let size = |sixel: &str| sixel_pixel(sixel, (0, 0)).map(|(size, _)| size);
    assert_eq!(size("q~")?, (1, 12));
    assert_eq!(size("0;1;0q~")?, (1, 12));
    assert_eq!(size("2q~")?, (1, 27));
    assert_eq!(size("9q~")?, (1, 6));
    assert_eq!(size("q\"1;1~")?, (1, 6));
    assert_eq!(size("q\"3;1~")?, (1, 18));
    assert_eq!(size("q\";0~")?, (1, 6));
    assert_eq!(size("9q\"1;2~")?, (2, 6));
    assert_eq!(size("q~\"1;1~")?, (2, 12));
    assert_eq!(size("q$\"1;1~")?, (1, 12));
    Ok(())
}

/// DEC STD 070, 7.2: P2 1 leaves the pixels no sixel sets as they are
/// (clear); 0 and 2 set them to the background, colour register 0 here,
/// over the picture and the extent the raster attributes give (8.1.2),
/// which sixels may draw past: it is no clip.
#[test]
fn sixel_background_select_and_extent() -> Result {
    let red = Some([255, 0, 0, 255]);
    // A red pixel at 0, 1 of a picture 4 by 6.
    let picture = "q\"1;1;4;6#1;2;100;0;0A";
    let at = |p2: &str, x, y| sixel_pixel(&picture.replacen('q', p2, 1), (x, y));
    assert_eq!(at("0;1q", 0, 0)?, ((4, 6), Some([0; 4])));
    assert_eq!(at("0;1q", 3, 5)?, ((4, 6), Some([0; 4])));
    assert_eq!(at("0;1q", 0, 1)?, ((4, 6), red));
    for p2 in ["q", "0;0q", "0;2q"] {
        assert_eq!(at(p2, 3, 5)?, ((4, 6), Some([0, 0, 0, 255])), "{p2}");
        assert_eq!(at(p2, 0, 1)?, ((4, 6), red), "{p2}");
    }
    let blue = Some([0, 0, 255, 255]);
    let defined = sixel_pixel("0;2q\"1;1;2;6#0;2;0;0;100#1;2;100;0;0A", (1, 0))?;
    assert_eq!(defined, ((2, 6), blue), "register 0 as it ends");
    assert_eq!(sixel_pixel("0;1q\"1;1;1;1!3~", (2, 5))?.0, (3, 6));
    Ok(())
}

/// DEC STD 070, sixel graphics, 9.1 (C0 codes): "SUB - process as if a
/// 3/15 code (blank Sixel) was received"; "CAN - exit DCS mode processing,
/// exit Sixel mode". So a SUB between two sixels leaves a blank one between
/// them, in the background P2 0 sets (7.2), and shows no error character;
/// a CAN ends the picture, which is not placed: the standard does not say
/// it is, and CAN ends a control string "without execution" (3.5.1.2.1).
#[test]
fn sub_inside_sixel_is_a_blank_sixel_and_can_ends_it() -> Result {
    let red = Some([255, 0, 0, 255]);
    let blank = Some([0, 0, 0, 255]);
    let picture = "q\"1;1#1;2;100;0;0~\x1a~";
    assert_eq!(sixel_pixel(picture, (0, 0))?, ((3, 6), red));
    assert_eq!(sixel_pixel(picture, (1, 0))?, ((3, 6), blank));
    assert_eq!(sixel_pixel(picture, (2, 5))?, ((3, 6), red));
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    p.process(b"\x1bPq\"1;1#1;2;100;0;0~\x1a~\x1b\\x")?;
    assert_eq!(p.screen().pictures().count(), 1);
    let text = lines(&p).concat();
    assert!(text.contains('x') && !text.contains('\u{2426}'), "{text:?}");
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    p.process(b"\x1bPq\"1;1#1;2;100;0;0~~\x18x")?;
    assert_eq!(p.screen().pictures().count(), 0);
    assert_eq!(lines(&p).first().map(|l| l.trim_end()), Some("x"));
    Ok(())
}

/// DEC STD 070, sixel graphics, 9.1 (C0 codes): "BEL - same action as in
/// text mode"; "All other C0 Codes are ignored", as 0/8 to 0/13 "have no
/// effect". So a control between a repeat's introducer and its count, or
/// a colour's numbers, leaves the command whole, and BEL rings.
#[test]
fn c0_controls_inside_sixel_are_ignored_and_bel_rings() -> Result {
    let red = Some([255, 0, 0, 255]);
    // A repeat of 3 with ENQ and SO inside it: three sixels wide.
    assert_eq!(
        sixel_pixel("q\"1;1#1;2;100;0;0!\x053\x0e~", (2, 0))?,
        ((3, 6), red)
    );
    // A colour with ACK between its numbers: register 1 is red still.
    assert_eq!(
        sixel_pixel("q\"1;1#1;2;100\x06;0;0~", (0, 0))?,
        ((1, 6), red)
    );
    struct Bells(usize);
    impl fux_vt::Sink for Bells {
        fn reply(&mut self, _: &[u8]) {}
        fn event(&mut self, event: fux_vt::Event<'_>) {
            if matches!(event, fux_vt::Event::Bell) {
                self.0 += 1;
            }
        }
    }
    let options = PICTURES.with(fux_vt::Feature::Events);
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, options)?;
    let mut bells = Bells(0);
    p.process_with(b"\x1bPq#1;2;100;0;0~\x07~\x1b\\", &mut bells)?;
    assert_eq!(bells.0, 1);
    let shown = p.screen().pictures().next().ok_or("no picture")?;
    assert_eq!(shown.picture.width(), 2);
    Ok(())
}

/// Without `Feature::Pictures`, sixel is ignored as any other DCS is:
/// nothing is placed, the cells hold what was written around it, the
/// cursor stays, and the terminal says no sixel though its host draws it.
#[test]
fn sixel_is_ignored_without_pictures() -> Result {
    let mut p = Parser::new(Size::new(6, 10)?, 10)?;
    p.set_host_graphics(fux_vt::Graphics {
        cell: Some((4, 8)),
        sixel: true,
    });
    let mut replies = Vec::new();
    p.process_with_replies(
        b"ab\x1bPq\"1;1;4096;2160#1;2;100;0;0!6~-!6~\x1b\\cd\x1b[c\x1b[?1;1S",
        |r| replies.extend_from_slice(r),
    )?;
    assert_eq!(p.screen().pictures().count(), 0);
    assert_eq!(lines(&p).first().map(|l| l.trim_end()), Some("abcd"));
    assert_eq!(p.screen().cursor_position(), (0, 4));
    assert_eq!(String::from_utf8_lossy(&replies), "\x1b[?1;2c");
    Ok(())
}

/// The host's graphics are what a program is told: sixel in DA1, and the
/// cell's and the screen's pixel sizes.
#[test]
fn graphics_queries_are_answered_from_the_host() -> Result {
    let mut p = Parser::with_options(Size::new(6, 10)?, 10, PICTURES)?;
    let replies = |p: &mut Parser, q: &[u8]| -> std::result::Result<String, Error> {
        let mut out = Vec::new();
        p.process_with_replies(q, |r| out.extend_from_slice(r))?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    assert_eq!(
        replies(&mut p, b"\x1b[16t")?,
        "",
        "no size known, none told"
    );
    assert_eq!(replies(&mut p, b"\x1b[c")?, "\x1b[?1;2c");
    p.set_host_graphics(fux_vt::Graphics {
        cell: Some((9, 18)),
        sixel: true,
    });
    assert_eq!(replies(&mut p, b"\x1b[c")?, "\x1b[?1;2;4c");
    assert_eq!(replies(&mut p, b"\x1b[16t")?, "\x1b[6;18;9t");
    assert_eq!(replies(&mut p, b"\x1b[14t")?, "\x1b[4;108;90t");
    assert_eq!(replies(&mut p, b"\x1b[?1;1S")?, "\x1b[?1;0;256S");
    Ok(())
}
