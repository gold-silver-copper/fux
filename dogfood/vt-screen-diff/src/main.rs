//! `vt-screen-diff [--size ROWSxCOLS] [--first] LEFT RIGHT`: replays two
//! terminal recordings through fux-vt and says how their final screens
//! differ: size, which screen is shown, cursor, title, and each row's
//! cells, by text and attributes. With `--first`, also the first step after
//! which their screens' cells differ. Exits 0 when nothing differs, 1 when
//! something does, 2 on trouble, as diff does.
mod recording;

use fux_vt::{Attributes, CellRef, Color, Feature, Mode, Options, Parser, Screen, Size};
use recording::{Recording, Step, What};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::process::ExitCode;

const USAGE: &str = "usage: vt-screen-diff [--size ROWSxCOLS] [--first] LEFT RIGHT\n\
    LEFT and RIGHT are asciinema casts (v2, v3) or raw terminal output.\n\
    --size  the terminal's size where a recording does not say (default 24x80)\n\
    --first also report the first step after which the screens differ\n";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(differ) => ExitCode::from(u8::from(differ)),
        Err(message) => {
            eprintln!("vt-screen-diff: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<bool, String> {
    let mut size = None;
    let mut first = false;
    let mut files = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(false);
            }
            "--first" => first = true,
            "--size" => {
                let value = args.next().ok_or("--size needs ROWSxCOLS")?;
                let (rows, cols) = value.split_once('x').ok_or("--size needs ROWSxCOLS")?;
                let number = |s: &str| s.parse::<f64>().map_err(|e| format!("--size {value}: {e}"));
                size = Some(recording::size(number(rows)?, number(cols)?)?);
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ => files.push(arg),
        }
    }
    let [left, right] = files.as_slice() else {
        return Err(format!("two recordings are needed\n{USAGE}"));
    };
    let read = |path: &String| {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        recording::parse(&bytes).map_err(|e| format!("{path}: {e}"))
    };
    let (left_rec, right_rec) = (read(left)?, read(right)?);
    let fallback = size
        .or_else(|| Size::new(24, 80).ok())
        .ok_or("no default size")?;
    let start = |r: &Recording| Term::new(size.or(r.size).unwrap_or(fallback));
    let (mut a, mut b) = (start(&left_rec)?, start(&right_rec)?);
    let mut out = format!("--- {left}\n+++ {right}\n");
    let mut differ = false;
    let mut applied = 0;
    if first {
        let found = diverge(&mut a, &mut b, &left_rec.steps, &right_rec.steps);
        applied = found
            .as_ref()
            .map_or(usize::MAX, |(step, _)| step.map_or(0, |s| s + 1));
        match found {
            Some((step, report)) => {
                differ = true;
                let at = |s: Option<&Step>| match s {
                    Some(Step {
                        time: Some(t),
                        offset,
                        ..
                    }) => format!("{t:.3}s, byte {offset}"),
                    Some(Step { offset, .. }) => format!("byte {offset}"),
                    None => "ended".to_owned(),
                };
                let _ = match step {
                    Some(step) => writeln!(
                        out,
                        "first differ after step {} (left: {}; right: {}):",
                        step + 1,
                        at(left_rec.steps.get(step)),
                        at(right_rec.steps.get(step))
                    ),
                    None => writeln!(out, "first differ from the start:"),
                };
                out.push_str(&report);
                out.push_str("final screens:\n");
            }
            None => out.push_str("the screens never differ\n"),
        }
    }
    for step in left_rec.steps.get(applied..).unwrap_or_default() {
        a.apply(step);
    }
    for step in right_rec.steps.get(applied..).unwrap_or_default() {
        b.apply(step);
    }
    let report = describe(&a, &b, true);
    differ |= !report.is_empty();
    if report.is_empty() {
        out.push_str("the final screens are the same\n");
    }
    out.push_str(&report);
    for (term, name) in [(&a, left), (&b, right)] {
        if term.parser.refused() > 0 {
            let _ = writeln!(
                out,
                "warning: {name}: the terminal was refused room {} times",
                term.parser.refused()
            );
        }
    }
    print!("{out}");
    Ok(differ)
}

/// A recording's terminal. `Feature::Events` reads the title its program
/// sets.
struct Term {
    parser: Parser,
}

