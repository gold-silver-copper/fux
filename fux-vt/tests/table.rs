//! The conformance table, `tests/data/conformance.txt`: a row for each
//! control function ECMA-48 (8.3), DEC STD 070 (its function pages) and
//! ctlseqs (its headings) define, written from the references by
//! `fux-vt-gen catalogue`, with fux-vt's status for each kept by hand, and
//! rows of its own for the modes and protocols the references leave in one
//! heading. This test keeps the table true:
//!
//! - every row has a known status, and a note unless it is implemented;
//! - every implemented, partial or departing row names tests, and every
//!   test it names exists;
//! - each row's probe, fed to a parser in a fixed context, does what the
//!   row says it does (`unhandled`, `reply`, `event`, `effect` or
//!   `nothing`), and that agrees with its status: an implemented function
//!   whose bytes are merely ignored is drift, unless its note says it "has
//!   no effect" (NUL, the delimiters of strings fux-vt drops);
//! - each implemented ECMA-48 function with a single `Pn` defaulting to 1
//!   does the same with the parameter left out, 0 and 1 (2.3.3, with ZDM
//!   set, F.4.2);
//! - `CONFORMANCE.md` is what the table renders (`FUX_VT_BLESS=1` writes
//!   it).

use fux_vt::{Event, Feature, Graphics, Identity, Options, Parser, Sink, Size, Unhandled};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const TABLE: &str = include_str!("data/conformance.txt");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

const STATUSES: [&str; 5] = [
    "implemented",
    "partial",
    "departure",
    "not implemented",
    "not applicable",
];
const OUTCOMES: [&str; 5] = ["unhandled", "reply", "event", "effect", "nothing"];

struct Row<'a> {
    line: usize,
    key: &'a str,
    name: &'a str,
    title: &'a str,
    detail: &'a str,
    status: &'a str,
    probe: &'a str,
    tests: &'a str,
    note: &'a str,
}

fn rows() -> Vec<Row<'static>> {
    TABLE
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.starts_with('#') && !l.is_empty())
        .map(|(i, l)| {
            let f: Vec<&str> = l.split('\t').collect();
            let at = |n: usize| f.get(n).copied().unwrap_or_default();
            Row {
                line: i.saturating_add(1),
                key: at(0),
                name: at(1),
                title: at(2),
                detail: at(3),
                status: at(4),
                probe: at(5),
                tests: at(6),
                note: at(7),
            }
        })
        .collect()
}

#[test]
fn every_row_has_a_status_and_its_reason() {
    let mut bad = Vec::new();
    for r in rows() {
        let line = r.line;
        if TABLE.lines().nth(line - 1).map(|l| l.split('\t').count()) != Some(8) {
            bad.push(format!("line {line}: not eight tab-separated columns"));
        }
        if !STATUSES.contains(&r.status) {
            bad.push(format!("line {line} {}: status {:?}", r.key, r.status));
        }
        if r.status != "implemented" && r.note.is_empty() {
            bad.push(format!("line {line} {}: {} with no note", r.key, r.status));
        }
        if ["implemented", "partial", "departure"].contains(&r.status) && r.tests.is_empty() {
            bad.push(format!("line {line} {}: {} with no tests", r.key, r.status));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Every `fn` in the crate's test sources, by the module path the README
/// names them with: `tests/x.rs` and `tests/x/*.rs` as `x`, `src/a/b.rs`
/// and `src/a/b/tests.rs` as `a::b` and `a::b::tests`.
fn test_functions() -> BTreeMap<String, Vec<String>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(ROOT);
    let mut files = Vec::new();
    walk(&root.join("tests"), &mut files);
    walk(&root.join("src"), &mut files);
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(root).unwrap_or(&file).with_extension("");
        let parts: Vec<String> = rel
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let module = match parts.split_first() {
            Some((top, rest)) if top == "tests" => rest.first().cloned().unwrap_or_default(),
            Some((_, rest)) => rest.join("::"),
            None => continue,
        };
        let names: Vec<String> = text
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                let l = l.strip_prefix("pub ").unwrap_or(l);
                let name = l.strip_prefix("fn ")?;
                Some(name.split(['(', '<']).next()?.to_string())
            })
            .collect();
        for module in [module.clone(), format!("{module}::tests")] {
            out.entry(module.replace("::tests::tests", "::tests"))
                .or_default()
                .extend(names.iter().cloned());
        }
    }
    out
}

