//! Extended grapheme cluster boundaries (UAX #29), decided one character at
//! a time.
//!
//! [`Cluster`] is the state of the cluster being printed: a few bits, enough
//! for every rule that looks further back than one character (emoji ZWJ
//! sequences, regional indicator pairs, Indic conjuncts). Boundaries so
//! never depend on the text a cell stores, which may be cut short, and each
//! character costs one table lookup. The tables are generated from the
//! Unicode Character Database by `fux-vt/gen`.

#[rustfmt::skip]
mod tables;

pub use tables::UNICODE_VERSION;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The one character unicode-width makes 3 columns wide: U+17D8 KHMER SIGN
/// BEYYAL.
const THREE: char = '\u{17d8}';

/// The columns `c` takes as fux-vt prints it, `None` for a control: its
/// width by unicode-width, but that a cell holds 1 column or 2, so U+17D8,
/// the one character unicode-width makes 3, takes 1, as in xterm 411 and
/// glibc (UAX #11 leaves a terminal's widths to it). A host that lays out
/// what fux-vt holds (fux's painter) takes its widths from here.
#[inline]
pub fn char_width(c: char) -> Option<usize> {
    // U+17D8 is the only character unicode-width says 3 for.
    match c.width() {
        Some(3) => Some(1),
        width => width,
    }
}

/// The columns `text` takes, as [`char_width`] counts its characters:
/// unicode-width's, with U+17D8 at 1.
#[inline]
pub fn str_width(text: &str) -> usize {
    let width = text.width();
    // Text narrower than 3 holds no U+17D8: it is looked for only past that.
    if width < 3 {
        return width;
    }
    let threes = text.chars().filter(|c| *c == THREE).count();
    width.saturating_sub(threes.saturating_mul(2))
}

/// Whether `c`, printed right after the grapheme cluster `cluster`, would
/// continue it rather than start the next one, as fux-vt joins what is
/// printed (UAX #29): an emoji modifier after an emoji, a combining mark
/// after anything, anything but a control after a Prepend character. A
/// host that paints cells side by side to a terminal (fux) uses it to tell
/// where the terminal may join a cell to the one before it.
pub fn continues_cluster(cluster: &str, c: char) -> bool {
    // No rule joins a character of these kinds to what comes before it,
    // but for a Prepend character: every printable ASCII character (they
    // are all Other), and most of what is not ASCII (box drawing, CJK,
    // letters). Unless the cluster ends in a Prepend character, that needs
    // no look at the cluster.
    let next = Properties::of(c);
    if matches!(
        next.kind,
        Break::Other | Break::Prepend | Break::Control | Break::Cr
    ) && !next.pictographic
        && next.conjunct != Conjunct::Consonant
        && (cluster.chars().next_back())
            .is_none_or(|last| Properties::of(last).kind != Break::Prepend)
    {
        return false;
    }
    Cluster::of(cluster).push(c)
}

closed! {
    /// Grapheme_Cluster_Break, numbered as the generated tables store it:
    /// in the order declared, which is `fux-vt/gen`'s `BREAKS`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum Break {
        #[default]
        Other,
        Cr,
        Lf,
        Control,
        Extend,
        Zwj,
        RegionalIndicator,
        Prepend,
        SpacingMark,
        L,
        V,
        T,
        Lv,
        Lvt,
    }
}

impl Break {
    /// The kind the low four bits of `bits` number; a number past the last
    /// is `Other`.
    fn of(bits: u8) -> Self {
        let kind = Self::ALL.get(usize::from(bits & 0x0f));
        kind.copied().unwrap_or_default()
    }
}

/// Indic_Conjunct_Break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conjunct {
    None,
    Consonant,
    Extend,
    Linker,
}

/// A character's segmentation properties.
#[derive(Clone, Copy, Debug)]
struct Properties {
    kind: Break,
    pictographic: bool,
    conjunct: Conjunct,
}

impl Properties {
    fn of(c: char) -> Self {
        let code = u32::from(c);
        let low = code & ((1 << tables::SHIFT) - 1);
        let bits = code
            .checked_shr(tables::SHIFT)
            .and_then(|block| usize::try_from(block).ok())
            .and_then(|block| tables::BLOCK.get(block))
            .and_then(|&block| {
                let start = usize::from(block).checked_shl(tables::SHIFT)?;
                start.checked_add(usize::try_from(low).ok()?)
            })
            .and_then(|at| tables::PROPERTIES.get(at))
            .copied()
            .unwrap_or(0);
        Self {
            kind: Break::of(bits),
            pictographic: bits & 0x10 != 0,
            conjunct: match (bits >> 5) & 3 {
                1 => Conjunct::Consonant,
                2 => Conjunct::Extend,
                3 => Conjunct::Linker,
                _ => Conjunct::None,
            },
        }
    }
}

