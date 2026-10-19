use super::*;
use crate::test_rng::Rng;

/// The ASCII run path writes cells without asking about grapheme clusters.
/// That is only right because no ASCII character continues a cluster (fux-vt
/// never joins after a Prepend), and because the run leaves its last cell
/// for a following mark or selector to join. Both paths must agree on text
/// that mixes ASCII with every kind of cluster, at every wrap position.
#[test]
fn ascii_run_path_equals_scalar_dispatch_around_grapheme_clusters() -> Result<(), Error> {
    let text = "ab1\u{fe0f}\u{20e3}x\u{2764}\u{fe0f}y#\u{fe0f}\u{20e3}e\u{301}\u{302}z\
        \u{1f1ef}\u{1f1f5}q\u{1f469}\u{200d}\u{1f52c}w\u{928}\u{93f}v\u{600}5\u{200d}k\
        \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}!\r\n\x1b[1mA\u{301}\x1b[0m";
    for cols in 1..=12 {
        let mut fast = Parser::new(Size::of(6, cols), 8)?;
        let mut slow = fast.clone();
        fast.process(text.as_bytes())?;
        scalar(&mut slow, text.as_bytes(), false, &mut Log::default())?;
        let difference = first_difference(&format!("{fast:?}"), &format!("{slow:?}"));
        assert_eq!(difference, None, "{cols} columns");
    }
    // A run over a row that holds a wide glyph: written up to it.
    for cols in 2..=12 {
        let mut fast = Parser::new(Size::of(3, cols), 8)?;
        let mut slow = fast.clone();
        let text = wide_glyph_rows(3, cols);
        fast.process(&text)?;
        scalar(&mut slow, &text, false, &mut Log::default())?;
        let difference = first_difference(&format!("{fast:?}"), &format!("{slow:?}"));
        assert_eq!(difference, None, "{cols} columns, a wide glyph");
    }
    Ok(())
}

/// The run path for text past ASCII (`Screen::text`) writes glyphs a row
/// at a time without asking what `print` asks of each. It must do what
/// `print` does, character by character, in every context that changes
/// a glyph's printing: margins, origin mode, double-width rows, wide
/// glyphs overwritten at either half, a pending wrap, autowrap off,
/// protection, both screens, insert mode, an open link, a Prepend left by
/// the call before, recorded prints, DEC Special Graphics, and glyphs
/// already there between ones that change.
#[test]
fn the_text_run_path_equals_scalar_dispatch_in_context() -> Result<(), Error> {
    let text = "界全角─│ЖéﾊШ한";
    let contexts: &[&str] = &[
        "",
        "\x1b[?69h\x1b[3;8s\x1b[1;5H",
        "\x1b[?69h\x1b[3;8s\x1b[?6h",
        "\x1b#6",
        "\x1b[2;3H界界界界\x1b[2;4H",
        "\x1b[2;3H界界界界\x1b[2;3Hx",
        "\x1b[1;11Hx",
        "\x1b[?7l\x1b[1;9H",
        "\x1b[1\"q",
        "\x1b[?1049h",
        "abcdefghij\r\x1b[4h",
        "\x1b]8;id=1;http://x\x1b\\",
        "\u{600}",
        "\x1b(0",
        "\x1b[2;3r\x1b[3;1H",
        "AAШA\r",
        "\x1b]8;id=1;http://x\x1b\\AAШA\r",
    ];
    let long = format!("{text}{text}\u{301}{text}");
    for (n, context) in contexts.iter().enumerate() {
        let inputs = [long.as_str(), "ЖШ界", "Ж界", "ЖШШШ"];
        for (input, record) in inputs.iter().flat_map(|i| [(i, false), (i, true)]) {
            let every = Feature::ALL.into_iter().collect::<Options>();
            let mut fast = Parser::with_options(Size::of(4, 12), 8, every)?;
            fast.record_prints(record);
            let mut slow = fast.clone();
            for piece in [context.as_bytes(), input.as_bytes()] {
                fast.process(piece)?;
                scalar(&mut slow, piece, false, &mut Log::default())?;
            }
            let difference = first_difference(&format!("{fast:?}"), &format!("{slow:?}"));
            assert_eq!(difference, None, "context {n}, {input}, recording {record}");
        }
    }
    Ok(())
}

