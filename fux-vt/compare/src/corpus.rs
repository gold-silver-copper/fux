//! The corpus: what real programs wrote to a terminal, recorded by
//! `record` (`corpus/record.sh` records every scenario), and replayed here
//! through fux-vt beside other engines.
//!
//! A recording is replayed as a case of its own size, one step for each
//! step recorded: the program starting, then the output after each line
//! of keys. fux-vt is set up as fux sets up a pane (no reflow, the kitty
//! keyboard protocol, no identity), as the program was recorded, with
//! fux's 10000 rows of history. After each step the engines are compared
//! with fux-vt field by field, as in `run`.
//!
//! **xterm decides what it can tell.** Where xterm runs, any field it can
//! tell that differs from fux-vt fails the step. A field xterm cannot tell
//! (underline colour, pending wrap, the kitty flags), and every field once
//! xterm abstains (it does from SGR 58 on, see its file), is decided by the
//! vote of the other engines, as in `run`. A difference the other engines
//! outvote fux-vt on, where xterm agrees with fux-vt, is shown, not failed.
//!
//! Each recording has a status, like a family's: expected to agree, or
//! differing for a recorded reason. `corpus` fails if one expected to
//! agree does not.
use crate::case::{self, Case, Snippet, Step, Verdict};
use crate::engine::{self, ENGINES};
use crate::families::Status;
use crate::snapshot::{self, Field};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where the recordings are.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// The reason Claude Code's recordings differ at their last step.
const CLAUDE_TITLE: &str = "Claude Code sets an empty title as it exits (OSC 0 ;). \
    xterm then shows its default title, `xterm` (a quirk of xterm's, in its file), and \
    fux-vt keeps the empty title. Every step before the last agrees beside xterm. \
    `corpus --engines xterm,panel claude` shows it.";

/// What is expected of each recording; one not listed here is reported as
/// new, and fails.
pub const STATUSES: &[(&str, Status)] = &[
    ("bash", Status::Agree),
    ("claude", Status::Differs(CLAUDE_TITLE)),
    ("claude-ghostty", Status::Differs(CLAUDE_TITLE)),
    ("claude-main", Status::Differs(CLAUDE_TITLE)),
    ("delta-diff", Status::Agree),
    ("delta-log", Status::Agree),
    ("fzf", Status::Agree),
    ("fzf-height", Status::Agree),
    ("gls", Status::Agree),
    ("helix", Status::Agree),
    ("less", Status::Agree),
    ("man", Status::Agree),
    ("tmux", Status::Agree),
    ("vim", Status::Agree),
    ("zsh", Status::Agree),
];

/// A recording, as `record` saved it.
pub struct Recording {
    pub name: String,
    pub program: String,
    pub version: String,
    pub rows: u16,
    pub cols: u16,
    /// Each step's keys (escaped, as typed) and the output after them.
    pub steps: Vec<(String, Vec<u8>)>,
}

impl Recording {
    /// Every byte the program wrote, in order.
    pub fn bytes(&self) -> Vec<u8> {
        self.steps
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }

    /// The recording as a case: one step for each step recorded.
    pub fn case(&self) -> Case {
        Case {
            rows: self.rows,
            cols: self.cols,
            history: 10_000,
            reflow: false,
            newline_before_resize: false,
            steps: self
                .steps
                .iter()
                .map(|(_, bytes)| {
                    Step::Output(vec![Snippet {
                        family: usize::MAX,
                        bytes: bytes.clone(),
                    }])
                })
                .collect(),
        }
    }
}

fn field<'a>(
    value: &'a serde_json::Value,
    name: &str,
    path: &Path,
) -> Result<&'a serde_json::Value, String> {
    value
        .get(name)
        .ok_or(format!("{}: no {name:?}", path.display()))
}

