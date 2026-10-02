//! A case: a terminal's size and history, and the output and resizes given
//! to fux-vt and to a panel of other engines. Running one compares fux-vt
//! with each after every step, and fux-vt fails where most of them differ
//! from it; shrinking one keeps it failing while taking away all it can.
use crate::engine::{ENGINES, Engine, SUBJECT, Setup};
use crate::escape;
use crate::families::{self, FAMILIES};
use crate::rng::Rng;
use crate::snapshot::{self, Diff, Field, Snapshot};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A piece of output, from one family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub family: usize,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Snippets given to both terminals together, in one call.
    Output(Vec<Snippet>),
    Resize(u16, u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub rows: u16,
    pub cols: u16,
    pub history: usize,
    pub reflow: bool,
    /// Whether to settle the cursor before each resize, by giving both
    /// [`SETTLE`]: autowrap on, and the cursor on a glyph at the start of
    /// its line (it overwrites the line's first cell). Where a reflow leaves the cursor, and whether it
    /// reflows at all, are choices the two make differently:
    ///
    /// - With a wrap pending, Ghostty keeps it pending, so the next glyph
    ///   starts a line; fux-vt puts the cursor after the text.
    /// - After a line that exactly fills the new width, Ghostty moves the
    ///   cursor to the start of a new row; fux-vt leaves it waiting to wrap.
    /// - A screen that grows pulls blank rows back from history in fux-vt,
    ///   not in Ghostty.
    /// - Ghostty reflows only with autowrap on; fux-vt always does.
    ///
    /// Random cases set this, so they compare everything else a reflow
    /// does; named cases and replays do not.
    pub newline_before_resize: bool,
    pub steps: Vec<Step>,
}

/// What one engine made of a step, beside fux-vt.
pub struct Verdict {
    pub engine: usize,
    /// What differs, compared on the fields the engine can tell; empty when
    /// it agrees with fux-vt.
    pub differences: Vec<Diff>,
    pub snapshot: Snapshot,
}

/// How a case went: the step after which fux-vt was first outvoted (none:
/// it never was), and fux-vt's snapshot and every engine's verdict then,
/// or at the end. An engine that failed or panicked abstains from then on,
/// and is listed with what went wrong.
pub struct Outcome {
    pub step: Option<usize>,
    pub fux: Snapshot,
    pub verdicts: Vec<Verdict>,
    pub abstained: Vec<(usize, String)>,
}

impl Outcome {
    pub fn agrees(&self) -> bool {
        self.step.is_none()
    }

    /// How many engines differ from fux-vt.
    pub fn differing(&self) -> usize {
        self.verdicts
            .iter()
            .filter(|v| !v.differences.is_empty())
            .count()
    }
}

/// Where fux-vt is outvoted, field by field: for each place some engine
/// differs (a cell's text, one attribute, the cursor, a mode), the engines
/// that can tell that field vote, and fux-vt is outvoted there when more
/// of them share one other value than agree with fux-vt. Engines that
/// differ from fux-vt each in their own way do not outvote it; a tie does
/// not. With one engine, wherever it differs.
pub fn outvoted_on(verdicts: &[Verdict]) -> Vec<String> {
    let mut dissent: BTreeMap<&str, (Field, BTreeMap<&str, usize>)> = BTreeMap::new();
    for verdict in verdicts {
        for diff in &verdict.differences {
            let (_, values) = dissent
                .entry(diff.key.as_str())
                .or_insert_with(|| (diff.field, BTreeMap::new()));
            let count = values.entry(diff.other.as_str()).or_insert(0);
            *count = count.saturating_add(1);
        }
    }
    dissent
        .into_iter()
        .filter(|(_, (field, values))| {
            let told = verdicts
                .iter()
                .filter(|v| ENGINES.get(v.engine).is_some_and(|k| field.told_by(&k.can)))
                .count();
            let differing: usize = values.values().sum();
            let agreeing = told.saturating_sub(differing);
            values.values().any(|&n| n > agreeing)
        })
        .map(|(key, _)| key.to_owned())
        .collect()
}

fn outvoted(verdicts: &[Verdict]) -> bool {
    !outvoted_on(verdicts).is_empty()
}

/// What a panic said, from its payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".to_owned())
}

/// Calls into an engine, turning a panic into an error, so one engine's
/// crash costs its vote, not the run.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|payload| Err(format!("panicked: {}", panic_message(payload.as_ref()))))
}

struct Running {
    fux: Box<dyn Engine>,
    engines: Vec<(usize, Box<dyn Engine>)>,
    abstained: Vec<(usize, String)>,
}

