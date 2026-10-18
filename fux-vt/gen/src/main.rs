//! Generates fux-vt's Unicode tables (grapheme segmentation and widths)
//! from the Unicode Character Database, and the conformance cases its tests
//! run.
//!
//! ```sh
//! cargo run --manifest-path fux-vt/gen/Cargo.toml -- <ucd directory>
//! cargo run --manifest-path fux-vt/gen/Cargo.toml -- catalogue references
//! ```
//!
//! The second form writes the conformance table's catalogue, the control
//! functions the references define (`catalogue.rs`).
//!
//! The directory holds, from `https://www.unicode.org/Public/<version>/ucd/`,
//! `auxiliary/GraphemeBreakProperty.txt`, `auxiliary/GraphemeBreakTest.txt`,
//! `emoji/emoji-data.txt`, `emoji/emoji-variation-sequences.txt`,
//! `DerivedCoreProperties.txt`, `EastAsianWidth.txt`,
//! `HangulSyllableType.txt`, `PropList.txt` and `UnicodeData.txt`, all in
//! one directory (see fux-vt/gen/README.md). The version is read from them
//! (`UnicodeData.txt` names none), and they must agree.
//!
//! Output, relative to the repository root:
//! - `fux-vt/src/unicode/tables.rs`: sixteen bits of properties per code
//!   point, in a two-stage table.
//! - `fux-vt/tests/data/GraphemeBreakTest.txt`: the conformance cases alone.
//!
//! The widths are fux-vt's width rule (fux-vt's README, "Width"): each
//! character's columns as unicode-width 0.2.2 counts them for Unicode
//! 17.0.0 (its `scripts/unicode.py`: `load_zero_widths`, then East Asian
//! Wide and Fullwidth as 2, the rest as 1), but U+17D8 at 1, not 3; and
//! the properties its string rule reads within a cluster.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod catalogue;

/// Grapheme_Cluster_Break values, numbered as fux-vt's `unicode.rs` reads them.
const BREAKS: [&str; 14] = [
    "Other",
    "CR",
    "LF",
    "Control",
    "Extend",
    "ZWJ",
    "Regional_Indicator",
    "Prepend",
    "SpacingMark",
    "L",
    "V",
    "T",
    "LV",
    "LVT",
];
/// Bit 4: Extended_Pictographic.
const PICTOGRAPHIC: u16 = 1 << 4;
/// Bits 5 and 6: Indic_Conjunct_Break, 0 for None.
const CONJUNCTS: [(&str, u16); 3] = [
    ("Consonant", 1 << 5),
    ("Extend", 2 << 5),
    ("Linker", 3 << 5),
];
/// Bits 7 and 8: the columns the character takes alone, 0 to 2; 3 for a
/// control (C0, DEL and C1), which takes none.
const WIDTH_SHIFT: u32 = 7;
const CONTROL: u16 = 3;
/// Bit 9: Emoji_Presentation, but neither an emoji modifier nor a regional
/// indicator (unicode-width's `WidthInfo::EMOJI_PRESENTATION`).
const PRESENTATION: u16 = 1 << 9;
/// Bit 10: an emoji presentation sequence starts with it (`emoji style`,
/// with U+FE0F, in emoji-variation-sequences.txt).
const EMOJI_STYLE: u16 = 1 << 10;
/// Bit 11: a text presentation sequence starts with it (`text style`, with
/// U+FE0E) and it is Emoji_Presentation, outside the Enclosed Ideographic
/// Supplement (U+1F200 to U+1F2FF), which stays wide.
const TEXT_STYLE: u16 = 1 << 11;
/// Bit 12: Emoji_Modifier_Base.
const MODIFIER_BASE: u16 = 1 << 12;
/// Bit 13: a combining mark (Mn, Mc, Me) that is Default_Ignorable: it
/// interrupts no ligature in unicode-width's string rule.
const LIGATURE_TRANSPARENT: u16 = 1 << 13;