/// Each of `rows` rows of `cols` columns with a wide glyph before its last
/// column, then a run of ASCII over the whole row.
fn wide_glyph_rows(rows: u16, cols: u16) -> Vec<u8> {
    let mut text = Vec::new();
    for row in 1..=rows {
        text.extend_from_slice(
            format!("\x1b[{row};{}H\u{4e2d}\r", cols.saturating_sub(1)).as_bytes(),
        );
        text.extend(std::iter::repeat_n(b'a', usize::from(cols)));
    }
    text
}

/// The ASCII run path writes up to a half of a wide glyph and goes on past
/// it: a run over a row that holds one costs the row, not a retry of the
/// rest of the run for every byte.
#[test]
fn a_run_over_a_wide_glyph_costs_its_length() -> Result<(), Error> {
    let mut p = Parser::new(Size::of(40, 8000), 0)?;
    let text = wide_glyph_rows(40, 8000);
    let start = std::time::Instant::now();
    p.process(&text)?;
    let spent = start.elapsed();
    assert!(spent < std::time::Duration::from_secs(1), "{spent:?}");
    let row = p.screen().window().text((39, 0), (39, 7999), 8000, 8000)?;
    assert_eq!(row, std::iter::repeat_n('a', 8000).collect::<String>());
    Ok(())
}

/// The final bytes of the CSIs programs send most, and of some they do
/// not, with modes and queries among them.
const FINALS: &[u8] = b"HABCDEFGJKdfmrsuhlbXLMPST@`aenctqpgxyzIZ~";

/// One parameter: empty, small, a few digits, or too large for a `u16`.
fn parameter(r: &mut Rng, out: &mut Vec<u8>) {
    match r.below(10) {
        0 => {}
        1 => out.extend_from_slice(b"0"),
        2 => out.extend_from_slice(b"99999999"),
        3 => out.extend_from_slice(b"65535"),
        4 => out.extend_from_slice(b"2026"),
        5 => out.extend_from_slice(b"1049"),
        _ => out.extend_from_slice(r.below(300).to_string().as_bytes()),
    }
}

/// A CSI, mostly well formed: a private marker now and then, up to three
/// parameters mostly and past the 32 a sequence holds sometimes,
/// subparameters, intermediates, a control or a stray byte inside, or cut
/// short.
fn csi(r: &mut Rng, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    if r.chance(25) {
        out.push(r.byte_in(0x3c, 0x3f));
    }
    let count = if r.chance(5) {
        r.below(8).saturating_add(29)
    } else {
        r.below(4)
    };
    for i in 0..count {
        if i > 0 {
            out.push(if r.chance(15) { b':' } else { b';' });
        }
        parameter(r, out);
        if r.chance(2) {
            out.push(
                r.pick(b"\x00\x07\x08\x0a\x0d\x18\x1a\x1b\x7f\x3c\x3f\x80\xc3")
                    .unwrap_or(0),
            );
        }
    }
    if r.chance(10) {
        for _ in 0..=r.below(3) {
            out.push(r.byte_in(0x20, 0x2f));
        }
    }
    if r.chance(95) {
        out.push(if r.chance(80) {
            r.pick(FINALS).unwrap_or(b'H')
        } else {
            r.byte_in(0x40, 0x7e)
        });
    }
}

