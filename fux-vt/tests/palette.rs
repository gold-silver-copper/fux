//! `Feature::Palette`: the colours a program sets, queries and resets
//! (xterm's ctlseqs, "Operating System Commands": OSC 4, 5, 10 to 19, 104,
//! 105, 110 to 119). The expected answers are xterm 411's, asked the same
//! (under Xvfb, `-xrm 'XTerm*allowColorOps: true'`, 80 by 25): each
//! sequence written, and what xterm wrote back.

use fux_vt::{Event, Feature, Options, Parser, Sink, Size};
#[path = "corpus/pieces.rs"]
mod pieces;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Everything the parser gave the host, in order, as text.
#[derive(Debug, Default, PartialEq, Eq)]
struct Heard(Vec<String>);

impl Sink for Heard {
    fn reply(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes)
            .replace('\x1b', "^[")
            .replace('\x07', "^G");
        self.0.push(text);
    }
    fn event(&mut self, event: Event<'_>) {
        self.0.push(match event {
            Event::ColorQuery { number, bel } => {
                format!("ask {number} {}", if bel { "bel" } else { "st" })
            }
            other @ (Event::Title(_)
            | Event::IconName(_)
            | Event::Bell
            | Event::Clipboard { .. }
            | _) => {
                format!("{other:?}")
            }
        });
    }
}

const PALETTE: Options = Options::new().with(Feature::Palette);
const BOTH: Options = Options::new().with(Feature::Palette).with(Feature::Events);

/// What `options` give the host for `input`, and the parser after it; the
/// same whatever pieces the input comes in.
fn run(options: Options, input: &[u8]) -> Result<(Heard, Parser)> {
    let mut parser = Parser::with_options(Size::new(25, 80)?, 0, options)?;
    let mut heard = Heard::default();
    parser.process_with(input, &mut heard)?;
    for size in [1, 2, 7] {
        let mut again = Parser::with_options(Size::new(25, 80)?, 0, options)?;
        let mut pieces_heard = Heard::default();
        for chunk in pieces::pieces(input, size) {
            again.process_with(chunk, &mut pieces_heard)?;
        }
        assert_eq!(pieces_heard, heard, "in pieces of {size}");
    }
    Ok((heard, parser))
}

fn said(list: &[&str]) -> Heard {
    Heard(list.iter().map(|s| (*s).to_owned()).collect())
}

/// An entry not set is answered with xterm's default; a set entry with its
/// colour, each channel's byte twice, in the query's form and with its
/// terminator.
#[test]
fn palette_queries_are_answered_with_the_colour_or_xterms_default() -> Result {
    let (heard, parser) = run(
        PALETTE,
        b"\x1b]4;0;?;1;?;4;?;12;?\x1b\\\x1b]4;17;?;232;?;255;?\x07\
          \x1b]4;1;#123456\x1b\\\x1b]4;1;?\x1b\\",
    )?;
    assert_eq!(
        heard,
        said(&[
            "^[]4;0;rgb:0000/0000/0000^[\\",
            "^[]4;1;rgb:cdcd/0000/0000^[\\",
            "^[]4;4;rgb:0000/0000/eeee^[\\",
            "^[]4;12;rgb:5c5c/5c5c/ffff^[\\",
            "^[]4;17;rgb:0000/0000/5f5f^G",
            "^[]4;232;rgb:0808/0808/0808^G",
            "^[]4;255;rgb:eeee/eeee/eeee^G",
            "^[]4;1;rgb:1212/3434/5656^[\\",
        ])
    );
    assert_eq!(
        parser.screen().palette_color(1),
        Some([0x12, 0x34, 0x56].into())
    );
    assert_eq!(parser.screen().palette_color(0), None);
    assert!(parser.screen().colors_changed());
    Ok(())
}

