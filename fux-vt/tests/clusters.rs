//! The width rules in context (README, "Grapheme clusters" and "Where the
//! standards leave the choice"): each rule that makes a cluster two columns
//! wide, printed at a row's start, in the last column, with a wrap
//! pending, at a right margin, under IRM, through a reflow and at the
//! cluster limit, and REP after clusters; each in one piece, a byte at a
//! time and split in two anywhere, with the same cells. The expected cells
//! are derived by hand from the README's rows, not read from fux-vt.

use fux_vt::{CLUSTER_CAPACITY, Feature, Options, Parser, Size, clusters, str_width};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Clusters two columns wide by the width rule (unicode-width 0.2.2's
/// string width): VS16 after a narrow emoji, a flag of two regional
/// indicators (one column each alone), an emoji modifier sequence after a
/// narrow base, a keycap, a Prepend character with a digit, a consonant
/// and its spacing vowel sign (fux-vt's spacing-mark rule: one column
/// each), a two-part vowel sign decomposed, a Khmer conjunct (two letters
/// joined by a coeng, which takes none), and four leading jamo (GB6),
/// which sum to 8 and take 2. All but the last begin with a narrow
/// character, and widen as the rest joins.
const RULES: [(&str, &str); 9] = [
    ("VS16", "\u{2764}\u{fe0f}"),
    ("flag", "\u{1f1ef}\u{1f1f5}"),
    ("modifier", "\u{261d}\u{1f3fb}"),
    ("keycap", "1\u{fe0f}\u{20e3}"),
    ("Prepend", "\u{600}1"),
    ("spacing mark", "\u{995}\u{9be}"),
    ("two-part vowel sign", "\u{995}\u{9c7}\u{9be}"),
    ("coeng", "\u{1780}\u{17d2}\u{1781}"),
    ("jamo", "\u{1100}\u{1100}\u{1100}\u{1100}"),
];

/// A row's cells, the second halves of wide glyphs left out: each one's
/// text, and whether it is wide.
fn cells(parser: &Parser, row: u16) -> Vec<(String, bool)> {
    let screen = parser.screen();
    (0..screen.size().cols())
        .filter_map(|col| screen.cell(row, col))
        .filter(|cell| !cell.is_wide_continuation())
        .map(|cell| (cell.contents().to_string(), cell.is_wide()))
        .collect()
}

/// What a screen shows: every row's cells, the cursor and a pending wrap.
type Shown = (Vec<Vec<(String, bool)>>, (u16, u16), bool);

fn shown(parser: &Parser) -> Shown {
    let screen = parser.screen();
    let rows = (0..screen.size().rows())
        .map(|r| cells(parser, r))
        .collect();
    (rows, screen.cursor_position(), screen.pending_wrap())
}

/// `bytes` fed to a fresh parser in one piece, a byte at a time and split
/// in two at every byte: all must show the same, which is returned.
fn fed(rows: u16, cols: u16, options: Options, bytes: &[u8]) -> Result<Shown> {
    let size = Size::new(rows, cols)?;
    let new = || Parser::with_options(size, 1 << 12, options);
    let mut whole = new()?;
    whole.process(bytes)?;
    let want = shown(&whole);
    let mut bytewise = new()?;
    for byte in bytes {
        bytewise.process(std::slice::from_ref(byte))?;
    }
    assert_eq!(
        shown(&bytewise),
        want,
        "a byte at a time: {:?}",
        bytes.escape_ascii()
    );
    for at in 1..bytes.len() {
        let (head, tail) = bytes.split_at_checked(at).ok_or("a split")?;
        let mut split = new()?;
        split.process(head)?;
        split.process(tail)?;
        assert_eq!(
            shown(&split),
            want,
            "split at {at}: {:?}",
            bytes.escape_ascii()
        );
    }
    Ok(want)
}

fn plain(rows: u16, cols: u16, bytes: &[u8]) -> Result<Shown> {
    fed(rows, cols, Options::new(), bytes)
}

fn c(text: &str, wide: bool) -> (String, bool) {
    (text.to_string(), wide)
}

/// `n` empty cells.
fn empty(n: usize) -> Vec<(String, bool)> {
    std::iter::repeat_n(c("", false), n).collect()
}

/// A row: the cells given, then empty ones to `cols` columns.
fn row(given: &[(String, bool)], cols: usize) -> Vec<(String, bool)> {
    let used: usize = given
        .iter()
        .map(|(_, wide)| if *wide { 2 } else { 1 })
        .sum();
    [given.to_vec(), empty(cols.saturating_sub(used))].concat()
}

/// Each is two columns by the width rule.
#[test]
fn the_rules_make_two_columns() {
    for (name, x) in RULES {
        assert_eq!(str_width(x), 2, "{name}");
    }
}

/// The cells of `text` printed at a row's start, as `clusters` names them.
fn named(text: &str) -> Vec<(String, bool)> {
    clusters(text)
        .map(|(cell, width)| c(cell, width == 2))
        .collect()
}

/// The cells a screen shows for `text` printed at a row's start, empty
/// ones left out.
fn printed(text: &str) -> Result<Vec<(String, bool)>> {
    let mut parser = Parser::new(Size::new(1, 64)?, 0)?;
    parser.process(text.as_bytes())?;
    Ok(cells(&parser, 0)
        .into_iter()
        .filter(|(text, _)| !text.is_empty())
        .collect())
}

/// `clusters` names the cells a screen prints, and `str_width` their
/// columns: for each case of the Unicode conformance test, its controls
/// left out; and by hand, a character of no width joining the cell before
/// though it joins no cluster (README), with a spacing mark after it
/// joining too, a mark with no cell before it, a wide character the text
/// presentation selector makes 1 column by the rule (its cell stays
/// wide), and cells past the capacity.
#[test]
fn clusters_names_the_cells_a_screen_prints() -> Result {
    let cases = include_str!("data/GraphemeBreakTest.txt");
    let mut checked = 0;
    for case in cases.lines().filter(|line| !line.starts_with('#')) {
        let text: String = (case.split_whitespace())
            .filter_map(|hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32))
            .filter(|c| !c.is_control())
            .collect();
        assert_eq!(named(&text), printed(&text)?, "{text:?}");
        checked += 1;
    }
    assert!(checked > 500, "{checked} cases");
    let mark = |n| std::iter::repeat_n('\u{301}', n);
    // 127 bytes: a 64th mark would pass the capacity.
    let long: String = std::iter::once('e').chain(mark(100)).collect();
    // 126 bytes, then U+200B, dropped, and U+0308, which would fit.
    let full: String = std::iter::once('\u{e9}')
        .chain(mark(62))
        .chain(['\u{200b}', '\u{308}'])
        .collect();
    for (text, want, width) in [
        ("一\u{1161}\u{e33}", vec![c("一\u{1161}\u{e33}", true)], 2),
        (
            "a\u{200b}\u{308}\u{903}\u{903}",
            vec![c("a\u{200b}\u{308}\u{903}\u{903}", true)],
            2,
        ),
        (
            "\u{308}\u{903}x",
            vec![c("\u{903}", false), c("x", false)],
            2,
        ),
        ("\u{231a}\u{fe0e}", vec![c("\u{231a}\u{fe0e}", true)], 2),
        (&long, vec![c(long.get(..127).ok_or("long")?, false)], 1),
        (&full, vec![c(full.get(..126).ok_or("full")?, false)], 1),
    ] {
        assert_eq!(named(text), want, "{text:?}");
        assert_eq!(printed(text)?, want, "{text:?}");
        assert_eq!(str_width(text), width, "{text:?}");
    }
    Ok(())
}

