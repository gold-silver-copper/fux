//! Unicode's own data files, printed through the parser into cells: each
//! test checks only what its file decides, and says whether that is
//! normative or fux-vt's policy. `fux-vt-gen` writes the files' cases into
//! `tests/data` from the version the tables follow (17.0.0). UTF-8
//! decoding, the fifth file's assertion (The Unicode Standard, 3.9, Tables
//! 3-8 to 3-11), is `conformance::utf8_tables_3_8_to_3_11_substitute_maximal_subparts`.

use fux_vt::{Parser, Size, UNICODE_VERSION, char_width};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A file's cases, after the header naming the version the tables follow.
fn cases(text: &'static str, name: &str) -> Result<impl Iterator<Item = &'static str>> {
    let (major, minor, update) = UNICODE_VERSION;
    let header = format!("# {name}-{major}.{minor}.{update}.txt");
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.starts_with(&header)) {
        return Err(format!("tests/data/{name}.txt is not {header}").into());
    }
    Ok(lines.filter(|l| !l.is_empty()))
}

/// Code points in hex, space-separated, as text.
fn text(hex: &str) -> String {
    hex.split_whitespace()
        .filter_map(|h| u32::from_str_radix(h, 16).ok())
        .filter_map(char::from_u32)
        .collect()
}

/// `text` printed alone from the first column of a row wide enough.
fn printed(text: &str) -> Result<Parser> {
    let mut parser = Parser::new(Size::new(1, 200)?, 0)?;
    parser.process(text.as_bytes())?;
    Ok(parser)
}

/// The cells a row holds, each a glyph's text and its width, the second
/// halves of wide glyphs left out, up to the cursor.
fn glyphs(parser: &Parser) -> Vec<(String, usize)> {
    let screen = parser.screen();
    let end = usize::from(screen.cursor_position().1);
    (0..end)
        .filter_map(|col| screen.cell(0, u16::try_from(col).ok()?))
        .filter(|cell| !cell.is_wide_continuation())
        .map(|cell| {
            (
                cell.contents().to_string(),
                if cell.is_wide() { 2 } else { 1 },
            )
        })
        .collect()
}

/// GraphemeBreakTest (normative, UAX #29): each case printed on a row
/// wide enough takes one cell a cluster, holding the cluster's text. A
/// case holding a control (Cc) is left out: a control is carried out, not
/// stored. Where UAX #29 begins a cluster with a character of no width (a
/// format control, a zero-width Prepend character, a mark or a lone vowel
/// or trailing jamo after a break), the README's departure decides
/// instead, as no cell can hold a cluster at no width ("A character of no
/// width that joins no cluster"): the cluster joins the cell before, or,
/// at the row's start, loses its characters of no width and takes a cell
/// with what is left.
#[test]
fn every_grapheme_break_case_prints_a_cell_a_cluster() -> Result {
    let file = include_str!("data/GraphemeBreakTest.txt");
    let (mut as_uax_29, mut departing, mut controls) = (0, 0, 0);
    for case in cases(file, "GraphemeBreakTest")? {
        let mut clusters: Vec<String> = Vec::new();
        let mut join = false;
        for token in case.split_whitespace() {
            match token {
                "÷" => join = false,
                "×" => join = true,
                hex => {
                    let c = text(hex);
                    match clusters.last_mut() {
                        Some(last) if join => last.push_str(&c),
                        _ => clusters.push(c),
                    }
                }
            }
        }
        if clusters.iter().any(|c| c.chars().any(char::is_control)) {
            controls += 1;
            continue;
        }
        let no_width = |c: char| char_width(c) == Some(0);
        let mut cells: Vec<String> = Vec::new();
        for cluster in &clusters {
            match cells.last_mut() {
                Some(last) if cluster.starts_with(no_width) => last.push_str(cluster),
                _ => {
                    let rest = cluster.trim_start_matches(no_width);
                    if !rest.is_empty() {
                        cells.push(rest.to_string());
                    }
                }
            }
        }
        let parser = printed(&clusters.concat())?;
        let shown: Vec<String> = glyphs(&parser).into_iter().map(|(t, _)| t).collect();
        assert_eq!(shown, cells, "{case}");
        if cells == clusters {
            as_uax_29 += 1;
        } else {
            departing += 1;
        }
    }
    // Of the file's 766 cases (Unicode 17.0.0).
    assert_eq!((as_uax_29, departing, controls), (329, 226, 211));
    Ok(())
}

