use super::*;

#[test]
fn stored_cells_read_back_as_they_were_made() {
    let attributes = Attributes {
        foreground: Packed::new(Color::Idx(9)),
        background: Packed::new(Color::Rgb([1, 2, 3].into())),
        underline_color: Packed::new(Color::Idx(4)),
        flags: (0x1ff & !Attributes::RAPID_BLINK) | Attributes::UNDERLINE,
    };
    let seventeen = "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
    let mut cells = Cells::new(3).unwrap_or_default();
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

#[test]
fn cells_past_a_locator_s_reach_say_their_text_was_not_kept() -> Result<(), Error> {
    let cluster: String = std::iter::once('e')
        .chain(std::iter::repeat_n('\u{301}', 15))
        .collect();
    let mut cells = Cells::new(1 << 20)?;
    let set = |cells: &mut Cells, i: usize| {
        cells.set(i, CellRef::new(&cluster, false, Attributes::default()))
    };
    let mut i = 0;
    while set(&mut cells, i) {
        assert_eq!(cells.get(i).map(|c| c.contents()), Some(cluster.as_str()));
        i = i.saturating_add(1);
    }
    assert!(i > 500_000 && cells.text_len() <= 1 << 24, "{i} kept");
    assert!(!set(&mut cells, i.saturating_add(1)) && !set(&mut cells, i.saturating_add(2)));
    Ok(())
}

#[test]
fn underline_styles_keep_to_their_own_bits() {
    use super::UnderlineStyle;
    let others = Attributes::new(Color::Idx(1), Color::Rgb([1, 2, 3].into()))
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
    let mut a = Cells::new(4).unwrap_or_default();
    assert!(a.set(1, CellRef::new(family, true, Attributes::default())));
    a.set(2, CellRef::wide_continuation());
    // Copied cell by cell, through a third run, in another order.
    let mut c = Cells::new(4).unwrap_or_default();
    for i in [3, 2, 1, 0] {
        if let Some(cell) = a.get(i) {
            c.set(i, cell);
        }
    }
    let mut b = Cells::new(4).unwrap_or_default();
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
    assert_eq!(b, Cells::new(4).unwrap_or_default());
    assert_eq!(b.range(2..9).len(), 2);
}

#[test]
fn range_comparisons_ignore_storage_and_detect_cell_changes() -> Result<(), Error> {
    let long = "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
    for text in ["a", "é", "界", long, "👨‍👩‍👧‍👦"] {
        let mut a = Cells::new(6)?;
        let mut b = Cells::new(6)?;
        a.set(1, CellRef::new(text, false, Attributes::default()));
        b.set(1, CellRef::new(long, false, Attributes::default()));
        b.set(1, CellRef::new(text, false, Attributes::default()));
        assert_eq!(a, b, "unused stored text must not affect equality");
        for changed in [
            CellRef::new(text, false, Attributes::default().with_bold(true)),
            CellRef::new(text, true, Attributes::default()),
            CellRef::new("different", false, Attributes::default()),
            CellRef::wide_continuation(),
        ] {
            b.set(1, changed);
            assert_ne!(a, b);
            for start in 0..=8 {
                for end in start..=10 {
                    assert_eq!(
                        a.range_eq(&b, start..end),
                        !(start <= 1 && end > 1),
                        "{text:?}, {start}..{end}"
                    );
                }
            }
        }
    }
    Ok(())
}