/// At a row's start, the cluster is one wide cell, and the cursor two
/// columns on.
#[test]
fn each_rule_at_a_rows_start() -> Result {
    for (name, x) in RULES {
        let got = plain(1, 6, format!("{x}z").as_bytes())?;
        let want = (vec![row(&[c(x, true), c("z", false)], 6)], (0, 3), false);
        assert_eq!(got, want, "{name}");
    }
    Ok(())
}

/// In the last column every cluster ends as a wide glyph printed there
/// does, at the next line's start: a narrow cell whose cluster becomes two
/// wide moves there (mode 2027's spec, "Variation Selector 16"), a wide
/// first character goes there (the README's "Grapheme clusters" and
/// DECAWM rows).
#[test]
fn each_rule_in_the_last_column() -> Result {
    for (name, x) in RULES {
        let got = plain(2, 3, format!("ab{x}").as_bytes())?;
        let want = (
            vec![
                row(&[c("a", false), c("b", false)], 3),
                row(&[c(x, true)], 3),
            ],
            (1, 2),
            false,
        );
        assert_eq!(got, want, "{name}");
    }
    Ok(())
}

/// With a wrap pending, the cluster's first character wraps first, and the
/// cluster is one wide cell at the next line's start.
#[test]
fn each_rule_with_a_wrap_pending() -> Result {
    for (name, x) in RULES {
        let got = plain(2, 3, format!("abc{x}").as_bytes())?;
        let abc = row(&[c("a", false), c("b", false), c("c", false)], 3);
        assert_eq!(
            got,
            (vec![abc, row(&[c(x, true)], 3)], (1, 2), false),
            "{name}"
        );
    }
    Ok(())
}

