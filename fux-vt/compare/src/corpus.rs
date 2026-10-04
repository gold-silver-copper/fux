//! The corpus: what real programs wrote to a terminal, recorded by
//! `record` (`corpus/record.sh` records every scenario), and replayed here
//! through fux-vt beside other engines.
//!
//! A recording is replayed as a case of its own size, one step for each
//! step recorded: the program starting, then the output after each line
//! of keys. fux-vt is set up as fux sets up a pane (reflow, the kitty
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
//! **A resize is judged by the program's answer to it.** A step that
//! resized the terminal is two steps of the case: the resize, and what the
//! program wrote after it. The engines are compared after both, but only
//! the second is judged. Right after the resize, before the program has
//! redrawn, each engine shows its own way of resizing (whether it reflows,
//! what comes back from history, where the cursor lands), which they choose
//! differently on purpose, as `run` avoids by settling the cursor before
//! each resize; fux's panes reflow, as the panel does and xterm does not. What the
//! program draws for its new size is what is judged. What it leaves as the
//! resize left it (a shell's earlier lines, history) stays as each engine
//! resized it, and a recording where that differs has the reason in its
//! status.
//!
//! Each recording has a status, like a family's: expected to agree, or
//! differing for a recorded reason. `corpus` fails if one expected to
//! agree does not.
use crate::case::{self, Case, Snippet, Step, Verdict};
use crate::engine::{self, ENGINES, FUX_VT};
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

/// The reason the recordings with emoji sequences differ, from the start.
const CLUSTERS: &str = "An emoji sequence (a modifier, a ZWJ sequence, a flag, VS16) is one \
    wide cell in fux-vt, as in Ghostty and wezterm: the `clusters` family's recorded choice. \
    xterm keeps each code point a cell of its own (and VS16 narrow), and alacritty, libvterm \
    and avt mostly do. helix and less lay a line out by clusters as fux-vt does (Ghostty and \
    wezterm agree with fux-vt on helix-unicode and less-small); vim, neovim and micro by code \
    points, each placed by a cursor move of its own, so their emoji differ in every engine, \
    Ghostty joining a modifier printed after a cursor move to the cell before it. \
    `corpus --engines xterm,ghostty,wezterm vim-unicode` shows it.";

/// The reason zsh-resize differs.
const REFLOW: &str = "fux's panes reflow (fux::pane::OPTIONS), as Ghostty, wezterm and \
    libvterm do: at the shrink to 20x60, zsh's line typed before it is rewrapped onto two \
    rows with the cursor on the second, and zsh redraws its prompt from there, so the line \
    shows twice, as in those three. xterm, alacritty and avt, as run here, cut the line \
    instead. Every step before the shrink agrees. `corpus zsh-resize` shows it.";

/// The reason tmux-resize differs.
const ALTERNATE_RESIZE: &str = "Leaving the alternate screen after it shrank, with the cursor \
    low, and grew again, xterm puts the cursor where the alternate screen's moved to, fux-vt \
    and the panel where 1049 saved it, so tmux's `[exited]` lands on another row: a choice of \
    xterm's resize (`replay --engines xterm,ghostty,alacritty --size 40x120 --history 10000 \
    --no-reflow '\\e[?1049h\\e[34;1H' resize:10x40 resize:40x120 '\\e[?1049lX'`). Every step \
    before the last agrees.";

