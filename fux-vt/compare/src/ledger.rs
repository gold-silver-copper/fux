//! `ledger`: the conformance ledger's four scores, per suite, for every
//! engine on one denominator.
//!
//! The ledger (`ledger.json`, beside this crate's manifest) is one table
//! of checks from every conformance suite that runs: each esctest2 test,
//! each named case (`cases`) and each corpus recording. Each check has its
//! feature (one of the frozen groups in `features`), its authority (an id
//! in `authorities`: a standard's document, edition, section and quoted
//! text, or xterm's, or `open` where none is known), its terminal level
//! and mode, its applicability, and how the suite's expectation stands to
//! the authority (`verdict`):
//!
//! - `agrees`: the suite expects what the authority says;
//! - `departs`: it expects what the authority's text contradicts; an
//!   engine passes by the standard only if it does what the text says
//!   (`follows`, or its failure's message holds `follows_when`);
//! - `allows`: the authority allows the suite's answer and others; an
//!   engine passes if it passes the suite or gives another allowed answer
//!   (as `departs` reads it);
//! - `open`: no one has read the check against its authority yet.
//!
//! Every tag has its `evidence`: where the tag came from (the esctest
//! area's function, the test's own decorators, its listed reason,
//! `cases::EXPECTED`, a family's authority, or a reading of the test).
//!
//! The scores, for each suite and engine, read from the suites' JSON:
//!
//! - **raw:** the checks it passes, as the suite reports, over the checks
//!   the suite runs;
//! - **standard-adjusted:** each check scored by its verdict, over the
//!   checks that apply (a `departs` check passed is a fail);
//! - **xterm-compat:** the checks it passes among those real xterm passes
//!   (xterm's own column is left out);
//! - **feature:** the features all of whose checks that apply it passes.
//!
//! A check an engine has no result for is unavailable: counted in the
//! denominator, never as a pass, and shown apart.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

pub const USAGE: &str = "usage: fux-vt-compare ledger [--results DIR] [--json FILE] [--table FILE]

Scores every suite in the conformance ledger (ledger.json) from the JSON
results in DIR: esctest's (--json, any column), cases' (cases --json, one
file a subject) and the corpus's (corpus --json, one file a subject).
--table writes the whole ledger as one Markdown table.";

pub struct Ledger {
    pub base: String,
    pub features: BTreeMap<String, String>,
    pub authorities: BTreeMap<String, Authority>,
    pub checks: Vec<Check>,
}

pub struct Authority {
    /// `standard`, `xterm` or `open`.
    pub kind: String,
    pub document: String,
    pub edition: String,
    pub section: String,
    pub quote: String,
    /// The levels the document gives the function, as it writes them.
    pub levels: String,
}

pub struct Check {
    pub suite: String,
    pub id: String,
    pub feature: String,
    pub authority: String,
    pub verdict: String,
    /// `applies`, `n/a: WHY` (left out of the standard-adjusted and
    /// feature scores, for every engine) or `skipped: WHY` (the suite
    /// does not run it).
    pub applicability: String,
    pub level: Option<u8>,
    pub mode: String,
    pub follows: Vec<String>,
    pub follows_when: Option<String>,
    pub evidence: BTreeMap<String, String>,
    pub note: String,
}

impl Check {
    fn skipped(&self) -> bool {
        self.applicability.starts_with("skipped")
    }

    fn applies(&self) -> bool {
        self.applicability == "applies"
    }
}

pub const VERDICTS: &[&str] = &["agrees", "departs", "allows", "open"];

