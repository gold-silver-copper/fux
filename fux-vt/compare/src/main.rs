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
mod corpus;
mod engine;
mod engines;
mod escape;
mod esctest;
mod families;
mod inventory;
mod record;
mod rng;
mod scoreboard;
mod snapshot;
mod transparency;

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
       fux-vt-compare verdicts [--seed N] [--cases N] [--family NAME]... [--engines LIST]
                               [--no-reflow]
       fux-vt-compare replay [--engines LIST] [--size RxC] [--history N] [--no-reflow]
                             [--newline-before-resize] STEP...
       fux-vt-compare bench [--engines LIST] [--mb N] [WORKLOAD...]
       fux-vt-compare record --keys FILE --out PREFIX [--size RxC] [--program NAME]
                             [--version TEXT] [--env KEY=VALUE]... [--dir DIR]
                             [--scrub OLD=NEW]... [--note TEXT] -- PROGRAM ARGS...
       fux-vt-compare corpus [--engines LIST] [--show] [--json FILE] [NAME...]
       fux-vt-compare inventory [NAME...]
       fux-vt-compare transparency [--engines LIST] [--chunk N] [--json FILE]
                                   [--multiplexers] [NAME... | --size RxC STEP...]
       fux-vt-compare esctest [--in-fux] [--subset] [FILTER] (esctest --help: the rest)
       fux-vt-compare scoreboard DIR
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
         here. Exit 1 if a case in a family expected to agree fails, or
         the engines that decide a family with a recorded verdict (xterm,
         for most) outvote fux-vt on one of its cases.
verdicts the families with a recorded verdict (or those named), each
         beside the engines that decide it (default; --list names them):
         their named cases, then random cases from each with plain text
         (100 each with a verdict, by default). Exit 1 if they outvote
         fux-vt on any.
replay   one case: STEP is output, written as `run` prints it ('\\e[1mX'),
         or resize:RxC. Prints each engine's verdict after every step, and
         the screens.
bench    each engine's speed on the same workloads, in MB/s (default: the
         synthetic ones and the corpus all together; `corpus` adds each
         recording alone).
record   runs PROGRAM on a PTY (--size, else 40x120) as a pane of fux
         runs it, types each line of keys in FILE (a line `!resize RxC`
         resizes it instead), and keeps every byte it writes (PREFIX.bin)
         and what was run and typed (PREFIX.json).
         fux-vt answers its queries as fux does. Its environment is TERM
         and the --env pairs alone (and PATH, if they have none). --scrub
         replaces OLD in the output before it is saved, for what the setup
         cannot keep out (a host name).
corpus   the recordings in corpus/ (default: all), each replayed through
         fux-vt beside the engines (default: xterm and the panel),
         compared after every step. xterm decides the fields it can tell;
         the panel's vote the rest, and all once xterm abstains. Exit 1 if
         a recording expected to agree does not. --show prints fux-vt's
         screen at the end of each; --json writes the results to FILE.
inventory every sequence the recordings (default: all) send, normalized,
         with how often, from which programs, and what fux-vt does with
         it, as Markdown (corpus/INVENTORY.md is its output).
transparency
         each recording (default: all) directly and through fux, used as
         a library as its server runs one client showing one pane, both
         read by one engine (default: ghostty; any in process): the pane's
         rectangle of the client's screen against the direct screen,
         after each step and at each frame of synchronized output
         (--chunk N: every N bytes too). Exit 1 if any differs but as
         recorded. --size: one replay of the STEPs, as replay takes them.
         --multiplexers: the same through tmux and zellij (if installed),
         each a server of its own with a client on a PTY, as a score.
         --json writes the results to FILE.
esctest  xterm's conformance suite, esctest2, against fux-vt set up as fux's
         panes are, and with --in-fux in a real fux pane too. Exit 1 if a
         test fails that esctest-expected.txt does not list, or one listed
         passes.
scoreboard
         the results run.sh quick, full, deep and fuzz left in DIR,
         gathered into DIR/scoreboard.json and DIR/scoreboard.md.
engines  every engine: whether it can run here, whether it votes, and what
         it cannot tell.
--list   the families, what each covers, and its status.

LIST is engine names joined by commas, or `panel` (the default for run,
survey, matrix and replay: the voters that can run here), `all` (every
engine that can run here, the default for cases and bench), `in-process`
or `xterm`.

