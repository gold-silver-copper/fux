use super::*;

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
                    scalar.byte(*byte, &mut |r| b.push(r.to_vec()))?;
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