impl Term {
    fn new(size: Size) -> Result<Term, String> {
        let parser = Parser::with_options(size, 0, Options::from(Feature::Events))
            .map_err(|e| e.to_string())?;
        Ok(Term { parser })
    }

    fn screen(&self) -> &Screen {
        self.parser.screen()
    }

    /// Carries out one step, as a host does: every byte read, past each
    /// frame's start and end and each budget stop. A refusal of room is
    /// counted by the parser, and the replay goes on.
    fn apply(&mut self, step: &Step) {
        match &step.what {
            What::Output(bytes) => {
                // An error says room was refused in a call that read every
                // byte. Replies and events go nowhere.
                let _ = self.parser.process_all(bytes, &mut ());
            }
            // A refused resize leaves the screen as it was, and is counted.
            What::Resize(size) => drop(self.parser.resize(*size)),
        }
    }
}

/// Replays both a step at a time until their screens' cells first differ:
/// that step's index (both replayed through it; `None` if they differ
/// before any) and the difference then, or
/// `None` if they never do (both replayed whole).
/// A row whose identity and version, on both sides, were a pair found the
/// same before is the same still: a row's version changes with every edit
/// to it, and a scroll moves rows without editing them. So each step
/// compares only the rows edited, or moved into a new pairing.
fn diverge(
    a: &mut Term,
    b: &mut Term,
    left: &[Step],
    right: &[Step],
) -> Option<(Option<usize>, String)> {
    let mut same_pairs = HashSet::new();
    for step in std::iter::once(None).chain((0..left.len().max(right.len())).map(Some)) {
        if let Some(step) = step {
            if let Some(s) = left.get(step) {
                a.apply(s);
            }
            if let Some(s) = right.get(step) {
                b.apply(s);
            }
        }
        let (sa, sb) = (a.screen(), b.screen());
        let key = |row: Option<fux_vt::Row<'_>>| row.map(|r| (r.id(), r.version()));
        let pairs: Vec<_> = (0..sa.size().rows().max(sb.size().rows()))
            .map(|row| (row, (key(sa.window().row(row)), key(sb.window().row(row)))))
            .collect();
        let unknown = pairs
            .iter()
            .filter(|(_, pair)| !same_pairs.contains(pair))
            .map(|&(row, _)| row);
        if rows_differ(a, b, unknown) {
            return Some((step, describe(a, b, false)));
        }
        same_pairs = pairs.into_iter().map(|(_, pair)| pair).collect();
    }
    None
}

fn rows_differ(a: &Term, b: &Term, mut rows: impl Iterator<Item = u16>) -> bool {
    a.screen().size() != b.screen().size()
        || a.screen().mode(Mode::AlternateScreen) != b.screen().mode(Mode::AlternateScreen)
        || rows.any(|row| {
            let (ra, rb) = (a.screen().window().row(row), b.screen().window().row(row));
            match (ra, rb) {
                (Some(ra), Some(rb)) => {
                    ra.len() != rb.len()
                        || ra
                            .cells()
                            .zip(rb.cells())
                            .any(|(ca, cb)| !same(Some(ca), Some(cb)))
                }
                (ra, rb) => ra.is_some() != rb.is_some(),
            }
        })
}

fn same(a: Option<CellRef<'_>>, b: Option<CellRef<'_>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            shown(Some(a)) == shown(Some(b))
                && a.is_wide() == b.is_wide()
                && a.is_wide_continuation() == b.is_wide_continuation()
                && a.attributes() == b.attributes()
        }
        (None, None) => true,
        _ => false,
    }
}