const CODE_POINTS: usize = 0x11_0000;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let usage = "usage: fux-vt-gen <ucd directory> | fux-vt-gen catalogue <references directory>";
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut args = std::env::args_os().skip(1);
    let first = args.next().ok_or(usage)?;
    if first == "catalogue" {
        let references = args.next().map(PathBuf::from).ok_or(usage)?;
        return catalogue::run(&references, &root.join("fux-vt/tests/data/conformance.txt"));
    }
    let ucd = PathBuf::from(first);

    let breaks = read(&ucd, "GraphemeBreakProperty.txt")?;
    let emoji = read(&ucd, "emoji-data.txt")?;
    let variations = read(&ucd, "emoji-variation-sequences.txt")?;
    let derived = read(&ucd, "DerivedCoreProperties.txt")?;
    let east_asian = read(&ucd, "EastAsianWidth.txt")?;
    let hangul = read(&ucd, "HangulSyllableType.txt")?;
    let props = read(&ucd, "PropList.txt")?;
    let data = read(&ucd, "UnicodeData.txt")?;
    let test = read(&ucd, "GraphemeBreakTest.txt")?;
    let version = version_of(&breaks, "GraphemeBreakProperty")?;
    for (text, name) in [
        (&derived, "DerivedCoreProperties"),
        (&east_asian, "EastAsianWidth"),
        (&hangul, "HangulSyllableType"),
        (&props, "PropList"),
        (&test, "GraphemeBreakTest"),
    ] {
        let other = version_of(text, name)?;
        if other != version {
            return Err(format!("{name} is Unicode {other}, not {version}").into());
        }
    }
    // The emoji files name their version on a line of their own, without
    // the update number.
    for (text, name) in [
        (&emoji, "emoji-data"),
        (&variations, "emoji-variation-sequences"),
    ] {
        let emoji_version = text
            .lines()
            .find_map(|line| line.strip_prefix("# Version: "))
            .ok_or(format!("{name}: no version line"))?;
        if !version.starts_with(&format!("{emoji_version}.")) {
            return Err(format!("{name} is version {emoji_version}, not {version}").into());
        }
    }

    let mut properties = vec![0u16; CODE_POINTS];
    for (range, fields) in entries(&breaks) {
        let value = BREAKS
            .iter()
            .position(|name| *name == fields[0])
            .ok_or_else(|| format!("unknown Grapheme_Cluster_Break {}", fields[0]))?;
        for p in &mut properties[range] {
            *p = (*p & !0x0f) | u16::try_from(value)?;
        }
    }
    for (range, fields) in entries(&emoji) {
        if fields[0] == "Extended_Pictographic" {
            for p in &mut properties[range] {
                *p |= PICTOGRAPHIC;
            }
        }
    }
    for (range, fields) in entries(&derived) {
        if fields[0] != "InCB" {
            continue;
        }
        let bits = CONJUNCTS
            .iter()
            .find(|(name, _)| fields.get(1) == Some(name))
            .map(|(_, bits)| *bits)
            .ok_or_else(|| format!("unknown InCB {:?}", fields.get(1)))?;
        for p in &mut properties[range] {
            *p |= bits;
        }
    }

    let widths = widths(&breaks, &derived, &east_asian, &hangul, &props);
    for (p, width) in properties.iter_mut().zip(widths) {
        *p |= width << WIDTH_SHIFT;
    }
    emoji_bits(&mut properties, &emoji, &props, &variations)?;
    let marks = marks(&data)?;
    for (range, fields) in entries(&derived) {
        if fields[0] == "Default_Ignorable_Code_Point" {
            for code in range.filter(|&c| marks[c]) {
                properties[code] |= LIGATURE_TRANSPARENT;
            }
        }
    }

    let tables = tables(&properties, &version)?;
    write(&root.join("fux-vt/src/unicode/tables.rs"), &tables)?;
    write(
        &root.join("fux-vt/tests/data/GraphemeBreakTest.txt"),
        &conformance(&test, &version),
    )?;
    Ok(())
}