impl Running {
    /// Applies `f` to fux-vt, whose errors end the case, and to every
    /// engine, whose errors make it abstain.
    fn each(&mut self, f: impl Fn(&mut dyn Engine) -> Result<(), String>) -> Result<(), String> {
        guarded(|| f(self.fux.as_mut())).map_err(|e| format!("fux-vt: {e}"))?;
        let mut kept = Vec::with_capacity(self.engines.len());
        for (index, mut engine) in std::mem::take(&mut self.engines) {
            match guarded(|| f(engine.as_mut())) {
                Ok(()) => kept.push((index, engine)),
                Err(e) => self.abstained.push((index, e)),
            }
        }
        self.engines = kept;
        Ok(())
    }

    fn compare(&mut self) -> Result<(Snapshot, Vec<Verdict>), String> {
        let fux = guarded(|| self.fux.snapshot(0)).map_err(|e| format!("fux-vt: {e}"))?;
        let mut verdicts = Vec::with_capacity(self.engines.len());
        let mut kept = Vec::with_capacity(self.engines.len());
        for (index, mut engine) in std::mem::take(&mut self.engines) {
            let kind = ENGINES.get(index).ok_or("no such engine")?;
            match guarded(|| engine.snapshot(fux.history.len())) {
                Ok(snapshot) => {
                    let differences =
                        snapshot::differences(&fux.masked(&kind.can), &snapshot.masked(&kind.can));
                    verdicts.push(Verdict {
                        engine: index,
                        differences,
                        snapshot,
                    });
                    kept.push((index, engine));
                }
                Err(e) => self.abstained.push((index, e)),
            }
        }
        self.engines = kept;
        Ok((fux, verdicts))
    }
}

impl Case {
    fn setup(&self) -> Setup {
        Setup {
            rows: self.rows,
            cols: self.cols,
            history: self.history,
            reflow: self.reflow,
        }
    }

    /// Runs the case beside the `panel` (indices into [`ENGINES`]),
    /// comparing after creation and after every step, and stopping where
    /// fux-vt is first outvoted. An error means an engine refused or
    /// failed something, not that they differed.
    pub fn run(&self, panel: &[usize]) -> Result<Outcome, String> {
        self.run_until(panel, true)
    }

