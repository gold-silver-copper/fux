//! fux-vt's width rule: the columns a character takes alone, and the
//! columns a grapheme cluster takes.
//!
//! A character's width is unicode-width 0.2.2's for Unicode 17.0.0 (its
//! `scripts/unicode.py`), generated into fux-vt's table by `fux-vt/gen`,
//! with two departures: U+17D8 KHMER SIGN BEYYAL, which it makes 3
//! columns, takes 1, as in xterm 411 and glibc, since a cell holds 1
//! column or 2; and a spacing mark (General_Category Mc) of
//! Canonical_Combining_Class 0 it makes 0 (one that is Grapheme_Extend, as
//! U+09BE BENGALI VOWEL SIGN AA) takes 1, as wcwidth 0.9.2 (ucs-detect's
//! reference; decision 885) measures it, so a consonant and its spacing
//! vowel sign take two columns. One of another class stays 0: a spacing
//! virama (class 9), which joins consonants, as in wcwidth 0.9.2, where
//! glibc gives it 1; and the Hangul tone marks, the Vietnamese reading
//! marks (East Asian Wide, which glibc gives 2) and the musical stems and
//! flags, which canonical ordering (Unicode 3.11) moves among the marks
//! after them.
//! Such a mark is the second half of a two-part vowel sign or a composed
//! letter (U+09CB is U+09C7 U+09BE, U+0B94 U+0B92 U+0BD7), so right after
//! another spacing mark it adds no column, and a cluster takes the columns
//! of its form with each such pair composed, as NFC composes them: each
//! such mark is of class 0, which canonical ordering never moves and any
//! character between blocks (Unicode 3.11), so canonical equivalents take
//! the same columns, even where a leading mark of no width was dropped
//! (`Screen::print_as`).
//!
//! A cluster's width is unicode-width 0.2.2's string width of that cluster
//! alone (its `width_in_str`, read from the last character back), for the
//! rules that can hold within one cluster: emoji presentation (U+FE0F) and
//! text presentation (U+FE0E) sequences, emoji modifiers, emoji ZWJ and tag
//! sequences, keycaps, regional indicator pairs, a curly quote with U+FE01,
//! and Kirat Rai's vowel signs. Its rule that a Khmer letter subjoined by a
//! coeng adds no column takes no part: the coeng (Mn) takes none and the
//! letter its one, so a conjunct of two letters takes two columns, as
//! wcwidth 0.9.2 and glibc's widths summed make it. Its rules for ligatures
//! between characters that never share a cluster (Arabic Lam-Alef, Hebrew
//! Alef-Lamed, Buginese, Tifinagh, Lisu and Old Turkic) take no part: a
//! cluster is laid out alone. Text of several clusters takes the sum of
//! theirs.

use super::{Cluster, bits};

/// Where a character's columns alone are in its bits: 0 to 2, or 3 for a
/// control.
const WIDTH_SHIFT: u32 = 7;
/// Its bits for emoji presentation (unicode-width's `EMOJI_PRESENTATION`
/// class), the start of an emoji and of a text presentation sequence, and
/// an emoji modifier base.
const PRESENTATION: u16 = 1 << 9;
const EMOJI_STYLE: u16 = 1 << 10;
const TEXT_STYLE: u16 = 1 << 11;
const MODIFIER_BASE: u16 = 1 << 12;
/// Its bit for a Default_Ignorable combining mark, which interrupts no
/// ligature.
const TRANSPARENT: u16 = 1 << 13;
/// Its bit for a character the cluster rule's arms look at, before or
/// after it: a variation selector, ZWJ, the keycap, a regional indicator,
/// an emoji modifier, a tag, Kirat Rai's vowel signs, or a line feed after
/// a carriage return. Text with none of them takes its
/// characters' columns summed.
pub(super) const CONTEXT: u16 = 1 << 14;
/// Its bit for a spacing mark (General_Category Mc). One the rule looks at
/// too is a spacing mark unicode-width makes 0 and fux-vt 1.
const SPACING: u16 = 1 << 15;

/// The columns `c` takes alone as fux-vt prints it, `None` for a control.
/// A host that lays out what fux-vt holds (fux's painter) takes its widths
/// from here.
#[inline]
pub fn char_width(c: char) -> Option<usize> {
    of_bits(bits(c))
}

