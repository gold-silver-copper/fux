use super::*;

#[test]
fn cells_are_32_bytes_and_hold_17_bytes_inline() {
    assert_eq!(std::mem::size_of::<Cell>(), 32);
    let attributes = Attributes {
        foreground: Packed::new(Color::Idx(9)),
        background: Packed::new(Color::Rgb(1, 2, 3)),
        underline_color: Packed::new(Color::Idx(4)),
        flags: (0x1ff & !Attributes::RAPID_BLINK) | Attributes::UNDERLINE,
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

/// The underline's style takes three bits of the flags, apart from every
/// other style's: each style, set over any other attributes, reads back
/// alone, and setting it changes nothing else.
#[test]
fn underline_styles_keep_to_their_own_bits() {
    use super::UnderlineStyle;
    let others = Attributes::new(Color::Idx(1), Color::Rgb(1, 2, 3))
        .with_bold(true)
        .with_dim(true)
        .with_italic(true)
        .with_inverse(true)
        .with_blink(Blink::Rapid)
        .with_hidden(true)
        .with_strikeout(true)
        .with_underline_color(Color::Idx(5));
    for n in 0..=5 {
        let style = UnderlineStyle::from_number(n).unwrap_or_default();
        assert_eq!(style.number(), n);
        for base in [Attributes::default(), others] {
            let a = base
                .with_underline_style(UnderlineStyle::Dashed)
                .with_underline_style(style);
            assert_eq!(a.underline_style(), style);
            assert_eq!(a.underline(), n != 0);
            assert_eq!(a.with_underline_style(UnderlineStyle::None), base);
        }
    }
    assert_eq!(UnderlineStyle::from_number(6), None);
    assert_eq!(
        Attributes::default().with_underline(true).underline_style(),
        UnderlineStyle::Single
    );
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

/// A small deterministic generator (splitmix64).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        usize::try_from(x.checked_rem(u64::try_from(n).unwrap_or(1)).unwrap_or(0)).unwrap_or(0)
    }
}

/// `range_eq` is `range(..).eq(range(..))`, faster: on two rows of cells
/// edited at random, every way cells are written (text short and long,
/// wide halves, attributes, fills, whole cells, copies from the other
/// row), over random ranges, in and past the rows. The rows start the
/// same and drift, so both answers come up.
#[test]
fn range_eq_agrees_with_comparing_each_cell() {
    let mut r = Rng(0x5eed_ce11);
    let texts = [
        "",
        "a",
        "b",
        "é",
        "\u{754c}",
        "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}",
        "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    ];
    let attributes = |r: &mut Rng| Attributes {
        foreground: Packed::new(Color::Idx(u8::try_from(r.below(3)).unwrap_or(0))),
        flags: if r.below(4) == 0 { Attributes::BOLD } else { 0 },
        ..Attributes::default()
    };
    let (mut same, mut differ) = (0usize, 0usize);
    for case in 0..3_000 {
        let len = 1 + r.below(40);
        let mut a = Cells::new(len);
        let mut b = Cells::new(len);
        for _ in 0..r.below(60) {
            let row = if r.below(2) == 0 { &mut a } else { &mut b };
            let i = r.below(len + 2);
            let text = texts.get(r.below(texts.len())).copied().unwrap_or("");
            let attrs = attributes(&mut r);
            match r.below(6) {
                0 | 1 => {
                    row.set_text(i, text, r.below(5) == 0, attrs);
                }
                2 => row.set_attributes(i, attrs),
                3 => {
                    let end = i.saturating_add(r.below(5));
                    row.fill(i..end, Cell::new(text, false, attrs).unwrap_or_default());
                }
                4 => row.set_cell(i, Cell::new(text, false, attrs).unwrap_or_default()),
                _ => {
                    // A copy of the other row's cell at `i`, both ways.
                    if let Some(cell) = a.get(i).map(|c| (c.contents().to_owned(), c)) {
                        let (text, cell) = cell;
                        b.set_text(i, &text, cell.is_wide(), cell.attributes());
                    }
                }
            }
            for _ in 0..3 {
                let start = r.below(len + 3);
                let end = start.saturating_add(r.below(len + 3));
                let slow = a.range(start..end).eq(b.range(start..end));
                assert_eq!(
                    a.range_eq(&b, start..end),
                    slow,
                    "case {case}, {start}..{end}"
                );
                if slow { same += 1 } else { differ += 1 }
            }
            assert_eq!(a == b, a.iter().eq(b.iter()), "case {case}");
        }
    }
    assert!(same > 1000 && differ > 1000, "{same} alike, {differ} not");
}