/// Bytes that exercise the parser everywhere: sequences of every kind
/// mostly, text, controls, strings, and bytes at random; with `long`,
/// OSC strings of about the most bytes kept of one now and then.
fn sequences(r: &mut Rng, fragments: usize, long: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..fragments {
        match r.below(23) {
            0..=6 => csi(r, &mut out),
            7 | 8 => {
                out.push(0x1b);
                for _ in 0..r.below(4) {
                    out.push(r.byte_in(0x20, 0x2f));
                }
                if r.chance(90) {
                    out.push(r.byte_in(0x30, 0x7e));
                }
            }
            9 => out.extend_from_slice(
                r.pick(&[
                    &b"\x1b]0;title\x07"[..],
                    b"\x1b]2;name\x1b\\",
                    b"\x1b]8;id=1;http://x\x1b\\",
                    b"\x1b]8;;\x07",
                    b"\x1b]133;A\x07",
                    b"\x1b]10;?\x07",
                    b"\x1b]4;1;rgb:00/ff/00\x07",
                ])
                .unwrap_or_default(),
            ),
            10 => out.extend_from_slice(
                r.pick(&[
                    &b"\x1bP$qm\x1b\\"[..],
                    b"\x1bP$q q\x1b\\",
                    b"\x1bP1;2|xyz\x1b\\",
                    b"\x1b_apc\x1b\\",
                    b"\x1bXsos\x07",
                ])
                .unwrap_or_default(),
            ),
            11 => out.extend_from_slice(
                r.pick(&[
                    &b"\x1b[?2026h"[..],
                    b"\x1b[?2026l",
                    b"\x1b[?2026s",
                    b"\x1b[?2026r",
                    b"\x1b[?1049h",
                    b"\x1b[?1049l",
                    b"\x1b[?7l",
                    b"\x1b[?6h",
                    b"\x1b[4h",
                    b"\x1b[3b",
                    b"\x1b(0",
                    b"\x1b(B",
                    b"\x1b7",
                    b"\x1b8",
                    b"\x1b(((B",
                    // A frame begun by XTRESTORE, which goes on, then an
                    // `h` in a string or ending an escape.
                    b"\x1b[?2026h\x1b[?2026s\x1b[?2026r\x1b]2;high\x07",
                    b"\x1b[?2026h\x1b[?2026s\x1b[?2026rk\x1bhk",
                ])
                .unwrap_or_default(),
            ),
            12..=15 => {
                for _ in 0..=r.below(12) {
                    out.push(r.byte_in(0x20, 0x7e));
                }
            }
            // Text past ASCII, alone or a run of it (`Screen::text`'s):
            // narrow and wide glyphs, box drawing, marks, joiners, a
            // Prepend, Hangul jamo, emoji and their modifiers and flags.
            16 => {
                for _ in 0..=r.below(3).saturating_mul(r.below(8)) {
                    let text = r.pick(&[
                        "é", "界", "\u{301}", "👍", "🇺🇸", "\u{fe0f}", "Ж", "─", "│", "全",
                        "\u{600}", "\u{1100}", "\u{1161}", "한", "ष", "\u{94d}", "\u{200d}", "🏽",
                        "\u{17d8}", "ﾊ",
                    ]);
                    out.extend_from_slice(text.unwrap_or_default().as_bytes());
                }
            }
            17 | 18 => out.push(
                r.pick(b"\r\n\x08\t\x07\x18\x1a\x00\x0e\x0f\x0b")
                    .unwrap_or(b'\n'),
            ),
            19 => {
                for _ in 0..=r.below(3) {
                    out.push(r.byte_in(0, 0xff));
                }
            }
            20 if r.chance(70) => out.push(0x1b),
            // An OSC string at and around the bytes kept of one: 16 with
            // prompt marks alone, `OSC_PAYLOAD_LIMIT` with events, links
            // or the palette.
            20 => {
                out.extend_from_slice(
                    r.pick(&[&b"\x1b]2;"[..], b"\x1b]133;A;"])
                        .unwrap_or_default(),
                );
                let length = if long && r.chance(20) {
                    OSC_PAYLOAD_LIMIT
                        .saturating_sub(4)
                        .saturating_add(r.below(8))
                } else {
                    r.below(24)
                };
                out.extend(std::iter::repeat_n(b'k', length));
                out.extend_from_slice(
                    r.pick(&[&b"\x07"[..], b"\x1b\\", b"\x18"])
                        .unwrap_or_default(),
                );
            }
            // A string of every kind, of any length, its data any bytes
            // but mostly printable, ended or not: taken in bulk where it
            // begins, and where a call goes on inside it.
            21 => {
                out.extend_from_slice(
                    r.pick(&[
                        &b"\x1b_"[..],
                        b"\x1bX",
                        b"\x1b^",
                        b"\x1bP1;2|",
                        b"\x1bP+q",
                        b"\x1bP$q",
                        b"\x1bP0;1q",
                        b"\x1b]2;",
                        b"\x1b]8;;",
                    ])
                    .unwrap_or_default(),
                );
                for _ in 0..r.below(if long { 3000 } else { 60 }) {
                    out.push(match r.below(40) {
                        0 => r.byte_in(0, 0x1f),
                        1 => 0x7f,
                        2 => r.byte_in(0x80, 0xff),
                        _ => r.byte_in(0x20, 0x7e),
                    });
                }
                if r.chance(80) {
                    out.extend_from_slice(r.pick(&[&b"\x1b\\"[..], b"\x07"]).unwrap_or_default());
                }
            }
            _ => {
                out.extend_from_slice(b"\x1b[");
                parameter(r, &mut out);
            }
        }
    }
    out
}

