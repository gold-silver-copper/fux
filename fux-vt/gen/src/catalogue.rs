//! The conformance table's catalogue: every control function the
//! references define, read from the references themselves, so that the
//! table (`fux-vt/tests/data/conformance.txt`) has a row for each and the
//! code decides none of them.
//!
//! - ECMA-48, 5th edition: each function of 8.3, with its representation
//!   and parameter defaults.
//! - DEC STD 070: each function page, found by its "Levels:" line, with
//!   that line. The PDF is a scan: `DEC_FIXES` mends what its text layer
//!   misreads.
//! - ctlseqs (xterm's, at the patch `references/fetch.sh` fetches): each
//!   heading that names a control.
//!
//! The PDFs are read through `pdftotext -layout` (poppler). A row already
//! in the table keeps its status, probe, tests and note; a new one has the
//! status `unknown`, which the table's test fails; a row whose function is
//! no longer in the references is dropped, and named.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use crate::Result;

/// One function a reference defines: the table's generated columns.
struct Entry {
    /// The reference and where in it: unique in the table.
    key: String,
    /// Its mnemonic, or the sequence for ctlseqs.
    name: String,
    title: String,
    /// ECMA-48: representation and defaults; DEC STD 070: its levels;
    /// ctlseqs: empty.
    detail: String,
}

/// DEC STD 070's pages whose scanned text misreads the heading: the PDF's
/// page (from 1), and the printed page, mnemonic and title it shows.
const DEC_FIXES: &[(usize, &str, &str, &str)] = &[
    (
        173,
        "3-68",
        "",
        "ANNOUNCE SUBSET OF CODE EXTENSION FACILITIES",
    ),
    (177, "3-72", "S8C1T", "SELECT 8-BIT C1 TRANSMISSION"),
    (178, "3-73", "SI, LS0", "SHIFT IN, LOCKING SHIFT ZERO"),
    (179, "3-74", "SO, LS1", "SHIFT OUT, LOCKING SHIFT ONE"),
    (221, "4-17", "DA", "DEVICE ATTRIBUTES (Primary)"),
    (226, "4-22", "DECSCL", "SELECT CONFORMANCE LEVEL"),
    (228, "4-24", "DA2", "DEVICE ATTRIBUTES (Secondary)"),
    (
        230,
        "4-26",
        "DA3, DECRPTUI",
        "DEVICE ATTRIBUTES (Tertiary), REPORT TERMINAL UNIT ID",
    ),
    (232, "4-28", "DECID", "IDENTIFY TERMINAL"),
    (
        233,
        "4-29",
        "DECSR, DECSRC",
        "SECURE RESET, SECURE RESET CONFIRMATION",
    ),
    (235, "4-31", "DECSTR", "SOFT TERMINAL RESET"),
    (239, "4-35", "DSR", "DEVICE STATUS REPORT"),
    (306, "5-51", "HVP", "HORIZONTAL/VERTICAL POSITION"),
    (394, "5-139", "", "INSERT OR REPLACE GRAPHIC CHARACTER"),
    (417, "5-162", "DECSED", "SELECTIVE ERASE IN DISPLAY"),
    (427, "5-172", "DECSERA", "SELECTIVE ERASE RECTANGULAR AREA"),
    (717, "7-38", "DECPEX", "SET/RESET PRINT EXTENT MODE"),
    (1020, "13-5", "DECELR", "ENABLE LOCATOR REPORTS"),
    (1021, "13-6", "DECLRP", "LOCATOR REPORT"),
    (1023, "13-8", "DECRQLP", "REQUEST LOCATOR POSITION"),
    (1024, "13-9", "DECSLE", "SELECT LOCATOR EVENTS"),
    (1025, "13-10", "DECEFR", "ENABLE FILTER RECTANGLE"),
    (1028, "13-13", "DECLKD", "LOCATOR KEY DEFINITION"),
    (
        1030,
        "13-15",
        "DSR",
        "DEVICE STATUS REPORT (Locator Status)",
    ),
    (1047, "14-11", "DECSASD", "SELECT ACTIVE STATUS DISPLAY"),
    (1048, "14-12", "DECSSDT", "SELECT STATUS DISPLAY TYPE"),
];

/// The hand-kept columns of a row: status, probe, tests and note.
const HAND: usize = 4;

