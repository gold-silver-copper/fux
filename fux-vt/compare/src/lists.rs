//! `lists`: every list the comparisons judge by, as one JSON, and the
//! list-diff report against checkpoint 0's.
//!
//! The lists are esctest's expected failures (`esctest-expected*.txt`),
//! the corpus statuses (`corpus::STATUSES`), transparency's recorded
//! differences (`transparency::KNOWN`), the family statuses
//! (`families::FAMILIES`), their named cases and the cases' expected
//! fields (`cases`), the named cases recorded as differing
//! (`cases::DIFFERING`), the recordings whose replies differ and the reply
//! changes applied to them (`corpus::REPLIES_DIFFER`, `REPLY_CHANGES`), the
//! encoder verdicts (`encoders::VERDICTS`), and the
//! conformance ledger's tags (`ledger.json`, by `SUITE:ID`). Each
//! is a map from an entry's id to the entry, every entry with its
//! `citation`: the reason, section or quote it rests on.
//!
//! `lists --against FROZEN` diffs them against `FROZEN` (checkpoint 0's
//! `lists --json`), and prints each entry added, removed or changed beside
//! its line in `list-changes.txt`: `LIST ID CITATION`, the source of the
//! change. It fails on a change with no such line, and on a line for an
//! entry that did not change. `--esctest DIR` names, for each test added to
//! an esctest list, the engines whose esctest JSON in DIR has it passing.
use crate::cases::{CASES, EXPECTED};
use crate::families::{Authority, FAMILIES, Status};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const USAGE: &str =
    "usage: fux-vt-compare lists [--json FILE] [--against FROZEN] [--esctest DIR]";