/// What a sink was given, each thing as its `Debug` shows it.
#[derive(Default)]
struct Log(Vec<String>);

impl Sink for Log {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.push(format!("reply {bytes:?}"));
    }
    fn event(&mut self, event: Event<'_>) {
        self.0.push(format!("event {event:?}"));
    }
    fn unhandled(&mut self, sequence: Unhandled<'_>) {
        self.0.push(format!("unhandled {sequence:?}"));
    }
}

/// `Parser::run` with every byte through `byte`, the general path every
/// fast path keeps, the budget's stops included.
fn scalar(
    parser: &mut Parser,
    bytes: &[u8],
    until_stop: bool,
    sink: &mut impl Sink,
) -> Result<Option<(usize, Stop)>, Error> {
    if bytes.is_empty() {
        return Ok(None);
    }
    parser.screen.begin();
    let (start, refused) = (crate::budget::spent(), parser.refused);
    let degrades = crate::degrades();
    parser.end = match until_stop {
        true => start.saturating_add(crate::budget::LIMIT.saturating_sub(parser.spent)),
        false => u64::MAX,
    };
    for (i, &byte) in bytes.iter().enumerate() {
        let framed = parser.byte(byte, sink).or_else(|| parser.over::<true>());
        if until_stop && let Some(stop) = framed {
            parser.spend_since(start, stop);
            parser.count_degrades(degrades);
            return Ok(Some((i.saturating_add(1), stop)));
        }
    }
    if until_stop {
        parser.spend_since(start, Stop::Ended);
    }
    parser.count_degrades(degrades);
    match parser.refused == refused {
        true => Ok(None),
        false => Err(Error::Capacity),
    }
}

/// Random options, each on or off.
fn options(r: &mut Rng) -> Options {
    let features = Feature::ALL.into_iter().filter(|_| r.chance(50));
    features
        .collect::<Options>()
        .with_identity(r.chance(50).then_some(Identity {
            name: "fux",
            version: "1.2.3",
        }))
}

/// Every fast path of `Parser::run` (printable runs, UTF-8 text, and
/// escape sequences read in a loop of their own) does exactly what the
/// general path, every byte through `byte`, does: random bytes, mostly
/// escape sequences of every shape, in random pieces, at random sizes and
/// options, through `process_with` and `process_until_stop`; after every
/// piece the two parsers' whole state, as `Debug` shows it, everything
/// their sinks were given, and what each call returned are the same.
#[test]
fn fast_paths_equal_the_general_path() -> Result<(), Error> {
    equal_to_the_general_path(0..3000, false)
}