/// What is expected of each recording; one not listed here is reported as
/// new, and fails.
pub const STATUSES: &[(&str, Status)] = &[
    ("bash", Status::Agree),
    ("bash-complete", Status::Agree),
    ("bash-history", Status::Agree),
    ("bash-small", Status::Agree),
    ("bat-diff", Status::Agree),
    ("bat-markdown", Status::Agree),
    ("bat-page", Status::Agree),
    ("btop", Status::Agree),
    ("btop-small", Status::Agree),
    ("cargo-build", Status::Agree),
    ("cargo-errors", Status::Agree),
    ("cargo-test", Status::Agree),
    ("clang-errors", Status::Agree),
    ("claude", Status::Differs(CLAUDE_TITLE)),
    ("claude-ghostty", Status::Differs(CLAUDE_TITLE)),
    ("claude-main", Status::Differs(CLAUDE_TITLE)),
    ("claude-resize", Status::Differs(CLAUDE_TITLE)),
    ("claude-small", Status::Differs(CLAUDE_TITLE)),
    ("delta-diff", Status::Agree),
    ("delta-log", Status::Agree),
    ("delta-show", Status::Agree),
    ("delta-wide", Status::Agree),
    ("emacs-dired", Status::Agree),
    ("emacs-mx", Status::Agree),
    ("emacs-resize", Status::Agree),
    ("emacs-scroll", Status::Agree),
    ("emacs-split", Status::Agree),
    ("fish", Status::Agree),
    ("fish-complete", Status::Agree),
    ("fish-history", Status::Agree),
    ("fish-small", Status::Agree),
    ("fzf", Status::Agree),
    ("fzf-height", Status::Agree),
    ("fzf-multi", Status::Agree),
    ("fzf-preview", Status::Agree),
    ("fzf-small", Status::Agree),
    ("git-add-p", Status::Agree),
    ("git-diff", Status::Agree),
    ("git-graph", Status::Agree),
    ("gls", Status::Agree),
    ("gls-long", Status::Agree),
    ("gls-wide", Status::Agree),
    ("helix", Status::Agree),
    ("helix-picker", Status::Agree),
    ("helix-resize", Status::Agree),
    ("helix-select", Status::Agree),
    ("helix-small", Status::Agree),
    ("helix-unicode", Status::Differs(CLUSTERS)),
    ("htop", Status::Agree),
    ("htop-small", Status::Agree),
    ("htop-tree", Status::Agree),
    ("lazygit", Status::Agree),
    ("lazygit-small", Status::Agree),
    ("lazygit-stage", Status::Agree),
    ("less", Status::Agree),
    ("less-chop", Status::Agree),
    ("less-color", Status::Agree),
    ("less-small", Status::Differs(CLUSTERS)),
    ("man", Status::Agree),
    ("man-long", Status::Agree),
    ("man-small", Status::Agree),
    ("man-tables", Status::Agree),
    ("man-wide", Status::Agree),
    ("mc", Status::Agree),
    ("mc-small", Status::Agree),
    ("micro-edit", Status::Agree),
    ("micro-small", Status::Differs(CLUSTERS)),
    ("micro-split", Status::Agree),
    ("ncdu", Status::Agree),
    ("nnn", Status::Agree),
    ("nnn-detail", Status::Agree),
    ("npm-install", Status::Agree),
    ("nvim-diagnostics", Status::Agree),
    ("nvim-diff", Status::Agree),
    ("nvim-help", Status::Agree),
    ("nvim-insert", Status::Agree),
    ("nvim-netrw", Status::Agree),
    ("nvim-resize", Status::Agree),
    ("nvim-scroll", Status::Agree),
    ("nvim-search", Status::Agree),
    ("nvim-small", Status::Agree),
    ("nvim-split", Status::Agree),
    ("nvim-tabs", Status::Agree),
    ("nvim-terminal", Status::Agree),
    ("nvim-unicode", Status::Differs(CLUSTERS)),
    ("nvim-visual", Status::Agree),
    ("nvim-wide", Status::Agree),
    ("pico", Status::Agree),
    ("ranger", Status::Agree),
    ("tig", Status::Agree),
    ("tig-blame", Status::Agree),
    ("tig-tree", Status::Agree),
    ("tmux", Status::Agree),
    ("tmux-copy", Status::Agree),
    ("tmux-resize", Status::Differs(ALTERNATE_RESIZE)),
    ("tmux-small", Status::Agree),
    ("tmux-vim", Status::Agree),
    ("top", Status::Agree),
    ("vim", Status::Agree),
    ("vim-diff", Status::Agree),
    ("vim-help", Status::Agree),
    ("vim-insert", Status::Agree),
    ("vim-resize", Status::Agree),
    ("vim-small", Status::Agree),
    ("vim-terminal", Status::Agree),
    ("vim-unicode", Status::Differs(CLUSTERS)),
    ("zellij", Status::Agree),
    ("zellij-small", Status::Agree),
    ("zsh", Status::Agree),
    ("zsh-history", Status::Agree),
    ("zsh-menu", Status::Agree),
    ("zsh-resize", Status::Differs(REFLOW)),
    ("zsh-small", Status::Agree),
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
    /// The steps that resized the terminal (`!resize` in the keys) rather
    /// than typing: the step's index and the new size, rows and columns.
    /// The step's output is what the program wrote after the resize.
    pub resizes: Vec<(usize, u16, u16)>,
}

