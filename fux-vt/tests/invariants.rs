//! Structural invariants of fux-vt's grid, checked after every operation
//! of the permanent corpora and of generated input.
mod corpus;
#[path = "corpus/pieces.rs"]
mod pieces;

#[path = "corpus/invariants.rs"]
mod invariants;
use fux_vt::{Feature, Identity, Options, Parser, Sink};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn terminal_edge_streams_preserve_primary_history_and_modes() -> Result {
    let mut p = Parser::new(23, 80, 40)?;
    let mut main = None;
    for (i, bytes) in corpus::terminal_edge().iter().enumerate() {
        p.process(bytes)?;
        invariants::check(&p);
        let s = p.screen();
        let text = s.window(0, 23, 80).text((0, 0), (22, 79), 2000, 4000)?;
        match i {
            0 => {
                assert!(text.starts_with("LINE-09\n"));
                assert!(text.ends_with("MAIN-END"));
                assert_eq!(s.history_len(), 8);
                main = Some(p.clone());
            }
            1 => {
                assert!(s.alternate_screen());
                assert_eq!(text.trim_end_matches('\n'), "ALTERNATE");
                assert_eq!(s.history_len(), 0);
            }
            2 => {
                assert!(!s.alternate_screen());
                if let Some(ref main) = main {
                    invariants::equal(&p, main);
                }
            }
            3 => {
                assert!(s.application_cursor());
                assert_eq!(text.trim_end_matches('\n'), "APP");
            }
            4 => {
                assert_eq!(text.trim_end_matches('\n'), "BEFORE22mAFTERjunkEND");
            }
            _ => {}
        }
    }
    Ok(())
}

/// The replies a parser sent.
#[derive(Default)]
struct Replies(Vec<u8>);
impl Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

/// The adversarial corpus, after the terminal-edge streams, fed whole and in
/// pieces of one to seven bytes: the two parsers agree (screens, history,
/// links, prompt marks, replies) and the invariants hold after every
/// operation. Without options; and with every feature, with and without
/// reflow, links opened and closed, prompts marked and cells
/// inserted and deleted among the operations, and the screen resized now
/// and then. Then one 160 KiB stream.
#[test]
fn the_adversarial_corpus_is_chunk_invariant_and_bounded() -> Result {
    let extra: [&[u8]; 11] = [
        b"\x1b]8;;http://a\x07",
        b"\x1b]8;id=k;http://b\x1b\\",
        b"\x1b]8;;\x07",
        b"\x1b]133;A\x07",
        b"\x1b]133;A;aid=1;cl=m\x1b\\",
        b"\x1b]133;L\x07",
        b"\x1b]133;C\x07",
        b"\x1b[2@",
        b"\x1b[3P",
        b"\x1b[4h",
        b"\x1b[4l",
    ];
    let every = Feature::ALL.into_iter().filter(|&f| f != Feature::Reflow);
    let every = every.collect::<Options>().with_identity(Some(Identity {
        name: "fux-vt",
        version: "1.2.3",
    }));
    for (options, extras) in [
        (Options::new(), &[][..]),
        (every, &extra[..]),
        (every.with(Feature::Reflow), &extra[..]),
    ] {
        for seed in 0..10 {
            for (rows, cols) in [(1, 1), (1, 12), (12, 1), (2, 3), (4, 12), (24, 80)] {
                let mut whole = Parser::with_options(rows, cols, 8, options)?;
                let mut split = whole.clone();
                let mut state = seed;
                let streams = corpus::terminal_edge().into_iter();
                for mut bytes in streams.chain(corpus::operations(seed, 4096)) {
                    let r = corpus::splitmix(&mut state);
                    if let Some(more) = extras.get(usize::try_from(r % 16)?) {
                        bytes.extend_from_slice(more);
                    }
                    let (mut a, mut b) = (Replies::default(), Replies::default());
                    whole.process_with(&bytes, &mut a)?;
                    let size = usize::try_from(r % 7)?.saturating_add(1);
                    for chunk in pieces::pieces(&bytes, size) {
                        split.process_with(chunk, &mut b)?;
                    }
                    assert_eq!(a.0, b.0, "replies");
                    if !extras.is_empty() && r.is_multiple_of(97) {
                        let (rows, cols) = (u16::try_from(r % 9)? + 1, u16::try_from(r % 31)? + 1);
                        whole.resize(rows, cols)?;
                        split.resize(rows, cols)?;
                    }
                    invariants::equal(&whole, &split);
                    invariants::check(&whole);
                }
            }
        }
    }
    let mut p = Parser::new(24, 80, 8)?;
    for operation in corpus::operations(1, 160 * 1024) {
        p.process(&operation)?;
        invariants::check(&p);
    }
    Ok(())
}