/// The lists, by name, each an object of entries by id.
pub fn all() -> Result<Map<String, Value>, String> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut lists = Map::new();
    let mut names: Vec<String> = std::fs::read_dir(here)
        .map_err(|e| format!("{}: {e}", here.display()))?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("esctest-expected") && n.ends_with(".txt"))
        .collect();
    names.sort();
    for file in names {
        let mut list = Map::new();
        for (name, scope, kind, why) in crate::esctest::listed(&here.join(&file))? {
            list.insert(
                name,
                json!({ "scope": scope, "kind": kind, "citation": why }),
            );
        }
        lists.insert(
            file.trim_end_matches(".txt").to_owned(),
            Value::Object(list),
        );
    }
    let status = |s: Status| match s {
        Status::Agree => json!({ "status": "agree", "citation": "" }),
        Status::Differs(why) => json!({ "status": "differs", "citation": why }),
        Status::Decided { why, by, authority } => json!({
            "status": "decided",
            "by": by,
            "authority": match authority {
                Authority::Standard { section, test } => json!({ "section": section, "test": test }),
                Authority::Xterm => json!("xterm"),
            },
            "citation": why,
        }),
    };
    let corpus: Map<String, Value> = crate::corpus::STATUSES
        .iter()
        .map(|&(name, exceptions)| {
            let each: Vec<Value> = exceptions
                .iter()
                .map(|e| json!({ "steps": [e.steps.0, e.steps.1], "field": e.field, "citation": e.reason }))
                .collect();
            let reasons: BTreeSet<&str> = exceptions.iter().map(|e| e.reason).collect();
            let citation = reasons.into_iter().collect::<Vec<_>>().join(" ");
            (
                name.to_owned(),
                json!({
                    "status": if exceptions.is_empty() { "agree" } else { "differs" },
                    "exceptions": each,
                    "citation": citation,
                }),
            )
        })
        .collect();
    lists.insert("corpus-statuses".into(), Value::Object(corpus));
    let known: Map<String, Value> = crate::transparency::KNOWN
        .iter()
        .map(|k| {
            let recordings = match k.recordings {
                crate::transparency::Recordings::Every => json!("every"),
                crate::transparency::Recordings::Only(names) => json!(names),
            };
            (
                k.name.to_owned(),
                json!({ "recordings": recordings, "engines": k.engines, "citation": k.why }),
            )
        })
        .collect();
    lists.insert("transparency-known".into(), Value::Object(known));
    let families: Map<String, Value> = FAMILIES
        .iter()
        .map(|f| {
            let mut entry = status(f.status);
            if let Some(o) = entry.as_object_mut() {
                o.insert("ratty_only".into(), json!(f.ratty_only));
            }
            (f.name.to_owned(), entry)
        })
        .collect();
    lists.insert("families".into(), Value::Object(families));
    let cases: Map<String, Value> = CASES
        .iter()
        .map(|&(name, family, (rows, cols), steps)| {
            (
                name.to_owned(),
                json!({ "family": family, "size": format!("{rows}x{cols}"), "steps": steps, "citation": "" }),
            )
        })
        .collect();
    lists.insert("cases".into(), Value::Object(cases));
    let expected: Map<String, Value> = EXPECTED
        .iter()
        .map(|e| {
            let fields: Map<String, Value> = e
                .fields
                .iter()
                .map(|&(k, v)| (k.to_owned(), json!(v)))
                .collect();
            (
                e.case.to_owned(),
                json!({ "section": e.section, "fields": fields, "citation": e.quote }),
            )
        })
        .collect();
    lists.insert("expected-fields".into(), Value::Object(expected));
    let differing: Map<String, Value> = (crate::cases::DIFFERING.iter())
        .map(|d| (d.case.to_owned(), json!({ "on": d.on, "citation": d.why })))
        .collect();
    lists.insert("cases-differing".into(), Value::Object(differing));
    let replies: Map<String, Value> = (crate::corpus::REPLIES_DIFFER.iter())
        .map(|&(name, why)| (name.to_owned(), json!({ "citation": why })))
        .collect();
    lists.insert("corpus-replies-differ".into(), Value::Object(replies));
    let changes: Map<String, Value> = (crate::corpus::REPLY_CHANGES.iter())
        .map(|&(was, now, why)| (was.to_owned(), json!({ "now": now, "citation": why })))
        .collect();
    lists.insert("corpus-reply-changes".into(), Value::Object(changes));
    let verdicts: Map<String, Value> = crate::encoders::VERDICTS
        .iter()
        .map(|&(name, why)| (name.to_owned(), json!({ "citation": why })))
        .collect();
    lists.insert("encoder-verdicts".into(), Value::Object(verdicts));
    let ledger = crate::ledger::load()?;
    let tags: Map<String, Value> = ledger
        .checks
        .iter()
        .map(|c| {
            let a = ledger.authorities.get(&c.authority);
            (
                format!("{}:{}", c.suite, c.id),
                json!({
                    "feature": c.feature,
                    "authority": c.authority,
                    "section": a.map(|a| format!("{} ({}), {}", a.document, a.edition, a.section)),
                    "verdict": c.verdict,
                    "applicability": c.applicability,
                    "citation": a.map_or("", |a| a.quote.as_str()),
                }),
            )
        })
        .collect();
    lists.insert("ledger".into(), Value::Object(tags));
    Ok(lists)
}

/// How an entry changed: added, removed, or changed with the keys that did.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Removed,
    Changed(Vec<String>),
}

/// Every entry that differs between `old` and `new`, by list and id.
pub fn changes(
    old: &Map<String, Value>,
    new: &Map<String, Value>,
) -> Vec<(String, String, Change)> {
    let empty = Map::new();
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for list in names {
        let a = old.get(list).and_then(Value::as_object).unwrap_or(&empty);
        let b = new.get(list).and_then(Value::as_object).unwrap_or(&empty);
        let mut ids: Vec<&String> = a.keys().chain(b.keys()).collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            let change = match (a.get(id), b.get(id)) {
                (None, Some(_)) => Change::Added,
                (Some(_), None) => Change::Removed,
                (Some(x), Some(y)) if x != y => {
                    let (x, y) = (x.as_object(), y.as_object());
                    let mut keys: Vec<String> = x
                        .into_iter()
                        .chain(y)
                        .flat_map(|o| o.keys().cloned())
                        .collect();
                    keys.sort();
                    keys.dedup();
                    keys.retain(|k| x.and_then(|o| o.get(k)) != y.and_then(|o| o.get(k)));
                    Change::Changed(keys)
                }
                _ => continue,
            };
            out.push((list.clone(), id.clone(), change));
        }
    }
    out
}

