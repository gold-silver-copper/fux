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

/// Grapheme_Cluster_Break, numbered as the generated tables store it.
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

impl Break {
    fn of(bits: u8) -> Self {
        match bits & 0x0f {
            1 => Self::Cr,
            2 => Self::Lf,
            3 => Self::Control,
            4 => Self::Extend,
            5 => Self::Zwj,
            6 => Self::RegionalIndicator,
            7 => Self::Prepend,
            8 => Self::SpacingMark,
            9 => Self::L,
            10 => Self::V,
            11 => Self::T,
            12 => Self::Lv,
            13 => Self::Lvt,
            _ => Self::Other,
        }
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
    /// Either way the state moves on to include `c`. Unlike UAX #29 (GB9b),
    /// nothing joins a Prepend character: terminals keep them apart, so a
    /// letter or a space is never swallowed into its cell.
    pub(crate) fn push(&mut self, c: char) -> bool {
        self.push_with(c, false)
    }

    /// `push`, with GB9b (`Prepend ×`) followed if `prepend_joins`.
    fn push_with(&mut self, c: char, prepend_joins: bool) -> bool {
        let next = Properties::of(c);
        let joins = self.joins(next, prepend_joins);
        *self = self.after(next, joins);
        joins
    }

    fn joins(&self, next: Properties, prepend_joins: bool) -> bool {
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
            // GB9b, not followed by `push`.
            (Prepend, _) => prepend_joins,
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