impl Recording {
    /// Every byte the program wrote, in order.
    pub fn bytes(&self) -> Vec<u8> {
        self.steps
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }

    /// The size the terminal is resized to before step `index`'s output,
    /// if that step resized it.
    pub fn resize_at(&self, index: usize) -> Option<(u16, u16)> {
        self.resizes
            .iter()
            .find(|(i, _, _)| *i == index)
            .map(|&(_, rows, cols)| (rows, cols))
    }

    /// The step recorded (counted from 0) that the case's step `index`
    /// belongs to, as `Outcome::step` counts them: none for 0, before any
    /// step, the program's start for 1. A step that resized is two in the
    /// case, the resize and the output after it.
    pub fn recorded_step(&self, index: usize) -> Option<usize> {
        let mut case_steps = 0usize;
        for i in 0..self.steps.len() {
            let these = if self.resize_at(i).is_some() { 2 } else { 1 };
            case_steps = case_steps.saturating_add(these);
            if index > 0 && case_steps >= index {
                return Some(i);
            }
        }
        None
    }

    /// Whether each comparison the case makes is judged, in order: one
    /// before any step, then one after each. All are but those right after
    /// a resize, before the program's answer to it (see the module
    /// documentation).
    pub fn judged(&self) -> Vec<bool> {
        std::iter::once(true)
            .chain((0..self.steps.len()).flat_map(|i| {
                let resized = self.resize_at(i).map(|_| false);
                resized.into_iter().chain([true])
            }))
            .collect()
    }

