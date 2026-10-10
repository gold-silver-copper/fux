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
        match r.below(22) {
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
            16 => out.extend_from_slice(
                r.pick(&["é", "界", "\u{301}", "👍", "🇺🇸", "\u{fe0f}"])
                    .unwrap_or_default()
                    .as_bytes(),
            ),
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
/// fast path keeps. As before any fast path but printable ASCII's, an `h`
/// with a frame begun ends `process_until_frame` unless it is printed.
fn scalar(
    parser: &mut Parser,
    bytes: &[u8],
    until_frame: bool,
    sink: &mut impl Sink,
) -> Result<Option<usize>, Error> {
    if bytes.is_empty() {
        return Ok(None);
    }
    parser.screen.begin();
    for (i, &byte) in bytes.iter().enumerate() {
        if parser.byte(byte, sink)? && until_frame {
            return Ok(Some(i.saturating_add(1)));
        }
    }
    Ok(None)
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
/// options, through `process_with` and `process_until_frame`; after every
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
                        fast.process_until_frame(piece_left, &mut a),
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
                    Ok(Some(n)) => piece_left = piece_left.get(n..).unwrap_or_default(),
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
    // RIS with history makes both grids afresh, the alternate unmade.
    let mut parser = Parser::new(Size::of(5, 10), 100)?;
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
