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
        rendition: if id % 5 == 4 {
            LineRendition::DoubleWidth
        } else {
            LineRendition::Single
        },
        cells,
        width,
    }
}

/// What a reader finds of each row: its identity, version, flags (whether
/// it is soft-wrapped; whether a prompt starts on it, and whether it is
/// double width), width and cells.
type Found = (u64, u64, bool, (bool, bool), u16, Vec<Compact>);
fn found(history: &History) -> Vec<Found> {
    (0..history.len())
        .filter_map(|index| history.get(index))
        .map(|row| {
            (
                row.kept.id.0,
                row.kept.version,
                row.kept.wrapped(),
                (row.kept.prompt(), row.rendition.is_double()),
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
/// go past the budget; and the bytes counted are those the rows keep: 24
/// a row, 8 a cell.
#[test]
fn rows_read_back_as_they_came_while_the_oldest_go() -> Result<(), Error> {
    let budget = 64 << 10;
    let mut history = History::new(budget);
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
            (id.is_multiple_of(3), id % 5 == 4),
            width,
            cells,
        ));
        history.trim();
        while expected.len() > history.len() {
            expected.pop_front();
        }
        assert_eq!(
            found(&history),
            Vec::from(expected.clone()),
            "after row {id}"
        );
        let kept: usize = expected.iter().map(|row| row.5.len()).sum();
        assert_eq!(history.kept_cells(), kept);
        // What it counts is what it holds, and within the budget.
        assert_eq!(history.bytes(), history.heap());
        assert!(history.bytes() <= budget);
    }
    let last = history.len().saturating_sub(1);
    assert_eq!(history.position(RowId(2999)), Some(last));
    assert_eq!(history.position(RowId(0)), None);
    Ok(())
}

/// Blocks the rows have left are let go: the history's memory stops
/// growing at its budget, and fills most of it.
#[test]
fn memory_stops_growing_at_the_budget() -> Result<(), Error> {
    let budget = 256 << 10;
    let mut history = History::new(budget);
    for round in 0..4 {
        for id in 0..2000u64 {
            let cells = row(usize::try_from(id % 120).unwrap_or(0), b'a', 0);
            history.push(arriving(id, &cells, 120))?;
            history.trim();
            assert!(
                history.heap() <= budget,
                "round {round}: {}",
                history.heap()
            );
        }
        // Less than two blocks and the ring's doubling short of it.
        assert!(
            history.heap() > budget / 2,
            "round {round}: {}",
            history.heap()
        );
    }
    Ok(())
}

/// Block numbers are 16 bits and wrap (`BLOCKS`, HAR-37): a long-lived
/// pane makes 65,536 blocks in about 268 M cells. Numbered from just below
/// the wrap, rows of a block each read back as they came across it, while
/// the oldest go past the budget, and the bytes counted stay those held.
#[test]
fn block_numbers_wrap() -> Result<(), Error> {
    let budget = 256 << 10;
    let mut history = History::new(budget);
    history.first_block = u16::MAX - 3;
    history.newest_block = u16::MAX - 3;
    let mut expected = std::collections::VecDeque::new();
    for id in 0..200u64 {
        // Each row more than half a block: a block of its own.
        let len = (BLOCK / 2).saturating_add(usize::try_from(id % 7).unwrap_or(0));
        let first = b'a'.wrapping_add(u8::try_from(id % 26).unwrap_or(0));
        let cells = row(len, first, 0);
        let width = u16::try_from(len).unwrap_or(u16::MAX);
        history.push(arriving(id, &cells, width))?;
        expected.push_back((
            id,
            id.wrapping_mul(3),
            id % 2 == 1,
            (id.is_multiple_of(3), id % 5 == 4),
            width,
            cells,
        ));
        history.trim();
        while expected.len() > history.len() {
            expected.pop_front();
        }
        assert_eq!(
            found(&history),
            Vec::from(expected.clone()),
            "after row {id}"
        );
        assert_eq!(history.bytes(), history.heap());
        assert!(history.bytes() <= budget);
    }
    assert!(
        history.first_block < 1000,
        "never wrapped: {}",
        history.first_block
    );
    assert!(history.len() > 1);
    Ok(())
}