/// A glob: `*` matches any run of characters.
fn matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(at) => {
                rest = rest
                    .get(at.saturating_add(part.len())..)
                    .unwrap_or_default()
            }
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[test]
fn every_test_a_row_names_exists() {
    let functions = test_functions();
    let mut bad = Vec::new();
    for r in rows() {
        for spec in r.tests.split(", ").filter(|s| !s.is_empty()) {
            let spec = spec.trim_matches('`');
            if !exists(&functions, spec) {
                bad.push(format!("line {} {}: no test {spec}", r.line, r.key));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The README's tables name the tests of each row in their last column:
/// each must exist, as the table's do.
#[test]
fn every_test_the_readme_names_exists() -> Result<(), Box<dyn std::error::Error>> {
    let functions = test_functions();
    let readme = std::fs::read_to_string(Path::new(ROOT).join("README.md"))?;
    let mut bad = Vec::new();
    let mut named = 0;
    for (i, line) in readme.lines().enumerate() {
        let Some(last) = line.strip_suffix(" |").and_then(|l| l.rsplit(" | ").next()) else {
            continue;
        };
        for spec in last.split(", ") {
            let Some(spec) = spec.strip_prefix('`').and_then(|s| s.strip_suffix('`')) else {
                continue;
            };
            if !spec.contains("::") {
                continue;
            }
            named += 1;
            if !exists(&functions, spec) {
                bad.push(format!("README.md:{}: no test {spec}", i + 1));
            }
        }
    }
    assert!(named >= 90, "only {named} tests named");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    Ok(())
}

/// Whether a test spec, `module::name` (`*` matching any run) or a module
/// alone, names something in `functions`.
fn exists(functions: &BTreeMap<String, Vec<String>>, spec: &str) -> bool {
    match spec.rsplit_once("::") {
        Some((module, pattern)) => {
            functions
                .get(module)
                .is_some_and(|names| names.iter().any(|n| matches(pattern, n)))
                || functions.contains_key(spec)
        }
        None => functions.contains_key(spec),
    }
}

/// What a sink was given.
#[derive(Default)]
struct Record {
    unhandled: usize,
    replies: Vec<Vec<u8>>,
    events: usize,
}
impl Sink for Record {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.push(bytes.to_vec());
    }
    fn event(&mut self, _event: Event<'_>) {
        self.events = self.events.saturating_add(1);
    }
    fn unhandled(&mut self, _sequence: Unhandled<'_>) {
        self.unhandled = self.unhandled.saturating_add(1);
    }
}

/// A probe's bytes: `\e` is ESC, `\xHH` a byte, `\\` a backslash.
fn bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match rest.split_first() {
            Some((b'e', tail)) => {
                out.push(0x1b);
                rest = tail;
            }
            Some((b'x', tail)) => {
                let hex = tail.get(..2).and_then(|h| std::str::from_utf8(h).ok());
                out.push(
                    hex.and_then(|h| u8::from_str_radix(h, 16).ok())
                        .unwrap_or(0),
                );
                rest = tail.get(2..).unwrap_or_default();
            }
            Some((b'\\', tail)) => {
                out.push(b'\\');
                rest = tail;
            }
            _ => out.push(b'\\'),
        }
    }
    out
}

/// The context every probe runs in: a 6 by 12 screen, every feature on
/// but the one `without` names (`without Pictures: BYTES`) and an identity
/// (but `without Identity`), four lines of text and the cursor on the third line's
/// fifth column, an SGR last so no character is the last printed.
fn context(without: &str) -> Result<Parser, Box<dyn std::error::Error>> {
    let options = fux_vt::Feature::ALL
        .iter()
        .fold(Options::new(), |o, f| {
            o.set(*f, format!("{f:?}") != without)
        })
        .with_identity((without != "Identity").then_some(Identity {
            name: "fux",
            version: "1.2.3",
        }));
    let mut parser = Parser::with_options(Size::new(6, 12)?, 1 << 16, options)?;
    parser.set_host_graphics(fux_vt::CellSize::new(10, 20).map_or(Graphics::Text, Graphics::Sixel));
    let text = b"\x1b[Habcdefghijk\r\nlmnopqrstuv\r\nwxyzABCDEFG\r\nHIJKLMNOPQR\x1b[3;5H\x1b[m";
    parser.process_with(text, &mut Record::default())?;
    Ok(parser)
}

/// The screen's whole state, as `Debug` shows it, but its version, which
/// every call to the parser moves on (the host's "something may have
/// changed"), and its revision, which counts the bytes read.
fn state(parser: &Parser) -> String {
    let text = format!("{:#?}", parser.screen());
    let counts = ["    version: ", "    revision: "];
    let lines = text
        .lines()
        .filter(|l| !counts.iter().any(|c| l.starts_with(c)));
    lines.collect::<Vec<_>>().join("\n")
}

/// What `probe` (`[SETUP >> ]BYTES`) does in the context: the setup's
/// effects are the baseline.
fn observe(probe: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    let (without, probe) = match probe.strip_prefix("without ") {
        Some(rest) => rest.split_once(": ").unwrap_or(("", rest)),
        None => ("", probe),
    };
    let (setup, probe) = probe.split_once(" >> ").unwrap_or(("", probe));
    let mut parser = context(without)?;
    parser.process_with(&bytes(setup), &mut Record::default())?;
    let before = state(&parser);
    let mut record = Record::default();
    parser.process_with(&bytes(probe), &mut record)?;
    Ok(if record.unhandled > 0 {
        "unhandled"
    } else if !record.replies.is_empty() {
        "reply"
    } else if record.events > 0 {
        "event"
    } else if state(&parser) != before {
        "effect"
    } else {
        "nothing"
    })
}

#[test]
fn every_probe_does_what_its_row_says() -> Result<(), Box<dyn std::error::Error>> {
    let mut bad = Vec::new();
    for r in rows() {
        if r.probe.is_empty() {
            if r.status == "implemented" {
                bad.push(format!(
                    "line {} {}: implemented with no probe",
                    r.line, r.key
                ));
            }
            continue;
        }
        let Some((probe, expected)) = r.probe.rsplit_once(" => ") else {
            bad.push(format!(
                "line {} {}: probe with no `=> outcome`",
                r.line, r.key
            ));
            continue;
        };
        if !OUTCOMES.contains(&expected) {
            bad.push(format!("line {} {}: outcome {expected:?}", r.line, r.key));
        }
        let seen = observe(probe)?;
        if seen != expected {
            bad.push(format!(
                "line {} {}: {probe} => {expected}, but it is {seen}",
                r.line, r.key
            ));
        }
        // Bytes that are merely ignored, or refused, are no implementation,
        // unless the function is to do nothing (NUL, a string's delimiters),
        // which its note says; what is not implemented has no effect.
        let nothing_is_its_effect = seen == "nothing" && r.note.contains("has no effect");
        let consistent = match r.status {
            "implemented" | "departure" => {
                seen != "unhandled" && (seen != "nothing" || nothing_is_its_effect)
            }
            "not implemented" | "not applicable" => ["unhandled", "nothing"].contains(&seen),
            _ => true,
        };
        if !consistent {
            bad.push(format!(
                "line {} {}: {} but {seen}",
                r.line, r.key, r.status
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    Ok(())
}

/// ECMA-48 2.3.3: "the identified selection shall include the default
/// value both in explicit and in implicit representations"; with ZDM
/// permanently DEFAULT (F.4.2), 0 is the default too.
#[test]
fn an_implemented_parameter_default_is_its_implicit_and_explicit_form()
-> Result<(), Box<dyn std::error::Error>> {
    let mut bad = Vec::new();
    let mut checked = 0;
    for r in rows() {
        let Some(final_byte) = single_pn_final(r.detail) else {
            continue;
        };
        if r.status != "implemented" {
            continue;
        }
        let mut screens = Vec::new();
        for parameter in ["", "0", "1"] {
            let mut parser = context("")?;
            let mut bytes = format!("\x1b[{parameter}").into_bytes();
            bytes.extend_from_slice(&final_byte);
            let mut record = Record::default();
            parser.process_with(&bytes, &mut record)?;
            screens.push((state(&parser), record.replies));
        }
        checked += 1;
        if screens.iter().any(|s| Some(s) != screens.first()) {
            bad.push(format!(
                "line {} {}: CSI F, CSI 0 F and CSI 1 F differ",
                r.line, r.key
            ));
        }
    }
    assert!(checked >= 20, "only {checked} functions checked");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    Ok(())
}

/// The intermediate and final bytes of a `CSI Pn ...` representation whose
/// one parameter defaults to 1: `CSI Pn 02/00 04/00` is ` @`.
fn single_pn_final(detail: &str) -> Option<Vec<u8>> {
    let rep = detail.strip_prefix("Representation: CSI Pn ")?;
    let (rep, default) = rep.split_once("; ")?;
    if default != "Parameter default value: Pn = 1" {
        return None;
    }
    rep.split(' ')
        .map(|code| {
            let (column, row) = code.split_once('/')?;
            let (column, row) = (column.parse::<u8>().ok()?, row.parse::<u8>().ok()?);
            column.checked_mul(16)?.checked_add(row)
        })
        .collect()
}

/// The table as CONFORMANCE.md shows it: each reference's count by status,
/// then its rows.
fn render() -> String {
    let rows = rows();
    let mut out = String::from(
        "# fux-vt conformance\n\n\
         Every control function ECMA-48 (5th edition, 8.3), DEC STD 070 (its function \
         pages) and xterm's ctlseqs (its headings, patch 411) define, with fux-vt's status, \
         the tests that assert it and why. Rows of fux-vt's own name the modes and protocols \
         a reference leaves in one heading. Generated by `tests/table.rs` from \
         `tests/data/conformance.txt`, whose first four columns `fux-vt-gen catalogue` reads \
         from the references; the test fails when this file, a probe or a named test \
         drifts. The behaviour of each function is in the README.\n\n\
         | Reference | ",
    );
    for s in STATUSES {
        let _ = write!(out, "{s} | ");
    }
    out.push_str("all |\n| --- |");
    out.push_str(
        &std::iter::repeat_n(" ---: |", STATUSES.len().saturating_add(1)).collect::<String>(),
    );
    out.push('\n');
    let sources = ["ECMA-48", "DEC STD 070", "ctlseqs", "fux-vt"];
    let source_of = |key: &str| {
        sources
            .iter()
            .position(|s| key.starts_with(&format!("{s} ")))
            .unwrap_or(sources.len().saturating_sub(1))
    };
    for (i, source) in sources.iter().enumerate() {
        let mine: Vec<&Row> = rows.iter().filter(|r| source_of(r.key) == i).collect();
        let _ = write!(out, "| {source} | ");
        for s in STATUSES {
            let _ = write!(out, "{} | ", mine.iter().filter(|r| r.status == s).count());
        }
        let _ = writeln!(out, "{} |", mine.len());
    }
    for (i, source) in sources.iter().enumerate() {
        let _ = write!(
            out,
            "\n## {source}\n\n| Function | Name | Status | Tests | Note |\n| --- | --- | --- | --- | --- |\n"
        );
        for r in rows.iter().filter(|r| source_of(r.key) == i) {
            let at = r.key.strip_prefix(source).unwrap_or(r.key).trim();
            let name = if r.title.is_empty() {
                r.name.to_string()
            } else {
                format!("{} ({})", r.name, r.title)
            };
            let tests: Vec<String> = r
                .tests
                .split(", ")
                .filter(|t| !t.is_empty())
                .map(|t| format!("`{}`", t.trim_matches('`')))
                .collect();
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                cell(at),
                cell(&name),
                r.status,
                tests.join(", "),
                cell(r.note)
            );
        }
    }
    out
}

/// Text for a Markdown table cell: pipes escaped.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

#[test]
fn conformance_md_is_the_table() {
    let path = Path::new(ROOT).join("CONFORMANCE.md");
    let rendered = render();
    if std::env::var_os("FUX_VT_BLESS").is_some() {
        let _ = std::fs::write(&path, &rendered);
        return;
    }
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        written == rendered,
        "CONFORMANCE.md is out of date: run the test with FUX_VT_BLESS=1"
    );
}

#[test]
fn the_parser_has_every_feature_on_in_the_context() -> Result<(), Box<dyn std::error::Error>> {
    let options = context("")?.options();
    assert!(Feature::ALL.iter().all(|f| options.has(*f)));
    Ok(())
}