/// How the two screens differ, as text; empty if they don't. The cursor
/// and title are told only with `state`: a screen mid-way has its cursor
/// mid-line.
fn describe(a: &Term, b: &Term, state: bool) -> String {
    let (sa, sb) = (a.screen(), b.screen());
    let mut out = String::new();
    let pair = |out: &mut String, what: &str, l: String, r: String| {
        if l != r {
            let _ = writeln!(out, "{what}: {l} | {r}");
        }
    };
    let size = |s: &Screen| format!("{}x{}", s.size().rows(), s.size().cols());
    pair(&mut out, "size", size(sa), size(sb));
    let shown = |s: &Screen| {
        if s.mode(Mode::AlternateScreen) {
            "alternate"
        } else {
            "main"
        }
        .to_owned()
    };
    pair(&mut out, "screen", shown(sa), shown(sb));
    if state {
        let cursor = |s: &Screen| {
            let (row, col) = s.cursor_position();
            format!("row {row} col {col}")
        };
        pair(&mut out, "cursor", cursor(sa), cursor(sb));
        pair(
            &mut out,
            "title",
            format!("{:?}", String::from_utf8_lossy(sa.title().bytes)),
            format!("{:?}", String::from_utf8_lossy(sb.title().bytes)),
        );
    }
    for row in 0..sa.size().rows().max(sb.size().rows()) {
        let (ra, rb) = (sa.window().row(row), sb.window().row(row));
        let cols = ra.map_or(0, |r| r.len()).max(rb.map_or(0, |r| r.len()));
        // Runs of differing cells, merged while each side's attributes stay.
        let mut runs: Vec<Run> = Vec::new();
        for col in 0..cols {
            let (ca, cb) = (ra.and_then(|r| r.cell(col)), rb.and_then(|r| r.cell(col)));
            if same(ca, cb) {
                continue;
            }
            let (ta, aa, tb, ab) = (text(ca), attrs(ca), text(cb), attrs(cb));
            match runs.last_mut() {
                Some(run) if run.end + 1 == col && run.left.1 == aa && run.right.1 == ab => {
                    run.end = col;
                    run.left.0.push_str(&ta);
                    run.right.0.push_str(&tb);
                }
                _ => runs.push(Run {
                    start: col,
                    end: col,
                    left: (ta, aa),
                    right: (tb, ab),
                }),
            }
        }
        if runs.is_empty() {
            continue;
        }
        let line = |r: Option<fux_vt::Row<'_>>| r.map(|r| r.text()).unwrap_or_default();
        let _ = writeln!(
            out,
            "row {row}:\n  - {}\n  + {}",
            line(ra).trim_end(),
            line(rb).trim_end()
        );
        for run in runs {
            let cols = if run.start == run.end {
                format!("col {}", run.start)
            } else {
                format!("cols {}-{}", run.start, run.end)
            };
            let side = |(t, a): &(String, String)| {
                if a.is_empty() {
                    format!("{t:?}")
                } else {
                    format!("{t:?} [{a}]")
                }
            };
            let _ = writeln!(out, "  {cols}: {} | {}", side(&run.left), side(&run.right));
        }
    }
    out
}

struct Run {
    start: usize,
    end: usize,
    left: (String, String),
    right: (String, String),
}

/// A cell's text as it reads: a blank is a space, as a printed space is, a wide glyph's second
/// half nothing, and a cell past a row's end `∅`.
fn text(cell: Option<CellRef<'_>>) -> String {
    shown(cell).to_owned()
}

fn shown<'a>(cell: Option<CellRef<'a>>) -> &'a str {
    match cell {
        None => "∅",
        Some(c) if c.is_wide_continuation() => "",
        Some(c) if c.contents().is_empty() => " ",
        Some(c) => c.contents(),
    }
}

/// A cell's attributes as words, the default ones left out.
fn attrs(cell: Option<CellRef<'_>>) -> String {
    let Some(cell) = cell else {
        return String::new();
    };
    let a: Attributes = cell.attributes();
    let mut words: Vec<String> = a.sgr_flags().map(|(_, name)| name.to_owned()).collect();
    let color = |c: Color| match c {
        Color::Default => None,
        Color::Idx(i) => Some(i.to_string()),
        Color::Rgb(rgb) => Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)),
    };
    words.extend(color(a.foreground()).map(|c| format!("fg={c}")));
    words.extend(color(a.background()).map(|c| format!("bg={c}")));
    if a.underline() {
        words.push(format!("underline={:?}", a.underline_style()).to_lowercase());
    }
    words.extend(color(a.underline_color()).map(|c| format!("ul={c}")));
    if a.blink() != fux_vt::Blink::None {
        words.push(format!("blink={:?}", a.blink()).to_lowercase());
    }
    if cell.is_wide() {
        words.push("wide".to_owned());
    }
    words.join(" ")
}