    /// Runs the case, stopping where fux-vt is first outvoted if `stop`,
    /// else running every step and giving the verdicts at the end.
    pub fn run_until(&self, panel: &[usize], stop: bool) -> Result<Outcome, String> {
        let setup = self.setup();
        let mut running = Running {
            fux: (SUBJECT.make)(&setup)?,
            engines: Vec::with_capacity(panel.len()),
            abstained: Vec::new(),
        };
        for &index in panel {
            let kind = ENGINES.get(index).ok_or("no such engine")?;
            match guarded(|| (kind.make)(&setup)) {
                Ok(engine) => running.engines.push((index, engine)),
                Err(e) => running.abstained.push((index, e)),
            }
        }
        let (fux, verdicts) = running.compare()?;
        if outvoted(&verdicts) && (stop || self.steps.is_empty()) {
            return Ok(Outcome {
                step: Some(0),
                fux,
                verdicts,
                abstained: running.abstained,
            });
        }
        let mut last = (fux, verdicts);
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                Step::Output(snippets) => {
                    let bytes: Vec<u8> = snippets
                        .iter()
                        .flat_map(|s| s.bytes.iter().copied())
                        .collect();
                    running.each(|e| e.process(&bytes))?;
                }
                Step::Resize(rows, cols) => {
                    if self.newline_before_resize {
                        running.each(|e| e.process(SETTLE))?;
                    }
                    running.each(|e| e.resize(*rows, *cols))?;
                }
            }
            let (fux, verdicts) = running.compare()?;
            let at_end = i.saturating_add(1) == self.steps.len();
            if outvoted(&verdicts) && (stop || at_end) {
                return Ok(Outcome {
                    step: Some(i.saturating_add(1)),
                    fux,
                    verdicts,
                    abstained: running.abstained,
                });
            }
            last = (fux, verdicts);
        }
        Ok(Outcome {
            step: None,
            fux: last.0,
            verdicts: last.1,
            abstained: running.abstained,
        })
    }

    fn differs(&self, panel: &[usize]) -> bool {
        self.run(panel).is_ok_and(|o| !o.agrees())
    }

    /// The families the case's output comes from, by name, each once.
    pub fn families(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        for step in &self.steps {
            let found: Vec<&'static str> = match step {
                Step::Output(snippets) => snippets
                    .iter()
                    .filter_map(|s| FAMILIES.get(s.family).map(|f| f.name))
                    .collect(),
                Step::Resize(..) => vec![families::RESIZE],
            };
            for name in found {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The command that replays the case beside `panel`.
    pub fn command(&self, panel: &[usize]) -> String {
        let names: Vec<&str> = panel
            .iter()
            .filter_map(|&i| ENGINES.get(i).map(|k| k.name))
            .collect();
        let mut out = format!(
            "fux-vt-compare replay --engines {} --size {}x{} --history {}",
            names.join(","),
            self.rows,
            self.cols,
            self.history
        );
        if !self.reflow {
            out.push_str(" --no-reflow");
        }
        if self.newline_before_resize {
            out.push_str(" --newline-before-resize");
        }
        for step in &self.steps {
            match step {
                Step::Output(snippets) => {
                    let bytes: Vec<u8> = snippets
                        .iter()
                        .flat_map(|s| s.bytes.iter().copied())
                        .collect();
                    let _ = write!(out, " '{}'", escape::escape(&bytes));
                }
                Step::Resize(rows, cols) => {
                    let _ = write!(out, " resize:{rows}x{cols}");
                }
            }
        }
        out
    }

    /// The smallest case found that still differs, by taking away steps,
    /// then snippets, then bytes, and making the screen smaller, as long as
    /// it keeps differing; at most `budget` runs.
    pub fn shrink(&self, panel: &[usize], budget: usize) -> Case {
        let mut best = self.clone();
        let runs = std::cell::Cell::new(0usize);
        let try_it = |candidate: Case, best: &mut Case| -> bool {
            if runs.get() >= budget {
                return false;
            }
            runs.set(runs.get().saturating_add(1));
            if candidate.differs(panel) {
                *best = candidate;
                true
            } else {
                false
            }
        };
        loop {
            let mut progress = false;
            // Steps, last first, so a difference made early stays.
            let mut i = best.steps.len();
            while i > 0 {
                i = i.saturating_sub(1);
                let mut candidate = best.clone();
                candidate.steps = without(&best.steps, i);
                progress |= try_it(candidate, &mut best);
            }
            // Snippets within each step.
            for s in 0..best.steps.len() {
                let mut k = match best.steps.get(s) {
                    Some(Step::Output(snippets)) => snippets.len(),
                    _ => 0,
                };
                while k > 0 {
                    k = k.saturating_sub(1);
                    let mut candidate = best.clone();
                    if let Some(Step::Output(snippets)) = candidate.steps.get_mut(s) {
                        *snippets = without(snippets, k);
                    }
                    progress |= try_it(candidate, &mut best);
                }
            }
            // Characters within each snippet of text, eight at a time, then
            // four, two and one. Sequences are never cut: a sequence cut
            // short is another sequence, or broken UTF-8, from no family.
            for s in 0..best.steps.len() {
                let count = match best.steps.get(s) {
                    Some(Step::Output(snippets)) => snippets.len(),
                    _ => 0,
                };
                for k in 0..count {
                    let mut chunk = 8usize;
                    while chunk > 0 {
                        let mut at = 0usize;
                        while let Some(Step::Output(snippets)) = best.steps.get(s) {
                            let Some(snippet) = snippets.get(k) else {
                                break;
                            };
                            if !shrinkable(snippet.family) {
                                break;
                            }
                            let units = units(&snippet.bytes);
                            if at >= units.len() {
                                break;
                            }
                            let end = at.saturating_add(chunk);
                            let mut candidate = best.clone();
                            if let Some(Step::Output(snippets)) = candidate.steps.get_mut(s)
                                && let Some(snippet) = snippets.get_mut(k)
                            {
                                snippet.bytes = units
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| *i < at || *i >= end)
                                    .flat_map(|(_, unit)| unit.iter().copied())
                                    .collect();
                            }
                            if try_it(candidate, &mut best) {
                                progress = true;
                            } else {
                                at = at.saturating_add(chunk);
                            }
                        }
                        chunk = chunk.checked_div(2).unwrap_or(0);
                    }
                }
            }
            // A smaller screen and no history.
            for shrink in [
                // Cases that resize keep their history: see RESIZE_HISTORY.
                |c: &mut Case| {
                    if !c.steps.iter().any(|s| matches!(s, Step::Resize(..))) {
                        c.history = 0;
                    }
                },
                |c: &mut Case| c.rows = c.rows.saturating_sub(1).max(1),
                |c: &mut Case| c.cols = c.cols.saturating_sub(1).max(1),
                |c: &mut Case| c.rows = c.rows.div_ceil(2),
                |c: &mut Case| c.cols = c.cols.div_ceil(2),
            ] {
                let mut candidate = best.clone();
                shrink(&mut candidate);
                if candidate != best {
                    progress |= try_it(candidate, &mut best);
                }
            }
            best.steps.retain(|step| match step {
                Step::Output(snippets) => snippets.iter().any(|s| !s.bytes.is_empty()),
                Step::Resize(..) => true,
            });
            if !progress || runs.get() >= budget {
                return best;
            }
        }
    }
}

/// Whether the shrinker may take characters out of a family's snippets:
/// the families of plain text, never those of sequences.
fn shrinkable(family: usize) -> bool {
    FAMILIES
        .get(family)
        .is_some_and(|f| ["text", "wide", "clusters"].contains(&f.name))
}

