use super::*;

/// A cell's text, `set` into a fresh run of one.
fn texts(cells: &mut [Cell], spill: &mut Spill) -> String {
    let line = Line { cells, spill };
    (0..line.cells.len())
        .map(|i| line.text(i).to_owned())
        .collect::<Vec<_>>()
        .join("|")
}

/// What follows a cell's text is zero, however it was made, so `same`
/// and `is_ascii`, which compare only the text in use, agree with `==`.
#[test]
fn text_past_the_length_is_zero_and_the_quick_comparisons_agree() {
    let bold = Attributes::default().with_bold(true);
    let red = Attributes::new(Color::Idx(1), Color::Rgb(1, 2, 3));
    let mut cells = vec![
        Cell::default(),
        Cell::blank(bold),
        Cell::continuation(),
        Cell::ascii(b'a', Attributes::default()),
        Cell::ascii(b'a', bold),
        Cell::ascii(b'b', red),
        Cell::glyph('界', 2, red),
        Cell::glyph('é', 1, Attributes::default()),
        Cell::glyph(' ', 1, bold),
        Cell::new("xy", false, red).unwrap_or_default(),
        Cell::default().with_spilled(7, 30),
        Cell::glyph('a', 2, bold).with_spilled(7, 30),
    ];
    let mut spill = Spill::default();
    let mut row = [Cell::glyph('a', 1, bold), Cell::blank(red)];
    for _ in 0..70 {
        let mut line = Line {
            cells: &mut row,
            spill: &mut spill,
        };
        line.append(0, '\u{301}');
        line.append(1, '\u{302}');
        cells.extend(row);
    }
    for cell in &cells {
        let used = cell.used();
        assert!(cell.text.iter().skip(used).all(|b| *b == 0), "{cell:?}");
        for other in &cells {
            assert_eq!(cell.same(other), cell == other, "{cell:?} {other:?}");
        }
        for (byte, attributes) in [(b'a', Attributes::default()), (b'a', bold), (b'b', red)] {
            assert_eq!(
                cell.is_ascii(byte, attributes),
                *cell == Cell::ascii(byte, attributes),
                "{cell:?}"
            );
        }
    }
}

#[test]
fn cells_are_32_bytes_and_hold_17_bytes_inline() {
    assert_eq!(std::mem::size_of::<Cell>(), 32);
    let attributes = Attributes {
        foreground: Packed::new(Color::Idx(9)),
        background: Packed::new(Color::Rgb(1, 2, 3)),
        underline_color: Packed::new(Color::Idx(4)),
        flags: 0x1ff & !Attributes::RAPID_BLINK,
    };
    let seventeen = "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
    assert_eq!(seventeen.len(), Cell::INLINE_CAPACITY);
    let cell = Cell::new(seventeen, true, attributes).unwrap_or_default();
    assert!(!cell.is_spilled() && cell.is_wide() && cell.has_contents());
    let spill = Spill::default();
    let view = CellRef::new(&cell, &spill);
    assert_eq!(view.contents(), seventeen);
    assert_eq!(view.attributes(), attributes);
    assert!(view.bold() && view.dim() && view.italic() && view.underline() && view.inverse());
    assert!(view.hidden() && view.strikeout() && view.blink() == Blink::Slow);
    assert_eq!(view.underline_color(), Color::Idx(4));
    assert!(Cell::new(&format!("{seventeen}x"), false, attributes).is_none());
}

