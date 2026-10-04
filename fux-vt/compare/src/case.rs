//! A case: a terminal's size and history, and the output and resizes given
//! to fux-vt and to a panel of other engines. Running one compares fux-vt
//! with each after every step, and fux-vt fails where most of them differ
//! from it; shrinking one keeps it failing while taking away all it can.
//!
//! fux-vt is the subject unless the case names another engine
//! (`Case::subject`, `--subject`): that engine is then judged as fux-vt is,
//! and fux-vt is an engine of the panel. "fux-vt" below means the subject.
use crate::engine::{self, Engine, FUX_VT, Setup};
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
    /// The engine judged ([`FUX_VT`] unless `--subject` names another),
    /// by its index (`engine::kind`). The panel never holds it.
    pub subject: usize,
}

/// What one engine made of a step, beside fux-vt.
#[derive(Clone)]
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
///
/// On a cell blank in fux-vt and in an engine, the engine does not vote on
/// a part of the blank's style it makes otherwise than xterm (its
/// [`engine::Blanks`](crate::engine::Blanks)): the engines split three ways
/// there by choice, so its value is its choice, not a judgement of the
/// cell. Its difference is still shown.
pub fn outvoted_on(verdicts: &[Verdict]) -> Vec<String> {
    type Dissent<'a> = (
        Field,
        Option<(usize, usize)>,
        bool,
        BTreeMap<&'a str, usize>,
    );
    let mut dissent: BTreeMap<&str, Dissent<'_>> = BTreeMap::new();
    for verdict in verdicts {
        for diff in &verdict.differences {
            if !votes(verdict, diff.field, diff.styled_cell, diff.fux_blank) {
                continue;
            }
            let (_, _, _, values) = dissent.entry(diff.key.as_str()).or_insert_with(|| {
                (
                    diff.field,
                    diff.styled_cell,
                    diff.fux_blank,
                    BTreeMap::new(),
                )
            });
            let count = values.entry(diff.other.as_str()).or_insert(0);
            *count = count.saturating_add(1);
        }
    }
    dissent
        .into_iter()
        .filter(|(_, (field, cell, fux_blank, values))| {
            let told = verdicts
                .iter()
                .filter(|v| votes(v, *field, *cell, *fux_blank))
                .count();
            let differing: usize = values.values().sum();
            let agreeing = told.saturating_sub(differing);
            values.values().any(|&n| n > agreeing)
        })
        .map(|(key, _)| key.to_owned())
        .collect()
}

/// Whether `verdict`'s engine votes on `field`, of the screen cell `cell`
/// for a part of a cell's style (where fux-vt's cell is blank if
/// `fux_blank`): it can tell the field, read the cell's style, and, on a
/// cell blank on both sides, makes that part of its blanks as xterm does.
fn votes(verdict: &Verdict, field: Field, cell: Option<(usize, usize)>, fux_blank: bool) -> bool {
    engine::kind(verdict.engine).is_some_and(|k| {
        field.told_by(&k.can)
            && cell.is_none_or(|(y, x)| {
                read_style(&verdict.snapshot, y, x)
                    && (k.blanks.as_xterm(field) || !(fux_blank && blank(&verdict.snapshot, y, x)))
            })
    })
}

/// Whether an engine read the style of the screen cell at row `y`,
/// column `x`.
fn read_style(snapshot: &Snapshot, y: usize, x: usize) -> bool {
    snapshot
        .screen
        .get(y)
        .is_none_or(|line| line.unread_from.is_none_or(|from| x < from))
}