fux-vt is set up as fux and ratty set it up (reflow, an identity), with the
DECRQM answers, in-band resize, colour-scheme reports, the kitty keyboard
protocol, hyperlinks and prompt marks fux's panes have; --no-reflow without
reflow, fux-vt's default, which leaves out the families that need reflow.";

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
    show: bool,
    /// Whether `--size` was given.
    sized: bool,
    /// `record`'s own options.
    out: Option<String>,
    keys: Option<String>,
    program: Option<String>,
    version: String,
    env: Vec<String>,
    dir: Option<String>,
    scrub: Vec<String>,
    note: String,
    /// Where `corpus` writes its results.
    json: Option<String>,
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
        show: false,
        sized: false,
        out: None,
        keys: None,
        program: None,
        version: String::new(),
        env: Vec::new(),
        dir: None,
        scrub: Vec::new(),
        note: String::new(),
        json: None,
        rest: Vec::new(),
    };
    let mut words = std::env::args().skip(1).peekable();
    if let Some(first) = words.peek()
        && [
            "run",
            "survey",
            "matrix",
            "cases",
            "verdicts",
            "replay",
            "bench",
            "engines",
            "record",
            "corpus",
            "inventory",
            "scoreboard",
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
            "--show" => args.show = true,
            "--engines" => args.engines = Some(value("--engines")?),
            "--no-reflow" => args.reflow = false,
            "--newline-before-resize" => args.newline_before_resize = true,
            "--size" => {
                args.size = dimensions(&value("--size")?)?;
                args.sized = true;
            }
            "--history" => args.history = number("--history", &value("--history")?)?,
            "--mb" => args.mb = number("--mb", &value("--mb")?)?,
            "--out" => args.out = Some(value("--out")?),
            "--keys" => args.keys = Some(value("--keys")?),
            "--program" => args.program = Some(value("--program")?),
            "--version" => args.version = value("--version")?,
            "--env" => args.env.push(value("--env")?),
            "--dir" => args.dir = Some(value("--dir")?),
            "--scrub" => args.scrub.push(value("--scrub")?),
            "--note" => args.note = value("--note")?,
            "--json" => args.json = Some(value("--json")?),
            "--" => {
                args.rest.extend(words.by_ref());
                break;
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
            Status::Decided { why, by } => format!("DECIDED by {}: {why}", by.join(", ")),
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
        let unlike = kind.blanks.unlike_xterm();
        if !unlike.is_empty() {
            println!(
                "           blanks unlike xterm's (no vote on a blank's): {}",
                unlike.join(", ")
            );
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
            Status::Decided { .. } => "decided",
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

/// Each engine's verdict, as a line of marks: `+` agrees, `-` differs, `!`
/// abstains (it failed or panicked).
fn marks(outcome: &case::Outcome) -> String {
    outcome
        .verdicts
        .iter()
        .map(|v| {
            let name = ENGINES.get(v.engine).map_or("?", |k| k.name);
            let mark = if v.differences.is_empty() { '+' } else { '-' };
            format!("{mark}{name}")
        })
        .chain(
            outcome
                .abstained
                .iter()
                .map(|(index, _)| format!("!{}", ENGINES.get(*index).map_or("?", |k| k.name))),
        )
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
        let status = FAMILIES.get(index).map(|f| f.status);
        let marks = marks(&outcome);
        if let Some(Status::Decided { by, .. }) = status {
            let deciders = match by {
                [one] => format!("{one} decides"),
                _ => format!("{} decide", by.join(", ")),
            };
            match deciders_agree(&outcome, by) {
                Some(true) | None => {
                    agree = agree.saturating_add(1);
                    println!("ok       {name} (family {family}: {deciders})   {marks}");
                }
                Some(false) => {
                    ok = false;
                    differ = differ.saturating_add(1);
                    println!(
                        "FAIL     {name} (family {family}: {deciders}, against fux-vt)   {marks}"
                    );
                    print!("{}", case::report(&case, &panel, &outcome));
                }
            }
            continue;
        }
        let expected = status == Some(Status::Agree);
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
        "{agree} agree in families expected to agree or decided, {fixed} agree in families that differ elsewhere, {differ} fail"
    );
    Ok(ok)
}

/// Whether the engines named in `by` agree with fux-vt in `outcome`, by
/// their vote alone (one engine alone outvotes fux-vt wherever it differs);
/// none if none of them ran or all abstained.
fn deciders_agree(outcome: &case::Outcome, by: &[&str]) -> Option<bool> {
    let deciders: Vec<case::Verdict> = outcome
        .verdicts
        .iter()
        .filter(|v| ENGINES.get(v.engine).is_some_and(|k| by.contains(&k.name)))
        .cloned()
        .collect();
    (!deciders.is_empty()).then(|| case::outvoted_on(&deciders).is_empty())
}

/// The families with a recorded verdict, each beside the engines that
/// decide it (or `--engines`): their named cases at the end of each, then
/// random cases from each with plain text, where the deciding engines' vote
/// decides (xterm alone outvotes fux-vt wherever it differs).
fn verdicts(args: &Args) -> Result<bool, String> {
    let text = families::find("text").ok_or("no text family")?;
    let chosen: Vec<usize> = if args.families.is_empty() {
        (0..FAMILIES.len())
            .filter(|&i| {
                usable(i, args.reflow)
                    && FAMILIES
                        .get(i)
                        .is_some_and(|f| matches!(f.status, Status::Decided { .. }))
            })
            .collect()
    } else {
        chosen_families(args)?
    };
    let count = args.cases.unwrap_or(100);
    let mut ok = true;
    for &i in &chosen {
        let f = FAMILIES.get(i).ok_or("no such family")?;
        let panel = match (args.engines.as_deref(), f.status) {
            (Some(list), _) => engines(list)?,
            (None, Status::Decided { by, .. }) => engines(&by.join(","))?,
            (None, Status::Agree | Status::Differs(_)) => engines("xterm")?,
        };
        println!("== {} (beside {})", f.name, names(&panel));
        for &(name, family, (rows, cols), words) in cases::CASES {
            if family != f.name {
                continue;
            }
            let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
            let steps = steps(&words, i)?;
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
            let marks = marks(&outcome);
            if outcome.agrees() {
                println!("   ok    {name}   {marks}");
            } else {
                ok = false;
                println!("   FAIL  {name}   {marks}");
                print!("{}", case::report(&case, &panel, &outcome));
            }
        }
        let set: Vec<usize> = if i == text { vec![i] } else { vec![i, text] };
        let mut r = Rng::new(args.seed);
        let (mut judged, mut failed, mut unheard) = (0usize, 0usize, 0usize);
        let mut shown = false;
        // Cases xterm abstains from (SGR 58) are drawn again, up to ten
        // times as many as asked for.
        while judged < count && judged.saturating_add(unheard) < count.saturating_mul(10) {
            let case = case::random(&mut r, &set, args.reflow);
            let outcome = case.run(&panel)?;
            if outcome.verdicts.is_empty() {
                unheard = unheard.saturating_add(1);
                continue;
            }
            judged = judged.saturating_add(1);
            if outcome.agrees() {
                continue;
            }
            failed = failed.saturating_add(1);
            if !shown {
                shown = true;
                let small = case.shrink(&panel, 4000);
                print!("{}", case::report(&small, &panel, &small.run(&panel)?));
            }
        }
        ok &= failed == 0 && judged > 0;
        println!(
            "   {failed} of {judged} random cases failed (seed {}); {unheard} more had no verdict",
            args.seed
        );
    }
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

fn record(args: &Args) -> Result<bool, String> {
    let program = args.rest.first().ok_or("record: no program to run")?;
    let (rows, cols) = if args.sized { args.size } else { (40, 120) };
    let request = record::Request {
        rows,
        cols,
        out: args.out.as_ref().ok_or("record needs --out")?.into(),
        program: args.program.clone().unwrap_or_else(|| program.clone()),
        version: args.version.clone(),
        env: args.env.clone(),
        dir: args.dir.as_ref().map(Into::into),
        keys: args.keys.as_ref().ok_or("record needs --keys")?.into(),
        scrub: args.scrub.clone(),
        note: args.note.clone(),
        argv: args.rest.clone(),
    };
    let done = record::record(&request)?;
    println!(
        "{}: {} bytes in {} steps, {} bytes of replies",
        request.out.display(),
        done.bytes,
        done.steps,
        done.replies
    );
    Ok(true)
}

fn main() -> ExitCode {
    // An engine's panic is caught and costs it its vote (`case::guarded`),
    // and reported there; the default hook would print each one again.
    std::panic::set_hook(Box::new(|_| {}));
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some((first, rest)) = argv.split_first()
        && first == record::LAUNCH
    {
        let error = record::launched(rest).err().unwrap_or_default();
        eprintln!("fux-vt-compare: {error}");
        return ExitCode::from(127);
    }
    if let Some((first, rest)) = argv.split_first()
        && first == "transparency"
    {
        // Its options are its own (see `transparency::run`).
        return match transparency::run(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("fux-vt-compare: {e}\n\n{USAGE}");
                ExitCode::from(2)
            }
        };
    }
    if let Some((first, rest)) = argv.split_first()
        && first == "esctest"
    {
        return match esctest::run(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("fux-vt-compare esctest: {e}");
                ExitCode::from(2)
            }
        };
    }
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
        "verdicts" => verdicts(&args),
        "replay" => replay(&args),
        "record" => record(&args),
        "inventory" => inventory::run(&args.rest),
        "scoreboard" => scoreboard::run(&args.rest),
        "corpus" => corpus::run(
            &panel(&args, "xterm,panel")?,
            &args.rest,
            args.show,
            args.json.as_deref(),
        ),
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