/// `list-changes.txt`'s lines: (list, id) to the citation.
pub fn citations(text: &str) -> Result<BTreeMap<(String, String), String>, String> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.splitn(3, char::is_whitespace);
        let (Some(list), Some(id), Some(why)) = (words.next(), words.next(), words.next()) else {
            return Err(format!(
                "list-changes.txt line {}: not LIST ID CITATION",
                n.saturating_add(1)
            ));
        };
        let why = why.trim();
        if why.is_empty() {
            return Err(format!(
                "list-changes.txt line {}: no citation",
                n.saturating_add(1)
            ));
        }
        if out
            .insert((list.to_owned(), id.to_owned()), why.to_owned())
            .is_some()
        {
            return Err(format!(
                "list-changes.txt line {}: {list} {id} twice",
                n.saturating_add(1)
            ));
        }
    }
    Ok(out)
}

/// For each esctest JSON in `dir`, its column's label and the tests that
/// pass in it.
fn passes(dir: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut out = Vec::new();
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
        for (column, value) in &d {
            let Some(tests) = value.get("tests").and_then(Value::as_object) else {
                continue;
            };
            let label = match (column.as_str(), d.get("terminal").and_then(Value::as_str)) {
                ("direct", Some(engine)) => engine.to_owned(),
                ("direct", None) | ("fux_vt", _) => "fux-vt".to_owned(),
                (other, _) => other.to_owned(),
            };
            let passed = tests
                .iter()
                .filter(|(_, t)| t.get("outcome").and_then(Value::as_str) == Some("pass"))
                .map(|(name, _)| name.clone())
                .collect();
            out.push((label, passed));
        }
    }
    Ok(out)
}