/// At the right margin (DECLRMM, DECSLRM 1;4) the margin's column is the
/// last column: the same as at the screen's edge, the wrap going to the
/// left margin.
#[test]
fn each_rule_at_the_right_margin() -> Result {
    for (name, x) in RULES {
        let got = plain(2, 8, format!("\x1b[?69h\x1b[1;4sabc{x}").as_bytes())?;
        let abc = [c("a", false), c("b", false), c("c", false)];
        let want = (vec![row(&abc, 8), row(&[c(x, true)], 8)], (1, 2), false);
        assert_eq!(got, want, "{name}");
    }
    Ok(())
}

/// Under IRM the cluster is inserted as a wide glyph is: what was there
/// moves right two columns, whether the cluster came wide or widened.
#[test]
fn each_rule_under_irm() -> Result {
    for (name, x) in RULES {
        let got = plain(1, 6, format!("ab\r\x1b[4h{x}").as_bytes())?;
        let want = (
            vec![row(&[c(x, true), c("a", false), c("b", false)], 6)],
            (0, 2),
            false,
        );
        assert_eq!(got, want, "{name}");
    }
    Ok(())
}

/// Through a reflow (`Feature::Reflow`) the wide cell stays whole: at a
/// width that leaves it one column, it goes to the next row, a blank
/// spacer where it was.
#[test]
fn each_rule_through_a_reflow() -> Result {
    for (name, x) in RULES {
        let mut p = Parser::with_options(Size::new(3, 4)?, 1 << 12, Feature::Reflow.into())?;
        p.process(format!("ab{x}").as_bytes())?;
        assert_eq!(
            cells(&p, 0),
            row(&[c("a", false), c("b", false), c(x, true)], 4),
            "{name}"
        );
        p.resize(Size::new(3, 3)?)?;
        assert_eq!(
            cells(&p, 0),
            row(&[c("a", false), c("b", false)], 3),
            "{name}"
        );
        assert_eq!(cells(&p, 1), row(&[c(x, true)], 3), "{name}");
    }
    Ok(())
}

/// At the cluster limit: a cluster keeps at most `CLUSTER_CAPACITY` bytes,
/// what would pass it dropped, never split into another cell; the cell
/// stays wide, and what follows takes the next cell.
#[test]
fn each_rule_at_the_cluster_limit() -> Result {
    let marks = std::iter::repeat_n("\u{301}", 100).collect::<String>();
    for (name, x) in RULES {
        let got = plain(1, 6, format!("{x}{marks}z").as_bytes())?;
        let kept = (CLUSTER_CAPACITY - x.len()) / "\u{301}".len();
        let cluster = format!(
            "{x}{}",
            std::iter::repeat_n("\u{301}", kept).collect::<String>()
        );
        let want = (
            vec![row(&[c(&cluster, true), c("z", false)], 6)],
            (0, 3),
            false,
        );
        assert_eq!(got, want, "{name}");
    }
    Ok(())
}

/// A mark in the first column joins the last cell of the row above when
/// that row is soft-wrapped, and is dropped when it is not (README,
/// "Cells").
#[test]
fn a_mark_after_a_soft_wrap_joins_the_row_above() -> Result {
    let got = plain(2, 3, "abcd\r\u{301}".as_bytes())?;
    let above = row(&[c("a", false), c("b", false), c("c\u{301}", false)], 3);
    assert_eq!(got, (vec![above, row(&[c("d", false)], 3)], (1, 0), false));
    let got = plain(2, 3, "abc\r\n\u{301}".as_bytes())?;
    let above = row(&[c("a", false), c("b", false), c("c", false)], 3);
    assert_eq!(got, (vec![above, row(&[], 3)], (1, 0), false));
    Ok(())
}

