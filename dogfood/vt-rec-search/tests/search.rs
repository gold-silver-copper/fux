use vt_rec_search::{At, Index, Recording};

fn index(name: &str, bytes: &[u8]) -> Result<Index, String> {
    let mut index = Index::default();
    index.add(name, &Recording::read(bytes.to_vec())?, (6, 20))?;
    Ok(index)
}

/// (from, until, row, col) of each stay of `word`.
fn stays(index: &Index, word: &str) -> Vec<(At, At, u16, u16)> {
    index
        .search(word)
        .iter()
        .map(|h| (h.from, h.until, h.row, h.col))
        .collect()
}

#[test]
fn a_cast_tells_when_and_where_a_word_was_on_screen() -> Result<(), String> {
    let cast = [
        r#"{"version": 2, "width": 20, "height": 4}"#,
        r#"[0.5, "o", "\r\nHello world\r\n"]"#,
        // Placed by a cursor move, not by the byte stream's order.
        r#"[1.0, "o", "\u001b[3;10Hneedle\u001b[1;1H"]"#,
        // A line down from the bottom: Hello scrolls up but stays.
        r#"[2.0, "o", "\u001b[4;1H\n"]"#,
        r#"[2.5, "o", "\n\n"]"#,
        r#"[3.0, "o", "\u001b[2J"]"#,
        r#"[3.5, "r", "10x2"]"#,
        r#"[4.0, "o", "日本 wide"]"#,
    ]
    .join("\n");
    let index = index("t.cast", cast.as_bytes())?;
    // One stay, though it scrolled up a row at 2 s, then out at 2.5 s.
    assert_eq!(
        stays(&index, "HELLO"),
        [(At::Secs(0.5), At::Secs(2.5), 1, 0)]
    );
    assert_eq!(
        stays(&index, "needle"),
        [(At::Secs(1.0), At::Secs(2.5), 2, 9)]
    );
    // The wide glyphs take two columns each, on the resized screen.
    assert_eq!(
        stays(&index, "wide"),
        [(At::Secs(4.0), At::Secs(4.0), 1, 5)]
    );
    assert_eq!(index.search("ne*").len(), 1);
    let screen = index
        .screen_at(0, At::Secs(1.2))
        .ok_or("a screen at 1.2 s")?;
    assert_eq!(
        screen.rows.iter().map(|r| &**r).collect::<Vec<_>>(),
        ["", "Hello world", "         needle", ""]
    );
    Ok(())
}

#[test]
fn a_raw_log_indexes_what_the_screen_showed_not_the_bytes() -> Result<(), String> {
    // "abc" is overwritten on its own line before any line ends; the
    // alternate screen hides the main screen's words while it is shown.
    let raw = b"abc\rjello\n\x1b[?1049h\x1b[2;3Hhidden\n\x1b[?1049lback\n";
    let index = index("raw.log", raw)?;
    assert!(
        index.search("abc").is_empty(),
        "never on screen when looked at"
    );
    let end = At::Byte(raw.len() as u64);
    assert_eq!(end, At::Byte(44));
    assert_eq!(
        stays(&index, "jello"),
        [(At::Byte(10), At::Byte(31), 0, 0), (end, end, 0, 0)]
    );
    assert_eq!(stays(&index, "hidden"), [(At::Byte(31), end, 1, 2)]);
    // Where 1049 saved the cursor: a bare line feed keeps the column.
    assert_eq!(stays(&index, "back"), [(end, end, 1, 5)]);
    Ok(())
}