/// The ledger committed beside the manifest.
pub fn load() -> Result<Ledger, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ledger.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A ledger from its JSON. Every string field but the check's own
/// (`suite`, `id`, `feature`, `authority`, `verdict`, `applicability`) may
/// be left out, as empty.
pub fn parse(text: &str) -> Result<Ledger, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let text_of = |o: &Value, key: &str| {
        o.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let needed = |o: &Value, key: &str| {
        o.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(format!("a check without {key}: {o}"))
    };
    let strings = |o: Option<&Value>| -> BTreeMap<String, String> {
        o.and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
            .collect()
    };
    let authorities = v
        .get("authorities")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(id, a)| {
            (
                id.clone(),
                Authority {
                    kind: text_of(a, "kind"),
                    document: text_of(a, "document"),
                    edition: text_of(a, "edition"),
                    section: text_of(a, "section"),
                    quote: text_of(a, "quote"),
                    levels: text_of(a, "levels"),
                },
            )
        })
        .collect();
    let mut checks = Vec::new();
    for c in v
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        checks.push(Check {
            suite: needed(c, "suite")?,
            id: needed(c, "id")?,
            feature: needed(c, "feature")?,
            authority: needed(c, "authority")?,
            verdict: needed(c, "verdict")?,
            applicability: needed(c, "applicability")?,
            level: c
                .get("level")
                .and_then(Value::as_u64)
                .and_then(|n| u8::try_from(n).ok()),
            mode: text_of(c, "mode"),
            follows: c
                .get("follows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect(),
            follows_when: c
                .get("follows_when")
                .and_then(Value::as_str)
                .map(str::to_owned),
            evidence: strings(c.get("evidence")),
            note: text_of(c, "note"),
        });
    }
    Ok(Ledger {
        base: text_of(&v, "base"),
        features: strings(v.get("features")),
        authorities,
        checks,
    })
}

/// What is wrong with the ledger's tags: a feature or authority not
/// defined, a verdict not known, a standard without its document, edition,
/// section or quote, a `departs` or `allows` check with no way to tell who
/// follows the text, a tag without its evidence, a check twice.
pub fn problems(ledger: &Ledger) -> Vec<String> {
    let mut out = Vec::new();
    for (id, a) in &ledger.authorities {
        match a.kind.as_str() {
            "standard" | "xterm" => {
                for (what, v) in [
                    ("document", &a.document),
                    ("edition", &a.edition),
                    ("section", &a.section),
                    ("quote", &a.quote),
                ] {
                    if v.trim().is_empty() {
                        out.push(format!("authority {id}: no {what}"));
                    }
                }
            }
            "open" => {}
            other => out.push(format!("authority {id}: kind {other:?}")),
        }
    }
    let mut seen = BTreeSet::new();
    for c in &ledger.checks {
        let at = format!("{} {}", c.suite, c.id);
        if !seen.insert((c.suite.as_str(), c.id.as_str())) {
            out.push(format!("{at}: listed twice"));
        }
        if !ledger.features.contains_key(&c.feature) {
            out.push(format!("{at}: no feature {:?}", c.feature));
        }
        if !ledger.authorities.contains_key(&c.authority) {
            out.push(format!("{at}: no authority {:?}", c.authority));
        }
        if !VERDICTS.contains(&c.verdict.as_str()) {
            out.push(format!("{at}: verdict {:?}", c.verdict));
        }
        if matches!(c.verdict.as_str(), "departs" | "allows")
            && c.follows.is_empty()
            && c.follows_when.is_none()
        {
            out.push(format!(
                "{at}: {} with no way to tell who follows the text",
                c.verdict
            ));
        }
        if !(c.applies() || c.skipped() || c.applicability.starts_with("n/a: ")) {
            out.push(format!("{at}: applicability {:?}", c.applicability));
        }
        for tag in ["feature", "authority", "verdict"] {
            if c.evidence.get(tag).is_none_or(|e| e.trim().is_empty()) {
                out.push(format!("{at}: no evidence for its {tag}"));
            }
        }
    }
    out
}

/// One engine's outcome on a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// Failed, with the suite's message.
    Fail(String),
    /// No result: an infrastructure or adapter limit, or not run.
    Unavailable,
}

/// The suites' results: by suite, engine and check id.
pub type Results = BTreeMap<String, BTreeMap<String, BTreeMap<String, Outcome>>>;