    /// The recording as a case: one step for each step recorded, and a
    /// resize before the output of each step that resized.
    pub fn case(&self) -> Case {
        Case {
            rows: self.rows,
            cols: self.cols,
            history: 10_000,
            reflow: fux::pane::OPTIONS.reflow,
            newline_before_resize: false,
            steps: self
                .steps
                .iter()
                .enumerate()
                .flat_map(|(i, (_, bytes))| {
                    let resize = self
                        .resize_at(i)
                        .map(|(rows, cols)| Step::Resize(rows, cols));
                    resize.into_iter().chain([Step::Output(vec![Snippet {
                        family: usize::MAX,
                        bytes: bytes.clone(),
                    }])])
                })
                .collect(),
            subject: FUX_VT,
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
    let mut resizes = Vec::new();
    let mut from = 0usize;
    for (i, step) in field(&json, "steps", path)?
        .as_array()
        .ok_or(format!("{}: steps is not a list", path.display()))?
        .iter()
        .enumerate()
    {
        if let Some(size) = step.get("resize") {
            let dimension = |d: usize| -> Result<u16, String> {
                size.get(d)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|&n| n > 0)
                    .ok_or(format!("{}: a bad resize", path.display()))
            };
            resizes.push((i, dimension(0)?, dimension(1)?));
        }
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
        resizes,
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
            format!("{mark}{}", engine::name(v.engine))
        })
        .chain(
            outcome
                .abstained
                .iter()
                .map(|(i, _)| format!("!{}", engine::name(*i))),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let why: String = outcome
        .abstained
        .iter()
        .map(|(i, why)| format!("\n         {} abstains: {why}", engine::name(*i)))
        .collect();
    format!("{marks}{why}")
}

/// Where the case's step `case_step` (as `Outcome::step` counts them) is
/// in the recording: "after step N of M, keys K".
fn place(recording: &Recording, case_step: Option<usize>) -> String {
    let recorded = case_step.and_then(|s| recording.recorded_step(s));
    let step = recorded.map_or(0, |i| i.saturating_add(1));
    let keys = match recorded {
        None | Some(0) => "(start)".to_owned(),
        Some(i) => match recording.resize_at(i) {
            Some((rows, cols)) => format!("(resized to {rows}x{cols})"),
            None => recording
                .steps
                .get(i)
                .map_or(String::new(), |(k, _)| format!("'{k}'")),
        },
    };
    format!(
        "after step {step} of {}, keys {keys}",
        recording.steps.len()
    )
}

/// What went wrong at a recording's failing step: its keys, the fields,
/// each engine's differences, and the subject's screen beside the deciding
/// engine's (xterm's, or the first that differs).
fn report(recording: &Recording, subject: usize, outcome: &case::Outcome) -> String {
    let subject = engine::name(subject);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "  {}, {}: {}",
        recording.program,
        recording.version,
        place(recording, outcome.step)
    );
    let failed = failures(&outcome.verdicts);
    if !failed.is_empty() {
        let shown: Vec<&str> = failed.iter().take(6).map(String::as_str).collect();
        let more = if failed.len() > 6 { ", ..." } else { "" };
        let _ = writeln!(out, "  fails on: {}{more}", shown.join(", "));
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
        let name = engine::name(verdict.engine);
        let screens = snapshot::side_by_side_beside(subject, &outcome.fux, &verdict.snapshot, name);
        for line in screens.lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

fn status(name: &str) -> Option<Status> {
    STATUSES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Runs a recording's case as `corpus` judges it, stopping where the
/// subject first fails.
fn first_failure(
    recording: &Recording,
    case: &Case,
    panel: &[usize],
) -> Result<case::Outcome, String> {
    // `run_judged` judges once before the steps and once after each.
    let judged = recording.judged();
    let at = std::cell::Cell::new(0usize);
    case.run_judged(panel, true, |_, v| {
        let i = at.get();
        at.set(i.saturating_add(1));
        judged.get(i).copied().unwrap_or(true) && !failures(v).is_empty()
    })
}

/// Replays the recordings named (all if none) beside `panel`, and says how
/// each went. False if one expected to agree fails, or has no status.
/// `show` prints the subject's screen at the end of each. With a subject
/// other than fux-vt, see [`run_subject`].
pub fn run(
    subject: usize,
    panel: &[usize],
    names: &[String],
    show: bool,
    json: Option<&str>,
) -> Result<bool, String> {
    if subject != FUX_VT {
        return run_subject(subject, panel, names, show);
    }
    let recordings = recordings(names)?;
    let names_of: Vec<&str> = panel.iter().map(|&i| engine::name(i)).collect();
    println!("engines: {}", names_of.join(" "));
    let started = Instant::now();
    let mut ok = true;
    let (mut agree, mut differ) = (0usize, 0usize);
    let mut results = Vec::new();
    for recording in &recordings {
        let case = recording.case();
        let outcome = first_failure(recording, &case, panel)?;
        let expected = status(&recording.name);
        let label = format!(
            "{} ({} steps, {} bytes)",
            recording.name,
            recording.steps.len(),
            recording.bytes().len()
        );
        results.push(serde_json::json!({
            "recording": recording.name,
            "agrees": outcome.agrees(),
            "recorded": match &expected {
                Some(Status::Differs(why) | Status::Decided { why, .. }) => Some(why.to_string()),
                _ => None,
            },
        }));
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
                    print!("{}", report(recording, FUX_VT, &outcome));
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
                print!("{}", report(recording, FUX_VT, &outcome));
            }
        }
        if show {
            let end = case.run_until(&[], false)?;
            for line in &end.fux.screen {
                println!("    |{}", line.text());
            }
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("{agree} recordings agree, {differ} differ ({seconds:.1}s)");
    if let Some(path) = json {
        let value = serde_json::json!({
            "check": "corpus",
            "engines": names_of,
            "recordings": recordings.len(),
            "agree": agree,
            "differ": differ,
            "seconds": seconds,
            "ok": ok,
            "results": results,
        });
        let mut text =
            serde_json::to_string_pretty(&value).map_err(|e| format!("the results: {e}"))?;
        text.push('\n');
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(ok)
}

/// The recordings judged with another engine as the subject, as fux-vt is
/// judged: xterm decides what it can tell, the panel (where fux-vt now
/// votes) the rest. The statuses are fux-vt's, so none is applied: each
/// recording's outcome is reported, and only an error fails.
fn run_subject(
    subject: usize,
    panel: &[usize],
    names: &[String],
    show: bool,
) -> Result<bool, String> {
    let recordings = recordings(names)?;
    let name = engine::name(subject);
    let names_of: Vec<&str> = panel.iter().map(|&i| engine::name(i)).collect();
    println!("subject: {name}, judged as fux-vt is (fux-vt's statuses are not applied)");
    println!("engines: {}", names_of.join(" "));
    let started = Instant::now();
    let (mut agree, mut differ) = (0usize, 0usize);
    for recording in &recordings {
        let case = Case {
            subject,
            ..recording.case()
        };
        let outcome = first_failure(recording, &case, panel)?;
        let label = format!(
            "{} ({} steps, {} bytes)",
            recording.name,
            recording.steps.len(),
            recording.bytes().len()
        );
        if outcome.agrees() {
            agree = agree.saturating_add(1);
            println!("agrees   {label}   {}", marks(&outcome));
        } else {
            differ = differ.saturating_add(1);
            println!(
                "differs  {label}: {}; on {}   {}",
                place(recording, outcome.step),
                listed(&failures(&outcome.verdicts)),
                marks(&outcome)
            );
            if !names.is_empty() {
                print!("{}", report(recording, subject, &outcome));
            }
        }
        if show {
            let end = case.run_until(&[], false)?;
            for line in &end.fux.screen {
                println!("    |{}", line.text());
            }
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("{name}: {agree} recordings agree, {differ} differ ({seconds:.1}s)");
    Ok(true)
}

/// The first six of `keys`, joined, and `...` if there are more.
fn listed(keys: &[String]) -> String {
    let shown: Vec<&str> = keys.iter().take(6).map(String::as_str).collect();
    let more = if keys.len() > 6 { ", ..." } else { "" };
    format!("{}{more}", shown.join(", "))
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
            assert!(recording.resize_at(0).is_none());
            assert!(!crate::escape::escape(&recording.bytes()).is_empty());
        }
        Ok(())
    }

    /// A step that resized is two steps of the case: the resize, then the
    /// output after it.
    #[test]
    fn a_resize_is_its_own_step_of_the_case() {
        let recording = super::Recording {
            name: "r".into(),
            program: "p".into(),
            version: "v".into(),
            rows: 4,
            cols: 10,
            steps: vec![
                (String::new(), b"a".to_vec()),
                ("j".into(), b"b".to_vec()),
                (String::new(), b"c".to_vec()),
                ("k".into(), b"d".to_vec()),
            ],
            resizes: vec![(2, 3, 8)],
        };
        let case = recording.case();
        assert_eq!(case.steps.len(), 5);
        assert_eq!(case.steps.get(2), Some(&crate::case::Step::Resize(3, 8)));
        let recorded: Vec<Option<usize>> = (0..=5).map(|s| recording.recorded_step(s)).collect();
        assert_eq!(
            recorded,
            [None, Some(0), Some(1), Some(2), Some(2), Some(3)]
        );
        // Judged: before any step, after each but the resize itself.
        assert_eq!(recording.judged(), [true, true, true, false, true, true]);
    }
}
