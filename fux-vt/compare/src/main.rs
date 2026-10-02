//! `fux-vt-compare`: fux-vt beside other terminal emulators. The same
//! output goes to fux-vt and to a panel of engines, and after every piece
//! of it, and every resize, each engine's screen is compared with fux-vt's
//! cell by cell (text, width, every attribute and colour), with the cursor,
//! the modes, soft wraps, the title, cursor reports and recent history.
//! fux-vt fails a case where most of the panel differs from it. A failing
//! case is shrunk to the smallest that still fails, and printed with the
//! command that replays it. `bench` times the engines on the same output.
mod bench;
mod case;
mod cases;
mod engine;
mod engines;
mod escape;
mod families;
mod rng;
mod snapshot;

use case::{Case, Snippet, Step};
use engine::ENGINES;
use families::{FAMILIES, Status};
use rng::Rng;
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
usage: fux-vt-compare [run] [--seed N] [--cases N] [--family NAME]... [--all]
                      [--engines LIST] [--no-reflow]
       fux-vt-compare survey [--seed N] [--cases N] [--engines LIST] [--no-reflow]
       fux-vt-compare matrix [--seed N] [--cases N] [--engines LIST] [--no-reflow]
       fux-vt-compare cases [--engines LIST] [--no-reflow] [NAME...]
       fux-vt-compare replay [--engines LIST] [--size RxC] [--history N] [--no-reflow]
                             [--newline-before-resize] STEP...
       fux-vt-compare bench [--engines LIST] [--mb N] [WORKLOAD...]
       fux-vt-compare engines
       fux-vt-compare --list

run      random cases from the families expected to agree (or those named,
         or --all). A case fails where most of the panel differs from
         fux-vt; every failure is shrunk and printed. Exit 1 if any.
survey   each family alone (with plain text), counting the cases that
         fail, and the smallest of the first: how to see what a family's
         status should be.
matrix   each family alone beside each engine alone, and beside the whole
         panel: the share of cases that differ, as a table.
cases    the named cases (default: all), beside every engine that can run
         here. Exit 1 if a case in a family expected to agree fails.
replay   one case: STEP is output, written as `run` prints it ('\\e[1mX'),
         or resize:RxC. Prints each engine's verdict after every step, and
         the screens.
bench    each engine's speed on the same workloads (default: all), in MB/s.
engines  every engine: whether it can run here, whether it votes, and what
         it cannot tell.
--list   the families, what each covers, and its status.

LIST is engine names joined by commas, or `panel` (the default for run,
survey, matrix and replay: the voters that can run here), `all` (every
engine that can run here, the default for cases and bench) or
`in-process`.

fux-vt is set up as ratty sets it up (reflow, an identity, the kitty
keyboard protocol); --no-reflow sets it up as fux does, which leaves out
the families that need ratty's setup.";

struct Args {
    command: String,
    seed: u64,
    cases: Option<usize>,
    families: Vec<String>,
    all: bool,
    engines: Option<String>,
    reflow: bool,
    size: (u16, u16),
    history: usize,
    newline_before_resize: bool,
    mb: usize,
    rest: Vec<String>,
}

fn number<T: std::str::FromStr>(name: &str, text: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    text.parse().map_err(|e| format!("{name}: {e}"))
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        command: "run".into(),
        seed: 1,
        cases: None,
        families: Vec::new(),
        all: false,
        engines: None,
        reflow: true,
        size: (6, 20),
        history: 0,
        newline_before_resize: false,
        mb: 8,
        rest: Vec::new(),
    };
    let mut words = std::env::args().skip(1).peekable();
    if let Some(first) = words.peek()
        && [
            "run", "survey", "matrix", "cases", "replay", "bench", "engines",
        ]
        .contains(&first.as_str())
    {
        args.command = first.clone();
        words.next();
    }
    while let Some(word) = words.next() {
        let mut value = |name: &str| words.next().ok_or(format!("{name} needs a value"));
        match word.as_str() {
            "--seed" => args.seed = number("--seed", &value("--seed")?)?,
            "--cases" => args.cases = Some(number("--cases", &value("--cases")?)?),
            "--family" => args.families.push(value("--family")?),
            "--all" => args.all = true,
            "--engines" => args.engines = Some(value("--engines")?),
            "--no-reflow" => args.reflow = false,
            "--newline-before-resize" => args.newline_before_resize = true,
            "--size" => args.size = dimensions(&value("--size")?)?,
            "--history" => args.history = number("--history", &value("--history")?)?,
            "--mb" => args.mb = number("--mb", &value("--mb")?)?,
            "--list" => args.command = "list".into(),
            "-h" | "--help" => args.command = "help".into(),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            _ => args.rest.push(word),
        }
    }
    Ok(args)
}