/// Reads every result JSON in `dir`: esctest's columns, cases' and the
/// corpus's subjects.
pub fn results(dir: &Path) -> Result<Results, String> {
    let mut out = Results::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for path in files {
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let Ok(Value::Object(d)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        read_one(&d, &mut out);
    }
    Ok(out)
}

fn read_one(d: &Map<String, Value>, out: &mut Results) {
    let subject = d
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or("fux-vt")
        .to_owned();
    match d.get("check").and_then(Value::as_str) {
        Some("cases") => {
            let checks = out.entry("cases".into()).or_default();
            for r in d
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = r.get("case").and_then(Value::as_str) else {
                    continue;
                };
                let failures: Vec<&str> = r
                    .get("failures")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let outcome = if failures.is_empty() {
                    Outcome::Pass
                } else {
                    Outcome::Fail(failures.join("; "))
                };
                checks
                    .entry(subject.clone())
                    .or_default()
                    .insert(id.to_owned(), outcome);
                // xterm's own verdict, as a column of its own.
                if let Some(agrees) = r.get("xterm_agrees").and_then(Value::as_bool) {
                    let o = if agrees {
                        Outcome::Pass
                    } else {
                        Outcome::Fail("xterm differs".into())
                    };
                    checks
                        .entry(format!("xterm-beside-{subject}"))
                        .or_default()
                        .insert(id.to_owned(), o);
                }
            }
        }
        Some("corpus") => {
            let checks = out.entry("corpus".into()).or_default();
            for r in d
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = r.get("recording").and_then(Value::as_str) else {
                    continue;
                };
                let agrees = match d.get("subject") {
                    Some(_) => r.get("subject").and_then(|s| s.get("agrees")),
                    None => r.get("agrees"),
                };
                let outcome = match agrees.and_then(Value::as_bool) {
                    Some(true) => Outcome::Pass,
                    Some(false) => Outcome::Fail("departs from xterm and the panel".into()),
                    None => Outcome::Unavailable,
                };
                checks
                    .entry(subject.clone())
                    .or_default()
                    .insert(id.to_owned(), outcome);
            }
        }
        _ if d.get("suite").and_then(Value::as_str) == Some("esctest2") => {
            let checks = out.entry("esctest".into()).or_default();
            for (column, value) in d {
                let Some(tests) = value.get("tests").and_then(Value::as_object) else {
                    continue;
                };
                let engine = match (column.as_str(), d.get("terminal").and_then(Value::as_str)) {
                    ("direct", Some(e)) => e.to_owned(),
                    ("direct", None) => "fux-vt".to_owned(),
                    ("fux_vt", _) => "fux-vt".to_owned(),
                    ("fux", _) => "fux-vt in fux".to_owned(),
                    (other, _) => other.to_owned(),
                };
                let column = checks.entry(engine).or_default();
                for (id, t) in tests {
                    let outcome = match t.get("outcome").and_then(Value::as_str) {
                        Some("pass") => Outcome::Pass,
                        Some("fail")
                            if t.get("class").and_then(Value::as_str) == Some("engine") =>
                        {
                            Outcome::Fail(
                                t.get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            )
                        }
                        _ => Outcome::Unavailable,
                    };
                    column.insert(id.clone(), outcome);
                }
            }
        }
        _ => {}
    }
}

/// One engine's four scores on a suite: passes and denominators.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scores {
    pub raw: (usize, usize),
    pub standard: (usize, usize),
    pub xterm: Option<(usize, usize)>,
    pub feature: (usize, usize),
    pub unavailable: usize,
}

/// Whether `engine`, with `outcome`, does what the authority says on `c`.
fn by_standard(c: &Check, engine: &str, outcome: &Outcome) -> bool {
    let follows = c.follows.iter().any(|e| e == engine)
        || matches!((outcome, &c.follows_when), (Outcome::Fail(m), Some(w)) if m.contains(w.as_str()));
    match c.verdict.as_str() {
        "departs" => follows,
        "allows" => *outcome == Outcome::Pass || follows,
        _ => *outcome == Outcome::Pass,
    }
}