/// As `fast_paths_equal_the_general_path`, on the permanent corpus: the
/// adversarial operations and the terminal-edge streams.
#[test]
fn fast_paths_equal_the_general_path_on_the_permanent_corpus() -> Result<(), Error> {
    for (rows, cols) in [(1, 1), (4, 12), (24, 80)] {
        for seed in 0..6 {
            let mut fast = Parser::new(Size::of(rows, cols), 8)?;
            let mut slow = fast.clone();
            let corpus = test_corpus::operations(seed, 4096).into_iter();
            for operation in corpus.chain(test_corpus::terminal_edge()) {
                let (mut a, mut b) = (Log::default(), Log::default());
                fast.process_with(&operation, &mut a)?;
                scalar(&mut slow, &operation, false, &mut b)?;
                assert_eq!(a.0, b.0, "{rows}x{cols} seed {seed}");
                let difference = first_difference(&format!("{fast:?}"), &format!("{slow:?}"));
                assert_eq!(difference, None, "{rows}x{cols} seed {seed}");
            }
        }
    }
    Ok(())
}

/// As `fast_paths_equal_the_general_path`, with OSC strings at and past
/// the most bytes kept of one, in larger pieces.
#[test]
fn fast_paths_equal_the_general_path_on_long_strings() -> Result<(), Error> {
    equal_to_the_general_path(0..40, true)
}

fn equal_to_the_general_path(cases: std::ops::Range<u64>, long: bool) -> Result<(), Error> {
    for case in cases {
        let mut r = Rng(case);
        let rows = u16::try_from(r.below(6)).unwrap_or(0).saturating_add(1);
        let cols = u16::try_from(r.below(12)).unwrap_or(0).saturating_add(1);
        let mut fast = Parser::with_options(Size::of(rows, cols), r.below(8), options(&mut r))?;
        let mut slow = fast.clone();
        let fragments = r.below(80).saturating_add(1);
        let input = sequences(&mut r, fragments, long);
        let mut rest = &input[..];
        while !rest.is_empty() {
            let size = if r.chance(50) {
                r.below(8)
            } else if long {
                r.below(40_000)
            } else {
                r.below(400)
            };
            let (piece, tail) = rest
                .split_at_checked(size.saturating_add(1))
                .unwrap_or((rest, &[]));
            rest = tail;
            let until_frame = r.chance(30);
            let (mut a, mut b) = (Log::default(), Log::default());
            let mut piece_left = piece;
            loop {
                let (got, want) = if until_frame {
                    (
                        fast.process_until_stop(piece_left, &mut a),
                        scalar(&mut slow, piece_left, true, &mut b),
                    )
                } else {
                    (
                        fast.process_with(piece_left, &mut a).map(|()| None),
                        scalar(&mut slow, piece_left, false, &mut b),
                    )
                };
                assert_eq!(got, want, "case {case}: {}", input.escape_ascii());
                match got {
                    Ok(Some((n, _))) => piece_left = piece_left.get(n..).unwrap_or_default(),
                    Ok(None) | Err(_) => break,
                }
            }
            let shown = input.escape_ascii().to_string();
            assert_eq!(a.0, b.0, "case {case}: {shown}");
            let difference = first_difference(&format!("{fast:?}"), &format!("{slow:?}"));
            assert_eq!(difference, None, "case {case}: {shown}");
        }
    }
    Ok(())
}

/// Where `a` and `b` first differ, with some of each around it.
fn first_difference(a: &str, b: &str) -> Option<String> {
    let at = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))?;
    let around = |s: &str| {
        let start = at.saturating_sub(300);
        let end = at.saturating_add(200);
        s.bytes()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(char::from)
            .collect::<String>()
    };
    Some(format!(
        "at {at}:\n  fast: …{}…\n  slow: …{}…",
        around(a),
        around(b)
    ))
}