/// esctest's ChangeColor tests: each form of specification, set and asked
/// for again (xterm keeps 8 bits a channel: `#fff` is `f0f0`).
#[test]
fn specifications_are_kept_as_xterm_keeps_them() -> Result {
    for (spec, answer) in [
        ("rgb:f0f0/f0f0/f0f0", "f0f0/f0f0/f0f0"),
        ("rgb:8080/8080/8080", "8080/8080/8080"),
        ("rgb:ff00/8/80", "ffff/8888/8080"),
        ("rgb:1/22/333", "1111/2222/3333"),
        ("RGB:ff/00/00", "ffff/0000/0000"),
        ("#fff", "f0f0/f0f0/f0f0"),
        ("#888", "8080/8080/8080"),
        ("#f0f0f0", "f0f0/f0f0/f0f0"),
        ("#f00f00f00", "f0f0/f0f0/f0f0"),
        ("#800080008000", "8080/8080/8080"),
        ("#aaaabbbbcccc", "aaaa/bbbb/cccc"),
    ] {
        let input = format!("\x1b]4;0;{spec}\x1b\\\x1b]4;0;?\x1b\\");
        let (heard, _) = run(PALETTE, input.as_bytes())?;
        assert_eq!(
            heard,
            said(&[&format!("^[]4;0;rgb:{answer}^[\\")]),
            "{spec}"
        );
    }
    Ok(())
}

/// xterm's `ChangeAnsiColorRequest`: a pair whose specification cannot be
/// read, or whose number is out of range, ends the list; a number is read
/// as `atoi` reads it, so `x` is 0 and `1x` is 1; a query in the list sees
/// a colour set before it. xterm 411: `OSC 4;3;rgb:ff00/8/80;4;bogus;5;#123`
/// sets 3 alone; `OSC 4;x;#fff;2;#fff` sets 0 and 2; `OSC 4;-1;#fff;2;#fff`
/// sets nothing.
#[test]
fn a_list_of_colours_is_read_as_xterm_reads_it() -> Result {
    let (_, parser) = run(PALETTE, b"\x1b]4;3;#300;4;bogus;5;#123\x07")?;
    let screen = parser.screen();
    assert_eq!(screen.palette_color(3), Some([0x30, 0, 0].into()));
    assert_eq!(
        (screen.palette_color(4), screen.palette_color(5)),
        (None, None)
    );
    let (_, parser) = run(PALETTE, b"\x1b]4;x;#fff;2;#fff\x07\x1b]4;1x;#ff0000\x07")?;
    let white = Some([0xf0, 0xf0, 0xf0].into());
    let screen = parser.screen();
    assert_eq!(
        (screen.palette_color(0), screen.palette_color(2)),
        (white, white)
    );
    assert_eq!(screen.palette_color(1), Some([0xff, 0, 0].into()));
    for list in [
        &b"\x1b]4;-1;#fff;2;#fff\x07"[..],
        b"\x1b]4;261;#fff;2;#fff\x07",
        b"\x1b]4;2;\x07",
        b"\x1b]4;2\x07",
        b"\x1b]4;2;  #102030\x07",
        b"\x1b]4;2;red\x07",
    ] {
        let (_, parser) = run(PALETTE, list)?;
        assert!(!parser.screen().colors_changed(), "{list:?}");
    }
    let (heard, _) = run(PALETTE, b"\x1b]4;1;?;2;#00ff00;2;?\x07")?;
    assert_eq!(
        heard,
        said(&["^[]4;1;rgb:cdcd/0000/0000^G", "^[]4;2;rgb:0000/ffff/0000^G"])
    );
    Ok(())
}