fn load(path: &Path) -> Result<Recording, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = std::fs::read(path.with_extension("bin"))
        .map_err(|e| format!("{}: {e}", path.with_extension("bin").display()))?;
    let string = |name: &str| -> Result<String, String> {
        Ok(field(&json, name, path)?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    };
    let size = field(&json, "size", path)?;
    let dimension = |i: usize| -> Result<u16, String> {
        size.get(i)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(format!("{}: a bad size", path.display()))
    };
    let mut steps = Vec::new();
    let mut from = 0usize;
    for step in field(&json, "steps", path)?
        .as_array()
        .ok_or(format!("{}: steps is not a list", path.display()))?
    {
        let keys = field(step, "keys", path)?
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let end = field(step, "end", path)?
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(format!("{}: a step's end is not a number", path.display()))?;
        let output = bytes
            .get(from..end)
            .ok_or(format!("{}: a step ends past the bytes", path.display()))?;
        steps.push((keys, output.to_vec()));
        from = end;
    }
    if from != bytes.len() {
        return Err(format!("{}: bytes after the last step", path.display()));
    }
    Ok(Recording {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        program: string("program")?,
        version: string("version")?,
        rows: dimension(0)?,
        cols: dimension(1)?,
        steps,
    })
}

/// Every recording, by name, or those named.
pub fn recordings(names: &[String]) -> Result<Vec<Recording>, String> {
    let dir = dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let all = paths
        .iter()
        .map(|p| load(p))
        .collect::<Result<Vec<_>, _>>()?;
    for name in names {
        if !all.iter().any(|r| &r.name == name) {
            return Err(format!("no recording {name:?} in {}", dir.display()));
        }
    }
    Ok(all
        .into_iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
        .collect())
}

/// The fields fux-vt fails on after a step: see the module documentation.
pub fn failures(verdicts: &[Verdict]) -> Vec<String> {
    let xterm = engine::find("xterm");
    let decider = verdicts.iter().find(|v| Some(v.engine) == xterm);
    let others: Vec<&Verdict> = verdicts
        .iter()
        .filter(|v| Some(v.engine) != xterm)
        .collect();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    if let Some(x) = decider {
        failed.extend(x.differences.iter().map(|d| d.key.clone()));
    }
    let xterm_can = xterm.and_then(|i| ENGINES.get(i)).map(|k| k.can);
    let owned: Vec<Verdict> = others
        .iter()
        .map(|v| Verdict {
            engine: v.engine,
            differences: v.differences.clone(),
            snapshot: v.snapshot.clone(),
        })
        .collect();
    for key in case::outvoted_on(&owned) {
        let field: Option<Field> = owned
            .iter()
            .flat_map(|v| v.differences.iter())
            .find(|d| d.key == key)
            .map(|d| d.field);
        let xterm_tells = decider.is_some()
            && field.is_some_and(|f| xterm_can.is_some_and(|can| f.told_by(&can)));
        if !xterm_tells {
            failed.insert(key);
        }
    }
    failed.into_iter().collect()
}