/// NormalizationTest (normative for placement: The Unicode Standard,
/// conformance clause C6, canonical equivalents are interpreted alike): of
/// each line, the source, its NFC and its NFD (c1 to c3) printed alone
/// leave the cursor in one column, and so do its NFKC and NFKD (c4 and c5),
/// canonical equivalents of each other. The stored text is not compared:
/// it stays as written. The two sets need not agree: a compatibility form
/// is no canonical equivalent of its source.
#[test]
fn canonical_equivalents_take_the_same_columns() -> Result {
    let file = include_str!("data/NormalizationTest.txt");
    let mut checked = 0;
    for line in cases(file, "NormalizationTest")? {
        for set in line.split('|') {
            let mut columns = Vec::new();
            for form in set.split(';') {
                columns.push((printed(&text(form))?.screen().cursor_position().1, form));
            }
            let first = columns.first().map(|c| c.0);
            assert!(
                columns.iter().all(|c| Some(c.0) == first),
                "{line}: {columns:?}"
            );
        }
        checked += 1;
    }
    assert!(checked > 20_000, "{checked} lines");
    Ok(())
}

/// emoji-test (fux-vt's width policy): every fully-qualified,
/// minimally-qualified and component sequence is one cell, two columns
/// wide. Unqualified sequences are text, whose width is the width rule's.
#[test]
fn every_qualified_emoji_is_one_wide_cell() -> Result {
    let file = include_str!("data/emoji-test.txt");
    let mut checked = 0;
    let mut bad = Vec::new();
    for case in cases(file, "emoji-test")? {
        let (hex, status) = case.split_once(" ; ").ok_or("a case of two fields")?;
        if status == "unqualified" {
            continue;
        }
        let emoji = text(hex);
        let cells = glyphs(&printed(&emoji)?);
        if cells != [(emoji.clone(), 2)] {
            bad.push(format!("{hex} ({status}): {cells:?}"));
        }
        checked += 1;
    }
    assert!(checked > 4_900, "{checked} sequences");
    assert!(
        bad.is_empty(),
        "{} of {checked}:\n{}",
        bad.len(),
        bad.join("\n")
    );
    Ok(())
}

/// emoji-variation-sequences (fux-vt's width policy): a base with U+FE0F
/// (emoji style) is one cell, two columns wide; with U+FE0E (text style),
/// one cell as wide as the base alone.
#[test]
fn every_variation_sequence_takes_its_style_s_width() -> Result {
    let file = include_str!("data/emoji-variation-sequences.txt");
    let mut checked = 0;
    let mut bad = Vec::new();
    for case in cases(file, "emoji-variation-sequences")? {
        let hex = case.split(" ; ").next().ok_or("a case")?;
        let sequence = text(hex);
        let base: String = sequence.chars().take(1).collect();
        let want = if sequence.ends_with('\u{fe0f}') {
            2
        } else {
            glyphs(&printed(&base)?).first().map_or(0, |(_, w)| *w)
        };
        let cells = glyphs(&printed(&sequence)?);
        if cells != [(sequence.clone(), want)] {
            bad.push(format!("{hex}: {cells:?}, not one cell of {want}"));
        }
        checked += 1;
    }
    assert!(checked > 700, "{checked} sequences");
    assert!(
        bad.is_empty(),
        "{} of {checked}:\n{}",
        bad.len(),
        bad.join("\n")
    );
    Ok(())
}