/// Reads the references under `references` and rewrites the table at
/// `table`, keeping each row's hand-kept columns.
pub fn run(references: &Path, table: &Path) -> Result<()> {
    let ecma = pdftotext(&references.join("standards/ECMA-48_5th_edition_1991.pdf"))?;
    let dec =
        pdftotext(&references.join("dec/DEC_STD_070_Video_Systems_Reference_Manual_1991.pdf"))?;
    let xterm = std::fs::read(references.join("xterm/ctlseqs.html"))?;
    let mut entries = ecma48(&ecma)?;
    entries.extend(dec070(&dec)?);
    entries.extend(ctlseqs(&String::from_utf8_lossy(&xterm))?);

    let old = std::fs::read_to_string(table).unwrap_or_default();
    let mut kept: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut extra = Vec::new();
    for line in old.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let fields: Vec<&str> = line.split('\t').collect();
        let key = fields[0];
        if is_generated(key) {
            kept.insert(key, fields[fields.len().saturating_sub(HAND)..].to_vec());
        } else {
            extra.push(line);
        }
    }
    let mut out = String::from(HEADER);
    let mut seen = HashMap::new();
    for e in &entries {
        if seen.insert(e.key.as_str(), ()).is_some() {
            return Err(format!("two functions at {}", e.key).into());
        }
        let hand = kept
            .remove(e.key.as_str())
            .map(|h| h.join("\t"))
            .unwrap_or_else(|| "unknown\t\t\t".to_string());
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{hand}",
            e.key, e.name, e.title, e.detail
        )?;
    }
    for line in extra {
        writeln!(out, "{line}")?;
    }
    let mut gone: Vec<_> = kept.into_keys().collect();
    gone.sort_unstable();
    for key in gone {
        eprintln!("dropped: {key} (no longer in the references)");
    }
    std::fs::write(table, out)?;
    eprintln!(
        "wrote {} ({} functions from the references)",
        table.display(),
        entries.len()
    );
    Ok(())
}

const HEADER: &str = "\
# fux-vt's conformance table: one row per control function the references
# define, written by `fux-vt-gen catalogue` (its first four columns) and
# kept by hand (the rest). See fux-vt/CONFORMANCE.md, which `tests/table.rs`
# renders from it and checks. Columns, tab-separated:
# key, name, title, detail | status, probe, tests, note
";

/// Whether a key names a reference row (gen's), not a row kept by hand.
fn is_generated(key: &str) -> bool {
    ["ECMA-48 ", "DEC STD 070 ", "ctlseqs "]
        .iter()
        .any(|p| key.starts_with(p))
}

fn pdftotext(pdf: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .arg("-layout")
        .arg(pdf)
        .arg("-")
        .output()?;
    if !out.status.success() {
        return Err(format!("pdftotext failed on {}", pdf.display()).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Words joined by single spaces.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// ECMA-48 8.3: each `8.3.n MNEMONIC - TITLE` heading followed by its
/// `Notation:` line (which the contents' lines are not).
fn ecma48(text: &str) -> Result<Vec<Entry>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("8.3.") else {
            continue;
        };
        let Some((number, heading)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        let Some((name, title)) = heading.trim().split_once(" - ") else {
            continue;
        };
        let body = &lines[i + 1..(i + 4).min(lines.len())];
        if !body.iter().any(|l| l.trim().starts_with("Notation:")) {
            continue;
        }
        let mut detail = Vec::new();
        let body = lines[i + 1..]
            .iter()
            .take_while(|l| !l.trim().starts_with("8.3."));
        for l in body.take(8) {
            let l = l.trim();
            if l.starts_with("Representation:") || l.starts_with("Parameter default") {
                detail.push(squeeze(l));
            }
        }
        out.push(Entry {
            key: format!("ECMA-48 8.3.{number}"),
            name: name.trim().to_string(),
            title: squeeze(title),
            detail: detail.join("; "),
        });
    }
    out.sort_by_key(|e| {
        e.key
            .trim_start_matches("ECMA-48 8.3.")
            .parse::<u32>()
            .unwrap_or(0)
    });
    if out.len() != 162 {
        return Err(format!("ECMA-48 8.3: {} functions, not 162", out.len()).into());
    }
    Ok(out)
}

/// DEC STD 070: each PDF page with a `Levels:` line is a function page;
/// its heading is the last line above it that ends in a mnemonic.
fn dec070(text: &str) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for (index, page) in text.split('\x0c').enumerate() {
        let number = index + 1;
        let lines: Vec<&str> = page.lines().collect();
        let Some(at) = lines.iter().position(|l| is_levels(l)) else {
            continue;
        };
        let levels = levels(lines[at].trim().split_once(':').map_or("", |(_, l)| l));
        let (page_id, name, title) = match DEC_FIXES.iter().find(|f| f.0 == number) {
            Some(&(_, p, n, t)) => (p.to_string(), n.to_string(), t.to_string()),
            None => {
                let printed = lines.iter().find_map(|l| printed_page(l)).ok_or(format!(
                    "DEC STD 070, PDF page {number}: no page number (add it to DEC_FIXES)"
                ))?;
                let (name, title) =
                    lines[..at]
                        .iter()
                        .rev()
                        .find_map(|l| heading(l))
                        .ok_or(format!(
                            "DEC STD 070, PDF page {number}: no heading (add it to DEC_FIXES)"
                        ))?;
                (printed, name, title)
            }
        };
        let key = if name.is_empty() {
            format!("DEC STD 070 {page_id}")
        } else {
            format!("DEC STD 070 {page_id} {}", name.replace(", ", "/"))
        };
        out.push(Entry {
            key,
            name,
            title,
            detail: format!("Levels: {levels}"),
        });
    }
    Ok(out)
}

/// A `Levels:` line's levels, with what the scan misreads mended: `l`,
/// `I` and `1.2` for 1 and `1, 2`, `21<` for `2x`, a stray comma.
fn levels(text: &str) -> String {
    let words: Vec<String> = squeeze(text)
        .replace("1.2", "1, 2")
        .replace("21<", "2x")
        .split(' ')
        .map(|w| match w.strip_prefix(['l', 'I']) {
            Some(rest) if rest.is_empty() || rest.starts_with(['x', 'X', ',']) => {
                format!("1{rest}")
            }
            _ => w.to_string(),
        })
        .collect();
    words
        .join(" ")
        .replace(" ,", ",")
        .trim_end_matches([',', ' '])
        .to_string()
}

fn is_levels(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("Levels") || t.starts_with("Level:") || t.starts_with("Level "))
        && t.split_once(':')
            .is_some_and(|(word, _)| word.trim().len() <= 6)
}

