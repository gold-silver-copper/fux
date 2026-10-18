//! The width rules in context (README, "Grapheme clusters" and "Where the
//! standards leave the choice"): each rule that makes a cluster two columns
//! wide, printed at a row's start, in the last column, with a wrap
//! pending, at a right margin, under IRM, through a reflow and at the
//! cluster limit, and REP after clusters; each in one piece, a byte at a
//! time and split in two anywhere, with the same cells. The expected cells
//! are derived by hand from the README's rows, not read from fux-vt.

use fux_vt::{CLUSTER_CAPACITY, Feature, Options, Parser, Size, str_width};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Clusters two columns wide by the width rule (unicode-width 0.2.2's
/// string width): VS16 after a narrow emoji, a flag of two regional
/// indicators (one column each alone), an emoji modifier sequence after a
/// narrow base, a keycap, a Prepend character with a digit, and four
/// leading jamo (GB6), which sum to 8 and take 2. All but the last begin
/// with a narrow character, and widen as the rest joins.
const RULES: [(&str, &str); 6] = [
    ("VS16", "\u{2764}\u{fe0f}"),
    ("flag", "\u{1f1ef}\u{1f1f5}"),
    ("modifier", "\u{261d}\u{1f3fb}"),
    ("keycap", "1\u{fe0f}\u{20e3}"),
    ("Prepend", "\u{600}1"),
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

/// Each is two columns by the width rule, at most 2 (the jamo sum to 8).
#[test]
fn the_rules_make_two_columns() {
    for (name, x) in RULES {
        assert_eq!(str_width(x).min(2), 2, "{name}");
        assert!(str_width(x) == 2 || name == "jamo", "{name}");
    }
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

/// In the last column: a narrow cell whose cluster becomes two wide is not
/// widened there, and keeps the wrap pending; a wide first character does
/// not fit, and goes to the next line's start as a wide glyph does (the
/// README's "Grapheme clusters" and DECAWM rows).
#[test]
fn each_rule_in_the_last_column() -> Result {
    for (name, x) in RULES {
        let got = plain(2, 3, format!("ab{x}").as_bytes())?;
        let want = if name == "jamo" {
            (
                vec![
                    row(&[c("a", false), c("b", false)], 3),
                    row(&[c(x, true)], 3),
                ],
                (1, 2),
                false,
            )
        } else {
            (
                vec![
                    row(&[c("a", false), c("b", false), c(x, false)], 3),
                    row(&[], 3),
                ],
                (0, 2),
                true,
            )
        };
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
        let want = if name == "jamo" {
            (vec![row(&abc, 8), row(&[c(x, true)], 8)], (1, 2), false)
        } else {
            (
                vec![
                    row(&[abc.to_vec(), vec![c(x, false)]].concat(), 8),
                    row(&[], 8),
                ],
                (0, 3),
                true,
            )
        };
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