/// The bytes as characters, each invalid byte one of its own.
fn units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        let mut at = 0usize;
        for c in valid.chars() {
            let end = at.saturating_add(c.len_utf8());
            if let Some(unit) = valid.as_bytes().get(at..end) {
                out.push(unit);
            }
            at = end;
        }
        let invalid = chunk.invalid();
        for i in 0..invalid.len() {
            if let Some(unit) = invalid.get(i..=i) {
                out.push(unit);
            }
        }
    }
    out
}

fn without<T: Clone>(items: &[T], skip: usize) -> Vec<T> {
    items
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != skip)
        .map(|(_, x)| x.clone())
        .collect()
}

/// What settles the cursor before a resize: see `Case::newline_before_resize`.
pub const SETTLE: &[u8] = b"\x1b[?7h\r.\r";

/// History kept in cases that resize: more rows than one can write.
pub const RESIZE_HISTORY: usize = 10_000;

/// A screen size: mostly small, so cases run fast and edges are close;
/// now and then one or two cells, or wide.
fn size(r: &mut Rng, small: usize, large: usize) -> u16 {
    let n = match r.below(10) {
        0 => r.below(3).saturating_add(1),
        1 => r.below(large).saturating_add(1),
        _ => r.below(small).saturating_add(2),
    };
    u16::try_from(n).unwrap_or(1)
}

/// A random case drawing output from `families` (indices into
/// [`FAMILIES`]); resizes come only from the resize family.
pub fn random(r: &mut Rng, families: &[usize], reflow: bool) -> Case {
    let (rows, cols) = (size(r, 8, 40), size(r, 16, 100));
    let resizes = families
        .iter()
        .any(|&f| FAMILIES.get(f).is_some_and(|f| f.name == families::RESIZE));
    // A screen that grows pulls rows back from history, and Ghostty's is
    // bounded in bytes, not rows: with resizes, fux-vt keeps more history
    // than a case can fill, so both pull back the same rows.
    let history = if resizes {
        RESIZE_HISTORY
    } else {
        r.pick(&[0usize, 0, 3, 50]).copied().unwrap_or(0)
    };
    let output: Vec<usize> = families
        .iter()
        .copied()
        .filter(|&f| FAMILIES.get(f).is_some_and(|f| f.name != families::RESIZE))
        .collect();
    let steps = (0..r.below(10).saturating_add(1))
        .map(|_| {
            if resizes && (output.is_empty() || r.chance(15)) {
                Step::Resize(size(r, 8, 40), size(r, 16, 100))
            } else {
                Step::Output(
                    (0..r.below(10).saturating_add(1))
                        .filter_map(|_| {
                            let family = *r.pick(&output)?;
                            let bytes = (FAMILIES.get(family)?.generate)(r);
                            Some(Snippet { family, bytes })
                        })
                        .collect(),
                )
            }
        })
        .collect();
    Case {
        rows,
        cols,
        history,
        reflow,
        newline_before_resize: true,
        steps,
    }
}

/// The report for a case: what led to it, the command that replays it,
/// each engine's verdict and what it found different, and fux-vt's screen
/// beside the first engine that differs.
pub fn report(case: &Case, panel: &[usize], outcome: &Outcome) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  families: {}", case.families().join(", "));
    let _ = writeln!(out, "  replay:   {}", case.command(panel));
    let total = outcome.verdicts.len();
    let keys = outvoted_on(&outcome.verdicts);
    if !keys.is_empty() {
        let shown: Vec<&str> = keys.iter().take(6).map(String::as_str).collect();
        let more = if keys.len() > 6 { ", ..." } else { "" };
        let _ = writeln!(out, "  outvoted on: {}{more}", shown.join(", "));
    }
    match outcome.step {
        Some(0) => {
            let _ = writeln!(
                out,
                "  from the start, {} of {total} engines differ:",
                outcome.differing()
            );
        }
        Some(step) => {
            let _ = writeln!(
                out,
                "  after step {step}, {} of {total} engines differ:",
                outcome.differing()
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  fux-vt is not outvoted; {} of {total} engines differ:",
                outcome.differing()
            );
        }
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
            let _ = writeln!(out, "      ...");
        }
    }
    for (index, why) in &outcome.abstained {
        let name = ENGINES.get(*index).map_or("?", |k| k.name);
        let _ = writeln!(out, "    {name}: abstains: {why}");
    }
    if let Some(verdict) = outcome.verdicts.iter().find(|v| !v.differences.is_empty()) {
        let name = ENGINES.get(verdict.engine).map_or("?", |k| k.name);
        for line in snapshot::side_by_side(&outcome.fux, &verdict.snapshot, name).lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}
