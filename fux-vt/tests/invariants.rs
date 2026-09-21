mod corpus;
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
                let mut encoded = vec![(rows - 1) as u8, (cols - 1) as u8, 8];
                for op in &operations {
                    for bytes in op.chunks(254) {
                        encoded.push((bytes.len() - 1) as u8);
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
                let size = (corpus::splitmix(&mut state) % 7 + 1) as usize;
                for chunk in operation.chunks(size) {
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