/// `char_width` of the character whose bits in the table are `bits`.
#[inline]
pub(super) fn of_bits(bits: u16) -> Option<usize> {
    match bits.checked_shr(WIDTH_SHIFT).unwrap_or(0) & 3 {
        3 => None,
        width => Some(usize::from(width)),
    }
}

/// The columns `text` takes: each of its grapheme clusters' (UAX #29) by
/// the cluster rule, summed. A cell's cluster is one.
pub fn str_width(text: &str) -> usize {
    // Text with none of the characters a rule looks at takes the sum of
    // its characters' columns, as each cluster of it does.
    let mut sum = 0usize;
    for c in text.chars() {
        let bits = bits(c);
        if bits & CONTEXT != 0 {
            return by_clusters(text);
        }
        sum = sum.saturating_add(match c {
            '\n' | '\r' => 1,
            _ => of_bits(bits).unwrap_or(1),
        });
    }
    sum
}

/// `str_width` by each cluster, for text holding a character the rule's
/// arms look at.
#[cold]
fn by_clusters(text: &str) -> usize {
    let mut total = 0usize;
    let mut start = 0;
    let mut cluster: Option<Cluster> = None;
    for (at, c) in text.char_indices() {
        let joins = match &mut cluster {
            Some(cluster) => cluster.push(c),
            None => {
                cluster = Some(Cluster::start(c));
                true
            }
        };
        if !joins {
            total = total.saturating_add(cluster_width(text.get(start..at).unwrap_or("")));
            start = at;
        }
    }
    total.saturating_add(cluster_width(text.get(start..).unwrap_or("")))
}

/// What follows a character in its cluster, as the rule reads the cluster
/// from its end: unicode-width 0.2.2's `WidthInfo`, its states that can
/// hold within a cluster, numbered as it numbers them.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Next(u16);

impl Next {
    const DEFAULT: Self = Self(0);
    const LINE_FEED: Self = Self(0b0000_0000_0000_0001);
    const EMOJI_MODIFIER: Self = Self(0b0000_0000_0000_0010);
    const REGIONAL_INDICATOR: Self = Self(0b0000_0000_0000_0011);
    const SEVERAL_REGIONAL_INDICATOR: Self = Self(0b0000_0000_0000_0100);
    const EMOJI_PRESENTATION: Self = Self(0b0000_0000_0000_0101);
    const ZWJ_EMOJI_PRESENTATION: Self = Self(0b0001_0000_0000_0110);
    const VS16_ZWJ_EMOJI_PRESENTATION: Self = Self(0b1001_0000_0000_0110);
    const KEYCAP_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0001_0000_0000_0111);
    const VS16_KEYCAP_ZWJ_EMOJI_PRESENTATION: Self = Self(0b1001_0000_0000_0111);
    const REGIONAL_INDICATOR_ZWJ_PRESENTATION: Self = Self(0b0000_0000_0000_1001);
    const EVEN_REGIONAL_INDICATOR_ZWJ_PRESENTATION: Self = Self(0b0000_0000_0000_1010);
    const ODD_REGIONAL_INDICATOR_ZWJ_PRESENTATION: Self = Self(0b0000_0000_0000_1011);
    const TAG_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_0000);
    const TAG_D1_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_0001);
    const TAG_D2_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_0010);
    const TAG_D3_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_0011);
    const TAG_A1_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1001);
    const TAG_A2_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1010);
    const TAG_A3_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1011);
    const TAG_A4_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1100);
    const TAG_A5_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1101);
    const TAG_A6_END_ZWJ_EMOJI_PRESENTATION: Self = Self(0b0000_0000_0001_1110);
    const KIRAT_RAI_VOWEL_SIGN_E: Self = Self(0b0000_0000_0010_0000);
    const KIRAT_RAI_VOWEL_SIGN_AI: Self = Self(0b0000_0000_0010_0001);
    const VARIATION_SELECTOR_1_2_OR_3: Self = Self(0b0000_0010_0000_0000);
    const VARIATION_SELECTOR_15: Self = Self(0b0100_0000_0000_0000);
    const VARIATION_SELECTOR_16: Self = Self(0b1000_0000_0000_0000);

    /// The bit of a ligature's states, which a variation selector keeps.
    const LIGATURE: u16 = 0b0010_0000_0000_0000;

    fn is_ligature_transparent(self) -> bool {
        self.0 & 0b0000_1000_0000_0000 != 0
    }
    fn set_zwj_bit(self) -> Self {
        Self(self.0 | 0b0000_0100_0000_0000)
    }
    fn is_emoji_presentation(self) -> bool {
        self.0 & Self::VARIATION_SELECTOR_16.0 != 0
    }
    fn is_zwj_emoji_presentation(self) -> bool {
        self.0 & 0b1011_0000_0000_0000 == 0b1001_0000_0000_0000
    }
    /// U+FE0F before what follows: a ligature or a ZWJ sequence keeps its
    /// state.
    fn set_emoji_presentation(self) -> Self {
        if self.0 & Self::LIGATURE != 0 || self.0 & 0b1001_0000_0000_0000 == 0b0001_0000_0000_0000 {
            Self(self.0 | Self::VARIATION_SELECTOR_16.0)
        } else {
            Self::VARIATION_SELECTOR_16
        }
    }
    fn unset_emoji_presentation(self) -> Self {
        if self.0 & Self::LIGATURE != 0 {
            Self(self.0 & !Self::VARIATION_SELECTOR_16.0)
        } else {
            Self::DEFAULT
        }
    }
    fn is_text_presentation(self) -> bool {
        self.0 & Self::VARIATION_SELECTOR_15.0 != 0
    }
    fn set_text_presentation(self) -> Self {
        if self.0 & Self::LIGATURE != 0 {
            Self(self.0 | Self::VARIATION_SELECTOR_15.0)
        } else {
            Self::VARIATION_SELECTOR_15
        }
    }
    fn unset_text_presentation(self) -> Self {
        Self(self.0 & !Self::VARIATION_SELECTOR_15.0)
    }
    fn is_vs1_2_3(self) -> bool {
        self.0 & Self::VARIATION_SELECTOR_1_2_OR_3.0 != 0
    }
    fn set_vs1_2_3(self) -> Self {
        if self.0 & Self::LIGATURE != 0 {
            Self(self.0 | Self::VARIATION_SELECTOR_1_2_OR_3.0)
        } else {
            Self::VARIATION_SELECTOR_1_2_OR_3
        }
    }
    fn unset_vs1_2_3(self) -> Self {
        Self(self.0 & !Self::VARIATION_SELECTOR_1_2_OR_3.0)
    }
}