/// Output that shows, leaves, clears and resets the alternate screen.
const SCREENS: &[&[u8]] = &[
    b"\x1b[?47h",
    b"\x1b[?47l",
    b"\x1b[?1047h",
    b"\x1b[?1047l",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1b[?1049s",
    b"\x1b[?1049r",
    b"\x1b[?6h\x1b[2;3r",
    b"\x1b[!p",
    b"\x1bc",
    b"\x1b7",
];

/// The alternate screen, made when a program first shows it, is the
/// screen made at once: random output, resizes (with reflow and without),
/// RIS, DECSTR and every way of showing and leaving it, beside a parser
/// whose alternate screen was made as it was made; after every step the
/// lazy parser, its alternate screen made then, is the eager one in all
/// its state as `Debug` shows it, and holds no more cells than it.
#[test]
fn the_alternate_screen_made_late_is_the_one_made_at_once() -> Result<(), Error> {
    for case in 0..1500u64 {
        let mut r = Rng(case.wrapping_add(1 << 32));
        let rows = u16::try_from(r.below(6)).unwrap_or(0).saturating_add(1);
        let cols = u16::try_from(r.below(12)).unwrap_or(0).saturating_add(1);
        let mut lazy = Parser::with_options(Size::of(rows, cols), r.below(8), options(&mut r))?;
        let mut eager = lazy.clone();
        eager.screen_mut().make_alternate()?;
        for step in 0..r.below(40) {
            let resize = r.chance(15).then(|| {
                let rows = u16::try_from(r.below(7)).unwrap_or(0).saturating_add(1);
                let cols = u16::try_from(r.below(13)).unwrap_or(0).saturating_add(1);
                Size::of(rows, cols)
            });
            let output = if r.chance(40) {
                r.pick(SCREENS).unwrap_or_default().to_vec()
            } else {
                let fragments = r.below(6).saturating_add(1);
                sequences(&mut r, fragments, false)
            };
            let (a, b) = match resize {
                Some(size) => (lazy.resize(size), eager.resize(size)),
                None => (lazy.process(&output), eager.process(&output)),
            };
            assert_eq!(a, b, "case {case} step {step}");
            assert!(
                lazy.screen().storage_cells() <= eager.screen().storage_cells(),
                "case {case} step {step}"
            );
            // As before the lazy screen, the eager one's is always made:
            // RIS leaves it unmade again (`Grid::unmade`).
            eager.screen_mut().make_alternate()?;
            let mut made = lazy.clone();
            made.screen_mut().make_alternate()?;
            let difference = first_difference(&format!("{made:?}"), &format!("{eager:?}"));
            assert_eq!(difference, None, "case {case} step {step}");
        }
    }
    Ok(())
}

/// The alternate screen holds no cells until a program shows it: not as
/// made, nor resized, reset (RIS, DECSTR) or left, its modes saved and
/// restored unset; then as many as the screen.
#[test]
fn the_alternate_screen_holds_no_cells_until_shown() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(50, 200), 0)?;
    assert_eq!(parser.screen().storage_cells(), 50 * 200);
    parser.resize(Size::of(40, 120))?;
    assert_eq!(parser.screen().storage_cells(), 40 * 120);
    parser.process(b"\x1bc\x1b[!p\x1b[?47l\x1b[?1047l\x1b[?1049l\x1b[?1049s\x1b[?1049r")?;
    parser.resize(Size::of(30, 100))?;
    parser.process(b"\x1bc")?;
    assert_eq!(parser.screen().storage_cells(), 30 * 100);
    parser.process(b"\x1b[?1049h")?;
    assert_eq!(parser.screen().storage_cells(), 2 * 30 * 100);
    // RIS with history makes both grids afresh, the alternate unmade: a
    // history the style table leaves room in for rows.
    let mut parser = Parser::new(Size::of(5, 10), 64 << 10)?;
    parser.process(b"\x1b[?1049h\x1b[?1049l")?;
    let lines: Vec<u8> = std::iter::repeat_n(*b"x\n", 20).flatten().collect();
    parser.process(&lines)?;
    parser.process(b"\x1bc")?;
    assert_eq!(parser.screen().storage_cells(), 5 * 10);
    Ok(())
}