fn dimensions(text: &str) -> Result<(u16, u16), String> {
    let (rows, cols) = text.split_once('x').ok_or(format!("{text:?} is not RxC"))?;
    let n = |s: &str| s.parse::<u16>().map_err(|e| format!("{text:?}: {e}"));
    Ok((n(rows)?, n(cols)?))
}

/// The engines a LIST names, as indices into [`ENGINES`]. Named engines
/// must be able to run; `panel`, `all` and `in-process` skip those that
/// cannot, saying so once.
fn engines(list: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    let pick = |test: fn(&engine::Kind) -> bool, out: &mut Vec<usize>| {
        for (i, kind) in ENGINES.iter().enumerate() {
            if !test(kind) {
                continue;
            }
            match (kind.available)() {
                Ok(()) => out.push(i),
                Err(why) => eprintln!("({} cannot run here: {why})", kind.name),
            }
        }
    };
    for name in list.split(',').filter(|n| !n.is_empty()) {
        match name {
            "panel" => pick(|k| k.panel, &mut out),
            "all" => pick(|_| true, &mut out),
            "in-process" => pick(|k| k.in_process, &mut out),
            name => {
                let i = engine::find(name).ok_or(format!("no engine {name:?} (see engines)"))?;
                let kind = ENGINES.get(i).ok_or("no such engine")?;
                (kind.available)().map_err(|why| format!("{name} cannot run here: {why}"))?;
                out.push(i);
            }
        }
    }
    out.dedup();
    if out.is_empty() {
        return Err(format!("no engine in {list:?} can run here"));
    }
    Ok(out)
}

fn panel(args: &Args, default: &str) -> Result<Vec<usize>, String> {
    engines(args.engines.as_deref().unwrap_or(default))
}