/// Every engine's scores on `suite`, from `results`.
pub fn score(ledger: &Ledger, suite: &str, results: &Results) -> BTreeMap<String, Scores> {
    let checks: Vec<&Check> = ledger
        .checks
        .iter()
        .filter(|c| c.suite == suite && !c.skipped())
        .collect();
    let empty = BTreeMap::new();
    let columns = results.get(suite).unwrap_or(&empty);
    let mut out = BTreeMap::new();
    for (engine, got) in columns {
        if engine.starts_with("xterm-beside-") {
            continue;
        }
        let outcome = |c: &Check| got.get(&c.id).cloned().unwrap_or(Outcome::Unavailable);
        let mut s = Scores::default();
        let mut features: BTreeMap<&str, bool> = BTreeMap::new();
        for c in &checks {
            let o = outcome(c);
            s.raw.1 = s.raw.1.saturating_add(1);
            if o == Outcome::Pass {
                s.raw.0 = s.raw.0.saturating_add(1);
            }
            if o == Outcome::Unavailable {
                s.unavailable = s.unavailable.saturating_add(1);
            }
            if c.applies() {
                s.standard.1 = s.standard.1.saturating_add(1);
                if by_standard(c, engine, &o) {
                    s.standard.0 = s.standard.0.saturating_add(1);
                }
                let all = features.entry(c.feature.as_str()).or_insert(true);
                *all &= o == Outcome::Pass;
            }
        }
        s.feature = (features.values().filter(|&&p| p).count(), features.len());
        // Agreement with xterm: none for xterm itself. In the named cases,
        // xterm's own verdict beside the subject (the cases where xterm
        // ran); the corpus is judged by xterm wherever it can tell a
        // field, so its raw score is it; esctest's, the checks xterm
        // passes that the engine passes too.
        s.xterm = match suite {
            _ if engine == "xterm" => None,
            "corpus" => Some(s.raw),
            "cases" => columns.get(&format!("xterm-beside-{engine}")).map(|x| {
                let heard: Vec<&Outcome> = checks.iter().filter_map(|c| x.get(&c.id)).collect();
                (
                    heard.iter().filter(|o| ***o == Outcome::Pass).count(),
                    heard.len(),
                )
            }),
            _ => columns.get("xterm").map(|x| {
                let passed: Vec<&&Check> = checks
                    .iter()
                    .filter(|c| x.get(&c.id) == Some(&Outcome::Pass))
                    .collect();
                (
                    passed
                        .iter()
                        .filter(|c| outcome(c) == Outcome::Pass)
                        .count(),
                    passed.len(),
                )
            }),
        };
        out.insert(engine.clone(), s);
    }
    out
}

fn share((n, d): (usize, usize)) -> String {
    if d == 0 {
        return "—".into();
    }
    let tenths = n.saturating_mul(1000).checked_div(d).unwrap_or(0);
    format!("{n}/{d} ({}.{}%)", tenths / 10, tenths % 10)
}

/// The scores as Markdown, a table a suite, and as JSON.
pub fn report(ledger: &Ledger, results: &Results) -> (String, Value) {
    let mut md = format!(
        "## The conformance ledger at {}\n\n{} checks, {} features, {} authorities.\n",
        ledger.base,
        ledger.checks.len(),
        ledger.features.len(),
        ledger.authorities.len()
    );
    let mut json = Map::new();
    let suites: BTreeSet<&str> = ledger.checks.iter().map(|c| c.suite.as_str()).collect();
    for suite in suites {
        let scores = score(ledger, suite, results);
        let open = ledger
            .checks
            .iter()
            .filter(|c| c.suite == suite && (c.verdict == "open" || c.authority == "open"))
            .count();
        let _ = write!(
            md,
            "\n### {suite}\n\n{open} checks with an open authority or verdict.\n\n| Engine | raw | standard-adjusted | xterm-compat | feature | unavailable |\n|---|---|---|---|---|---|\n"
        );
        let mut rows = Map::new();
        for (engine, s) in &scores {
            let _ = writeln!(
                md,
                "| {engine} | {} | {} | {} | {} | {} |",
                share(s.raw),
                share(s.standard),
                s.xterm.map_or_else(|| "(xterm itself)".to_owned(), share),
                share(s.feature),
                s.unavailable
            );
            rows.insert(
                engine.clone(),
                json!({
                    "raw": [s.raw.0, s.raw.1],
                    "standard_adjusted": [s.standard.0, s.standard.1],
                    "xterm_compat": s.xterm.map(|x| [x.0, x.1]),
                    "feature": [s.feature.0, s.feature.1],
                    "unavailable": s.unavailable,
                }),
            );
        }
        if scores.is_empty() {
            md.push_str("| (no results) | | | | | |\n");
        }
        json.insert(suite.to_owned(), Value::Object(rows));
    }
    (md, json!({ "base": ledger.base, "suites": json }))
}