fn read(directory: &Path, name: &str) -> Result<String> {
    let path = directory.join(name);
    std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The version from a file's first line, `# Name-17.0.0.txt`.
fn version_of(text: &str, name: &str) -> Result<String> {
    text.lines()
        .next()
        .and_then(|line| line.strip_prefix(&format!("# {name}-")))
        .and_then(|rest| rest.strip_suffix(".txt"))
        .map(str::to_owned)
        .ok_or_else(|| format!("{name}: no version on the first line").into())
}

/// Each data line's code point range and its `;`-separated fields.
fn entries(text: &str) -> impl Iterator<Item = (std::ops::Range<usize>, Vec<&str>)> {
    text.lines().filter_map(|line| {
        let data = line.split('#').next()?.trim();
        if data.is_empty() {
            return None;
        }
        let mut fields = data.split(';').map(str::trim);
        let range = fields.next()?;
        let (first, last) = range.split_once("..").unwrap_or((range, range));
        let first = usize::from_str_radix(first, 16).ok()?;
        let last = usize::from_str_radix(last, 16).ok()?;
        Some((first..last + 1, fields.collect()))
    })
}

/// Each code point's columns alone (`WIDTH_SHIFT`'s two bits), as
/// unicode-width 0.2.2's `scripts/unicode.py` makes them outside East Asian
/// contexts: 0 for its zero widths (`load_zero_widths`), else 2 for East
/// Asian Wide and Fullwidth, else 1; U+17A4 at 2; and U+17D8, which it
/// makes 3, at 1 (fux-vt's one departure, as xterm 411 and glibc have it).
/// C0, DEL and C1 are controls.
fn widths(breaks: &str, derived: &str, east_asian: &str, hangul: &str, props: &str) -> Vec<u16> {
    let mut wide = vec![false; CODE_POINTS];
    for (range, fields) in entries(east_asian) {
        if matches!(fields[0], "W" | "F") {
            wide[range].fill(true);
        }
    }
    let mut zero = vec![false; CODE_POINTS];
    for (range, fields) in entries(derived) {
        if matches!(
            fields[0],
            "Default_Ignorable_Code_Point" | "Grapheme_Extend"
        ) {
            zero[range].fill(true);
        }
    }
    // Vowel and trailing jamo: a decomposed syllable is as wide as its
    // leading jamo, as glibc's wcwidth has it.
    for (range, fields) in entries(hangul) {
        if matches!(fields[0], "V" | "T") {
            zero[range].fill(true);
        }
    }
    // The Syriac abbreviation mark and four Arabic
    // Prepended_Concatenation_Marks; and every other Prepend character.
    for code in [0x070f, 0x0605, 0x0890, 0x0891, 0x08e2] {
        zero[code] = true;
    }
    let mut concatenation = vec![false; CODE_POINTS];
    for (range, fields) in entries(props) {
        if fields[0] == "Prepended_Concatenation_Mark" {
            concatenation[range].fill(true);
        }
    }
    for (range, fields) in entries(breaks) {
        if fields[0] == "Prepend" {
            for code in range.filter(|&c| !concatenation[c]) {
                zero[code] = true;
            }
        }
    }
    // HANGUL CHOSEONG FILLER, TIFINAGH CONSONANT JOINER, DEVANAGARI CARET.
    zero[0x115f] = false;
    zero[0x2d7f] = false;
    zero[0xa8fa] = true;
    let mut widths: Vec<u16> = (0..CODE_POINTS)
        .map(|code| match (zero[code], wide[code]) {
            (true, _) => 0,
            (false, true) => 2,
            (false, false) => 1,
        })
        .collect();
    widths[0x17a4] = 2;
    widths[0x17d8] = 1;
    for code in (0..0x20).chain(0x7f..0xa0) {
        widths[code] = CONTROL;
    }
    widths
}

/// The emoji bits: `PRESENTATION`, `MODIFIER_BASE`, `EMOJI_STYLE` and
/// `TEXT_STYLE`.
fn emoji_bits(properties: &mut [u16], emoji: &str, props: &str, variations: &str) -> Result<()> {
    let mut presentation = vec![false; CODE_POINTS];
    for (range, fields) in entries(emoji) {
        match fields[0] {
            "Emoji_Presentation" => presentation[range].fill(true),
            "Emoji_Modifier_Base" => properties[range]
                .iter_mut()
                .for_each(|p| *p |= MODIFIER_BASE),
            _ => {}
        }
    }
    let mut shown = presentation.clone();
    // unicode-width's later special cases take emoji modifiers and regional
    // indicators out of its emoji presentation class.
    for (range, fields) in entries(emoji).chain(entries(props)) {
        if matches!(fields[0], "Emoji_Modifier" | "Regional_Indicator") {
            shown[range].fill(false);
        }
    }
    for (p, shown) in properties.iter_mut().zip(shown) {
        if shown {
            *p |= PRESENTATION;
        }
    }
    for line in variations.lines() {
        let data = line.split('#').next().unwrap_or("");
        let mut fields = data.split(';').map(str::trim);
        let (Some(sequence), Some(style)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (bit, selector) = match style {
            "emoji style" => (EMOJI_STYLE, "FE0F"),
            "text style" => (TEXT_STYLE, "FE0E"),
            _ => continue,
        };
        let mut codes = sequence.split_whitespace();
        let (Some(first), Some(second)) = (codes.next(), codes.next()) else {
            return Err(format!("emoji-variation-sequences: {line:?}").into());
        };
        if second != selector {
            return Err(format!("emoji-variation-sequences: {line:?}").into());
        }
        let code = usize::from_str_radix(first, 16)?;
        // A text style that changes a width: of an emoji shown wide by
        // default, but not an ideograph's.
        if bit == TEXT_STYLE && (!presentation[code] || (0x1f200..0x1f300).contains(&code)) {
            continue;
        }
        properties[code] |= bit;
    }
    Ok(())
}

/// Whether each code point is a combining mark (General_Category Mn, Mc or
/// Me), by UnicodeData.txt.
fn marks(data: &str) -> Result<Vec<bool>> {
    let mut marks = vec![false; CODE_POINTS];
    let mut first = None;
    for line in data.lines() {
        let mut fields = line.split(';');
        let (Some(code), Some(name), Some(category)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("UnicodeData: {line:?}").into());
        };
        let code = usize::from_str_radix(code, 16)?;
        let mark = matches!(category, "Mn" | "Mc" | "Me");
        // A range is given by its first and last code points.
        let from = match name.ends_with(", Last>") {
            true => first
                .take()
                .ok_or(format!("UnicodeData: {line:?} begins no range"))?,
            false => code,
        };
        if name.ends_with(", First>") {
            first = Some(code);
        }
        marks[from..=code].fill(mark);
    }
    Ok(marks)
}

/// The two-stage table in the block size that makes it smallest.
fn tables(properties: &[u16], version: &str) -> Result<String> {
    let (shift, index, blocks) = (4..=10)
        .map(|shift| {
            let size = 1usize << shift;
            let mut seen: HashMap<&[u16], u16> = HashMap::new();
            let mut blocks: Vec<u16> = Vec::new();
            let mut index = Vec::new();
            for block in pieces(properties, size) {
                let next = u16::try_from(seen.len()).unwrap_or(u16::MAX);
                let id = *seen.entry(block).or_insert_with(|| {
                    blocks.extend_from_slice(block);
                    next
                });
                index.push(id);
            }
            (shift, index, blocks)
        })
        .min_by_key(|(_, index, blocks)| (index.len() + blocks.len()) * 2)
        .ok_or("no block size")?;
    let (major, minor, update) = {
        let mut parts = version.split('.').map(str::parse::<u8>);
        (
            parts.next().ok_or("version")??,
            parts.next().ok_or("version")??,
            parts.next().ok_or("version")??,
        )
    };

    let mut out = String::new();
    writeln!(
        out,
        "//! Grapheme segmentation and width properties, generated by fux-vt/gen\n\
         //! from the Unicode Character Database {version}. Do not edit: regenerate.\n\
         //!\n\
         //! Sixteen bits a code point: bits 0-3 Grapheme_Cluster_Break (numbered\n\
         //! as `Break` in unicode.rs), bit 4 Extended_Pictographic, bits 5-6\n\
         //! Indic_Conjunct_Break (0 None, 1 Consonant, 2 Extend, 3 Linker),\n\
         //! bits 7-8 the columns it takes alone (3: a control), bit 9 emoji\n\
         //! presentation, bits 10 and 11 an emoji and a text presentation\n\
         //! sequence starting with it, bit 12 Emoji_Modifier_Base, bit 13 a\n\
         //! combining mark that is Default_Ignorable.\n\
         //! `BLOCK` holds the block each run of `1 << SHIFT` code points uses;\n\
         //! `PROPERTIES` holds the distinct blocks.\n"
    )?;
    writeln!(
        out,
        "/// The Unicode version grapheme clusters are segmented and measured by."
    )?;
    writeln!(
        out,
        "pub const UNICODE_VERSION: (u8, u8, u8) = ({major}, {minor}, {update});"
    )?;
    writeln!(out, "pub(super) const SHIFT: u32 = {shift};")?;
    write_array(&mut out, "BLOCK", "u16", &index)?;
    write_array(&mut out, "PROPERTIES", "u16", &blocks)?;
    Ok(out)
}

fn write_array<T: std::fmt::Display>(
    out: &mut String,
    name: &str,
    ty: &str,
    values: &[T],
) -> Result<()> {
    writeln!(
        out,
        "pub(super) static {name}: [{ty}; {}] = [",
        values.len()
    )?;
    for line in pieces(values, 24) {
        out.push_str("   ");
        for value in line {
            write!(out, " {value},")?;
        }
        out.push('\n');
    }
    writeln!(out, "];")?;
    Ok(())
}

/// `items` in pieces of `size`, the last one shorter (fux's clippy.toml
/// forbids `chunks`, which panics on a zero size).
fn pieces<T>(items: &[T], size: usize) -> impl Iterator<Item = &[T]> {
    let mut rest = items;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (piece, tail) = rest.split_at_checked(size.max(1)).unwrap_or((rest, &[]));
        rest = tail;
        Some(piece)
    })
}

/// The conformance cases without their comments: one a line, `÷` a
/// boundary and `×` none, between hexadecimal code points.
fn conformance(text: &str, version: &str) -> String {
    let mut out = format!("# GraphemeBreakTest-{version}.txt, cases only (fux-vt/gen)\n");
    for line in text.lines() {
        let data = line.split('#').next().unwrap_or("").trim();
        if !data.is_empty() {
            out.push_str(data);
            out.push('\n');
        }
    }
    out
}