fn names(panel: &[usize]) -> String {
    panel
        .iter()
        .filter_map(|&i| ENGINES.get(i).map(|k| k.name))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a family runs with this setup: the ratty-only ones need reflow.
fn usable(index: usize, reflow: bool) -> bool {
    FAMILIES.get(index).is_some_and(|f| reflow || !f.ratty_only)
}

fn list() {
    for f in FAMILIES {
        let status = match f.status {
            Status::Agree => "agree".to_owned(),
            Status::Differs(why) => format!("DIFFER: {why}"),
        };
        let ratty = if f.ratty_only { " [needs reflow]" } else { "" };
        println!("{:<13} {}{ratty}\n              {status}", f.name, f.about);
    }
}

fn list_engines() {
    for kind in std::iter::once(&engine::SUBJECT).chain(ENGINES) {
        let here = match (kind.available)() {
            Ok(()) => "runs here".to_owned(),
            Err(why) => format!("cannot run here: {why}"),
        };
        let role = if kind.name == engine::SUBJECT.name {
            "the subject"
        } else if kind.panel {
            "votes"
        } else {
            "on request"
        };
        let place = if kind.in_process {
            "in process"
        } else {
            "own process"
        };
        println!("{:<10} {} ({role}, {place}; {here})", kind.name, kind.about);
        let missing = kind.can.missing();
        if !missing.is_empty() {
            println!("           cannot tell: {}", missing.join(", "));
        }
    }
}

/// Runs `count` random cases from `families` beside `panel`; returns how
/// many failed, after printing the first few, shrunk.
fn random(
    args: &Args,
    panel: &[usize],
    families: &[usize],
    count: usize,
    seed: u64,
    show: usize,
) -> Result<usize, String> {
    let mut r = Rng::new(seed);
    let mut shown: Vec<Case> = Vec::new();
    let mut failed = 0usize;
    for _ in 0..count {
        let case = case::random(&mut r, families, args.reflow);
        let outcome = case.run(panel)?;
        if outcome.agrees() {
            continue;
        }
        failed = failed.saturating_add(1);
        if shown.len() >= show {
            continue;
        }
        let small = case.shrink(panel, 4000);
        if shown.contains(&small) {
            continue;
        }
        let outcome = small.run(panel)?;
        print!("{}", case::report(&small, panel, &outcome));
        shown.push(small);
    }
    Ok(failed)
}

fn chosen_families(args: &Args) -> Result<Vec<usize>, String> {
    if args.all {
        Ok((0..FAMILIES.len())
            .filter(|&i| usable(i, args.reflow))
            .collect())
    } else if args.families.is_empty() {
        Ok((0..FAMILIES.len())
            .filter(|&i| {
                usable(i, args.reflow) && FAMILIES.get(i).is_some_and(|f| f.status == Status::Agree)
            })
            .collect())
    } else {
        args.families
            .iter()
            .map(|name| families::find(name).ok_or(format!("no family {name:?} (see --list)")))
            .collect()
    }
}

fn run(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "panel")?;
    let families = chosen_families(args)?;
    let family_names: Vec<&str> = families
        .iter()
        .filter_map(|&i| FAMILIES.get(i).map(|f| f.name))
        .collect();
    println!("engines: {}", names(&panel));
    println!("families: {}", family_names.join(" "));
    let count = args.cases.unwrap_or(20_000);
    let started = Instant::now();
    let failed = random(args, &panel, &families, count, args.seed, 5)?;
    println!(
        "{failed} of {count} cases failed (seed {}, {:.1}s)",
        args.seed,
        started.elapsed().as_secs_f64()
    );
    Ok(failed == 0)
}

fn survey(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "panel")?;
    println!("engines: {}", names(&panel));
    let text = families::find("text").ok_or("no text family")?;
    let count = args.cases.unwrap_or(300);
    for (i, f) in FAMILIES.iter().enumerate() {
        if !usable(i, args.reflow) {
            continue;
        }
        let set: Vec<usize> = if i == text { vec![i] } else { vec![i, text] };
        let status = match f.status {
            Status::Agree => "agree",
            Status::Differs(_) => "differs",
        };
        println!("== {} (expected: {status})", f.name);
        let failed = random(args, &panel, &set, count, args.seed, 1)?;
        println!("   {failed} of {count} failed");
    }
    Ok(true)
}

/// The share of `count` random cases from `families` that fail beside
/// `panel`, in percent.
fn share(args: &Args, panel: &[usize], families: &[usize], count: usize) -> Result<usize, String> {
    let mut r = Rng::new(args.seed);
    let mut failed = 0usize;
    for _ in 0..count {
        let case = case::random(&mut r, families, args.reflow);
        if !case.run(panel)?.agrees() {
            failed = failed.saturating_add(1);
        }
    }
    Ok(failed.saturating_mul(100).checked_div(count).unwrap_or(0))
}

fn matrix(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "panel")?;
    let text = families::find("text").ok_or("no text family")?;
    let count = args.cases.unwrap_or(200);
    print!("{:<13}", "% differing");
    for &e in &panel {
        print!(" {:>9}", ENGINES.get(e).map_or("?", |k| k.name));
    }
    println!(" {:>9}", "outvoted");
    for (i, f) in FAMILIES.iter().enumerate() {
        if !usable(i, args.reflow) {
            continue;
        }
        let set: Vec<usize> = if i == text { vec![i] } else { vec![i, text] };
        print!("{:<13}", f.name);
        for &e in &panel {
            print!(" {:>9}", share(args, &[e], &set, count)?);
        }
        println!(" {:>9}", share(args, &panel, &set, count)?);
    }
    println!("({count} cases per cell, seed {})", args.seed);
    Ok(true)
}