/// OSC 104 resets the entries listed, up to the first that is not a
/// number followed by `;` or the end, as xterm's `ResetAnsiColorRequest`
/// reads them; alone, every entry. xterm 411: after `OSC 104;3x;0`, 3 and 0
/// keep what was set.
#[test]
fn osc_104_resets_entries() -> Result {
    let set = b"\x1b]4;0;#111;1;#222;2;#333;3;#444\x07";
    let (_, parser) = run(PALETTE, &[&set[..], b"\x1b]104;1;2\x07"].concat())?;
    let s = parser.screen();
    assert_eq!(
        [s.palette_color(0), s.palette_color(1), s.palette_color(2)],
        [Some([0x10, 0x10, 0x10].into()), None, None]
    );
    let (_, parser) = run(PALETTE, &[&set[..], b"\x1b]104;3x;0\x07"].concat())?;
    let s = parser.screen();
    assert_eq!(
        (s.palette_color(0), s.palette_color(3)),
        (
            Some([0x10, 0x10, 0x10].into()),
            Some([0x40, 0x40, 0x40].into())
        )
    );
    let (_, parser) = run(PALETTE, &[&set[..], b"\x1b]104;1;x;0\x07"].concat())?;
    let s = parser.screen();
    assert_eq!(
        (s.palette_color(0), s.palette_color(1)),
        (Some([0x10, 0x10, 0x10].into()), None)
    );
    let (heard, parser) = run(PALETTE, &[&set[..], b"\x1b]104\x07\x1b]4;3;?\x07"].concat())?;
    assert!(!parser.screen().colors_changed());
    assert_eq!(heard, said(&["^[]4;3;rgb:cdcd/cdcd/0000^G"]));
    Ok(())
}

/// The special colours (ctlseqs: bold, underline, blink, reverse and
/// italic): OSC 5 ; c, or OSC 4 ; 256 + c, set and answered in the form
/// asked; OSC 105 resets those listed, and none alone, as xterm 411 (where
/// ctlseqs says every one); OSC 104 ; 256 + c resets one too. One never set is not answered: xterm's
/// default is its foreground, which fux-vt does not know. Neither is OSC 4
/// past 260, nor OSC 5 past 4.
#[test]
fn special_colours_are_set_queried_and_reset() -> Result {
    let (heard, parser) = run(
        PALETTE,
        b"\x1b]5;0;?;4;?\x07\x1b]5;0;#333;1;#444\x07\x1b]5;0;?\x07\x1b]4;256;?;257;?;261;?\x07\
          \x1b]4;260;#555\x07\x1b]5;4;?;5;?\x07",
    )?;
    assert_eq!(
        heard,
        said(&[
            "^[]5;0;rgb:3030/3030/3030^G",
            "^[]4;256;rgb:3030/3030/3030^G",
            "^[]4;257;rgb:4040/4040/4040^G",
            "^[]5;4;rgb:5050/5050/5050^G",
        ])
    );
    // Special colours are no palette entry, and change nothing drawn.
    assert!(!parser.screen().colors_changed());
    let set = b"\x1b]5;0;#333;1;#444;2;#555\x07";
    let (heard, _) = run(
        PALETTE,
        &[
            &set[..],
            b"\x1b]105;0\x07\x1b]104;257\x07\x1b]5;0;?;1;?;2;?\x07",
        ]
        .concat(),
    )?;
    assert_eq!(heard, said(&["^[]5;2;rgb:5050/5050/5050^G"]));
    let (heard, _) = run(
        PALETTE,
        &[&set[..], b"\x1b]105\x07\x1b]5;0;?;1;?;2;?\x07"].concat(),
    )?;
    assert_eq!(
        heard,
        said(&[
            "^[]5;0;rgb:3030/3030/3030^G",
            "^[]5;1;rgb:4040/4040/4040^G",
            "^[]5;2;rgb:5050/5050/5050^G",
        ])
    );
    Ok(())
}

