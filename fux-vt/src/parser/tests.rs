use super::*;

#[test]
#[ignore = "explicit release-mode performance measurement"]
fn measure_ascii_run_against_scalar_dispatch() -> Result<(), Error> {
    let input = b"The quick brown fox: printable ASCII 0123456789 abcdefghijklmnopqrstuvwxyz\r\n"
        .repeat(100_000);
    let mut fast = Parser::new(24, 80, 10_000)?;
    let mut scalar = fast.clone();
    let mut fast_us = Vec::new();
    let mut scalar_us = Vec::new();
    let mut plateau = None;
    for run in 0..6 {
        let started = std::time::Instant::now();
        for chunk in std::hint::black_box(&input).chunks(8192) {
            fast.process(chunk)?;
        }
        let a = started.elapsed().as_micros();
        let started = std::time::Instant::now();
        for chunk in std::hint::black_box(&input).chunks(8192) {
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
