use super::*;
use unicode_segmentation::UnicodeSegmentation;

/// Where `text` breaks, as byte offsets inside it, by the state machine.
fn breaks(text: &str, prepend_joins: bool) -> impl Iterator<Item = usize> {
    let mut chars = text.char_indices();
    let mut cluster = chars.next().map(|(_, c)| Cluster::start(c));
    chars
        .filter(move |&(_, c)| {
            cluster
                .as_mut()
                .is_some_and(|k| !k.push_with(c, prepend_joins))
        })
        .map(|(at, _)| at)
}

fn boundaries(text: &str, prepend_joins: bool) -> Vec<usize> {
    breaks(text, prepend_joins).collect()
}

/// Where `text` breaks by unicode-segmentation, which implements UAX #29 as
/// written.
fn oracle(text: &str) -> impl Iterator<Item = usize> {
    text.grapheme_indices(true).map(|(at, _)| at).skip(1)
}

#[test]
fn the_tables_are_the_version_unicode_segmentation_implements() {
    assert_eq!(
        UNICODE_VERSION,
        (
            u8::try_from(unicode_segmentation::UNICODE_VERSION.0).unwrap_or(0),
            u8::try_from(unicode_segmentation::UNICODE_VERSION.1).unwrap_or(0),
            u8::try_from(unicode_segmentation::UNICODE_VERSION.2).unwrap_or(0),
        )
    );
}

/// Every case of the Unicode conformance test, GraphemeBreakTest.txt, as
/// UAX #29 has it; and with fux-vt's one departure, a break after Prepend.
#[test]
fn every_conformance_case_breaks_where_uax_29_says() {
    let cases = include_str!("../../tests/data/GraphemeBreakTest.txt");
    let (mut checked, mut departures) = (0u32, 0u32);
    for case in cases.lines().filter(|line| !line.starts_with('#')) {
        let mut text = String::new();
        let mut expected = Vec::new();
        let mut tokens = case.split_whitespace().peekable();
        while let Some(token) = tokens.next() {
            match token {
                "÷" if !text.is_empty() && tokens.peek().is_some() => expected.push(text.len()),
                "÷" | "×" => {}
                hex => text.extend(u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)),
            }
        }
        assert_eq!(boundaries(&text, true), expected, "{case}");
        let ours = boundaries(&text, false);
        if ours != expected {
            // Only ever a break more, each right after a Prepend.
            for at in ours.iter().filter(|at| !expected.contains(at)) {
                let before = text.get(..*at).and_then(|t| t.chars().next_back());
                let before = before.map(|c| Properties::of(c).kind);
                assert_eq!(before, Some(Break::Prepend), "{case}");
            }
            assert!(expected.iter().all(|at| ours.contains(at)), "{case}");
            departures = departures.saturating_add(1);
        }
        checked = checked.saturating_add(1);
    }
    assert_eq!(checked, 766);
    assert!(departures > 0 && departures < 100, "{departures}");
}

/// Every assigned scalar value's properties, against unicode-segmentation:
/// each is put after and before characters whose rules single out one
/// property, so a code point classed wrongly breaks differently. Past plane 3
/// only the tags and variation selectors (E0000..E0FFF) are assigned, other
/// than private use; the rest is sampled every 61st code point.
#[test]
fn every_code_point_is_classed_as_unicode_segmentation_classes_it() {
    // What goes before the code point, and after it.
    let around: &[(&str, &str)] = &[
        ("a", ""),                     // Extend, ZWJ, SpacingMark
        ("\u{1100}", ""),              // Hangul L, V, LV, LVT
        ("\u{AC00}", ""),              // V, T after LV
        ("\u{AC01}", ""),              // T after LVT
        ("\u{1F1E6}", ""),             // Regional_Indicator
        ("\u{1F600}\u{200D}", ""),     // Extended_Pictographic
        ("\u{915}\u{94D}", ""),        // InCB Consonant
        ("\r", ""),                    // LF
        ("\u{600}", ""),               // what follows Prepend
        ("", "\u{301}"),               // Control, CR, LF
        ("", "a"),                     // Prepend
        ("", "\u{1161}"),              // L, LV, V before V
        ("", "\u{11A8}"),              // LV, LVT, V, T before T
        ("", "\u{1100}"),              // L before L
        ("", "\u{1F1E6}"),             // Regional_Indicator
        ("", "\u{200D}\u{1F600}"),     // Extended_Pictographic
        ("", "\u{94D}\u{915}"),        // InCB Consonant
        ("\u{915}", "\u{915}"),        // InCB Linker
        ("\u{915}", "\u{94D}\u{915}"), // InCB Extend
    ];
    let mut text = String::new();
    let mut checked = 0u32;
    let assigned = |p: &u32| *p < 0x4_0000 || (0xE_0000..0xE_1000).contains(p);
    let code_points = (0..=0x10_FFFF).filter(|p| assigned(p) || p % 61 == 0);
    for c in code_points.filter_map(char::from_u32) {
        for (before, after) in around {
            text.clear();
            text.push_str(before);
            text.push(c);
            text.push_str(after);
            if !breaks(&text, true).eq(oracle(&text)) {
                assert_eq!(
                    boundaries(&text, true),
                    oracle(&text).collect::<Vec<_>>(),
                    "{text:?}"
                );
            }
            checked = checked.saturating_add(1);
        }
    }
    assert!(checked > 5_000_000, "{checked}");
}

#[test]
fn a_cluster_rebuilt_from_its_text_continues_as_it_would_have() {
    for (text, next, joins) in [
        ("\u{1F469}\u{200D}", '\u{1F52C}', true),
        ("\u{1F469}", '\u{1F52C}', false),
        ("\u{1F1EF}", '\u{1F1F5}', true),
        ("\u{1F1EF}\u{1F1F5}", '\u{1F1E6}', false),
        ("\u{915}\u{94D}", '\u{937}', true),
        ("\u{915}", '\u{937}', false),
        ("\u{600}", 'a', false),
        ("", '\u{301}', true),
    ] {
        assert_eq!(Cluster::of(text).push(next), joins, "{text:?} {next:?}");
    }
}