/// The ledger as one Markdown table, a check a row, with its authority
/// spelled out and each tag's evidence.
pub fn table(ledger: &Ledger) -> String {
    let mut md = format!(
        "# The conformance ledger, frozen at {}\n\n| Suite | Check | Feature | Authority | Quote | Levels | Level, mode | Applicability | Verdict | Follows | Evidence | Note |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        ledger.base
    );
    let cell = |s: &str| s.replace('|', "\\|").replace('\n', " ");
    for c in &ledger.checks {
        let a = ledger.authorities.get(&c.authority);
        let source = a.map_or_else(
            || c.authority.clone(),
            |a| match a.kind.as_str() {
                "open" => "open".to_owned(),
                _ => format!("{} ({}), {}", a.document, a.edition, a.section),
            },
        );
        let level = match (c.level, c.mode.as_str()) {
            (Some(l), "") => format!("level {l}"),
            (Some(l), m) => format!("level {l}; {m}"),
            (None, m) => m.to_owned(),
        };
        let follows = c
            .follows
            .iter()
            .cloned()
            .chain(
                c.follows_when
                    .iter()
                    .map(|w| format!("a failure holding {w:?}")),
            )
            .collect::<Vec<_>>()
            .join(", ");
        let evidence = c
            .evidence
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(
            md,
            "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.suite,
            c.id,
            cell(&c.feature),
            cell(&source),
            cell(a.map_or("", |a| a.quote.as_str())),
            cell(a.map_or("", |a| a.levels.as_str())),
            cell(&level),
            cell(&c.applicability),
            c.verdict,
            cell(&follows),
            cell(&evidence),
            cell(&c.note)
        );
    }
    md
}

