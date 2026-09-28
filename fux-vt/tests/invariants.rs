mod corpus;
#[path = "corpus/fixtures.rs"]
mod fixtures;
#[path = "corpus/pieces.rs"]
mod pieces;

#[test]
fn seed_fuzz_with_golden_terminal_edge_and_tiny_operations() -> Result {
    let Ok(directory) = std::env::var("FUX_VT_FUZZ_CORPUS") else {
        return Ok(());
    };
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory)?;
    // Operation bytes 0xfd to 0xff are the target's own: raw pieces are at
    // most 253 bytes.
    let seed_with = |name: &str, header: [u8; 3], operations: &[&[u8]]| -> std::io::Result<()> {
        let mut encoded = header.to_vec();
        for operation in operations {
            for bytes in pieces::pieces(operation, 253) {
                encoded.push(u8::try_from(bytes.len() - 1).map_err(std::io::Error::other)?);
                encoded.extend_from_slice(bytes);
            }
        }
        // Explicitly exercise tiny resize followed by bounded window/copy.
        encoded.extend_from_slice(&[255, 0, 0, 254, 0, 1, 1, 0, 0, 0, 0, 1]);
        encoded.truncate(4096);
        std::fs::write(directory.join(name), encoded)
    };
    let seed = |name: &str, operations: &[&[u8]]| seed_with(name, [3, 11, 8], operations);
    for (name, operations) in fixtures::CASES {
        seed(&format!("fixture-{name}"), operations)?;
    }
    let terminal = corpus::terminal_edge();
    seed(
        "fixture-terminal-edge",
        &terminal.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )?;
    seed(
        "fixture-tiny",
        &["\x1bc界ABCD\r\nZ\x1b[1;1r\x1b[S\x1b[T".as_bytes()],
    )?;
    let mut history_copy = vec![1, 4, 2, 13];
    history_copy.extend_from_slice(b"abcdefgh\r\nlast");
    history_copy.extend_from_slice(&[255, 1, 9, 254, 1, 2, 10, 0, 0, 1, 9, 20]);
    std::fs::write(directory.join("fixture-history-copy"), history_copy)?;

    // Grapheme clusters, the new SGR and cursor sequences, and each option:
    // the rows byte's high bits choose reflow (0x10), the kitty keyboard
    // protocol (0x20) and an identity (0x40).
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let kiss =
        "\u{1F469}\u{1F3FD}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F468}\u{1F3FB}";
    let scotland = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
    let zalgo: String = std::iter::once('e')
        .chain(std::iter::repeat_n('\u{301}', 70))
        .collect();
    let clusters = format!(
        "{family}|{kiss}|{scotland}|{zalgo}|\u{1F1EF}\u{1F1F5}\u{1F1FA}|\u{915}\u{94D}\u{937}\u{93F}|\u{1100}\u{1161}\u{11A8}|\u{600}a\u{13437}\u{301}\u{17D8}x"
    );
    seed("fixture-graphemes", &[clusters.as_bytes()])?;
    seed_with(
        "fixture-graphemes-narrow",
        [0, 1, 2],
        &[clusters.as_bytes()],
    )?;
    let mut reflow = vec![0x13, 11, 8];
    for piece in [clusters.as_bytes(), b"\r\n", clusters.as_bytes()] {
        for bytes in pieces::pieces(piece, 253) {
            reflow.push(u8::try_from(bytes.len() - 1)?);
            reflow.extend_from_slice(bytes);
        }
    }
    reflow.extend_from_slice(&[255, 3, 2, 255, 5, 23, 255, 1, 0, 255, 3, 11]);
    std::fs::write(directory.join("fixture-graphemes-reflow"), reflow)?;
    // The grapheme operation, picking every character of its table once.
    let mut picks = vec![0x13, 11, 8, 0xfd, 63];
    picks.extend(0..63u8);
    picks.extend_from_slice(&[0xfd, 40]);
    picks.extend((0..40u8).map(|i| i.wrapping_mul(7)));
    std::fs::write(directory.join("fixture-graphemes-operation"), picks)?;
    seed_with(
        "fixture-kitty-identity",
        [0x63, 11, 0x38],
        &[
            b"\x1b[?u\x1b[>1u\x1b[>5u\x1b[=3;2u\x1b[?u\x1b[?1049h\x1b[>8u\x1b[?u\x1b[?1049l\x1b[<2u\x1b[?u",
            b"\x1b[>4;2m\x1b[>4m\x1b[>1;2m\x1b[c\x1b[>c\x1b[>q\x1b[?6nabcdefghijkl\x1b[6n\x1bc",
        ],
    )?;
    seed(
        "fixture-sgr-and-cursor",
        &[
            b"\x1b[5ma\x1b[6mb\x1b[25;8mc\x1b[28;9md\x1b[29;58;2;1;2;3me\x1b[58:5:9mf\x1b[59mg\x1b[m",
            b"\x1b[2;3fX\x1b[s\x1b[1;4;38;5;208mY\x1b[4;1HZ\x1b[uW\x1b[3J\x1b[?5W\x1b(B",
        ],
    )?;
    Ok(())
}
#[path = "corpus/invariants.rs"]
mod invariants;
use fux_vt::Parser;
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

#[test]
fn permanent_adversarial_corpus_is_chunk_invariant_and_bounded() -> Result {
    // Same generator/seeds as the temporary differential phase, plus one full
    // 160 KiB harness stream. No expected results are generated from fux-vt.
    for seed in 0..20 {
        for (rows, cols) in [(1, 1), (1, 12), (12, 1), (2, 2), (4, 12), (24, 80)] {
            let mut whole = Parser::new(rows, cols, 8)?;
            let mut split = Parser::new(rows, cols, 8)?;
            let mut state = seed;
            let operations = corpus::operations(seed, 4096);
            if let Ok(directory) = std::env::var("FUX_VT_FUZZ_CORPUS") {
                let path = std::path::Path::new(&directory);
                std::fs::create_dir_all(path)?;
                let mut encoded = vec![u8::try_from(rows - 1)?, u8::try_from(cols - 1)?, 8];
                for op in &operations {
                    for bytes in pieces::pieces(op, 253) {
                        encoded.push(u8::try_from(bytes.len() - 1)?);
                        encoded.extend_from_slice(bytes);
                    }
                }
                encoded.truncate(4096);
                std::fs::write(
                    path.join(format!("adversarial-{seed}-{rows}x{cols}")),
                    encoded,
                )?;
            }
            for operation in operations {
                whole.process(&operation)?;
                let size = usize::try_from(corpus::splitmix(&mut state) % 7 + 1)?;
                for chunk in pieces::pieces(&operation, size) {
                    split.process(chunk)?;
                }
                invariants::equal(&whole, &split);
                invariants::check(&whole);
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