/// `Page 5-41` in a page's header, as `5-41`.
fn printed_page(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("age ")?;
    let id: String = rest
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    let (chapter, page) = id.split_once('-')?;
    (!chapter.is_empty() && !page.is_empty()).then_some(id)
}

/// A heading line: an upper-case title, a gap of three or more spaces, and
/// a mnemonic.
fn heading(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let gap = t.find("   ")?;
    let (title, name) = (t[..gap].trim(), t[gap..].trim());
    let ok_title = title.chars().any(|c| c.is_ascii_uppercase())
        && !title.contains("EL-")
        && !title.contains("Digital")
        && !title.contains("VSRM");
    let ok_name = !name.is_empty()
        && !name.contains(' ')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == ',');
    (ok_title && ok_name).then(|| (name.replace(',', ", "), squeeze(title)))
}

/// ctlseqs: every `h4` anchor whose text begins with a control's name.
fn ctlseqs(html: &str) -> Result<Vec<Entry>> {
    const CONTROLS: &[&str] = &[
        "ESC", "CSI", "DCS", "OSC", "APC", "PM", "BEL", "BS", "CR", "ENQ", "FF", "LF", "SI", "SO",
        "SP", "TAB", "VT", "FS", "GS", "RS", "US",
    ];
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<a name=\"h4-") {
        rest = &rest[at + 12..];
        let Some((anchor, after)) = rest.split_once("\">") else {
            break;
        };
        let Some((text, after)) = after.split_once("</a>") else {
            break;
        };
        let name = squeeze(&unescape(&strip_tags(text)));
        if !CONTROLS.contains(&name.split(' ').next().unwrap_or_default()) {
            continue;
        }
        let section = anchor.split(':').next().unwrap_or_default();
        let prefix = match section {
            s if s.starts_with("Sixel") => "Sixel ",
            s if s.starts_with("ReGIS") => "ReGIS ",
            s if s.starts_with("Tektronix") => "Tek ",
            s if s.starts_with("VT52") => "VT52 ",
            _ => "",
        };
        // The title: the rest of the heading's paragraph, to its first
        // sentence's end.
        let paragraph = after.split("\n\n").next().unwrap_or_default();
        let paragraph = paragraph.split("<a name=").next().unwrap_or_default();
        let title = squeeze(&unescape(&strip_tags(paragraph)));
        let title = match title.find(". ") {
            Some(end) => title[..=end].to_string(),
            None => title,
        };
        out.push(Entry {
            key: format!("ctlseqs {prefix}{name}"),
            name,
            title,
            detail: String::new(),
        });
    }
    // A heading repeated in one section (ctlseqs has several `CSI Ps c`
    // for its graphics terminals) is told apart by its count.
    let mut counts: HashMap<String, usize> = HashMap::new();
    for e in &mut out {
        let n = counts.entry(e.key.clone()).or_default();
        *n += 1;
        if *n > 1 {
            write!(e.key, " ({n})")?;
        }
    }
    Ok(out)
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}
