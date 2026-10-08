use super::*;
use crate::test_rng::Rng;

/// A cell stored in `Cells` reads back as it was made, whichever part of
/// the text keeps it.
#[test]
fn stored_cells_read_back_as_they_were_made() {
    let attributes = Attributes {
        foreground: Packed::new(Color::Idx(9)),
        background: Packed::new(Color::Rgb(1, 2, 3)),
        underline_color: Packed::new(Color::Idx(4)),
        flags: (0x1ff & !Attributes::RAPID_BLINK) | Attributes::UNDERLINE,
    };
    let seventeen = "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
    let mut cells = Cells::new(3);
    for (i, text) in ["é", seventeen, &format!("{seventeen}{seventeen}")]
        .into_iter()
        .enumerate()
    {
        assert!(cells.set(i, CellRef::new(text, true, attributes)));
        let view = cells.get(i).unwrap_or_default();
        assert_eq!(view.contents(), text);
        assert!(view.is_wide() && view.has_contents() && !view.is_wide_continuation());
        assert_eq!(view.attributes(), attributes);
        assert!(view.bold() && view.dim() && view.italic() && view.underline() && view.inverse());
        assert!(view.hidden() && view.strikeout() && view.blink() == Blink::Slow);
        assert_eq!(view.underline_color(), Color::Idx(4));
    }
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
fn owned_cells_copy_text_from_wherever_it_is_and_compare_by_it() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let mut a = Cells::new(4);
    assert!(a.set(1, CellRef::new(family, true, Attributes::default())));
    a.set(2, CellRef::wide_continuation());
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
    b.fill(0..4, CellRef::default());
    assert_eq!(b, Cells::new(4));
    assert_eq!(b.range(2..9).len(), 2);
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
                    row.set(i, CellRef::new(text, r.below(5) == 0, attrs));
                }
                2 => row.set_attributes(i, attrs),
                3 => {
                    let end = i.saturating_add(r.below(5));
                    row.fill(i..end, CellRef::new(text, false, attrs));
                }
                4 => {
                    row.set(i, CellRef::wide_continuation());
                }
                _ => {
                    // A copy of the other row's cell at `i`, both ways.
                    if let Some(cell) = a.get(i).map(|c| (c.contents().to_owned(), c)) {
                        let (text, cell) = cell;
                        b.set(i, CellRef::new(&text, cell.is_wide(), cell.attributes()));
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