/// The dynamic colours (OSC 10 to 19): each parameter the next colour, `?`
/// asking for it. One the program set is answered by fux-vt; one it has
/// not is asked of the host, as without the palette. A specification that
/// cannot be read is skipped and the next parameter read (xterm 411:
/// `OSC 12;#0a0b0c;bogus;#111111` sets 12 and 14); OSC 110 to 119 reset
/// one, and nothing with a parameter (`OSC 111;5`).
#[test]
fn dynamic_colours_are_set_queried_and_reset() -> Result {
    let (heard, parser) = run(
        BOTH,
        b"\x1b]10;?;?\x1b\\\x1b]10;#123456;rgb:01/02/03\x07\x1b]10;?;?;?\x1b\\\
          \x1b]12;#0a0b0c;bogus;#111111\x07\x1b]12;?;?;?\x07",
    )?;
    assert_eq!(
        heard,
        said(&[
            "ask 10 st",
            "ask 11 st",
            "^[]10;rgb:1212/3434/5656^[\\",
            "^[]11;rgb:0101/0202/0303^[\\",
            "ask 12 st",
            "^[]12;rgb:0a0a/0b0b/0c0c^G",
            "ask 13 bel",
            "^[]14;rgb:1111/1111/1111^G",
        ])
    );
    let s = parser.screen();
    assert_eq!(s.dynamic_color(10), Some([0x12, 0x34, 0x56].into()));
    assert_eq!(s.dynamic_color(11), Some([1, 2, 3].into()));
    assert_eq!(s.dynamic_color(13), None);
    assert!(s.colors_changed());
    let set = b"\x1b]11;#010203\x07";
    let (heard, parser) = run(BOTH, &[&set[..], b"\x1b]111;5\x07\x1b]11;?\x07"].concat())?;
    assert_eq!(heard, said(&["^[]11;rgb:0101/0202/0303^G"]));
    assert!(parser.screen().colors_changed());
    let (heard, parser) = run(BOTH, &[&set[..], b"\x1b]111\x07\x1b]11;?\x07"].concat())?;
    assert_eq!(heard, said(&["ask 11 bel"]));
    assert!(!parser.screen().colors_changed());
    // Without events, a colour not set goes unanswered.
    let (heard, _) = run(PALETTE, b"\x1b]10;?\x07")?;
    assert_eq!(heard, Heard::default());
    Ok(())
}

/// RIS and DECSTR reset the palette, and leave the dynamic and special
/// colours, as xterm 411 does (`OSC 4;1;#123456`, `ESC c` or `CSI ! p`,
/// `OSC 4;1;?` answers `cdcd/0000/0000`; `OSC 10;#123456` and
/// `OSC 5;0;#654321` survive both).
#[test]
fn ris_and_decstr_reset_the_palette_alone() -> Result {
    for reset in [&b"\x1bc"[..], b"\x1b[!p"] {
        let input = [
            &b"\x1b]4;1;#123456\x07\x1b]10;#123456\x07\x1b]5;0;#654321\x07"[..],
            reset,
            b"\x1b]4;1;?\x07\x1b]10;?\x07\x1b]5;0;?\x07",
        ]
        .concat();
        let (heard, parser) = run(BOTH, &input)?;
        assert_eq!(
            heard,
            said(&[
                "^[]4;1;rgb:cdcd/0000/0000^G",
                "^[]10;rgb:1212/3434/5656^G",
                "^[]5;0;rgb:6565/4343/2121^G",
            ])
        );
        assert_eq!(parser.screen().palette_color(1), None);
    }
    Ok(())
}

/// Without the option the colours are ignored and kept nowhere, and OSC
/// 10 to 19 queries are events as they always were.
#[test]
fn without_the_option_colours_are_ignored() -> Result {
    let input = b"\x1b]4;1;#123456;1;?\x07\x1b]5;0;?\x07\x1b]10;#fff;?\x07\x1b]104\x07\x1b]110\x07";
    let (heard, parser) = run(Options::new(), input)?;
    assert_eq!(heard, Heard::default());
    assert!(!parser.screen().colors_changed());
    assert_eq!(parser.screen().palette_color(1), None);
    let (heard, parser) = run(Options::new().with(Feature::Events), input)?;
    assert_eq!(heard, said(&["ask 11 bel"]));
    assert_eq!(parser.screen().dynamic_color(10), None);
    Ok(())
}

/// An OSC string past `OSC_PAYLOAD_LIMIT` is no colour request.
#[test]
fn an_overlong_colour_request_is_ignored() -> Result {
    let mut input = b"\x1b]4;1;#123456".to_vec();
    input.extend(std::iter::repeat_n(b';', fux_vt::OSC_PAYLOAD_LIMIT));
    input.extend(b"\x07\x1b]4;1;?\x07");
    let (heard, parser) = run(PALETTE, &input)?;
    assert_eq!(parser.screen().palette_color(1), None);
    assert_eq!(heard, said(&["^[]4;1;rgb:cdcd/0000/0000^G"]));
    Ok(())
}

