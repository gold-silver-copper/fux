use super::*;
use unicode_segmentation::UnicodeSegmentation;

/// Where `text` breaks, as byte offsets inside it, by the state machine.
fn breaks(text: &str) -> impl Iterator<Item = usize> {
    let mut chars = text.char_indices();
    let mut cluster = chars.next().map(|(_, c)| Cluster::start(c));
    chars
        .filter(move |&(_, c)| cluster.as_mut().is_some_and(|k| !k.push(c)))
        .map(|(at, _)| at)
}

fn boundaries(text: &str) -> Vec<usize> {
    breaks(text).collect()
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
/// UAX #29 has it: the file of the version the tables are generated at,
/// as its header names it.
#[test]
fn every_conformance_case_breaks_where_uax_29_says() {
    let cases = include_str!("../../tests/data/GraphemeBreakTest.txt");
    let (major, minor, update) = UNICODE_VERSION;
    let header = format!("# GraphemeBreakTest-{major}.{minor}.{update}.txt");
    assert!(
        cases.lines().next().is_some_and(|l| l.starts_with(&header)),
        "GraphemeBreakTest.txt is not {header}"
    );
    let mut checked = 0u32;
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
        assert_eq!(boundaries(&text), expected, "{case}");
        checked = checked.saturating_add(1);
    }
    assert_eq!(checked, 766);
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
            if !breaks(&text).eq(oracle(&text)) {
                assert_eq!(
                    boundaries(&text),
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
        ("\u{600}", 'a', true),
        ("\u{600}", '\r', false),
        ("", '\u{301}', true),
    ] {
        assert_eq!(Cluster::of(text).push(next), joins, "{text:?} {next:?}");
    }
}

/// `continues_cluster` is the state machine's answer, the fast path for
/// printable ASCII included.
#[test]
fn continues_cluster_answers_as_the_state_machine() {
    let clusters = [
        "",
        "a",
        " ",
        "\u{1F44D}",
        "\u{1F469}\u{200D}",
        "\u{1F1EF}",
        "\u{915}\u{94D}",
        "\u{600}",
        "\u{1100}",
        "\r",
        "\u{e4}\u{356}",
    ];
    // Every character of the planes in use, and the tags and variation
    // selectors: the shortcuts answer as the state machine does for each.
    let nexts = ('\0'..='\u{2FFFF}').chain('\u{E0000}'..='\u{E01EF}');
    for next in nexts {
        for cluster in clusters {
            assert_eq!(
                continues_cluster(cluster, next),
                Cluster::of(cluster).push(next),
                "{cluster:?} {next:?}"
            );
        }
    }
    assert!(continues_cluster("\u{1F44D}", '\u{1F3FD}'));
    assert!(continues_cluster("a", '\u{301}'));
    assert!(!continues_cluster("\u{1F44D}", 'x'));
}

/// fux-vt's widths (threats.md B32): no character takes more than 2
/// columns, U+17D8 KHMER SIGN BEYYAL, the one unicode-width makes 3,
/// takes 1, as in xterm 411; every other is unicode-width 0.2.2's, of the
/// same Unicode version as the tables.
#[test]
fn every_character_is_as_wide_as_the_width_rule_says() {
    use unicode_width::UnicodeWidthChar;
    assert_eq!(unicode_width::UNICODE_VERSION, UNICODE_VERSION);
    for c in (0..=0x10_ffff).filter_map(char::from_u32) {
        let width = crate::char_width(c);
        assert!(width.is_none_or(|w| w <= 2), "{c:?} is {width:?} wide");
        if c != '\u{17d8}' {
            assert_eq!(width, c.width(), "{c:?}");
        }
    }
    assert_eq!(crate::char_width('\u{17d8}'), Some(1));
    assert_eq!(crate::str_width("\u{17d8}"), 1);
    assert_eq!(crate::str_width("a\u{17d8}b\u{17d8}"), 4);
    assert_eq!(crate::str_width("一e\u{301}"), 3);
}

/// The characters whose rules in unicode-width's string width look at a
/// neighbour: the variation selectors, ZWJ, the keycap, an emoji modifier,
/// a regional indicator, the tags, Kirat Rai's vowel signs, a combining
/// mark, the Tifinagh joiner and the Khmer coeng.
const CONTEXT: &[char] = &[
    '\u{fe00}',
    '\u{fe01}',
    '\u{fe0e}',
    '\u{fe0f}',
    '\u{200d}',
    '\u{20e3}',
    '\u{1f3fb}',
    '\u{1f3ff}',
    '\u{1f1e6}',
    '\u{1f1ef}',
    '\u{e0067}',
    '\u{e0031}',
    '\u{e007f}',
    '\u{16d63}',
    '\u{16d67}',
    '\u{16d68}',
    '\u{16d69}',
    '\u{301}',
    '\u{2d7f}',
    '\u{17d2}',
    '\u{a8fa}',
];

/// Whether `text` is one grapheme cluster, as fux-vt segments it.
fn one_cluster(text: &str) -> bool {
    breaks(text).next().is_none()
}

/// The cluster rule (`str_width` of one cluster) is unicode-width 0.2.2's
/// string width of that cluster, U+17D8 aside: for every character alone,
/// and before and after each of `CONTEXT`'s; for the ZWJ, keycap,
/// modifier, flag and tag sequences built of every emoji; and for every
/// cluster of the Unicode conformance test.
#[test]
fn a_cluster_is_as_wide_as_unicode_width_makes_it() {
    use unicode_width::UnicodeWidthStr;
    let mut checked = 0u32;
    let mut check = |text: &str| {
        if one_cluster(text) && !text.contains('\u{17d8}') {
            assert_eq!(crate::str_width(text), text.width(), "{text:?}");
            checked = checked.saturating_add(1);
        }
    };
    let all = || (0..=0x10_ffff).filter_map(char::from_u32);
    let mut text = String::new();
    for c in all() {
        // U+FE0F changes only an emoji's width, or what follows one.
        let emoji = bits(c) & (1 << 9 | 1 << 10 | 1 << 12) != 0;
        let selected = [&[c][..], &[c, '\u{fe0f}']];
        for chars in selected.iter().take(if emoji { 2 } else { 1 }) {
            for &k in CONTEXT {
                text.clear();
                text.extend(chars.iter().chain([&k]));
                check(&text);
                text.clear();
                text.extend([&k].into_iter().chain(chars.iter()));
                check(&text);
            }
        }
        check(c.encode_utf8(&mut [0; 4]));
    }
    let emoji: Vec<char> = all()
        .filter(|&c| bits(c) & (1 << 9 | 1 << 10 | 1 << 12) != 0)
        .collect();
    let tags = [
        "",
        "\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}",
        "\u{e0075}\u{e0073}\u{e0031}\u{e0032}",
    ];
    for &a in &emoji {
        for b in [
            '\u{1f468}',
            '\u{2764}',
            '\u{1f1fa}',
            '\u{1f3fd}',
            '#',
            '\u{1f3f4}',
        ] {
            check(&format!("{a}\u{200d}{b}"));
            check(&format!("{b}\u{200d}{a}"));
            check(&format!("{a}\u{fe0f}\u{200d}{b}\u{fe0f}"));
            check(&format!("{b}\u{fe0f}\u{200d}{a}\u{fe0f}\u{20e3}"));
            check(&format!("{a}\u{1f3fb}\u{200d}{b}\u{1f3ff}"));
        }
        for tag in tags {
            check(&format!("\u{1f3f4}{tag}\u{e007f}\u{200d}{a}"));
            check(&format!("{a}\u{200d}\u{1f3f4}{tag}\u{e007f}"));
        }
    }
    for flags in [
        "\u{1f1fa}",
        "\u{1f1fa}\u{1f1f8}",
        "\u{1f1fa}\u{1f1f8}\u{1f1fa}",
    ] {
        check(&format!("\u{1f468}\u{200d}{flags}"));
        check(&format!("{flags}\u{200d}\u{1f468}"));
    }
    // Conjuncts (GB9c): each consonant, a linker and a consonant, its own
    // and another script's, ZWJ or a mark between.
    let consonants: Vec<char> = all().filter(|&c| (bits(c) >> 5) & 3 == 1).collect();
    let linkers: Vec<char> = all().filter(|&c| (bits(c) >> 5) & 3 == 3).collect();
    for &c in &consonants {
        for &linker in &linkers {
            for d in [c, '\u{915}', '\u{1780}', '\u{17a0}', '\u{1a20}'] {
                check(&format!("{c}{linker}{d}"));
                check(&format!("{c}{linker}\u{200d}{d}"));
                check(&format!("{c}\u{17b4}{linker}{d}\u{fe0f}"));
            }
        }
    }
    for digit in ['0', '9', '#', '*'] {
        check(&format!("{digit}\u{fe0f}\u{20e3}"));
        check(&format!("\u{1f468}\u{200d}{digit}\u{fe0f}\u{20e3}"));
    }
    let cases = include_str!("../../tests/data/GraphemeBreakTest.txt");
    for case in cases.lines().filter(|line| !line.starts_with('#')) {
        for cluster in case.split('÷') {
            let text: String = (cluster.split_whitespace())
                .filter_map(|hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32))
                .collect();
            check(&text);
        }
    }
    assert!(checked > 17_000_000, "{checked} clusters");
}

/// `Cluster::start`'s ASCII shortcut is what the table gives.
#[test]
fn a_cluster_started_with_ascii_is_as_the_table_starts_it() {
    for c in ' '..='~' {
        assert_eq!(
            Cluster::start(c),
            Cluster::default().after(Properties::of(c), false),
            "{c:?}"
        );
    }
}