/// `c`'s columns alone, in the string rule (a control counts 1, as
/// unicode-width's string width counts it), and what it is to the
/// character before it (unicode-width's `lookup_width`).
fn alone(c: char, bits: u16) -> (i8, Next) {
    let next = match c {
        '\u{fe01}' => Next::VARIATION_SELECTOR_1_2_OR_3,
        '\u{fe0e}' => Next::VARIATION_SELECTOR_15,
        '\u{fe0f}' => Next::VARIATION_SELECTOR_16,
        '\u{16d67}' => Next::KIRAT_RAI_VOWEL_SIGN_E,
        '\u{16d68}' => Next::KIRAT_RAI_VOWEL_SIGN_AI,
        '\u{1f1e6}'..='\u{1f1ff}' => Next::REGIONAL_INDICATOR,
        '\u{1f3fb}'..='\u{1f3ff}' => Next::EMOJI_MODIFIER,
        _ if bits & PRESENTATION != 0 => Next::EMOJI_PRESENTATION,
        _ => Next::DEFAULT,
    };
    let width = match bits.checked_shr(WIDTH_SHIFT).unwrap_or(0) & 3 {
        0 => 0,
        2 => 2,
        _ => 1,
    };
    (width, next)
}

/// The columns one grapheme cluster takes, read from its end: each
/// character's, as what follows it in the cluster makes it.
fn cluster_width(cluster: &str) -> usize {
    let mut total = 0isize;
    let mut next = Next::DEFAULT;
    let mut widened = false;
    for c in cluster.chars().rev() {
        let bits = bits(c);
        widened |= bits & (CONTEXT | SPACING) == CONTEXT | SPACING;
        let (add, before) = in_cluster(c, bits, next);
        total = total.saturating_add(isize::from(add));
        next = before;
    }
    if widened {
        total = total.saturating_add(composed(cluster));
    }
    usize::try_from(total).unwrap_or(0)
}