/// The printable run, as the general path counts it: a byte at a time.
fn printable_slowly(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|b| (0x20..=0x7e).contains(*b))
        .count()
}

/// `printable`, found a word at a time, is the run counted a byte at a
/// time: every byte at every place of runs up to 24 long, after every
/// printable byte and before, and random bytes, mostly printable.
#[test]
fn printable_runs_are_found_a_word_at_a_time_as_a_byte_at_a_time() {
    for len in 1..=24usize {
        for at in 1..len {
            for byte in 0..=u8::MAX {
                for fill in *b" a~" {
                    let mut bytes = vec![fill; len];
                    if let Some(slot) = bytes.get_mut(at) {
                        *slot = byte;
                    }
                    assert_eq!(
                        printable(&bytes),
                        printable_slowly(&bytes),
                        "{len} bytes, {byte:#x} at {at}"
                    );
                }
            }
        }
    }
    let mut r = Rng(7);
    for _ in 0..20_000 {
        let len = r.below(40).saturating_add(1);
        let mut bytes: Vec<u8> = (0..len)
            .map(|_| {
                if r.chance(95) {
                    r.byte_in(0x20, 0x7e)
                } else {
                    r.byte_in(0, 0xff)
                }
            })
            .collect();
        if let Some(first) = bytes.first_mut() {
            *first = r.byte_in(0x20, 0x7e);
        }
        assert_eq!(printable(&bytes), printable_slowly(&bytes), "{bytes:?}");
    }
}

/// A frame ends, as `FrameScan` finds it, at the byte after which the
/// parser carrying the bytes out has synchronized output reset: random
/// bytes after a BSU, most escape sequences, with RM of several modes,
/// RIS, DECSTR and DECSCL (of a level fux-vt has, and one it has not)
/// among them, and XTSAVE holding the mode either way,
/// held a random piece more at a time and carried out a byte at a time;
/// and `process_until_stop`, reading them in random pieces as they come,
/// says `Stop::Ended` there.
#[test]
fn a_frame_scan_ends_where_the_parser_ends_the_frame() -> Result<(), Error> {
    for case in 0..3000 {
        let mut r = Rng(case);
        let mut parser = Parser::with_options(Size::of(4, 10), 8, options(&mut r))?;
        if r.chance(50) {
            parser.process(b"\x1b[?2026h\x1b[?2026s\x1b[?2026l")?;
        }
        assert_eq!(
            parser.process_until_stop(b"\x1b[?2026h", &mut Log::default()),
            Ok(Some((8, Stop::Begun)))
        );
        let fragments = r.below(40).saturating_add(1);
        let mut input = sequences(&mut r, fragments, false);
        for _ in 0..r.below(3) {
            let at = r.below(input.len().saturating_add(1));
            let end = r.pick(&[
                &b"\x1b[?25;2026l"[..],
                b"\x1bc",
                b"\x1b[!p",
                b"\x1b[?2026r",
                b"\x1b[61;1\"p",
                b"\x1b[63\"p",
            ]);
            let (head, tail) = input.split_at_checked(at).unwrap_or((&input, &[]));
            input = [head, end.unwrap_or_default(), tail].concat();
        }
        let mut scan = parser.frame_scan();
        let (mut held, mut found) = (0usize, None);
        while found.is_none() && held < input.len() {
            held = held
                .saturating_add(r.below(8))
                .saturating_add(1)
                .min(input.len());
            found = scan.end(input.get(..held).unwrap_or_default());
        }
        // Read as it comes, in random pieces: where `process_until_stop`
        // first says the frame ended.
        let (mut streamed, mut read, mut told) = (parser.clone(), 0usize, None);
        while told.is_none() && read < input.len() {
            let piece = r.below(8).saturating_add(1);
            let end = read.saturating_add(piece).min(input.len());
            let mut at = read;
            while let Some(rest) = input.get(at..end)
                && let Some((taken, frame)) =
                    streamed.process_until_stop(rest, &mut Log::default())?
            {
                at = at.saturating_add(taken);
                if frame == Stop::Ended {
                    told = Some(at);
                    break;
                }
            }
            read = end;
        }
        let mut ended = None;
        for (i, byte) in input.iter().enumerate() {
            parser.process(std::slice::from_ref(byte))?;
            if !parser.screen().mode(Mode::SynchronizedOutput) {
                ended = Some(i.saturating_add(1));
                break;
            }
        }
        assert_eq!(found, ended, "case {case}: {}", input.escape_ascii());
        assert_eq!(
            told,
            ended,
            "streamed, case {case}: {}",
            input.escape_ascii()
        );
    }
    Ok(())
}