#[test]
fn a_cluster_grows_inline_then_into_the_rows_text_up_to_its_capacity() {
    let mut cells = [
        Cell::glyph('\u{1F468}', 2, Attributes::default()),
        Cell::continuation(),
        Cell::ascii(b'x', Attributes::default()),
    ];
    let mut spill = Spill::default();
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let mut line = Line {
        cells: &mut cells,
        spill: &mut spill,
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
    assert_eq!(line.spill.len(), family.len(), "grown in place, not copied");
    assert_eq!(texts(&mut cells, &mut spill), format!("{family}||x"));

    // Marks past the cluster's capacity are refused; the rest stays.
    let mut cells = [Cell::ascii(b'e', Attributes::default())];
    let mut spill = Spill::default();
    let mut line = Line {
        cells: &mut cells,
        spill: &mut spill,
    };
    let kept = (0..100).filter(|_| line.append(0, '\u{301}')).count();
    assert_eq!(kept, (Cell::CLUSTER_CAPACITY - 1) / 2);
    assert_eq!(line.text(0).len(), Cell::CLUSTER_CAPACITY - 1);
}

#[test]
fn a_row_out_of_room_compacts_then_cuts_to_what_fits_inline() {
    let marks = |n| -> String {
        std::iter::once('e')
            .chain(std::iter::repeat_n('\u{301}', n))
            .collect()
    };
    // Two cells keep 192 bytes of text; each of these clusters is 61.
    let cluster = marks(30);
    let limit = Spill::limit(2);
    assert_eq!(limit, 192);
    let mut cells = [Cell::default(); 2];
    let mut spill = Spill::default();
    let mut line = Line {
        cells: &mut cells,
        spill: &mut spill,
    };
    // Overwritten clusters leave text behind, until the row compacts.
    for _ in 0..20 {
        assert!(line.set(0, Cell::default(), &cluster));
        assert!(line.spill.len() <= limit);
    }
    for _ in 0..20 {
        assert!(line.set(1, Cell::default(), &cluster));
        assert!(line.spill.len() <= limit);
        assert_eq!(line.text(0), cluster);
        assert_eq!(line.text(1), cluster);
    }
    // One past the cluster capacity is cut there, at a char boundary.
    let long = marks(70);
    assert!(!line.set(1, Cell::default(), &long));
    assert_eq!(line.text(1).len(), Cell::CLUSTER_CAPACITY - 1);
    // One that does not fit beside cell 0's, even compacted, is cut to what
    // fits inline, whole chars, in its one cell.
    let big = marks(60);
    assert!(line.set(1, Cell::default(), "x"));
    assert!(line.set(0, Cell::default(), &big));
    assert!(!line.set(1, Cell::default(), &big));
    assert_eq!(line.text(1).len(), Cell::INLINE_CAPACITY);
    assert!(big.starts_with(line.text(1)));
    assert_eq!(line.text(0), big);
}

#[test]
fn owned_cells_copy_text_from_wherever_it_is_and_compare_by_it() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let mut a = Cells::new(4);
    assert!(a.set_text(1, family, true, Attributes::default()));
    a.set_cell(2, Cell::wide_continuation());
    // Copied cell by cell, through a third run, in another order.
    let mut c = Cells::new(4);
    for i in [3, 2, 1, 0] {
        if let Some(cell) = a.get(i) {
            c.set(i, cell);
        }
    }
    let mut b = Cells::new(4);
    for i in 0..4 {
        if let Some(cell) = c.get(i) {
            b.set(i, cell);
        }
    }
    assert_eq!(a, b);
    assert_eq!(b.get(1).map(|c| c.contents()), Some(family));
    b.set_attributes(1, Attributes::default().with_bold(true));
    assert_ne!(a, b);
    assert!(b.get(1).is_some_and(|c| c.bold() && c.contents() == family));
    b.fill(0..4, Cell::default());
    assert_eq!(b, Cells::new(4));
    assert_eq!(b.range(2..9).len(), 2);
}

#[test]
fn a_one_byte_cell_reads_its_ascii_character() {
    let spill = Spill::default();
    for byte in 0..=0x7fu8 {
        let text = char::from(byte).to_string();
        let cell = Cell::new(&text, false, Attributes::default()).unwrap_or_default();
        assert_eq!(CellRef::new(&cell, &spill).contents(), text, "{byte}");
    }
    let cell = Cell::new("é", false, Attributes::default()).unwrap_or_default();
    assert_eq!(CellRef::new(&cell, &spill).contents(), "é");
    assert_eq!(CellRef::new(&Cell::default(), &spill).contents(), "");
}
