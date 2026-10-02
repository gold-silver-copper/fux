//! `fux-vt-ghostty`: fux-vt beside Ghostty's terminal core. The same output
//! goes to both, and after every piece of it, and every resize, their
//! screens are compared cell by cell (text, width, every attribute and
//! colour), with the cursor, the modes both track, soft wraps, the title,
//! cursor reports and recent history. A difference is shrunk to the
//! smallest case that still shows it, and printed with the command that
//! replays it.
mod case;
mod cases;
mod escape;
mod families;
mod ghostty;
mod rng;
mod snapshot;
mod vt;

use case::{Case, Snippet, Step};
use families::{FAMILIES, Status};
use rng::Rng;
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
usage: fux-vt-ghostty [run] [--seed N] [--cases N] [--family NAME]... [--all] [--no-reflow]
       fux-vt-ghostty survey [--seed N] [--cases N] [--no-reflow]
       fux-vt-ghostty cases [--no-reflow] [NAME...]
       fux-vt-ghostty replay [--size RxC] [--history N] [--no-reflow]
                             [--newline-before-resize] STEP...
       fux-vt-ghostty --list

run      random cases from the families expected to agree (or those named,
         or --all); every difference is shrunk and printed. Exit 1 if any.
survey   each family alone (with plain text), counting the cases that
         differ, and the smallest of the first: how to see what a family's
         status should be.
cases    the named cases (default: all). Exit 1 if a case in a family
         expected to agree differs.
replay   one case: STEP is output, written as `run` prints it ('\\e[1mX'),
         or resize:RxC. Prints what differs after each step and both
         screens.
--list   the families, what each covers, and its status.

fux-vt is set up as ratty sets it up (reflow, an identity, the kitty
keyboard protocol); --no-reflow sets it up as fux does, which leaves out
the resize and kitty families.";

struct Args {
    command: String,
    seed: u64,
    cases: usize,
    families: Vec<String>,
    all: bool,
    reflow: bool,
    size: (u16, u16),
    history: usize,
    newline_before_resize: bool,
    rest: Vec<String>,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        command: "run".into(),
        seed: 1,
        cases: 20_000,
        families: Vec::new(),
        all: false,
        reflow: true,
        size: (6, 20),
        history: 0,
        newline_before_resize: false,
        rest: Vec::new(),
    };
    let mut words = std::env::args().skip(1).peekable();
    if let Some(first) = words.peek()
        && ["run", "survey", "cases", "replay"].contains(&first.as_str())
    {
        args.command = first.clone();
        words.next();
    }
    while let Some(word) = words.next() {
        let mut value = |name: &str| words.next().ok_or(format!("{name} needs a value"));
        match word.as_str() {
            "--seed" => {
                args.seed = value("--seed")?
                    .parse()
                    .map_err(|e| format!("--seed: {e}"))?
            }
            "--cases" => {
                args.cases = value("--cases")?
                    .parse()
                    .map_err(|e| format!("--cases: {e}"))?
            }
            "--family" => args.families.push(value("--family")?),
            "--all" => args.all = true,
            "--no-reflow" => args.reflow = false,
            "--newline-before-resize" => args.newline_before_resize = true,
            "--size" => args.size = dimensions(&value("--size")?)?,
            "--history" => {
                args.history = value("--history")?
                    .parse()
                    .map_err(|e| format!("--history: {e}"))?;
            }
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

/// Runs `count` random cases from `families`; returns how many differed,
/// after printing the first few, shrunk.
fn random(
    args: &Args,
    families: &[usize],
    count: usize,
    seed: u64,
    show: usize,
) -> Result<usize, String> {
    let mut r = Rng::new(seed);
    let mut shown: Vec<Case> = Vec::new();
    let mut differed = 0usize;
    for _ in 0..count {
        let case = case::random(&mut r, families, args.reflow);
        let outcome = case.run()?;
        if outcome.agrees() {
            continue;
        }
        differed = differed.saturating_add(1);
        if shown.len() >= show {
            continue;
        }
        let small = case.shrink(4000);
        if shown.contains(&small) {
            continue;
        }
        let outcome = small.run()?;
        print!("{}", case::report(&small, &outcome));
        shown.push(small);
    }
    Ok(differed)
}

fn run(args: &Args) -> Result<bool, String> {
    let families: Vec<usize> = if args.all {
        (0..FAMILIES.len())
            .filter(|&i| usable(i, args.reflow))
            .collect()
    } else if args.families.is_empty() {
        (0..FAMILIES.len())
            .filter(|&i| {
                usable(i, args.reflow) && FAMILIES.get(i).is_some_and(|f| f.status == Status::Agree)
            })
            .collect()
    } else {
        args.families
            .iter()
            .map(|name| families::find(name).ok_or(format!("no family {name:?} (see --list)")))
            .collect::<Result<_, _>>()?
    };
    let names: Vec<&str> = families
        .iter()
        .filter_map(|&i| FAMILIES.get(i).map(|f| f.name))
        .collect();
    println!("families: {}", names.join(" "));
    let started = Instant::now();
    let differed = random(args, &families, args.cases, args.seed, 5)?;
    println!(
        "{} of {} cases differed (seed {}, {:.1}s)",
        differed,
        args.cases,
        args.seed,
        started.elapsed().as_secs_f64()
    );
    Ok(differed == 0)
}

fn survey(args: &Args) -> Result<bool, String> {
    let text = families::find("text").ok_or("no text family")?;
    let count = if args.cases == 20_000 {
        300
    } else {
        args.cases
    };
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
        let differed = random(args, &set, count, args.seed, 1)?;
        println!("   {differed} of {count} differed");
    }
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

fn cases(args: &Args) -> Result<bool, String> {
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
        let outcome = case.run()?;
        let expected = FAMILIES
            .get(index)
            .is_some_and(|f| f.status == Status::Agree);
        match (outcome.agrees(), expected) {
            (true, true) => {
                agree = agree.saturating_add(1);
                println!("ok       {name}");
            }
            (true, false) => {
                fixed = fixed.saturating_add(1);
                println!("agrees   {name} (family {family} differs elsewhere)");
            }
            (false, expect) => {
                differ = differ.saturating_add(1);
                if expect {
                    ok = false;
                    println!("FAIL     {name} (family {family} is expected to agree)");
                } else {
                    println!("differs  {name} (family {family})");
                }
                print!("{}", case::report(&case, &outcome));
            }
        }
    }
    println!(
        "{agree} agree in families expected to agree, {fixed} agree in families that differ elsewhere, {differ} differ"
    );
    Ok(ok)
}

fn replay(args: &Args) -> Result<bool, String> {
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
    for (i, step) in all.into_iter().enumerate() {
        case.steps.push(step);
        let outcome = case.run_until(false)?;
        if outcome.agrees() {
            println!("step {}: agree", i.saturating_add(1));
        } else {
            agrees = false;
            println!("step {}: differ", i.saturating_add(1));
            for line in &outcome.differences {
                println!("  {line}");
            }
        }
        if i.saturating_add(1) == args.rest.len() {
            print!("{}", snapshot::side_by_side(&outcome.fux, &outcome.ghostty));
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
        "help" => {
            println!("{USAGE}");
            Ok(true)
        }
        "survey" => survey(&args),
        "cases" => cases(&args),
        "replay" => replay(&args),
        _ => run(&args),
    });
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("fux-vt-ghostty: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