/// `ledger --results DIR [--json FILE] [--table FILE]`.
pub fn run(argv: &[String]) -> Result<bool, String> {
    let (mut dir, mut out, mut whole) = (None, None, None);
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "--results" => &mut dir,
            "--json" => &mut out,
            "--table" => &mut whole,
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        };
        *slot = Some(words.next().ok_or(format!("{word} needs a value"))?);
    }
    let ledger = load()?;
    let wrong = problems(&ledger);
    if !wrong.is_empty() {
        return Err(format!("ledger.json:\n  {}", wrong.join("\n  ")));
    }
    if let Some(path) = whole {
        std::fs::write(path, table(&ledger)).map_err(|e| format!("{path}: {e}"))?;
    }
    let Some(dir) = dir else {
        return Ok(true);
    };
    let (md, value) = report(&ledger, &results(Path::new(dir))?);
    print!("{md}");
    if let Some(path) = out {
        let mut text =
            serde_json::to_string_pretty(&value).map_err(|e| format!("the scores: {e}"))?;
        text.push('\n');
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{Ledger, Outcome, Results, load, parse, problems, score};
    use std::collections::BTreeMap;

    /// The committed ledger has no tag without its definition or evidence,
    /// and holds every listed esctest failure, every named case and every
    /// recording.
    #[test]
    fn the_ledger_is_whole() -> Result<(), String> {
        let ledger = load()?;
        assert_eq!(problems(&ledger), Vec::<String>::new());
        let has =
            |suite: &str, id: &str| ledger.checks.iter().any(|c| c.suite == suite && c.id == id);
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (id, ..) in crate::esctest::listed(&here.join("esctest-expected.txt"))? {
            assert!(has("esctest", &id), "esctest {id}");
        }
        for &(id, ..) in crate::cases::CASES {
            assert!(has("cases", id), "cases {id}");
        }
        for &(id, _) in crate::corpus::STATUSES {
            assert!(has("corpus", id), "corpus {id}");
        }
        Ok(())
    }

    fn ledger() -> Result<Ledger, String> {
        parse(
            r#"{ "base": "x", "features": { "f": "", "g": "" },
              "authorities": { "s": { "kind": "standard", "document": "D", "edition": "1", "section": "1", "quote": "q" } },
              "checks": [
                { "suite": "t", "id": "a", "feature": "f", "authority": "s", "verdict": "agrees", "applicability": "applies", "evidence": { "feature": "e", "authority": "e", "verdict": "e" } },
                { "suite": "t", "id": "b", "feature": "f", "authority": "s", "verdict": "departs", "follows_when": "got 73", "applicability": "applies", "evidence": { "feature": "e", "authority": "e", "verdict": "e" } },
                { "suite": "t", "id": "c", "feature": "g", "authority": "s", "verdict": "agrees", "applicability": "n/a: 8-bit", "evidence": { "feature": "e", "authority": "e", "verdict": "e" } },
                { "suite": "t", "id": "d", "feature": "g", "authority": "s", "verdict": "agrees", "applicability": "skipped: level 5", "evidence": { "feature": "e", "authority": "e", "verdict": "e" } }
              ] }"#,
        )
    }

    /// A check the suite departs from its standard on is scored by the
    /// standard: the suite's pass is a fail, and a failure giving the
    /// text's answer a pass. A check that does not apply leaves the
    /// standard-adjusted and feature scores; one the suite skips leaves
    /// them all. xterm's column is left out of xterm-compat, and an engine
    /// with no result on a check is unavailable, never a pass.
    #[test]
    fn each_score_counts_on_one_denominator() -> Result<(), String> {
        let ledger = ledger()?;
        assert_eq!(problems(&ledger), Vec::<String>::new());
        let column = |a: Option<Outcome>, b: Outcome, c: Outcome| {
            let mut m = BTreeMap::new();
            if let Some(a) = a {
                m.insert("a".to_owned(), a);
            }
            m.insert("b".to_owned(), b);
            m.insert("c".to_owned(), c);
            m
        };
        let mut engines = BTreeMap::new();
        engines.insert(
            "xterm".to_owned(),
            column(Some(Outcome::Pass), Outcome::Pass, Outcome::Pass),
        );
        engines.insert(
            "fux-vt".to_owned(),
            column(
                Some(Outcome::Pass),
                Outcome::Fail("but got 73".into()),
                Outcome::Fail(String::new()),
            ),
        );
        engines.insert(
            "other".to_owned(),
            column(None, Outcome::Fail("timeout".into()), Outcome::Pass),
        );
        let mut results = Results::new();
        results.insert("t".to_owned(), engines);
        let s = score(&ledger, "t", &results);
        let xterm = s.get("xterm").ok_or("no xterm")?;
        assert_eq!(
            (xterm.raw, xterm.standard, xterm.xterm, xterm.feature),
            ((3, 3), (1, 2), None, (1, 1))
        );
        let fux = s.get("fux-vt").ok_or("no fux-vt")?;
        assert_eq!(
            (fux.raw, fux.standard, fux.xterm, fux.feature),
            ((1, 3), (2, 2), Some((1, 3)), (0, 1))
        );
        let other = s.get("other").ok_or("no other")?;
        assert_eq!(
            (other.raw, other.standard, other.unavailable),
            ((1, 3), (0, 2), 1)
        );
        Ok(())
    }
}
