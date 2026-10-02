use super::*;

/// `input` in 8 KiB pieces, the last one shorter.
fn pieces(input: &[u8]) -> impl Iterator<Item = &[u8]> {
    let (whole, rest) = input.as_chunks::<8192>();
    whole.iter().map(|piece| piece.as_slice()).chain([rest])
}

#[test]
#[ignore = "explicit release-mode performance measurement"]
fn measure_ascii_run_against_scalar_dispatch() -> Result<(), Error> {
    let line = b"The quick brown fox: printable ASCII 0123456789 abcdefghijklmnopqrstuvwxyz\r\n";
    let input: Vec<u8> = std::iter::repeat_n(&line[..], 100_000)
        .flatten()
        .copied()
        .collect();
    let mut fast = Parser::new(24, 80, 10_000)?;
    let mut scalar = fast.clone();
    let mut fast_us = Vec::new();
    let mut scalar_us = Vec::new();
    let mut plateau = None;
    for run in 0..6 {
        let started = std::time::Instant::now();
        for chunk in pieces(std::hint::black_box(&input)) {
            fast.process(chunk)?;
        }
        let a = started.elapsed().as_micros();
        let started = std::time::Instant::now();
        for chunk in pieces(std::hint::black_box(&input)) {
            scalar.screen.begin()?;
            for &byte in chunk {
                scalar.byte(byte, &mut Replies(|_: &[u8]| {}))?;
            }
        }
        let b = started.elapsed().as_micros();
        let storage = fast.screen().storage_cells();
        if let Some(previous) = plateau {
            assert_eq!(storage, previous);
        }
        plateau = Some(storage);
        if run > 0 {
            fast_us.push(a);
            scalar_us.push(b);
        }
    }
    assert_eq!(
        fast.screen().cursor_position(),
        scalar.screen().cursor_position()
    );
    for offset in 0..fast.screen().history_len() + 24 {
        assert_eq!(
            fast.screen()
                .row_from_bottom(offset)
                .ok_or(Error::InvalidRange)?
                .cells,
            scalar
                .screen()
                .row_from_bottom(offset)
                .ok_or(Error::InvalidRange)?
                .cells
        );
    }
    println!(
        "PARSER-TIMES {{\"bytes_per_run\":{},\"warmups\":1,\"fast_us\":{fast_us:?},\"scalar_us\":{scalar_us:?},\"plateau_cells\":{},\"cell_bytes\":{}}}",
        input.len(),
        fast.screen().storage_cells(),
        std::mem::size_of::<crate::Cell>()
    );
    Ok(())
}

#[test]
fn ascii_run_path_equals_scalar_dispatch_on_the_permanent_corpus() -> Result<(), Error> {
    for (rows, cols) in [(1, 1), (1, 12), (4, 12), (24, 80)] {
        for seed in 0..20 {
            let mut fast = Parser::new(rows, cols, 8)?;
            let mut scalar = fast.clone();
            for operation in test_corpus::operations(seed, 4096)
                .into_iter()
                .chain(test_corpus::terminal_edge())
            {
                let mut a = Vec::new();
                let mut b = Vec::new();
                fast.process_with_replies(&operation, |r| a.push(r.to_vec()))?;
                scalar.screen.begin()?;
                for byte in &operation {
                    scalar.byte(*byte, &mut Replies(|r: &[u8]| b.push(r.to_vec())))?;
                }
                assert_eq!(a, b);
                let (a, b) = (fast.screen(), scalar.screen());
                assert_eq!(a.cursor_position(), b.cursor_position());
                assert_eq!(a.pending_wrap(), b.pending_wrap());
                assert_eq!(a.attributes(), b.attributes());
                assert_eq!(a.history_len(), b.history_len());
                for offset in 0..usize::from(rows) + a.history_len() {
                    let (a, b) = (
                        a.row_from_bottom(offset).ok_or(Error::InvalidRange)?,
                        b.row_from_bottom(offset).ok_or(Error::InvalidRange)?,
                    );
                    assert_eq!(
                        a.cells, b.cells,
                        "{rows}x{cols} seed={seed} offset={offset}"
                    );
                    assert_eq!(a.wrapped, b.wrapped);
                }
            }
        }
    }
    Ok(())
}

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
        let mut fast = Parser::new(6, cols, 8)?;
        let mut scalar = fast.clone();
        fast.process(text.as_bytes())?;
        scalar.screen.begin()?;
        for byte in text.as_bytes() {
            scalar.byte(*byte, &mut Replies(|_: &[u8]| {}))?;
        }
        let (a, b) = (fast.screen(), scalar.screen());
        assert_eq!(a.cursor_position(), b.cursor_position(), "{cols} columns");
        assert_eq!(a.pending_wrap(), b.pending_wrap(), "{cols} columns");
        for offset in 0..6 + a.history_len() {
            assert_eq!(
                a.row_from_bottom(offset).map(|r| r.cells),
                b.row_from_bottom(offset).map(|r| r.cells),
                "{cols} columns, row {offset} from the bottom"
            );
        }
    }
    Ok(())
}
