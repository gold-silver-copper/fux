//! Generates fux-vt's grapheme segmentation tables from the Unicode Character
//! Database, and the conformance cases its tests run.
//!
//! ```sh
//! cargo run --manifest-path fux-vt/gen/Cargo.toml -- <ucd directory>
//! ```
//!
//! The directory holds, from `https://www.unicode.org/Public/<version>/ucd/`,
//! `auxiliary/GraphemeBreakProperty.txt`, `auxiliary/GraphemeBreakTest.txt`,
//! `emoji/emoji-data.txt` and `DerivedCoreProperties.txt`, all in one
//! directory (see fux-vt/gen/README.md). The version is read from them, and
//! they must agree.
//!
//! Output, relative to the repository root:
//! - `fux-vt/src/unicode/tables.rs`: one byte of properties per code point,
//!   in a two-stage table.
//! - `fux-vt/tests/data/GraphemeBreakTest.txt`: the conformance cases alone.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
const PICTOGRAPHIC: u8 = 1 << 4;
/// Bits 5 and 6: Indic_Conjunct_Break, 0 for None.
const CONJUNCTS: [(&str, u8); 3] = [
    ("Consonant", 1 << 5),
    ("Extend", 2 << 5),
    ("Linker", 3 << 5),
];

const CODE_POINTS: usize = 0x11_0000;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let ucd = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: fux-vt-gen <ucd directory>")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    let breaks = read(&ucd, "GraphemeBreakProperty.txt")?;
    let emoji = read(&ucd, "emoji-data.txt")?;
    let derived = read(&ucd, "DerivedCoreProperties.txt")?;
    let test = read(&ucd, "GraphemeBreakTest.txt")?;
    let version = version_of(&breaks, "GraphemeBreakProperty")?;
    for (text, name) in [
        (&derived, "DerivedCoreProperties"),
        (&test, "GraphemeBreakTest"),
    ] {
        let other = version_of(text, name)?;
        if other != version {
            return Err(format!("{name} is Unicode {other}, not {version}").into());
        }
    }
    // emoji-data.txt names its version on a line of its own, without the
    // update number.
    let emoji_version = emoji
        .lines()
        .find_map(|line| line.strip_prefix("# Version: "))
        .ok_or("emoji-data: no version line")?;
    if !version.starts_with(&format!("{emoji_version}.")) {
        return Err(format!("emoji-data is version {emoji_version}, not {version}").into());
    }

    let mut properties = vec![0u8; CODE_POINTS];
    for (range, fields) in entries(&breaks) {
        let value = BREAKS
            .iter()
            .position(|name| *name == fields[0])
            .ok_or_else(|| format!("unknown Grapheme_Cluster_Break {}", fields[0]))?;
        for p in &mut properties[range] {
            *p = (*p & !0x0f) | u8::try_from(value)?;
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

/// The two-stage table in the block size that makes it smallest.
fn tables(properties: &[u8], version: &str) -> Result<String> {
    let (shift, index, blocks) = (4..=10)
        .map(|shift| {
            let size = 1usize << shift;
            let mut seen: HashMap<&[u8], u16> = HashMap::new();
            let mut blocks: Vec<u8> = Vec::new();
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
        .min_by_key(|(_, index, blocks)| index.len() * 2 + blocks.len())
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
        "//! Grapheme segmentation properties, generated by fux-vt/gen from the\n\
         //! Unicode Character Database {version}. Do not edit: regenerate.\n\
         //!\n\
         //! One byte a code point: bits 0-3 Grapheme_Cluster_Break (numbered as\n\
         //! `Break` in unicode.rs), bit 4 Extended_Pictographic, bits 5-6\n\
         //! Indic_Conjunct_Break (0 None, 1 Consonant, 2 Extend, 3 Linker).\n\
         //! `BLOCK` holds the block each run of `1 << SHIFT` code points uses;\n\
         //! `PROPERTIES` holds the distinct blocks.\n"
    )?;
    writeln!(
        out,
        "/// The Unicode version grapheme clusters are segmented by."
    )?;
    writeln!(
        out,
        "pub const UNICODE_VERSION: (u8, u8, u8) = ({major}, {minor}, {update});"
    )?;
    writeln!(out, "pub(super) const SHIFT: u32 = {shift};")?;
    write_array(&mut out, "BLOCK", "u16", &index)?;
    write_array(&mut out, "PROPERTIES", "u8", &blocks)?;
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
