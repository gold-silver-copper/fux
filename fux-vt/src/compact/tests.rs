use super::*;
use crate::test_rng::Rng;
use crate::Attributes;

/// The texts of a row's cells, joined by bars.
fn texts(line: &Line<'_>) -> String {
    (0..line.cells.len())
        .map(|i| line.text(i).to_owned())
        .collect::<Vec<_>>()
        .join("|")
}

/// Each way of making a cell reads back what it was made with, and the
/// quick comparisons agree with `==`.
#[test]
fn cells_read_back_what_they_were_made_with() {
    let text = Text::default();
    let styles = Styles::default();
    let red = Attributes::new(crate::Color::Idx(1), crate::Color::Default);
    let style = red.inline_style().unwrap_or(0);
    let cells = [
        (Compact::default(), "", false, false),
        (Compact::blank(style), "", false, false),
        (Compact::continuation(), "", false, true),
        (Compact::ascii(b'a', 0), "a", false, false),
        (Compact::ascii(b'~', style), "~", false, false),
        (Compact::glyph('é', 1, style), "é", false, false),
        (Compact::glyph('界', 2, style), "界", true, false),
        (Compact::glyph('\u{1F600}', 2, 0), "\u{1F600}", true, false),
        (
            Compact::blank(0).with_inline("e\u{301}".as_bytes()),
            "e\u{301}",
            false,
            false,
        ),
    ];
    for (cell, contents, wide, continuation) in cells {
        let read = cell.read(&text, &styles);
        assert_eq!(read.contents(), contents, "{cell:?}");
        assert_eq!(read.has_contents(), !contents.is_empty(), "{cell:?}");
        assert_eq!(
            (read.is_wide(), read.is_wide_continuation()),
            (wide, continuation)
        );
        let attributes = if cell.style() == style {
            red
        } else {
            Attributes::default()
        };
        assert_eq!(read.attributes(), attributes, "{cell:?}");
        for byte in *b"a~" {
            for s in [0, style] {
                assert_eq!(cell.is_ascii(byte, s), cell == Compact::ascii(byte, s));
                assert_eq!(cell.is_blank(s), cell == Compact::blank(s));
            }
        }
    }
    // Every ASCII character and every one-byte cell read as themselves.
    for byte in 1..=0x7fu8 {
        let cell = Compact::ascii(byte, 0);
        assert_eq!(
            cell.read(&text, &styles).contents(),
            char::from(byte).to_string()
        );
    }
    // Five bytes do not fit inline: the cell is left blank, its halves and
    // style kept.
    let cell = Compact::glyph('x', 2, style).with_inline(b"abcde");
    assert_eq!(cell, Compact::glyph('x', 2, style).with_inline(b""));
    assert!(cell.is_wide() && !cell.has_contents());
}

#[test]
fn a_cluster_grows_inline_then_into_the_rows_text_up_to_its_capacity() {
    let mut cells = [
        Compact::glyph('\u{1F468}', 2, 0),
        Compact::continuation(),
        Compact::ascii(b'x', 0),
    ];
    let mut text = Text::default();
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let mut line = Line {
        cells: &mut cells,
        text: &mut text,
    };
    for c in family.chars().skip(1) {
        assert!(line.append(0, c));
    }
    assert_eq!(line.text(0), family);
    assert!(
        line.cells
            .first()
            .is_some_and(|c| c.is_spilled() && c.is_wide())
    );
    // Short until 17 bytes, grown in place there; then long, grown in
    // place, as a row of `Cell`s grows it.
    assert_eq!(line.text.len(), family.len(), "grown in place, not copied");
    assert_eq!(line.text.short.len(), 14, "grown in place, not copied");
    assert_eq!(texts(&line), format!("{family}||x"));

    // Marks past the cluster's capacity are refused; the rest stays.
    let mut cells = [Compact::ascii(b'e', 0)];
    let mut text = Text::default();
    let mut line = Line {
        cells: &mut cells,
        text: &mut text,
    };
    let kept = (0..100).filter(|_| line.append(0, '\u{301}')).count();
    assert_eq!(kept, (Cell::CLUSTER_CAPACITY - 1) / 2);
    assert_eq!(line.text(0).len(), Cell::CLUSTER_CAPACITY - 1);
}

