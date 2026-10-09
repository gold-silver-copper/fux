//! Shared independent invariants for deterministic tests and cargo-fuzz.
use fux_vt::{CLUSTER_CAPACITY, Cells, Mode, Parser};

fn cells(row: fux_vt::Row<'_>) -> Vec<fux_vt::CellRef<'_>> {
    row.cells().collect()
}
use std::collections::HashSet;

pub fn check(p: &Parser) {
    let s = p.screen();
    let (rows, cols) = s.size().into();
    let mut ids = HashSet::new();
    assert_eq!(
        s.rows().len(),
        usize::from(rows).saturating_add(s.history_len())
    );
    for row in s.rows() {
        let offset = row.index();
        assert!(ids.insert(row.id()), "row identities alias");
        assert!(!row.is_empty());
        // The row's text stays within its budget.
        assert!(
            row.text_len() <= Cells::text_limit(row.len()),
            "row {offset}: {} bytes of text in {} cells",
            row.text_len(),
            row.len()
        );
        // Copied cell by cell, with their text, the row is the same.
        let copy: Cells = row.cells().collect();
        assert!(
            copy.iter().eq(row.cells()),
            "row {offset} copies differently"
        );
        for (i, cell) in row.cells().enumerate() {
            assert!(cell.contents().len() <= CLUSTER_CAPACITY);
            // A cell with contents shows them: its text is where it says.
            if cell.has_contents() {
                assert!(
                    !cell.contents().is_empty(),
                    "row {offset}: cell {i} lost its text"
                );
            }
            if cell.is_wide() {
                assert!(
                    i.checked_add(1)
                        .and_then(|j| row.cell(j))
                        .is_some_and(|c| c.is_wide_continuation()),
                    "orphan wide leader at {offset},{i}"
                );
            }
            if cell.is_wide_continuation() {
                assert!(
                    i.checked_sub(1)
                        .and_then(|i| row.cell(i))
                        .is_some_and(|c| c.is_wide()),
                    "orphan continuation at {offset},{i}"
                );
                assert!(!cell.has_contents());
            }
            // A link is a glyph's: a blank cell has none, a wide glyph's
            // halves have one, and it is within the spec's limits.
            let link = row.link(i);
            if let Some(link) = link {
                assert!(row.has_links());
                assert!(cell.has_contents() || cell.is_wide_continuation());
                assert!(!link.uri().is_empty() && link.uri().len() <= fux_vt::URI_LIMIT);
                assert!(
                    link.id()
                        .is_none_or(|id| !id.is_empty() && id.len() <= fux_vt::ID_LIMIT)
                );
                let printable = |s: &str| s.bytes().all(|b| (0x20..=0x7e).contains(&b));
                assert!(printable(link.uri()) && link.id().is_none_or(printable));
            }
            if cell.is_wide_continuation() {
                assert_eq!(link, i.checked_sub(1).and_then(|i| row.link(i)));
            }
        }
    }
    let mark = s.mark();
    for w in s.rows().map(|row| row.window()) {
        assert_eq!((w.rows(), w.cols()), (rows, cols));
        if let Some(last) = cols.checked_sub(1) {
            for y in 0..rows {
                assert!(!w.cell(y, last).is_some_and(|c| c.is_wide()));
            }
        }
    }
    assert_eq!(mark, s.mark(), "reading windows changed the terminal");
}

pub fn equal(a: &Parser, b: &Parser) {
    let (a, b) = (a.screen(), b.screen());
    assert_eq!(a.size(), b.size());
    assert_eq!(a.cursor_position(), b.cursor_position());
    assert_eq!(a.pending_wrap(), b.pending_wrap());
    assert_eq!(a.attributes(), b.attributes());
    assert_eq!(a.scroll_region(), b.scroll_region());
    assert_eq!(a.mouse_protocol_mode(), b.mouse_protocol_mode());
    assert_eq!(a.mouse_protocol_encoding(), b.mouse_protocol_encoding());
    assert_eq!(a.kitty_keyboard_flags(), b.kitty_keyboard_flags());
    assert_eq!(a.modify_other_keys(), b.modify_other_keys());
    for mode in [
        Mode::Autowrap,
        Mode::Origin,
        Mode::ShowCursor,
        Mode::ApplicationCursor,
        Mode::BracketedPaste,
        Mode::AlternateScreen,
        Mode::ColorSchemeUpdates,
    ] {
        assert_eq!(a.mode(mode), b.mode(mode), "{mode:?}");
    }
    assert_eq!(a.history_len(), b.history_len());
    assert_eq!(a.rows().len(), b.rows().len());
    for (a, b) in a.rows().zip(b.rows()) {
        let offset = a.index();
        assert_eq!(cells(a), cells(b), "row {offset}");
        assert_eq!(a.wrapped(), b.wrapped(), "row {offset}");
        assert_eq!(links(a), links(b), "row {offset}");
        assert_eq!(a.starts_prompt(), b.starts_prompt(), "row {offset}");
    }
    assert_eq!(a.hyperlink(), b.hyperlink());
}

/// Each cell's link: its URI, id and key.
fn links(row: fux_vt::Row<'_>) -> Vec<Option<(&str, Option<&str>, u64)>> {
    (0..row.len())
        .map(|col| row.link(col).map(|l| (l.uri(), l.id(), l.key())))
        .collect()
}
