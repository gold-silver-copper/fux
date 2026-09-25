//! Shared independent invariants for deterministic tests and cargo-fuzz.
use fux_vt::{Cell, Parser};
use std::collections::HashSet;

pub fn check(p: &Parser) {
    let s = p.screen();
    let (rows, cols) = s.size();
    assert!(rows > 0 && cols > 0);
    let (row, col) = s.cursor_position();
    assert!(
        row < rows && col <= cols,
        "cursor {row},{col} outside {rows}x{cols}"
    );
    let (top, bottom) = s.scroll_region();
    assert!(top <= bottom && bottom < rows);
    let mut ids = HashSet::new();
    // A count past usize would show as a missing row.
    for offset in 0..usize::from(rows).saturating_add(s.history_len()) {
        let row = s.row_from_bottom(offset);
        assert!(row.is_some(), "missing retained row");
        let Some(row) = row else {
            return;
        };
        assert!(ids.insert(row.id), "row identities alias");
        assert!(!row.cells.is_empty());
        for (i, cell) in row.cells.iter().enumerate() {
            assert!(cell.contents().len() <= 22);
            if cell.is_wide() {
                assert!(
                    i.checked_add(1)
                        .and_then(|j| row.cells.get(j))
                        .is_some_and(Cell::is_wide_continuation),
                    "orphan wide leader at {offset},{i}"
                );
            }
            if cell.is_wide_continuation() {
                assert!(
                    i.checked_sub(1)
                        .and_then(|i| row.cells.get(i))
                        .is_some_and(Cell::is_wide),
                    "orphan continuation at {offset},{i}"
                );
                assert!(!cell.has_contents());
            }
        }
    }
    let mark = s.mark();
    for offset in [0, 1, usize::MAX] {
        for width in [0, 1, cols.saturating_sub(1), cols, u16::MAX] {
            let w = s.window(offset, rows, width);
            assert!(w.offset <= s.history_len());
            assert!(w.cols <= cols && w.rows <= rows);
            if let Some(last) = w.cols.checked_sub(1) {
                for y in 0..w.rows {
                    assert!(!w.cell(y, last).is_some_and(Cell::is_wide));
                }
            }
        }
    }
    assert_eq!(mark, s.mark(), "reading windows changed the terminal");
}

pub fn equal(a: &Parser, b: &Parser) {
    let (a, b) = (a.screen(), b.screen());
    assert_eq!(a.size(), b.size());
    assert_eq!(a.cursor_position(), b.cursor_position());
    assert_eq!(a.attributes(), b.attributes());
    assert_eq!(a.autowrap(), b.autowrap());
    assert_eq!(a.origin_mode(), b.origin_mode());
    assert_eq!(a.scroll_region(), b.scroll_region());
    assert_eq!(a.hide_cursor(), b.hide_cursor());
    assert_eq!(a.application_cursor(), b.application_cursor());
    assert_eq!(a.bracketed_paste(), b.bracketed_paste());
    assert_eq!(a.alternate_screen(), b.alternate_screen());
    assert_eq!(a.mouse_protocol_mode(), b.mouse_protocol_mode());
    assert_eq!(a.mouse_protocol_encoding(), b.mouse_protocol_encoding());
    assert_eq!(a.history_len(), b.history_len());
    for offset in 0..usize::from(a.size().0).saturating_add(a.history_len()) {
        let (a, b) = (a.row_from_bottom(offset), b.row_from_bottom(offset));
        assert!(a.is_some() && b.is_some(), "missing row");
        let (Some(a), Some(b)) = (a, b) else {
            return;
        };
        assert_eq!(a.cells, b.cells, "row from bottom {offset}");
        assert_eq!(a.wrapped, b.wrapped, "row from bottom {offset}");
    }
}