#[test]
fn generated_edits_resizes_and_arbitrary_bytes_preserve_grid_invariants() -> Result {
    let edits: &[&[u8]] = &[
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[@",
        b"\x1b[P",
        b"\x1b[X",
        b"\x1b[S",
        b"\x1b[T",
        b"\x1b[2J",
        b"\x1b[1J",
        b"\x1b[K",
        b"\x1b[1K",
        b"\x1b[2K",
        b"\x1b7",
        b"\x1b8",
        b"\x1b[?47h",
        b"\x1b[?47l",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[?6h",
        b"\x1b[?6l",
        b"\x1b[?7h",
        b"\x1b[?7l",
        b"\x1bM",
        b"\n",
        b"\r",
        b"\t",
        b"\x08",
        "界".as_bytes(),
        "e\u{301}".as_bytes(),
        b"123456789",
        b"\x1bc",
    ];
    for seed in 0..100 {
        let mut state = seed;
        let mut p = Parser::new(4, 8, (seed % 5) as usize)?;
        for _ in 0..1000 {
            let n = corpus::splitmix(&mut state);
            match n % 8 {
                0 => p.resize(1 + ((n >> 8) % 8) as u16, 1 + ((n >> 16) % 12) as u16)?,
                1 => p.process(format!("\x1b[{};{}H", (n >> 8) % 10, (n >> 16) % 15).as_bytes())?,
                2 => p.process(format!("\x1b[{};{}r", (n >> 8) % 10, (n >> 16) % 10).as_bytes())?,
                3 => p.process(&n.to_le_bytes())?,
                _ => p.process(
                    edits
                        .get(((n >> 8) % edits.len() as u64) as usize)
                        .copied()
                        .unwrap_or(b"x"),
                )?,
            }
            invariants::check(&p);
        }
    }
    Ok(())
}

/// A row's text stays within its budget however it is filled and resized:
/// clusters that do not fit keep what fits inline, in their one cell.
#[test]
fn a_rows_text_stays_within_its_budget_through_resizes() -> Result {
    let zalgo: String = std::iter::once('e')
        .chain(std::iter::repeat_n('\u{301}', 60))
        .collect();
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    for options in [Options::default(), Options::new().with(Feature::Reflow)] {
        let mut p = Parser::with_options(3, 40, 10, options)?;
        for _ in 0..3 {
            for _ in 0..20 {
                p.process(zalgo.as_bytes())?;
                p.process(family.as_bytes())?;
            }
            p.process(b"\r\n")?;
        }
        invariants::check(&p);
        for (rows, cols) in [(3, 5), (6, 2), (2, 80), (3, 1), (3, 40)] {
            p.resize(rows, cols)?;
            invariants::check(&p);
        }
        // Each cell holds one cluster, whole or cut to what fits inline.
        let screen = p.screen();
        for offset in 0..screen.history_len() + 3 {
            for cell in screen.row_from_bottom(offset).ok_or("row")?.cells() {
                let text = cell.contents();
                assert!(
                    zalgo.starts_with(text) || family.starts_with(text),
                    "{text:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn degenerate_reflows_keep_every_invariant() -> Result {
    for (rows, cols) in [(1, 1), (1, 40), (40, 1), (2, 2)] {
        let mut p = Parser::with_options(rows, cols, 10, Options::new().with(Feature::Reflow))?;
        p.process("\u{4f60}\u{597d}ab\r\n\u{1f600}x".as_bytes())?;
        p.resize(1, 1)?;
        invariants::check(&p);
        p.process("\u{4f60}z".as_bytes())?;
        p.resize(rows, cols)?;
        invariants::check(&p);
    }
    Ok(())
}