/// REP after a cluster repeats the character that took the cell (its
/// first), and the copies are segmented as if printed again (README, REP
/// row): a flag's first regional indicator twice is a second flag; leading
/// jamo after one join it (GB6) in one cell; copies of a Prepend character
/// join one another and what follows (GB9b); and the copies of an emoji
/// widened by VS16 are the narrow emoji alone.
#[test]
fn rep_repeats_a_clusters_first_character_as_if_printed_again() -> Result {
    let flag = plain(1, 8, "\u{1f1ef}\u{1f1f5}\x1b[2b".as_bytes())?;
    let two = [c("\u{1f1ef}\u{1f1f5}", true), c("\u{1f1ef}\u{1f1ef}", true)];
    assert_eq!(flag, (vec![row(&two, 8)], (0, 4), false));
    let jamo = plain(1, 8, "\u{1100}\x1b[3b".as_bytes())?;
    let one = "\u{1100}\u{1100}\u{1100}\u{1100}";
    assert_eq!(jamo, (vec![row(&[c(one, true)], 8)], (0, 2), false));
    let heart = plain(1, 8, "\u{2764}\u{fe0f}\x1b[2b".as_bytes())?;
    let three = [
        c("\u{2764}\u{fe0f}", true),
        c("\u{2764}", false),
        c("\u{2764}", false),
    ];
    assert_eq!(heart, (vec![row(&three, 8)], (0, 4), false));
    let prepend = plain(1, 8, "\u{600}x\x1b[2by".as_bytes())?;
    let texts: Vec<String> = prepend
        .0
        .concat()
        .into_iter()
        .map(|(t, _)| t)
        .filter(|t| !t.is_empty())
        .collect();
    assert_eq!(texts, ["\u{600}x", "\u{600}\u{600}y"]);
    Ok(())
}

/// A character of no width that joins no cluster joins the cell before
/// (README, "A character of no width that joins no cluster"), and what
/// follows it is segmented after it, as UAX #29 segments the cell's text:
/// after a trailing jamo (T) a regional indicator begins a cluster (GB999),
/// so it pairs with no indicator before the jamo; after a lone vowel (V) a
/// spacing mark joins (GB9a), widening the cell.
#[test]
fn what_follows_a_character_joined_at_no_width_is_segmented_after_it() -> Result {
    let got = plain(1, 6, "\u{1f1f5}\u{11a8}\u{1f1ef}".as_bytes())?;
    let want = [c("\u{1f1f5}\u{11a8}", false), c("\u{1f1ef}", false)];
    assert_eq!(got, (vec![row(&want, 6)], (0, 2), false));
    let got = plain(1, 6, "x\u{1161}\u{e33}".as_bytes())?;
    assert_eq!(
        got,
        (vec![row(&[c("x\u{1161}\u{e33}", true)], 6)], (0, 2), false)
    );
    Ok(())
}

/// So too after a zero-width Prepend character, which joins nothing after
/// it: what follows it is segmented afresh, and a character of no width
/// joining no cluster there (ZWSP, Control: GB5) still joins the cell, and
/// what follows that is segmented after it. A lone vowel (V) then a spacing
/// mark (GB9a) widen the cell, after `x` and after U+0600; so do a tag (an
/// Extend) and an emoji modifier, which GB9 joins to it (the graphemes
/// fuzz target, run 39295133788).
#[test]
fn what_follows_a_character_joined_at_no_width_after_a_zero_width_prepend_is_segmented_after_it()
-> Result {
    for text in [
        "x\u{605}\u{200b}\u{1161}\u{e33}",
        "\u{600}\u{605}\u{200b}\u{1161}\u{e33}",
        "a\u{70f}\u{e00ff}\u{e0063}\u{1f3fb}",
    ] {
        let got = plain(1, 6, text.as_bytes())?;
        assert_eq!(
            got,
            (vec![row(&[c(text, true)], 6)], (0, 2), false),
            "{text:?}"
        );
    }
    Ok(())
}

/// A zero-width Prepend character joins the cell before it, and stays
/// apart from what UAX #29's GB9b would join it to, as the README's
/// departure says: `x\u{605}A` is `x\u{605}` and `A`, and
/// `x\u{605}\u{4e00}` three columns; so too when it joins a
/// cluster, after another zero-width Prepend or after a Prepend glyph,
/// U+0600.
#[test]
fn a_zero_width_prepend_character_joins_nothing_after_it() -> Result {
    for (text, first) in [
        ("x\u{605}A", "x\u{605}"),
        ("x\u{605}\u{605}A", "x\u{605}\u{605}"),
        ("x\u{d4e}\u{605}A", "x\u{d4e}\u{605}"),
        ("\u{600}\u{605}A", "\u{600}\u{605}"),
    ] {
        let mut p = Parser::new(Size::new(1, 6)?, 0)?;
        p.process(text.as_bytes())?;
        let cells: Vec<String> = (0..3)
            .filter_map(|x| p.screen().cell(0, x).map(|c| c.contents().to_string()))
            .collect();
        assert_eq!(cells, [first, "A", ""], "{text:?}");
        assert!(!fux_vt::continues_cluster(first, 'A'), "{text:?}");
        assert!(fux_vt::continues_cluster(first, '\u{301}'), "{text:?}");
    }
    let mut p = Parser::new(Size::new(1, 6)?, 0)?;
    p.process("x\u{605}\u{4e00}".as_bytes())?;
    assert_eq!(p.screen().cursor_position(), (0, 3));
    Ok(())
}