/// Whether the screen cell at row `y`, column `x` is a blank.
fn blank(snapshot: &Snapshot, y: usize, x: usize) -> bool {
    snapshot
        .screen
        .get(y)
        .and_then(|line| line.cells.get(x))
        .is_some_and(snapshot::Cell::is_blank)
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

/// What an engine made of a step beside the subject's `base` snapshot:
/// compared on what both the engine and the subject can tell.
pub fn verdict(subject: usize, base: &Snapshot, engine: usize, snapshot: Snapshot) -> Verdict {
    let can = match (engine::kind(subject), engine::kind(engine)) {
        (Some(s), Some(e)) => s.can.and(e.can),
        (None, Some(e)) => e.can,
        (Some(_) | None, None) => engine::Can::ALL,
    };
    Verdict {
        engine,
        differences: snapshot::differences(&base.masked(&can), &snapshot.masked(&can)),
        snapshot,
    }
}

struct Running {
    subject: usize,
    /// The rows of history the subject is asked for: the case's.
    history: usize,
    fux: Box<dyn Engine>,
    engines: Vec<(usize, Box<dyn Engine>)>,
    abstained: Vec<(usize, String)>,
}

impl Running {
    /// Applies `f` to the subject, whose errors end the case, and to every
    /// engine, whose errors make it abstain.
    fn each(&mut self, f: impl Fn(&mut dyn Engine) -> Result<(), String>) -> Result<(), String> {
        let subject = engine::name(self.subject);
        guarded(|| f(self.fux.as_mut())).map_err(|e| format!("{subject}: {e}"))?;
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

    /// The subject's snapshot and each engine's verdict beside it. The
    /// subject is asked for the case's rows of history (fux-vt gives all it
    /// keeps, whatever is asked), the engines for as many as it gave.
    fn compare(&mut self) -> Result<(Snapshot, Vec<Verdict>), String> {
        let subject = engine::name(self.subject);
        let history = self.history;
        let fux = guarded(|| self.fux.snapshot(history)).map_err(|e| format!("{subject}: {e}"))?;
        let mut verdicts = Vec::with_capacity(self.engines.len());
        let mut kept = Vec::with_capacity(self.engines.len());
        for (index, mut engine) in std::mem::take(&mut self.engines) {
            engine::kind(index).ok_or("no such engine")?;
            match guarded(|| engine.snapshot(fux.history.len())) {
                Ok(snapshot) => {
                    verdicts.push(verdict(self.subject, &fux, index, snapshot));
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

    /// Runs the case beside the `panel` (indices into [`ENGINES`], and
    /// [`FUX_VT`] when another engine is the subject), comparing after
    /// creation and after every step, and stopping where the subject is
    /// first outvoted. An error means the subject or an engine refused or
    /// failed something, not that they differed.
    ///
    /// [`ENGINES`]: crate::engine::ENGINES
    pub fn run(&self, panel: &[usize]) -> Result<Outcome, String> {
        self.run_until(panel, true)
    }

    /// Runs the case, stopping where fux-vt is first outvoted if `stop`,
    /// else running every step and giving the verdicts at the end.
    pub fn run_until(&self, panel: &[usize], stop: bool) -> Result<Outcome, String> {
        self.run_judged(panel, stop, |_, verdicts| outvoted(verdicts))
    }

    /// Runs the case as [`Case::run_until`] does, with `fails` saying, from
    /// the subject's snapshot and the verdicts after a step, whether the
    /// subject fails there.
    pub fn run_judged(
        &self,
        panel: &[usize],
        stop: bool,
        fails: impl Fn(&Snapshot, &[Verdict]) -> bool,
    ) -> Result<Outcome, String> {
        let setup = self.setup();
        let subject = engine::kind(self.subject).ok_or("no such subject")?;
        let mut running = Running {
            subject: self.subject,
            history: self.history,
            fux: (subject.make)(&setup)?,
            engines: Vec::with_capacity(panel.len()),
            abstained: Vec::new(),
        };
        for &index in panel {
            let kind = engine::kind(index).ok_or("no such engine")?;
            match guarded(|| (kind.make)(&setup)) {
                Ok(engine) => running.engines.push((index, engine)),
                Err(e) => running.abstained.push((index, e)),
            }
        }
        let (fux, verdicts) = running.compare()?;
        if fails(&fux, &verdicts) && (stop || self.steps.is_empty()) {
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
            if fails(&fux, &verdicts) && (stop || at_end) {
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
            .filter_map(|&i| engine::kind(i).map(|k| k.name))
            .collect();
        let subject = if self.subject == FUX_VT {
            String::new()
        } else {
            format!(" --subject {}", engine::name(self.subject))
        };
        let mut out = format!(
            "fux-vt-compare replay{subject} --engines {} --size {}x{} --history {}",
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
        subject: FUX_VT,
    }
}

/// The report for a case: what led to it, the command that replays it,
/// each engine's verdict and what it found different, and the subject's
/// screen beside the first engine that differs.
pub fn report(case: &Case, panel: &[usize], outcome: &Outcome) -> String {
    let subject = engine::name(case.subject);
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
                "  {subject} is not outvoted; {} of {total} engines differ:",
                outcome.differing()
            );
        }
    }
    for verdict in &outcome.verdicts {
        let name = engine::name(verdict.engine);
        if verdict.differences.is_empty() {
            let _ = writeln!(out, "    {name}: agrees");
            continue;
        }
        let _ = writeln!(out, "    {name}: differs");
        for diff in verdict.differences.iter().take(8) {
            let _ = writeln!(out, "      {}", diff.line_beside(subject, name));
        }
        if verdict.differences.len() > 8 {
            let _ = writeln!(out, "      ...");
        }
    }
    for (index, why) in &outcome.abstained {
        let name = engine::name(*index);
        let _ = writeln!(out, "    {name}: abstains: {why}");
    }
    if let Some(verdict) = outcome.verdicts.iter().find(|v| !v.differences.is_empty()) {
        let name = engine::name(verdict.engine);
        let shown = snapshot::side_by_side_beside(subject, &outcome.fux, &verdict.snapshot, name);
        for line in shown.lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Case, Snippet, Step, Verdict, outvoted_on};
    use crate::engine::{self, ENGINES, FUX_VT};
    use crate::snapshot::{self, Cell, Color, Field, Line, Snapshot, Style, Width};

    const PEN: Color = Color::Rgb(10, 20, 30);

    /// A screen of one cell: its text, foreground and whether it is bold.
    fn screen(text: &str, fg: Color, bold: bool) -> Snapshot {
        let style = Style {
            fg,
            bold,
            ..Style::default()
        };
        Snapshot {
            rows: 1,
            cols: 1,
            screen: vec![Line {
                cells: vec![Cell::new(text, Width::Narrow, style)],
                wrapped: false,
                prompt: false,
                unread_from: None,
            }],
            ..Snapshot::default()
        }
    }

    fn verdict(name: &str, fux: &Snapshot, other: Snapshot) -> Result<Verdict, String> {
        let engine = engine::find(name).ok_or(format!("no engine {name}"))?;
        let kind = ENGINES.get(engine).ok_or("no such engine")?;
        Ok(Verdict {
            engine,
            differences: snapshot::differences(&fux.masked(&kind.can), &other.masked(&kind.can)),
            snapshot: other,
        })
    }

    /// The default panel, after an erase with a bold pen of foreground
    /// [`PEN`]: Ghostty and alacritty keep neither on the blank, avt and
    /// wezterm both, libvterm and xterm the foreground alone.
    fn panel(fux: &Snapshot, libvterm: Snapshot) -> Result<Vec<Verdict>, String> {
        Ok(vec![
            verdict("ghostty", fux, screen("", Color::Default, false))?,
            verdict("alacritty", fux, screen("", Color::Default, false))?,
            verdict("libvterm", fux, libvterm)?,
            verdict("avt", fux, screen("", PEN, true))?,
            verdict("wezterm", fux, screen("", PEN, true))?,
        ])
    }

    /// The engines split three ways on a blank's style by choice, so an
    /// engine votes only on the parts it makes as xterm does: a blank's
    /// foreground libvterm and avt, its bold Ghostty, alacritty and
    /// libvterm. A blank as xterm makes it is not outvoted, even where
    /// libvterm, wrapping its own way on one column, has a bold glyph
    /// instead (`run --no-reflow`, seed 1, before the rule); a blank
    /// otherwise is.
    #[test]
    fn a_blank_s_style_is_judged_by_the_engines_that_make_it_as_xterm_does() -> Result<(), String> {
        let xterm = screen("", PEN, false);
        let none: Vec<String> = Vec::new();
        assert_eq!(outvoted_on(&panel(&xterm, screen("", PEN, false))?), none);
        let glyph = screen("k", Color::Rgb(255, 10, 20), true);
        assert_eq!(outvoted_on(&panel(&xterm, glyph)?), none);
        let bold = screen("", PEN, true);
        assert_eq!(
            outvoted_on(&panel(&bold, screen("", PEN, false))?),
            ["cell (0,0) bold"]
        );
        let no_fg = screen("", Color::Default, false);
        assert_eq!(
            outvoted_on(&panel(&no_fg, screen("", PEN, false))?),
            ["cell (0,0) fg"]
        );
        Ok(())
    }

    /// A one-step case of `rows` by `cols` with `subject` judged.
    fn one_step(rows: u16, cols: u16, bytes: &[u8], subject: usize) -> Case {
        Case {
            rows,
            cols,
            history: 0,
            reflow: true,
            newline_before_resize: false,
            steps: vec![Step::Output(vec![Snippet {
                family: usize::MAX,
                bytes: bytes.to_vec(),
            }])],
            subject,
        }
    }

    /// Another engine judged in fux-vt's place, with fux-vt on the panel:
    /// Ghostty keeps synchronized output on through DECSTR, fux-vt ends it
    /// (the named case `decstr-ends-synchronized-output`). Each, as the
    /// subject, is outvoted by the other alone, on the same field, with
    /// the sides swapped; the replay names the subject.
    #[test]
    fn another_engine_is_judged_as_fux_vt_is() -> Result<(), String> {
        let ghostty = engine::find("ghostty").ok_or("no ghostty")?;
        let bytes = b"\x1b[?2026h\x1b[!p";
        let case = one_step(1, 4, bytes, ghostty);
        let outcome = case.run_until(&[FUX_VT], false)?;
        assert!(!outcome.agrees());
        assert_eq!(outvoted_on(&outcome.verdicts), ["synchronized output"]);
        let verdict = outcome.verdicts.first().ok_or("no verdict")?;
        assert_eq!(verdict.engine, FUX_VT);
        let diff = verdict.differences.first().ok_or("no difference")?;
        assert_eq!(
            diff.line_beside("ghostty", "fux-vt"),
            "synchronized output: ghostty true, fux-vt false"
        );
        assert!(
            case.command(&[FUX_VT])
                .starts_with("fux-vt-compare replay --subject ghostty --engines fux-vt --size 1x4")
        );
        let fux = one_step(1, 4, bytes, FUX_VT);
        let outcome = fux.run_until(&[ghostty], false)?;
        assert_eq!(outvoted_on(&outcome.verdicts), ["synchronized output"]);
        let diff = outcome
            .verdicts
            .first()
            .and_then(|v| v.differences.first())
            .ok_or("no difference")?;
        assert_eq!(
            diff.line_beside("fux-vt", "ghostty"),
            "synchronized output: fux-vt false, ghostty true"
        );
        assert!(
            fux.command(&[ghostty])
                .starts_with("fux-vt-compare replay --engines ghostty --size 1x4")
        );
        Ok(())
    }

    /// A field the subject cannot tell is compared with no one, on both
    /// sides, as an engine's own `Can` masks it: Ghostty tells no link
    /// groups, so its two links to one URI read as one, where fux-vt and
    /// alacritty keep two (the named case
    /// `two-links-without-an-id-to-one-uri-are-two`). With Ghostty the
    /// subject, neither fux-vt nor alacritty differs on them.
    #[test]
    fn a_field_the_subject_cannot_tell_is_compared_with_no_one() -> Result<(), String> {
        let ghostty = engine::find("ghostty").ok_or("no ghostty")?;
        let alacritty = engine::find("alacritty").ok_or("no alacritty")?;
        let bytes = b"\x1b]8;;http://a.example/\x1b\\ab\x1b]8;;\x1b\\ \
            \x1b]8;;http://a.example/\x1b\\cd\x1b]8;;\x1b\\";
        let case = one_step(1, 6, bytes, ghostty);
        let outcome = case.run_until(&[FUX_VT, alacritty], false)?;
        let groups =
            |diffs: &[snapshot::Diff]| diffs.iter().filter(|d| d.field == Field::LinkGroup).count();
        let fux = outcome
            .verdicts
            .iter()
            .find(|v| v.engine == FUX_VT)
            .ok_or("no fux-vt verdict")?;
        // Unmasked, the second link differs in its group.
        assert_eq!(
            groups(&snapshot::differences(&outcome.fux, &fux.snapshot)),
            2
        );
        assert_eq!(outcome.verdicts.len(), 2);
        for verdict in &outcome.verdicts {
            assert_eq!(
                groups(&verdict.differences),
                0,
                "{}",
                engine::name(verdict.engine)
            );
        }
        assert!(outcome.agrees());
        Ok(())
    }
}