/// How far an emoji ZWJ sequence has come (GB11: `ExtPict Extend* ZWJ ×
/// ExtPict`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Emoji {
    #[default]
    None,
    /// An Extended_Pictographic character and any Extend after it.
    Pictograph,
    /// ... and a zero width joiner: another pictograph joins.
    Joined,
}

/// How far an Indic conjunct has come (GB9c: `Consonant [Extend Linker]*
/// Linker [Extend Linker]* × Consonant`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Indic {
    #[default]
    None,
    /// A consonant, and extenders after it but no linker.
    Consonant,
    /// ... and a linker: another consonant joins.
    Linked,
}

/// The state of a grapheme cluster, after its characters so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cluster {
    last: Break,
    emoji: Emoji,
    indic: Indic,
    /// The run of regional indicators ending the cluster is odd: the next
    /// one completes a flag.
    odd_indicators: bool,
}

impl Cluster {
    /// A cluster that starts with `c`.
    pub(crate) fn start(c: char) -> Self {
        Self::default().after(Properties::of(c), false)
    }

    /// The state of the cluster whose text is `text`, as if its characters
    /// had been printed one after another.
    pub(crate) fn of(text: &str) -> Self {
        let mut chars = text.chars();
        let mut cluster = chars.next().map(Self::start).unwrap_or_default();
        for c in chars {
            cluster.push(c);
        }
        cluster
    }

    /// Whether `c` continues the cluster, rather than starting the next one.
    /// Either way the state moves on to include `c`.
    pub(crate) fn push(&mut self, c: char) -> bool {
        let next = Properties::of(c);
        let joins = self.joins(next);
        *self = self.after(next, joins);
        joins
    }

    /// Whether the cluster ends with a Prepend character, which joins
    /// what follows it but a control (GB9b): printable ASCII too.
    pub(crate) fn prepended(self) -> bool {
        self.last == Break::Prepend
    }

    fn joins(&self, next: Properties) -> bool {
        use Break::{
            Control, Cr, Extend, L, Lf, Lv, Lvt, Prepend, RegionalIndicator, SpacingMark, T, V, Zwj,
        };
        match (self.last, next.kind) {
            // GB3, GB4, GB5.
            (Cr, Lf) => true,
            (Control | Cr | Lf, _) | (_, Control | Cr | Lf) => false,
            // GB6, GB7, GB8: Hangul syllables.
            (L, L | V | Lv | Lvt) | (Lv | V, V | T) | (Lvt | T, T) => true,
            // GB9, GB9a.
            (_, Extend | Zwj | SpacingMark) => true,
            // GB9b.
            (Prepend, _) => true,
            // GB9c.
            _ if self.indic == Indic::Linked && next.conjunct == Conjunct::Consonant => true,
            // GB11.
            _ if self.emoji == Emoji::Joined && next.pictographic => true,
            // GB12, GB13.
            (RegionalIndicator, RegionalIndicator) => self.odd_indicators,
            // GB999.
            _ => false,
        }
    }

    /// The state after `next`, which `joins` the cluster or starts one.
    fn after(self, next: Properties, joins: bool) -> Self {
        // A rule's progress carries on only within one cluster.
        fn within<T: Default>(joins: bool, state: T) -> T {
            if joins { state } else { T::default() }
        }
        let emoji = if next.pictographic {
            Emoji::Pictograph
        } else {
            match (within(joins, self.emoji), next.kind) {
                (Emoji::Pictograph, Break::Extend) => Emoji::Pictograph,
                (Emoji::Pictograph, Break::Zwj) => Emoji::Joined,
                _ => Emoji::None,
            }
        };
        let indic = match (within(joins, self.indic), next.conjunct) {
            (_, Conjunct::Consonant) => Indic::Consonant,
            (Indic::Consonant | Indic::Linked, Conjunct::Linker) => Indic::Linked,
            (state, Conjunct::Extend) => state,
            _ => Indic::None,
        };
        let odd_indicators = next.kind == Break::RegionalIndicator
            && !(joins && self.last == Break::RegionalIndicator && self.odd_indicators);
        Self {
            last: next.kind,
            emoji,
            indic,
            odd_indicators,
        }
    }
}

#[cfg(test)]
mod tests;