/// What fux-vt's rule for the spacing marks it widens changes in the
/// columns `cluster_width` counts: the cluster is composed first, each
/// pair of a character and such a mark that composes canonically
/// (`tables::COMPOSITIONS`) made one, from the start, as NFC composes;
/// then each character of it takes its columns, but such a mark right
/// after another spacing mark, which takes none.
fn composed(cluster: &str) -> isize {
    let alone = |c: char| of_bits(bits(c)).map_or(1, |w| isize::try_from(w).unwrap_or(1));
    // The columns `c` of the composed text takes, after `before`.
    let counted = |c: char, before: Option<char>| {
        let widened = bits(c) & (CONTEXT | SPACING) == CONTEXT | SPACING;
        if widened && before.is_some_and(|b| bits(b) & SPACING != 0) {
            0
        } else {
            alone(c)
        }
    };
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    let (mut before, mut pending) = (None, first);
    let (mut rule, mut summed) = (0isize, alone(first));
    for c in chars {
        summed = summed.saturating_add(alone(c));
        let pair = bits(c) & (CONTEXT | SPACING) == CONTEXT | SPACING;
        let whole = pair
            .then(|| {
                let found = super::tables::COMPOSITIONS
                    .binary_search_by(|&(first, second, _)| (first, second).cmp(&(pending, c)));
                found
                    .ok()
                    .and_then(|at| super::tables::COMPOSITIONS.get(at))
            })
            .flatten();
        match whole {
            Some(&(_, _, whole)) => pending = whole,
            None => {
                rule = rule.saturating_add(counted(pending, before));
                (before, pending) = (Some(pending), c);
            }
        }
    }
    rule.saturating_add(counted(pending, before))
        .saturating_sub(summed)
}

/// `c`'s columns (its table bits `bits`), with `next` after it (unicode-width 0.2.2's
/// `width_in_str`, its arms that can hold within a cluster), and what it
/// is to the character before it.
fn in_cluster(c: char, bits: u16, mut next: Next) -> (i8, Next) {
    if next.is_emoji_presentation() {
        if bits & EMOJI_STYLE != 0 {
            let width = if next.is_zwj_emoji_presentation() {
                0
            } else {
                2
            };
            return (width, Next::EMOJI_PRESENTATION);
        }
        next = next.unset_emoji_presentation();
    }
    if c <= '\u{a0}' {
        return match c {
            '\n' => (1, Next::LINE_FEED),
            '\r' if next == Next::LINE_FEED => (0, Next::DEFAULT),
            _ => (1, Next::DEFAULT),
        };
    }
    if next != Next::DEFAULT {
        match c {
            '\u{fe0f}' => return (0, next.set_emoji_presentation()),
            '\u{fe01}' => return (0, next.set_vs1_2_3()),
            '\u{fe0e}' => return (0, next.set_text_presentation()),
            _ => {}
        }
        if next.is_text_presentation() {
            if bits & TEXT_STYLE != 0 {
                return (1, Next::DEFAULT);
            }
            next = next.unset_text_presentation();
        } else if next.is_vs1_2_3() {
            if matches!(c, '\u{2018}' | '\u{2019}' | '\u{201c}' | '\u{201d}') {
                return (2, Next::DEFAULT);
            }
            next = next.unset_vs1_2_3();
        }
        if next.is_ligature_transparent() {
            if c == '\u{200d}' {
                return (0, next.set_zwj_bit());
            }
            if bits & TRANSPARENT != 0 {
                return (0, next);
            }
        }
        if let Some(found) = emoji_sequence(c, bits, next) {
            return found;
        }
        let kirat = match (next, c) {
            (Next::KIRAT_RAI_VOWEL_SIGN_E, '\u{16d63}' | '\u{16d69}')
            | (Next::KIRAT_RAI_VOWEL_SIGN_AI, '\u{16d63}') => Some((0, Next::DEFAULT)),
            (Next::KIRAT_RAI_VOWEL_SIGN_E, '\u{16d67}') => Some((0, Next::KIRAT_RAI_VOWEL_SIGN_AI)),
            (Next::KIRAT_RAI_VOWEL_SIGN_E, '\u{16d68}') => Some((1, Next::KIRAT_RAI_VOWEL_SIGN_E)),
            _ => None,
        };
        if let Some(found) = kirat {
            return found;
        }
    }
    alone(c, bits)
}