/// `esc_in` finds the first ESC at every place in and past a word, and
/// none where there is none.
#[test]
fn esc_in_finds_the_first_esc() {
    for len in 0..20 {
        for at in 0..len {
            let mut bytes = vec![b'x'; len];
            for (i, b) in bytes.iter_mut().enumerate() {
                if i == at || i == at.saturating_add(3) {
                    *b = 0x1b;
                } else if i % 3 == 0 {
                    *b = 0x9b;
                }
            }
            assert_eq!(esc_in(&bytes), Some(at), "{len} {at}");
        }
        assert_eq!(esc_in(&vec![0x1a; len]), None);
    }
}

/// The word scans for strings, `control_in` and `string_end`, find the
/// byte a byte-at-a-time scan finds, at every place in and past a word,
/// among bytes of every kind.
#[test]
fn the_string_scans_find_what_a_byte_at_a_time_finds() {
    let mut r = Rng(7);
    for _ in 0..20_000 {
        let bytes: Vec<u8> = (0..r.below(40))
            .map(|_| match r.below(6) {
                0 => r.byte_in(0, 0x1f),
                1 => 0x7f,
                _ => r.byte_in(0, 0xff),
            })
            .collect();
        let control = bytes.iter().position(|&b| b < 0x20 || b == 0x7f);
        let end = bytes.iter().position(|&b| matches!(b, 0x18 | 0x1a | 0x1b));
        assert_eq!(control_in(&bytes), control, "{bytes:?}");
        assert_eq!(string_end(&bytes), end, "{bytes:?}");
    }
}

/// A scroll of many rows inside a region spends what it does: the region's
/// slots turned once, not once a row (SCR-8), and the rows brought in. At
/// 4,095 rows of one column, 100 IL 4095s are 8,190 cells and 8,190 slots
/// each, about 20 M twentieths of a unit in all, far under the budget, so
/// the call takes them without a stop; charged a slot a row a row, each
/// was 16.8 M slots, and the second stopped it.
#[test]
fn a_region_scroll_spends_its_slots_once() -> Result<(), Error> {
    let mut parser = Parser::new(Size::of(4095, 1), 0)?;
    let bytes: Vec<u8> = std::iter::repeat_n(&b"\x1b[4095L"[..], 100)
        .flatten()
        .copied()
        .collect();
    let start = crate::budget::spent();
    assert_eq!(
        parser.process_until_stop(&bytes, &mut Log::default())?,
        None
    );
    let spent = crate::budget::spent().saturating_sub(start);
    assert!(spent <= 100 * (8_190 * 5 + 8_190 * 20), "{spent}");
    Ok(())
}
