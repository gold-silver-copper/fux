//! A resize without reflow keeps every row history has as it was, and
//! every row it takes from the screen with the screen row's width and
//! cells: its identity, version, flags, rendition, cells, text and links
//! (`Grid::resized`, which moves most rows over as they are kept).
use fux_vt::{Feature, Options, Parser, Row, Size};

/// What a row is, cell by cell.
fn fingerprint(row: &Row<'_>) -> String {
    let cells: Vec<String> = (0..row.len())
        .map(|c| format!("{:?}{:?}", row.cell(c), row.link(c)))
        .collect();
    format!(
        "{:?} v{} wrapped {} continues {} {:?} prompt {} len {} text {} links {} {}",
        row.id(),
        row.version(),
        row.wrapped(),
        row.continues(),
        row.rendition(),
        row.starts_prompt(),
        row.len(),
        row.text_len(),
        row.has_links(),
        cells.join("|")
    )
}

#[test]
fn a_resize_without_reflow_keeps_each_row_it_puts_in_history()
-> Result<(), Box<dyn std::error::Error>> {
    let options: Options = (Feature::ALL.into_iter())
        .filter(|f| *f != Feature::Reflow)
        .collect();
    let mut parser = Parser::with_options(Size::new(6, 20)?, 1 << 20, options)?;
    // Rows of every kind: plain, wide glyphs, a cluster kept as text, a
    // link, a double-width row, a prompt, a soft-wrapped line, a blank, and
    // coloured cells.
    let rows: [&[u8]; 9] = [
        b"plain text",
        "wide \u{754c}\u{754c} glyphs".as_bytes(),
        "a family \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466} held".as_bytes(),
        b"\x1b]8;;https://example.com\x07linked\x1b]8;;\x07 after",
        b"\x1b#6double",
        b"\x1b]133;A\x07$ prompt",
        b"a line too long for one row of twenty columns",
        b"",
        b"\x1b[31;44mred on blue\x1b[m",
    ];
    for _ in 0..3 {
        for row in rows {
            parser.process(row)?;
            parser.process(b"\r\n")?;
        }
    }
    for size in [(4, 20), (9, 20), (6, 12), (3, 31), (6, 20), (2, 7)] {
        let before: Vec<(fux_vt::RowId, String)> = (parser.screen().rows())
            .map(|r| (r.id(), fingerprint(&r)))
            .collect();
        parser.resize(Size::new(size.0, size.1)?)?;
        let screen = parser.screen();
        let history: Vec<Row<'_>> = screen.rows().take(screen.history_len()).collect();
        assert!(!history.is_empty());
        for row in history {
            let was = before.iter().find(|(id, _)| *id == row.id());
            assert_eq!(
                was.map(|(_, f)| f.as_str()),
                Some(fingerprint(&row).as_str()),
                "{size:?}"
            );
        }
    }
    Ok(())
}