/// Each engine's verdict at the end, as marks: `+` agrees, `-` differs,
/// `!` abstained (and why, on lines of their own).
fn marks(outcome: &case::Outcome) -> String {
    let marks = outcome
        .verdicts
        .iter()
        .map(|v| {
            let mark = if v.differences.is_empty() { '+' } else { '-' };
            format!("{mark}{}", ENGINES.get(v.engine).map_or("?", |k| k.name))
        })
        .chain(
            outcome
                .abstained
                .iter()
                .map(|(i, _)| format!("!{}", ENGINES.get(*i).map_or("?", |k| k.name))),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let why: String = outcome
        .abstained
        .iter()
        .map(|(i, why)| {
            format!(
                "\n         {} abstains: {why}",
                ENGINES.get(*i).map_or("?", |k| k.name)
            )
        })
        .collect();
    format!("{marks}{why}")
}

/// What went wrong at a recording's failing step: its keys, the fields,
/// each engine's differences, and fux-vt's screen beside the deciding
/// engine's (xterm's, or the first that differs).
fn report(recording: &Recording, outcome: &case::Outcome) -> String {
    let mut out = String::new();
    let step = outcome.step.unwrap_or(0);
    let keys = step
        .checked_sub(1)
        .and_then(|i| recording.steps.get(i))
        .map_or("(start)", |(k, _)| k.as_str());
    let _ = writeln!(
        out,
        "  {}, {}: after step {step} of {}, keys '{keys}'",
        recording.program,
        recording.version,
        recording.steps.len()
    );
    let failed = failures(&outcome.verdicts);
    if !failed.is_empty() {
        let shown: Vec<&str> = failed.iter().take(6).map(String::as_str).collect();
        let more = if failed.len() > 6 { ", ..." } else { "" };
        let _ = writeln!(out, "  fails on: {}{more}", shown.join(", "));
    }
    for verdict in &outcome.verdicts {
        let name = ENGINES.get(verdict.engine).map_or("?", |k| k.name);
        if verdict.differences.is_empty() {
            let _ = writeln!(out, "    {name}: agrees");
            continue;
        }
        let _ = writeln!(out, "    {name}: differs");
        for diff in verdict.differences.iter().take(8) {
            let _ = writeln!(out, "      {}", diff.line(name));
        }
        if verdict.differences.len() > 8 {
            let _ = writeln!(out, "      ... ({} in all)", verdict.differences.len());
        }
    }
    let xterm = engine::find("xterm");
    let shown = outcome
        .verdicts
        .iter()
        .find(|v| Some(v.engine) == xterm && !v.differences.is_empty())
        .or_else(|| outcome.verdicts.iter().find(|v| !v.differences.is_empty()));
    if let Some(verdict) = shown {
        let name = ENGINES.get(verdict.engine).map_or("?", |k| k.name);
        for line in snapshot::side_by_side(&outcome.fux, &verdict.snapshot, name).lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

fn status(name: &str) -> Option<Status> {
    STATUSES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Replays the recordings named (all if none) beside `panel`, and says how
/// each went. False if one expected to agree fails, or has no status.
/// `show` prints fux-vt's screen at the end of each.
pub fn run(panel: &[usize], names: &[String], show: bool) -> Result<bool, String> {
    let recordings = recordings(names)?;
    let names_of: Vec<&str> = panel
        .iter()
        .filter_map(|&i| ENGINES.get(i).map(|k| k.name))
        .collect();
    println!("engines: {}", names_of.join(" "));
    let started = Instant::now();
    let mut ok = true;
    let (mut agree, mut differ) = (0usize, 0usize);
    for recording in &recordings {
        let case = recording.case();
        let outcome = case.run_judged(panel, true, |v| !failures(v).is_empty())?;
        let expected = status(&recording.name);
        let label = format!(
            "{} ({} steps, {} bytes)",
            recording.name,
            recording.steps.len(),
            recording.bytes().len()
        );
        match (outcome.agrees(), expected) {
            (true, Some(Status::Agree)) => {
                agree = agree.saturating_add(1);
                println!("ok       {label}   {}", marks(&outcome));
            }
            (true, Some(_)) => {
                agree = agree.saturating_add(1);
                println!(
                    "agrees   {label} (recorded as differing)   {}",
                    marks(&outcome)
                );
            }
            (true, None) => {
                ok = false;
                agree = agree.saturating_add(1);
                println!(
                    "NEW      {label} (agrees; no status recorded)   {}",
                    marks(&outcome)
                );
            }
            (false, Some(Status::Differs(why) | Status::Decided { why, .. })) => {
                differ = differ.saturating_add(1);
                println!("differs  {label}: {why}   {}", marks(&outcome));
                if !names.is_empty() {
                    print!("{}", report(recording, &outcome));
                }
            }
            (false, expected) => {
                ok = false;
                differ = differ.saturating_add(1);
                let why = if expected.is_none() {
                    "no status recorded"
                } else {
                    "expected to agree"
                };
                println!("FAIL     {label} ({why})   {}", marks(&outcome));
                print!("{}", report(recording, &outcome));
            }
        }
        if show {
            let end = case.run_until(&[], false)?;
            for line in &end.fux.screen {
                println!("    |{}", line.text());
            }
        }
    }
    println!(
        "{agree} recordings agree, {differ} differ ({:.1}s)",
        started.elapsed().as_secs_f64()
    );
    Ok(ok)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_recording_loads_with_a_status() -> Result<(), String> {
        let all = super::recordings(&[])?;
        assert!(!all.is_empty());
        for recording in &all {
            assert!(
                super::status(&recording.name).is_some(),
                "{} has no status",
                recording.name
            );
            assert_eq!(recording.steps.first().map(|(k, _)| k.as_str()), Some(""));
            assert!(!crate::escape::escape(&recording.bytes()).is_empty());
        }
        Ok(())
    }
}