/// `c` within an emoji modifier, regional indicator, ZWJ or tag sequence,
/// with `next` after it: unicode-width 0.2.2's arms for them.
fn emoji_sequence(c: char, bits: u16, next: Next) -> Option<(i8, Next)> {
    let tag_letter = matches!(c, '\u{e0061}'..='\u{e007a}');
    let tag_digit = matches!(c, '\u{e0030}'..='\u{e0039}');
    let indicator = matches!(c, '\u{1f1e6}'..='\u{1f1ff}');
    Some(match next {
        Next::EMOJI_MODIFIER if bits & MODIFIER_BASE != 0 => (0, Next::EMOJI_PRESENTATION),
        Next::REGIONAL_INDICATOR | Next::SEVERAL_REGIONAL_INDICATOR if indicator => {
            (1, Next::SEVERAL_REGIONAL_INDICATOR)
        }
        Next::EMOJI_PRESENTATION
        | Next::SEVERAL_REGIONAL_INDICATOR
        | Next::EVEN_REGIONAL_INDICATOR_ZWJ_PRESENTATION
        | Next::ODD_REGIONAL_INDICATOR_ZWJ_PRESENTATION
        | Next::EMOJI_MODIFIER
            if c == '\u{200d}' =>
        {
            (0, Next::ZWJ_EMOJI_PRESENTATION)
        }
        Next::ZWJ_EMOJI_PRESENTATION if c == '\u{20e3}' => (0, Next::KEYCAP_ZWJ_EMOJI_PRESENTATION),
        Next::VS16_ZWJ_EMOJI_PRESENTATION if bits & EMOJI_STYLE != 0 => {
            (0, Next::EMOJI_PRESENTATION)
        }
        Next::VS16_KEYCAP_ZWJ_EMOJI_PRESENTATION if matches!(c, '0'..='9' | '#' | '*') => {
            (0, Next::EMOJI_PRESENTATION)
        }
        Next::ZWJ_EMOJI_PRESENTATION if indicator => (1, Next::REGIONAL_INDICATOR_ZWJ_PRESENTATION),
        Next::REGIONAL_INDICATOR_ZWJ_PRESENTATION
        | Next::ODD_REGIONAL_INDICATOR_ZWJ_PRESENTATION
            if indicator =>
        {
            (-1, Next::EVEN_REGIONAL_INDICATOR_ZWJ_PRESENTATION)
        }
        Next::EVEN_REGIONAL_INDICATOR_ZWJ_PRESENTATION if indicator => {
            (3, Next::ODD_REGIONAL_INDICATOR_ZWJ_PRESENTATION)
        }
        Next::ZWJ_EMOJI_PRESENTATION if matches!(c, '\u{1f3fb}'..='\u{1f3ff}') => {
            (0, Next::EMOJI_MODIFIER)
        }
        Next::ZWJ_EMOJI_PRESENTATION if c == '\u{e007f}' => {
            (0, Next::TAG_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A1_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A1_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A2_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A2_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A3_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A3_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A4_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A4_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A5_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A5_END_ZWJ_EMOJI_PRESENTATION if tag_letter => {
            (0, Next::TAG_A6_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A1_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A2_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A3_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A4_END_ZWJ_EMOJI_PRESENTATION
            if tag_digit =>
        {
            (0, Next::TAG_D1_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_D1_END_ZWJ_EMOJI_PRESENTATION if tag_digit => {
            (0, Next::TAG_D2_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_D2_END_ZWJ_EMOJI_PRESENTATION if tag_digit => {
            (0, Next::TAG_D3_END_ZWJ_EMOJI_PRESENTATION)
        }
        Next::TAG_A3_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A4_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A5_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_A6_END_ZWJ_EMOJI_PRESENTATION
        | Next::TAG_D3_END_ZWJ_EMOJI_PRESENTATION
            if c == '\u{1f3f4}' =>
        {
            (0, Next::EMOJI_PRESENTATION)
        }
        Next::ZWJ_EMOJI_PRESENTATION if alone(c, bits).1 == Next::EMOJI_PRESENTATION => {
            (0, Next::EMOJI_PRESENTATION)
        }
        _ => return None,
    })
}