fn steps(words: &[String], family: usize) -> Result<Vec<Step>, String> {
    words
        .iter()
        .map(|word| match word.strip_prefix("resize:") {
            Some(size) => dimensions(size).map(|(r, c)| Step::Resize(r, c)),
            None => {
                escape::unescape(word).map(|bytes| Step::Output(vec![Snippet { family, bytes }]))
            }
        })
        .collect()
}

/// Each engine's verdict, as a line of marks: `+` agrees, `-` differs.
fn marks(outcome: &case::Outcome) -> String {
    outcome
        .verdicts
        .iter()
        .map(|v| {
            let name = ENGINES.get(v.engine).map_or("?", |k| k.name);
            let mark = if v.differences.is_empty() { '+' } else { '-' };
            format!("{mark}{name}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cases(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "all")?;
    println!("engines: {}", names(&panel));
    let mut ok = true;
    let (mut agree, mut differ, mut fixed) = (0usize, 0usize, 0usize);
    for &(name, family, (rows, cols), words) in cases::CASES {
        if !args.rest.is_empty() && !args.rest.iter().any(|n| n == name) {
            continue;
        }
        let index = families::find(family).ok_or(format!("case {name}: no family {family}"))?;
        if !usable(index, args.reflow) {
            continue;
        }
        let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
        let steps = steps(&words, index)?;
        let resizes = steps.iter().any(|s| matches!(s, Step::Resize(..)));
        let case = Case {
            rows,
            cols,
            history: if resizes { case::RESIZE_HISTORY } else { 0 },
            reflow: args.reflow,
            newline_before_resize: false,
            steps,
        };
        let outcome = case.run_until(&panel, false)?;
        let expected = FAMILIES
            .get(index)
            .is_some_and(|f| f.status == Status::Agree);
        let marks = marks(&outcome);
        match (outcome.agrees(), expected) {
            (true, true) => {
                agree = agree.saturating_add(1);
                println!("ok       {name}   {marks}");
            }
            (true, false) => {
                fixed = fixed.saturating_add(1);
                println!("agrees   {name} (family {family} differs elsewhere)   {marks}");
            }
            (false, expect) => {
                differ = differ.saturating_add(1);
                if expect {
                    ok = false;
                    println!("FAIL     {name} (family {family} is expected to agree)   {marks}");
                } else {
                    println!("fails    {name} (family {family})   {marks}");
                }
                print!("{}", case::report(&case, &panel, &outcome));
            }
        }
    }
    println!(
        "{agree} agree in families expected to agree, {fixed} agree in families that differ elsewhere, {differ} fail"
    );
    Ok(ok)
}

fn replay(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "panel")?;
    let all = steps(&args.rest, usize::MAX)?;
    let mut case = Case {
        rows: args.size.0,
        cols: args.size.1,
        history: args.history,
        reflow: args.reflow,
        newline_before_resize: args.newline_before_resize,
        steps: Vec::new(),
    };
    let mut agrees = true;
    let total = all.len();
    for (i, step) in all.into_iter().enumerate() {
        case.steps.push(step);
        let outcome = case.run_until(&panel, false)?;
        let verdict = if outcome.agrees() { "passes" } else { "FAILS" };
        agrees &= outcome.agrees();
        println!(
            "step {}: {verdict}, {} of {} differ   {}",
            i.saturating_add(1),
            outcome.differing(),
            outcome.verdicts.len(),
            marks(&outcome)
        );
        if i.saturating_add(1) == total {
            print!("{}", case::report(&case, &panel, &outcome));
        }
    }
    Ok(agrees)
}

fn main() -> ExitCode {
    let result = parse().and_then(|args| match args.command.as_str() {
        "list" => {
            list();
            Ok(true)
        }
        "engines" => {
            list_engines();
            Ok(true)
        }
        "help" => {
            println!("{USAGE}");
            Ok(true)
        }
        "survey" => survey(&args),
        "matrix" => matrix(&args),
        "cases" => cases(&args),
        "replay" => replay(&args),
        "bench" => bench::run(&panel(&args, "all")?, &args.rest, args.mb),
        _ => run(&args),
    });
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("fux-vt-compare: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