/// The report: each change with its citation, as Markdown, and whether
/// every change has one and every citation a change.
pub fn report(
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    cited: &BTreeMap<(String, String), String>,
    esctest: &[(String, Vec<String>)],
) -> (String, bool) {
    let changes = changes(old, new);
    let mut ok = true;
    let mut out = format!(
        "## The list-diff report\n\n{} changes since checkpoint 0.\n",
        changes.len()
    );
    let mut list = "";
    for (name, id, change) in &changes {
        if name != list {
            list = name;
            out.push_str(&format!(
                "\n### {name}\n\n| Entry | Change | Citation |\n|---|---|---|\n"
            ));
        }
        let what = match change {
            Change::Added => "added".to_owned(),
            Change::Removed => "removed".to_owned(),
            Change::Changed(keys) => format!("changed: {}", keys.join(", ")),
        };
        let mut why = match cited.get(&(name.clone(), id.clone())) {
            Some(why) => why.clone(),
            None => {
                ok = false;
                "**UNCITED**: no line in list-changes.txt".to_owned()
            }
        };
        if name.starts_with("esctest-expected") && *change == Change::Added {
            let mut by: Vec<&str> = esctest
                .iter()
                .filter(|(_, passed)| passed.contains(id))
                .map(|(label, _)| label.as_str())
                .collect();
            by.sort_unstable();
            by.dedup();
            let reason = new
                .get(name)
                .and_then(|l| l.get(id))
                .and_then(|e| e.get("citation"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            why.push_str(&format!(
                "; passes in: {}; listed because: {reason}",
                if by.is_empty() {
                    "none read".to_owned()
                } else {
                    by.join(", ")
                }
            ));
        }
        out.push_str(&format!(
            "| `{id}` | {what} | {} |\n",
            why.replace('|', "\\|")
        ));
    }
    let stale: Vec<_> = cited
        .keys()
        .filter(|(l, i)| !changes.iter().any(|(cl, ci, _)| cl == l && ci == i))
        .collect();
    if !stale.is_empty() {
        ok = false;
        out.push_str("\n### Stale lines in list-changes.txt (no such change)\n\n");
        for (l, i) in stale {
            out.push_str(&format!("- {l} {i}\n"));
        }
    }
    (out, ok)
}

/// `lists [--json FILE] [--against FROZEN] [--esctest DIR]`.
pub fn run(argv: &[String]) -> Result<bool, String> {
    let (mut json_out, mut against, mut esctest) = (None, None, None);
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let slot = match word.as_str() {
            "--json" => &mut json_out,
            "--against" => &mut against,
            "--esctest" => &mut esctest,
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        };
        *slot = Some(words.next().ok_or(format!("{word} needs a value"))?);
    }
    let lists = all()?;
    let mut text = serde_json::to_string_pretty(&lists).map_err(|e| format!("the lists: {e}"))?;
    text.push('\n');
    match json_out {
        Some(path) => std::fs::write(path, &text).map_err(|e| format!("{path}: {e}"))?,
        None if against.is_none() => print!("{text}"),
        None => {}
    }
    let Some(frozen) = against else {
        return Ok(true);
    };
    let old: Map<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(frozen).map_err(|e| format!("{frozen}: {e}"))?,
    )
    .map_err(|e| format!("{frozen}: {e}"))?;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("list-changes.txt");
    let cited = citations(
        &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )?;
    let esctest = match esctest {
        Some(dir) => passes(Path::new(dir))?,
        None => Vec::new(),
    };
    let (report, ok) = report(&old, &lists, &cited, &esctest);
    print!("{report}");
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::{Change, changes, citations, report};
    use serde_json::{Map, Value, json};

    fn lists(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    /// A change with its citation passes; one without, or a citation with
    /// no change, fails; and the report names what changed in an entry.
    #[test]
    fn every_change_needs_its_citation_and_every_citation_its_change() -> Result<(), String> {
        let old = lists(json!({ "corpus-statuses": {
            "bash": { "status": "agree", "citation": "" },
            "vim": { "status": "differs", "citation": "clusters" },
        }}));
        let new = lists(json!({ "corpus-statuses": {
            "bash": { "status": "differs", "citation": "a reason" },
            "fish": { "status": "agree", "citation": "" },
        }}));
        let found = changes(&old, &new);
        assert_eq!(
            found,
            [
                (
                    "corpus-statuses".into(),
                    "bash".into(),
                    Change::Changed(vec!["citation".into(), "status".into()])
                ),
                ("corpus-statuses".into(), "fish".into(), Change::Added),
                ("corpus-statuses".into(), "vim".into(), Change::Removed),
            ]
        );
        let all = citations(
            "# comment\ncorpus-statuses bash  DEC STD 070, 5.1\ncorpus-statuses fish  recorded\ncorpus-statuses vim fixed in abc\n",
        )?;
        assert!(report(&old, &new, &all, &[]).1);
        let fewer =
            citations("corpus-statuses bash  DEC STD 070, 5.1\ncorpus-statuses vim fixed\n")?;
        let (text, ok) = report(&old, &new, &fewer, &[]);
        assert!(!ok);
        assert!(text.contains("| `fish` | added | **UNCITED**"));
        let stale = citations(
            "corpus-statuses bash a\ncorpus-statuses fish b\ncorpus-statuses vim c\ncases x d\n",
        )?;
        assert!(!report(&old, &new, &stale, &[]).1);
        assert!(citations("corpus-statuses bash").is_err());
        Ok(())
    }

    /// The lists read: every list has entries, every entry a citation key.
    #[test]
    fn every_list_reads_with_a_citation_per_entry() -> Result<(), String> {
        let all = super::all()?;
        for name in [
            "esctest-expected",
            "esctest-expected-ghostty",
            "corpus-statuses",
            "transparency-known",
            "families",
            "cases",
            "expected-fields",
            "encoder-verdicts",
            "ledger",
            "cases-differing",
            "corpus-replies-differ",
            "corpus-reply-changes",
        ] {
            let list = all
                .get(name)
                .and_then(Value::as_object)
                .ok_or(format!("no {name}"))?;
            assert!(!list.is_empty(), "{name}");
            for (id, entry) in list {
                assert!(entry.get("citation").is_some(), "{name} {id}");
            }
        }
        Ok(())
    }
}