#[test]
fn a_row_out_of_room_compacts_then_cuts_to_what_a_cell_holds_inline() {
    let marks = |n| -> String {
        std::iter::once('e')
            .chain(std::iter::repeat_n('\u{301}', n))
            .collect()
    };
    // Two cells keep 192 bytes of long text; each of these clusters is 61.
    let cluster = marks(30);
    let limit = Spill::limit(2);
    assert_eq!(limit, 192);
    let mut cells = [Compact::default(); 2];
    let mut text = Text::default();
    let mut line = Line {
        cells: &mut cells,
        text: &mut text,
    };
    // Overwritten clusters leave text behind, until the row compacts.
    for _ in 0..20 {
        assert!(line.set(0, Compact::default(), &cluster));
        assert!(line.text.len() <= limit);
    }
    for _ in 0..20 {
        assert!(line.set(1, Compact::default(), &cluster));
        assert!(line.text.len() <= limit);
        assert_eq!(line.text(0), cluster);
        assert_eq!(line.text(1), cluster);
    }
    // One past the cluster capacity is cut there, at a char boundary.
    let long = marks(70);
    assert!(!line.set(1, Compact::default(), &long));
    assert_eq!(line.text(1).len(), Cell::CLUSTER_CAPACITY - 1);
    // One that does not fit beside cell 0's, even compacted, is cut to what
    // a `Cell` holds inline, whole chars, in its one cell.
    let big = marks(60);
    assert!(line.set(1, Compact::default(), "x"));
    assert!(line.set(0, Compact::default(), &big));
    assert!(!line.set(1, Compact::default(), &big));
    assert_eq!(line.text(1).len(), Cell::INLINE_CAPACITY);
    assert!(big.starts_with(line.text(1)));
    assert_eq!(line.text(0), big);
    // Short clusters written over and over stay within a row's room.
    for n in 0..200 {
        let short = marks(2 + n % 7);
        assert!(line.set(n % 2, Compact::default(), &short));
        assert!(line.text.short.len() <= Text::short_limit(2));
        assert_eq!(line.text(n % 2), short);
    }
}


/// A row of grid cells keeps and cuts its clusters as a row of [`Cell`]s
/// does, and keeps as much long text: random clusters of every length,
/// short and long, set into random cells of rows of every width, compared
/// with `Cells`' row after each, and rebuilt in a narrower row.
#[test]
fn a_row_of_grid_cells_keeps_its_text_as_a_row_of_cells_does() {
    let mut r = Rng(0x7e47);
    let pieces = ["a", "\u{301}", "\u{754c}", "\u{1F468}", "\u{200D}", "é"];
    for case in 0..2_000 {
        let width = 1 + r.below(6);
        let mut cells = vec![Compact::default(); width];
        let mut text = Text::default();
        let mut model = vec![Cell::default(); width];
        let mut spill = Spill::default();
        for step in 0..1 + r.below(40) {
            let i = r.below(width + 1);
            let mut cluster = String::new();
            for _ in 0..r.below(50) {
                cluster.push_str(pieces.get(r.below(pieces.len())).copied().unwrap_or("a"));
            }
            let wide = r.below(2) == 0;
            let template = if wide {
                Compact::glyph('x', 2, 0)
            } else {
                Compact::default()
            };
            let mut line = Line {
                cells: &mut cells,
                text: &mut text,
            };
            let kept = line.set(i, template, &cluster);
            let mut cells_line = crate::cell::Line {
                cells: &mut model,
                spill: &mut spill,
            };
            let model_kept = cells_line.set(i, Cell::default(), &cluster);
            assert_eq!(kept, model_kept, "case {case} step {step}");
            for at in 0..width {
                assert_eq!(
                    line.text(at),
                    cells_line.text(at),
                    "case {case} step {step}"
                );
            }
            assert_eq!(
                line.text.len(),
                cells_line.spill.len(),
                "case {case} step {step}"
            );
        }
        // Rebuilt into a narrower row, as a resize does.
        let narrow = 1 + r.below(width);
        let mut short_cells: Vec<Compact> = cells.iter().take(narrow).copied().collect();
        let rebuilt = Line::rebuilt(&mut short_cells, &text);
        let mut short_model: Vec<Cell> = model.iter().take(narrow).copied().collect();
        let model_rebuilt = crate::cell::Line::rebuilt(&mut short_model, &spill);
        let line = Line {
            cells: &mut short_cells,
            text: &mut rebuilt.clone(),
        };
        let mut model_spill = model_rebuilt;
        let cells_line = crate::cell::Line {
            cells: &mut short_model,
            spill: &mut model_spill,
        };
        for at in 0..narrow {
            assert_eq!(line.text(at), cells_line.text(at), "case {case} rebuilt");
        }
        assert_eq!(rebuilt.len(), cells_line.spill.len(), "case {case} rebuilt");
    }
}