/// What `options` give the host for `input` on a parser whose host said its
/// terminal shows `host` for some of palette entries 0 to 15.
fn run_hosted(
    options: Options,
    host: &[(usize, [u8; 3])],
    input: &[u8],
) -> Result<(Heard, Parser)> {
    let mut parser = Parser::with_options(Size::new(25, 80)?, 0, options)?;
    let mut palette = [None; 16];
    for &(index, rgb) in host {
        *palette.get_mut(index).ok_or("an entry 0 to 15")? = Some(rgb.into());
    }
    parser.set_host_palette(palette);
    let mut heard = Heard::default();
    parser.process_with(input, &mut heard)?;
    Ok((heard, parser))
}

/// An entry the program has not set is answered with the host's colour for
/// it, if the host gave one; xterm's default otherwise, and past 15 always.
#[test]
fn an_entry_not_set_is_answered_with_the_hosts_colour() -> Result {
    let host = [(1, [0xbf, 0x61, 0x6a]), (15, [0xec, 0xef, 0xf4])];
    let (heard, parser) = run_hosted(PALETTE, &host, b"\x1b]4;1;?;2;?;15;?;17;?\x1b\\")?;
    assert_eq!(
        heard,
        said(&[
            "^[]4;1;rgb:bfbf/6161/6a6a^[\\",
            "^[]4;2;rgb:0000/cdcd/0000^[\\",
            "^[]4;15;rgb:ecec/efef/f4f4^[\\",
            "^[]4;17;rgb:0000/0000/5f5f^[\\",
        ])
    );
    // Nothing drawn changes: the program set no colour.
    assert_eq!(parser.screen().palette_color(1), None);
    assert!(!parser.screen().colors_changed());
    Ok(())
}

/// A colour the program set wins over the host's; once the program resets
/// it (OSC 104, DECSTR, RIS) the host's is answered again, which no reset
/// clears.
#[test]
fn the_programs_colour_wins_and_resets_bring_back_the_hosts() -> Result {
    let host = [(1, [0xbf, 0x61, 0x6a])];
    for reset in [
        &b"\x1b]104;1\x07"[..],
        b"\x1b]104\x07",
        b"\x1b[!p",
        b"\x1bc",
    ] {
        let input = [
            &b"\x1b]4;1;#123456\x07\x1b]4;1;?\x07"[..],
            reset,
            b"\x1b]4;1;?\x07",
        ]
        .concat();
        let (heard, _) = run_hosted(PALETTE, &host, &input)?;
        assert_eq!(
            heard,
            said(&["^[]4;1;rgb:1212/3434/5656^G", "^[]4;1;rgb:bfbf/6161/6a6a^G"]),
            "{reset:?}"
        );
    }
    Ok(())
}

/// A cleared host colour is answered with xterm's default again; without
/// the option nothing is answered.
#[test]
fn host_colours_are_cleared_and_need_the_option() -> Result {
    let (_, mut parser) = run_hosted(PALETTE, &[(1, [1, 2, 3])], b"")?;
    parser.set_host_palette([None; 16]);
    let mut heard = Heard::default();
    parser.process_with(b"\x1b]4;1;?\x07", &mut heard)?;
    assert_eq!(heard, said(&["^[]4;1;rgb:cdcd/0000/0000^G"]));
    // Clearing what was never set keeps no colours at all.
    let mut fresh = Parser::with_options(Size::new(25, 80)?, 0, PALETTE)?;
    fresh.set_host_palette([None; 16]);
    assert!(!fresh.screen().colors_changed());
    let (heard, _) = run_hosted(Options::new(), &[(1, [1, 2, 3])], b"\x1b]4;1;?\x07")?;
    assert_eq!(heard, said(&[]));
    Ok(())
}
