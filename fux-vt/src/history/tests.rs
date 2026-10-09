use super::*;

/// A row of `len` cells, each a letter from `first` on, in style `style`.
fn row(len: usize, first: u8, style: u32) -> Vec<Compact> {
    (0..len)
        .map(|i| {
            let letter = u8::try_from(i % 26).unwrap_or(0);
            Compact::ascii(first.wrapping_add(letter), style)
        })
        .collect()
}

fn arriving(id: u64, cells: &[Compact], width: u16) -> Arriving<'_> {
    Arriving {
        id: RowId(id),
        version: id.wrapping_mul(3),
        wrapped: id % 2 == 1,
        prompt: id.is_multiple_of(3),
        cells,
        width,
    }
}

/// What a reader finds of each row: its identity, version, flags, width
/// and cells.
fn found(history: &History) -> Vec<(u64, u64, bool, bool, u16, Vec<Compact>)> {
    (0..history.len())
        .filter_map(|index| history.get(index))
        .map(|row| {
            (
                row.kept.id.0,
                row.kept.version,
                row.kept.wrapped(),
                row.kept.prompt(),
                row.kept.width(),
                row.cells.to_vec(),
            )
        })
        .collect()
}

/// A row's cells are those before its blank tail; a coloured blank is no
/// part of the tail.
#[test]
fn a_row_keeps_the_cells_before_its_blank_tail() {
    let mut cells = row(5, b'a', 0);
    cells.extend([BLANK; 3]);
    assert_eq!(trimmed(&cells).len(), 5);
    cells.push(Compact::blank(7));
    cells.push(BLANK);
    assert_eq!(trimmed(&cells).len(), 9);
    assert!(trimmed(&[BLANK; 4]).is_empty());
    assert!(trimmed(&[]).is_empty());
}

/// Rows of every length, none, one, many, and longer than a block, read
/// back as they came, in order, through block after block, as the oldest
/// go; and the cells counted are those the rows keep.
#[test]
fn rows_read_back_as_they_came_while_the_oldest_go() -> Result<(), Error> {
    let limit = 40;
    let mut history = History::new(limit);
    let mut expected = std::collections::VecDeque::new();
    for id in 0..3000u64 {
        // Rows end anywhere in a block, now and then one longer than a
        // block.
        let len = match id % 11 {
            0 => 0,
            1 => 1,
            2 if id % 33 == 2 => BLOCK.saturating_add(5),
            n => usize::try_from(n).unwrap_or(0).saturating_mul(31),
        };
        let first = b'a'.wrapping_add(u8::try_from(id % 26).unwrap_or(0));
        let cells = row(len, first, u32::try_from(id % 5).unwrap_or(0));
        let width = u16::try_from(len.max(80)).unwrap_or(u16::MAX);
        history.push(arriving(id, &cells, width))?;
        expected.push_back((
            id,
            id.wrapping_mul(3),
            id % 2 == 1,
            id.is_multiple_of(3),
            width,
            cells,
        ));
        if history.len() > limit {
            history.pop();
            expected.pop_front();
        }
        assert_eq!(
            found(&history),
            Vec::from(expected.clone()),
            "after row {id}"
        );
        let kept: usize = expected.iter().map(|row| row.5.len()).sum();
        assert_eq!(history.kept_cells(), kept);
    }
    let last = history.len().saturating_sub(1);
    assert_eq!(history.position(RowId(2999)), Some(last));
    assert_eq!(history.position(RowId(0)), None);
    Ok(())
}

/// Blocks the rows have left are let go, but for the spare: the history's
/// memory stops growing once it holds as many rows as its limit.
#[test]
fn memory_stops_growing_at_the_limit() -> Result<(), Error> {
    let limit = 500;
    let mut history = History::new(limit);
    let mut most = 0;
    for round in 0..4 {
        for id in 0..2000u64 {
            let cells = row(usize::try_from(id % 120).unwrap_or(0), b'a', 0);
            history.push(arriving(id, &cells, 120))?;
            if history.len() > limit {
                history.pop();
            }
        }
        if round == 0 {
            most = history.heap();
        }
        assert!(
            history.heap() <= most,
            "round {round}: {} > {most}",
            history.heap()
        );
    }
    Ok(())
}
