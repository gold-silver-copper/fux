//! `fux-vt-compare`: fux-vt beside other terminal emulators. The same
//! output goes to fux-vt and to a panel of engines, and after every piece
//! of it, and every resize, each engine's screen is compared with fux-vt's
//! cell by cell (text, width, every attribute and colour), with the cursor,
//! the modes, soft wraps, the title, cursor reports and recent history.
//! fux-vt fails a case where most of the panel differs from it. A failing
//! case is shrunk to the smallest that still fails, and printed with the
//! command that replays it. `bench` times the engines on the same output.
mod answering;
mod bench;
mod case;
mod cases;
mod corpus;
/// fux-bench's counter, the file itself: one way to count instructions.
#[path = "../../../bench/src/count.rs"]
mod count;
/// `bench/perf.sh`, the wrapper `count` runs perf through.
const PERF_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/perf.sh");
mod encoders;
mod engine;
mod engines;
mod escape;
mod esctest;
mod families;
mod footprint;
mod instructions;
mod inventory;
mod ledger;
mod lists;
mod memory;
mod record;
mod rng;
mod scoreboard;
mod snapshot;
mod transparency;

use case::{Case, Snippet, Step};
use engine::{ENGINES, FUX_VT};
use families::{FAMILIES, Status};
use rng::Rng;
use std::process::ExitCode;
use std::time::Instant;

/// fux-vt's size for a screen of `rows` by `cols`.
fn vt_size(rows: u16, cols: u16) -> Result<fux_vt::Size, String> {
    fux_vt::Size::new(rows, cols).map_err(|e| format!("fux-vt: {rows}x{cols}: {e}"))
}

const USAGE: &str = "\
usage: fux-vt-compare [run] [--seed N] [--cases N] [--family NAME]... [--all]
                      [--engines LIST] [--no-reflow] [--subject NAME]
       fux-vt-compare survey [--seed N] [--cases N] [--engines LIST] [--no-reflow]
       fux-vt-compare matrix [--seed N] [--cases N] [--engines LIST] [--no-reflow]
       fux-vt-compare cases [--engines LIST] [--no-reflow] [--subject NAME] [--json FILE] [NAME...]
       fux-vt-compare verdicts [--seed N] [--cases N] [--family NAME]... [--engines LIST]
                               [--no-reflow]
       fux-vt-compare replay [--engines LIST] [--size RxC] [--history N] [--no-reflow]
                             [--newline-before-resize] [--subject NAME] STEP...
       fux-vt-compare bench [--engines LIST] [--mb N] [--json FILE] [WORKLOAD...]
       fux-vt-compare bench --instructions [--engines LIST] [--mb N] [--repeats N] [--jobs N]
                            [--json FILE] [WORKLOAD...]
       fux-vt-compare footprint [--engines LIST] [--jobs N] [--json FILE]
       fux-vt-compare record --keys FILE --out PREFIX [--size RxC] [--program NAME]
                             [--version TEXT] [--env KEY=VALUE]... [--dir DIR]
                             [--scrub OLD=NEW]... [--note TEXT] -- PROGRAM ARGS...
       fux-vt-compare corpus [--engines LIST] [--show] [--json FILE] [--subject NAME]
                             [--chunk N] [NAME...]
       fux-vt-compare inventory [NAME...]
       fux-vt-compare encoders [--seed N] [--cases N]
       fux-vt-compare transparency [--engines LIST] [--chunk N] [--json FILE]
                                   [--multiplexers] [NAME... | --size RxC STEP...]
       fux-vt-compare esctest [--terminal NAME] [--beside NAME] [--in-fux] [FILTER] (esctest --help: the rest)
       fux-vt-compare lists [--json FILE] [--against FROZEN] [--esctest DIR]
       fux-vt-compare ledger [--results DIR] [--json FILE] [--table FILE]
       fux-vt-compare scoreboard DIR [--keep KEPT COMMIT DATE]
       fux-vt-compare engines
       fux-vt-compare --list

run      random cases from the families expected to agree (or those named,
         or --all). A case fails where most of the panel differs from
         fux-vt; every failure is shrunk and printed. Then, unless families
         are named, the families that differ, a twentieth as many cases
         each, beside xterm (Ghostty for clusters) by their recorded
         departures, as verdicts judges them. Exit 1 if any.
survey   each family alone (with plain text), counting the cases that
         fail, and the smallest of the first: how to see what a family's
         status should be. Exit 1 if a family expected to agree fails.
matrix   each family alone beside each engine alone, and beside the whole
         panel: the share of cases that differ, as a table.
cases    the named cases (default: all), beside every engine that votes
         (default `panel,xterm.js,tmux,xterm`: all but vt100), judged
         after every step. Exit 1 if one
         fails: outvoted, or, in a family with a recorded verdict, against
         its section's expected fields or the engines that decide it.
verdicts the families with a recorded verdict (or those named), each
         beside the engines that decide it (default; --list names them):
         their named cases, then random cases from each with plain text
         (100 each with a verdict, by default). Exit 1 if they outvote
         fux-vt on any. The families that differ are judged too, each
         beside xterm (Ghostty for clusters), each snippet a write of its
         own: exit 1 on a difference none of its recorded departures
         (families.rs, DEPARTURES) covers.
replay   one case: STEP is output, written as `run` prints it ('\\e[1mX'),
         or resize:RxC. Prints each engine's verdict after every step, and
         the screens.
bench    each engine's speed on the same workloads, in MB/s (default: the
         synthetic ones and the corpus all together; `corpus` adds each
         recording alone; NAME@N is NAME fed N bytes a call, not 4096).
         --instructions: instructions retired per
         byte instead, each engine in this process and workload in a
         child process of its own, less the same child without the
         feeding, the median of --repeats (3) paired rounds, --jobs
         (CARGO_BUILD_JOBS, else 4) at once; --json writes them to FILE.
footprint
         the memory each engine in this process holds (default: all of
         them), each measured in a child process of its own: a screen
         empty and full of styled text, and 10,000 rows of history from
         each synthetic workload and from the corpus, at 80 and 200
         columns, --jobs (CARGO_BUILD_JOBS, else 4) at once. --json
         writes the measures to FILE.
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
         screen at the end of each; --json writes the results to FILE;
         --chunk N gives each step's output N bytes at a time, compared
         after each piece too, the exceptions still by recorded step.
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
         passes. --terminal ghostty runs it against Ghostty's core instead
         (esctest-expected-ghostty.txt); --beside fux-vt compares the two.
scoreboard
         the results run.sh quick, full, deep and fuzz left in DIR,
         gathered into DIR/scoreboard.json and DIR/scoreboard.md.
engines  every engine: whether it can run here, whether it votes, and what
         it cannot tell.
--list   the families, what each covers, and its status.

LIST is engine names joined by commas, or `panel` (the default for run,
survey, matrix and replay: the voters), `all` (every engine, the default
for cases and bench), `in-process` or `xterm`. Every engine a LIST means
must be able to run here, so a check is judged by the same engines
wherever it runs; --missing-ok lets `panel`, `all` and `in-process` leave
out one that cannot.

--subject NAME judges another engine in fux-vt's place (run, cases, corpus,
replay): ghostty or any engine in this process. It is judged as fux-vt is,
and fux-vt joins the panel (in the subject's place in LIST, or where LIST
names `fux-vt`); a field the subject cannot tell is compared with no one.
fux-vt's statuses are not applied to it: run, cases and corpus report its
outcomes and fail only on an error.

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
    /// `corpus --chunk N`: each step's output judged N bytes at a time.
    chunk: Option<usize>,
    /// How many children `footprint` and `bench --instructions` run at once.
    jobs: Option<usize>,
    /// `bench`: instructions retired, not MB/s.
    instructions: bool,
    /// `bench --instructions`: runs of each.
    repeats: usize,
    /// The engine judged in fux-vt's place (`run`, `cases`, `corpus`,
    /// `replay`): fux-vt then joins the panel.
    subject: Option<String>,
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
        chunk: None,
        jobs: None,
        instructions: false,
        repeats: 3,
        subject: None,
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
            "footprint",
            "engines",
            "record",
            "corpus",
            "inventory",
            "encoders",
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
            "--chunk" => args.chunk = Some(number("--chunk", &value("--chunk")?)?),
            "--out" => args.out = Some(value("--out")?),
            "--keys" => args.keys = Some(value("--keys")?),
            "--program" => args.program = Some(value("--program")?),
            "--version" => args.version = value("--version")?,
            "--env" => args.env.push(value("--env")?),
            "--dir" => args.dir = Some(value("--dir")?),
            "--scrub" => args.scrub.push(value("--scrub")?),
            "--note" => args.note = value("--note")?,
            "--json" => args.json = Some(value("--json")?),
            "--jobs" => args.jobs = Some(number("--jobs", &value("--jobs")?)?),
            "--repeats" => args.repeats = number("--repeats", &value("--repeats")?)?,
            "--instructions" => args.instructions = true,
            "--subject" => args.subject = Some(value("--subject")?),
            "--missing-ok" => MISSING_OK.store(true, std::sync::atomic::Ordering::Relaxed),
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

/// Whether `--missing-ok` was given: `panel`, `all` and `in-process` then
/// leave out an engine that cannot run here.
static MISSING_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The engines a LIST names, as indices into [`ENGINES`]. Every engine
/// named, or meant by `panel`, `all` or `in-process`, must be able to run,
/// so the judge is the same wherever a check runs; with `--missing-ok`
/// those three leave out one that cannot, saying so once.
fn engines(list: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    let missing_ok = MISSING_OK.load(std::sync::atomic::Ordering::Relaxed);
    let pick = |test: fn(&engine::Kind) -> bool, out: &mut Vec<usize>| -> Result<(), String> {
        for (i, kind) in ENGINES.iter().enumerate() {
            if !test(kind) {
                continue;
            }
            match (kind.available)() {
                Ok(()) => out.push(i),
                Err(why) if missing_ok => eprintln!("({} cannot run here: {why})", kind.name),
                Err(why) => {
                    return Err(format!(
                        "{} cannot run here ({why}); --missing-ok leaves it out",
                        kind.name
                    ));
                }
            }
        }
        Ok(())
    };
    for name in list.split(',').filter(|n| !n.is_empty()) {
        match name {
            "panel" => pick(|k| k.panel, &mut out)?,
            "all" => pick(|_| true, &mut out)?,
            "in-process" => pick(|k| k.in_process, &mut out)?,
            name => {
                let i = engine::find(name).ok_or(format!("no engine {name:?} (see engines)"))?;
                let kind = ENGINES.get(i).ok_or("no such engine")?;
                (kind.available)().map_err(|why| format!("{name} cannot run here: {why}"))?;
                out.push(i);
            }
        }
    }
    // Each engine once, wherever the list names it again (`panel,ghostty`).
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|&i| seen.insert(i));
    if out.is_empty() {
        return Err(format!("no engine in {list:?} can run here"));
    }
    Ok(out)
}

/// The engines a LIST names beside `subject`: as [`engines`] with fux-vt
/// the subject. With another, the list may name `fux-vt` too, and fux-vt
/// joins the panel in any case, where the list names it, else in the
/// subject's place (`panel` and `all` hold Ghostty), else at the end. The
/// subject itself is never on the panel.
fn engines_beside(list: &str, subject: usize) -> Result<Vec<usize>, String> {
    if subject == FUX_VT {
        return engines(list);
    }
    let mut out = Vec::new();
    for name in list.split(',').filter(|n| !n.is_empty()) {
        if name == engine::SUBJECT.name {
            out.push(FUX_VT);
        } else {
            out.extend(engines(name)?);
        }
    }
    if out.contains(&FUX_VT) {
        out.retain(|&i| i != subject);
    } else if let Some(at) = out.iter().position(|&i| i == subject) {
        if let Some(slot) = out.get_mut(at) {
            *slot = FUX_VT;
        }
        out.retain(|&i| i != subject);
    } else {
        out.push(FUX_VT);
    }
    let mut seen = Vec::with_capacity(out.len());
    out.retain(|&i| {
        let new = !seen.contains(&i);
        seen.push(i);
        new
    });
    Ok(out)
}

fn panel(args: &Args, default: &str) -> Result<Vec<usize>, String> {
    engines(args.engines.as_deref().unwrap_or(default))
}

/// The subject `--subject` names: fux-vt by default, else an engine that
/// runs in this process and can run here.
fn subject(args: &Args) -> Result<usize, String> {
    let Some(name) = args.subject.as_deref() else {
        return Ok(FUX_VT);
    };
    if name == engine::SUBJECT.name {
        return Ok(FUX_VT);
    }
    let i = engine::find(name).ok_or(format!("--subject: no engine {name:?} (see engines)"))?;
    let kind = ENGINES.get(i).ok_or("no such engine")?;
    if !kind.in_process {
        return Err(format!(
            "--subject: {name} runs in a process of its own; a subject runs in this one"
        ));
    }
    (kind.available)().map_err(|why| format!("--subject: {name} cannot run here: {why}"))?;
    Ok(i)
}

/// The panel beside the subject: `--engines` (else `default`), with
/// fux-vt in it when another engine is the subject.
fn panel_beside(args: &Args, default: &str, subject: usize) -> Result<Vec<usize>, String> {
    engines_beside(args.engines.as_deref().unwrap_or(default), subject)
}

fn names(panel: &[usize]) -> String {
    panel
        .iter()
        .map(|&i| engine::name(i))
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
            Status::Decided { why, by, authority } => {
                format!("DECIDED by {authority}, beside {}: {why}", by.join(", "))
            }
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
        if let Some((_, how)) = engine::SETTINGS.iter().find(|(n, _)| *n == kind.name) {
            println!("           set up with: {how}");
        }
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
    let subject = subject(args)?;
    let mut r = Rng::new(seed);
    let mut shown: Vec<Case> = Vec::new();
    let mut failed = 0usize;
    for _ in 0..count {
        let case = Case {
            subject,
            ..case::random(&mut r, families, args.reflow)
        };
        // An error (the subject refused, failed or panicked on the case) is
        // a failure of its own, shrunk while it still errs and shown with
        // its replay; the run goes on.
        let outcome = match case.run(panel) {
            Ok(outcome) => outcome,
            Err(e) => {
                failed = failed.saturating_add(1);
                if shown.len() < show {
                    let small = case.shrink_while(4000, |c| c.run(panel).is_err());
                    let why = small.run(panel).err().unwrap_or(e);
                    println!("ERROR    {why}");
                    println!("  replay:   {}", small.command(panel));
                    shown.push(small);
                }
                continue;
            }
        };
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

/// Random cases. With another subject than fux-vt, the families are still
/// chosen by fux-vt's statuses, so its failures are reported, and only an
/// error fails.
fn run(args: &Args) -> Result<bool, String> {
    // `run` takes no words: one is a command mistyped, which would run
    // 20,000 cases.
    if let Some(word) = args.rest.first() {
        return Err(format!(
            "{word:?} is no command (see --help); run takes no words"
        ));
    }
    let subject = subject(args)?;
    let panel = panel_beside(args, "panel", subject)?;
    let families = chosen_families(args)?;
    let family_names: Vec<&str> = families
        .iter()
        .filter_map(|&i| FAMILIES.get(i).map(|f| f.name))
        .collect();
    if subject != FUX_VT {
        println!("subject: {}", engine::name(subject));
    }
    println!("engines: {}", names(&panel));
    println!("families: {}", family_names.join(" "));
    let count = args.cases.unwrap_or(20_000);
    let started = Instant::now();
    let failed = random(args, &panel, &families, count, args.seed, 5)?;
    let whose = if subject == FUX_VT {
        String::new()
    } else {
        format!("{}: ", engine::name(subject))
    };
    println!(
        "{whose}{failed} of {count} cases failed (seed {}, {:.1}s)",
        args.seed,
        started.elapsed().as_secs_f64()
    );
    // The families that differ are drawn too, a twentieth as many cases
    // each, judged beside their engine by their recorded departures, as
    // `verdicts` judges them (HAR-47); unless the families are named, or
    // the engine can't run here, which is said.
    let mut departed = true;
    if subject == FUX_VT && args.families.is_empty() && !args.all {
        for (name, beside, _) in families::DEPARTURES {
            let Some(i) = families::find(name).filter(|&i| usable(i, args.reflow)) else {
                continue;
            };
            let Ok(panel) = engines(beside) else {
                println!("== {name}: not judged, as {beside} cannot run here");
                continue;
            };
            println!("== {name} (by its departures, beside {beside})");
            departed &= random_judged(args, i, &panel, (count / 20).max(50))?;
        }
    }
    Ok((failed == 0 && departed) || subject != FUX_VT)
}

fn survey(args: &Args) -> Result<bool, String> {
    let panel = panel(args, "panel")?;
    println!("engines: {}", names(&panel));
    let text = families::find("text").ok_or("no text family")?;
    let count = args.cases.unwrap_or(300);
    let mut ok = true;
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
        // A family expected to agree that fails is a regression, as `run`
        // has it; the others' counts are the survey's report.
        if f.status == Status::Agree && failed > 0 {
            ok = false;
        }
    }
    Ok(ok)
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
            let name = engine::name(v.engine);
            let mark = if v.differences.is_empty() { '+' } else { '-' };
            format!("{mark}{name}")
        })
        .chain(
            outcome
                .abstained
                .iter()
                .map(|(index, _)| format!("!{}", engine::name(*index))),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// A named case: its name, its family by name and by index, and the case,
/// with `subject` judged.
struct NamedCase {
    name: &'static str,
    family: &'static str,
    index: usize,
    case: Case,
}

/// The named cases `cases` runs (those in `args.rest`, else all) that
/// this setup can run.
fn named_cases(args: &Args, subject: usize) -> Result<Vec<NamedCase>, String> {
    let mut out = Vec::new();
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
        out.push(NamedCase {
            name,
            family,
            index,
            case: Case {
                rows,
                cols,
                history: if resizes { case::RESIZE_HISTORY } else { 0 },
                reflow: args.reflow,
                newline_before_resize: false,
                steps,
                subject,
            },
        });
    }
    Ok(out)
}

/// A named case's result for `--json` (the conformance ledger reads it):
/// where `subject` fails it as `case::judged` judges every subject, the
/// fields it could not be judged on (unavailable), and whether xterm, if
/// it ran, sees the subject's screen as its own.
fn case_result(
    named: (&str, &str, usize),
    subject: usize,
    (outcome, failures, at_end): &(case::Outcome, Vec<String>, bool),
) -> Result<serde_json::Value, String> {
    let (name, family, index) = named;
    // The fields the subject cannot tell where the judge would have
    // failed it: unavailable, so the case is no pass (`case::reread`).
    let reread = case::reread(&outcome.fux, &outcome.verdicts);
    let unavailable: Vec<String> =
        case::judged(index, name, subject, &outcome.fux, &reread, *at_end)?
            .into_iter()
            .filter(|f| !failures.contains(f))
            .collect();
    let xterm = engine::find("xterm").and_then(|x| {
        outcome
            .verdicts
            .iter()
            .find(|v| v.engine == x)
            .map(|v| v.differences.is_empty())
    });
    Ok(serde_json::json!({
        "case": name,
        "family": family,
        "failures": failures,
        "unavailable": unavailable,
        "xterm_agrees": xterm,
    }))
}

/// Writes `value` as pretty JSON to `path`.
fn write_json(path: &str, value: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| format!("the results: {e}"))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// The engines `cases` runs beside by default.
const CASES_PANEL: &str = "panel,xterm.js,tmux,xterm";

fn cases(args: &Args) -> Result<bool, String> {
    let subject = subject(args)?;
    if subject != FUX_VT {
        return cases_beside(args, subject);
    }
    // Every engine that votes: the panel, xterm.js and tmux (left out of
    // `panel` for their speed alone), and xterm, which decides most
    // decided families; not vt100, which does not vote.
    let panel = panel(args, CASES_PANEL)?;
    println!("engines: {}", names(&panel));
    let mut ok = true;
    let (mut agree, mut differ, mut fixed) = (0usize, 0usize, 0usize);
    let mut results = Vec::new();
    for NamedCase {
        name,
        family,
        index,
        case,
    } in named_cases(args, subject)?
    {
        // Judged after every step, stopping at the first it fails.
        let judged = case::judge_named(&case, &panel, index, name)?;
        results.push(case_result((name, family, index), subject, &judged)?);
        let (outcome, failures, _) = judged;
        let status = FAMILIES.get(index).map(|f| f.status);
        let marks = marks(&outcome);
        if let Some(Status::Decided { by, authority, .. }) = status {
            let deciders = match by {
                [] => format!("{authority} decides"),
                _ => format!("{authority} decides, beside {}", by.join(", ")),
            };
            // Judged by the section's expected fields and the deciders'
            // vote on the rest: deciders named must have given a verdict,
            // but in a standard's family one that could not read the case
            // (an adapter's limit, xterm's SGR 58) leaves the section's
            // fields to judge; a standard's family needs its cited test.
            let limited = outcome.abstained.iter().any(|(i, why)| {
                by.contains(&engine::name(*i)) && engine::abstention(why) == "adapter-limited"
            });
            let standard = matches!(authority, families::Authority::Standard { .. });
            let checked = authority.check(by).and_then(|()| {
                if !by.is_empty() && !heard(&outcome.verdicts, by) && !(standard && limited) {
                    Err(format!("none of {} gave a verdict", by.join(", ")))
                } else {
                    Ok(failures.clone())
                }
            });
            match checked {
                Ok(failures) if failures.is_empty() => {
                    agree = agree.saturating_add(1);
                    println!("ok       {name} (family {family}: {deciders})   {marks}");
                }
                Err(why) => {
                    ok = false;
                    differ = differ.saturating_add(1);
                    println!("FAIL     {name} (family {family}: {deciders}): unchecked, {why}");
                }
                Ok(failures) => {
                    ok = false;
                    differ = differ.saturating_add(1);
                    println!(
                        "FAIL     {name} (family {family}: {deciders}, against fux-vt) on {}   {marks}",
                        failures.join("; ")
                    );
                    if let Some(e) = cases::expected(name) {
                        println!("  {}: \"{}\"", e.section, e.quote);
                    }
                    print!("{}", case::report(&case, &panel, &outcome));
                }
            }
            continue;
        }
        // Any other family's case is judged on every field, a case recorded
        // as differing (`cases::DIFFERING`) only where its record says.
        match (failures.is_empty(), cases::differing(name)) {
            (true, None) => {
                agree = agree.saturating_add(1);
                println!("ok       {name}   {marks}");
            }
            (true, Some(record)) => {
                fixed = fixed.saturating_add(1);
                println!(
                    "differs  {name}, as recorded, on {}: {}   {marks}",
                    record.on.join(", "),
                    record.why
                );
            }
            (false, _) => {
                ok = false;
                differ = differ.saturating_add(1);
                println!(
                    "FAIL     {name} (family {family}) on {}   {marks}",
                    failures.join("; ")
                );
                print!("{}", case::report(&case, &panel, &outcome));
            }
        }
    }
    println!(
        "{agree} agree, in a family that agrees, differs or is decided, {fixed} differ as recorded, {differ} fail"
    );
    if let Some(path) = args.json.as_deref() {
        write_json(
            path,
            &serde_json::json!({
                "check": "cases",
                "subject": "fux-vt",
                "engines": panel.iter().map(|&i| engine::name(i)).collect::<Vec<_>>(),
                "settings": engine::settings(panel.iter().map(|&i| engine::name(i))),
                "results": results,
                "ok": ok,
            }),
        )?;
    }
    Ok(ok)
}

/// Whether any engine named in `by` gave a verdict.
fn heard(verdicts: &[case::Verdict], by: &[&str]) -> bool {
    verdicts
        .iter()
        .any(|v| engine::kind(v.engine).is_some_and(|k| by.contains(&k.name)))
}

/// The named cases with another engine as the subject, judged as fux-vt
/// is (fux-vt votes beside the others). The statuses are fux-vt's, so
/// each case's outcome is reported, and only an error fails.
///
/// At each case's end fux-vt is judged too, on the same screens, with the
/// subject voting in its place: the cases where one is outvoted and the
/// other is not are listed, each with the replay that shows it.
fn cases_beside(args: &Args, subject: usize) -> Result<bool, String> {
    let panel = panel_beside(args, CASES_PANEL, subject)?;
    // The panel fux-vt is judged beside: the subject where fux-vt was.
    let swapped: Vec<usize> = panel
        .iter()
        .map(|&i| if i == FUX_VT { subject } else { i })
        .collect();
    let name_of = engine::name(subject);
    println!("subject: {name_of}, judged as fux-vt is (fux-vt's statuses are not applied)");
    println!("engines: {}", names(&panel));
    let (mut agree, mut fux_agree, mut count) = (0usize, 0usize, 0usize);
    // Name, family, the fields the subject and fux-vt are outvoted on, and
    // the replays with each the subject.
    let mut split = Vec::new();
    let mut results = Vec::new();
    for named in named_cases(args, subject)? {
        count = count.saturating_add(1);
        let judged = case::judge_named(&named.case, &panel, named.index, named.name)?;
        results.push(case_result(
            (named.name, named.family, named.index),
            subject,
            &judged,
        )?);
        let (outcome, on, _) = judged;
        let marks = marks(&outcome);
        let deciders = match FAMILIES.get(named.index).map(|f| f.status) {
            Some(Status::Decided { by: [one], .. }) => format!(": {one} decides"),
            Some(Status::Decided { by, .. }) => format!(": {} decide", by.join(", ")),
            _ => String::new(),
        };
        // fux-vt, judged as the subject is, every step, with the subject
        // voting in its place.
        let as_fux = Case {
            subject: FUX_VT,
            ..named.case.clone()
        };
        let (_, fux_on, _) = case::judge_named(&as_fux, &swapped, named.index, named.name)?;
        if fux_on.is_empty() {
            fux_agree = fux_agree.saturating_add(1);
        }
        if on.is_empty() {
            agree = agree.saturating_add(1);
            println!(
                "agrees   {} (family {}{deciders})   {marks}",
                named.name, named.family
            );
        } else {
            println!(
                "outvoted {} (family {}{deciders}) on {}   {marks}",
                named.name,
                named.family,
                on.join(", ")
            );
            if !args.rest.is_empty() {
                print!("{}", case::report(&named.case, &panel, &outcome));
            }
        }
        if !on.is_empty() || !fux_on.is_empty() {
            split.push((
                named.name,
                named.family,
                on,
                fux_on,
                named.case.command(&panel),
                as_fux.command(&swapped),
            ));
        }
    }
    println!(
        "{name_of}: {agree} of {count} named cases agree; fux-vt, judged on the same cases \
         with {name_of} voting in its place: {fux_agree} of {count}"
    );
    let lists = [
        (
            format!("fux-vt ahead: {name_of} outvoted, fux-vt not"),
            "fux_vt_ahead",
            true,
            false,
        ),
        (
            format!("{name_of} ahead: fux-vt outvoted, {name_of} not"),
            "subject_ahead",
            false,
            true,
        ),
        ("both outvoted".to_owned(), "both", true, true),
    ];
    let mut json = serde_json::Map::new();
    for (title, key, mine, theirs) in lists {
        let chosen: Vec<_> = split
            .iter()
            .filter(|(_, _, on, fux_on, _, _)| on.is_empty() != mine && fux_on.is_empty() != theirs)
            .collect();
        println!("\n{title}: {}", chosen.len());
        let mut entries = Vec::new();
        for (name, family, on, fux_on, replay, replay_fux) in chosen {
            let shown = if mine { on } else { fux_on };
            println!("  {name} (family {family}): on {}", shown.join(", "));
            if mine && theirs {
                println!("    fux-vt on {}", fux_on.join(", "));
            }
            let replay = if mine { replay } else { replay_fux };
            println!("    replay: {replay}");
            entries.push(serde_json::json!({
                "case": name,
                "family": family,
                "subject_outvoted_on": on,
                "fux_vt_outvoted_on": fux_on,
                "replay": replay,
            }));
        }
        json.insert(key.to_owned(), serde_json::Value::Array(entries));
    }
    if let Some(path) = args.json.as_deref() {
        let mut value = serde_json::json!({
            "check": "cases",
            "subject": name_of,
            "engines": panel.iter().map(|&i| engine::name(i)).collect::<Vec<_>>(),
            "settings": engine::settings(
                panel.iter().map(|&i| engine::name(i)).chain([name_of]),
            ),
            "cases": count,
            "agree": agree,
            "fux-vt": { "agree": fux_agree },
            "results": results,
            "ok": true,
        });
        if let Some(object) = value.as_object_mut() {
            object.extend(json);
        }
        write_json(path, &value)?;
    }
    Ok(true)
}

/// The families with a recorded verdict, each beside the engines that
/// decide it (or `--engines`): their named cases at the end of each, then
/// random cases from each with plain text, where the deciding engines' vote
/// decides (xterm alone outvotes fux-vt wherever it differs).
fn verdicts(args: &Args) -> Result<bool, String> {
    let chosen: Vec<usize> = if args.families.is_empty() {
        (0..FAMILIES.len())
            .filter(|&i| {
                usable(i, args.reflow)
                    && FAMILIES.get(i).is_some_and(|f| match f.status {
                        Status::Decided { .. } => true,
                        Status::Differs(_) => families::departures(f.name).is_some(),
                        Status::Agree => false,
                    })
            })
            .collect()
    } else {
        chosen_families(args)?
    };
    let count = args.cases.unwrap_or(100);
    let mut ok = true;
    for &i in &chosen {
        let f = FAMILIES.get(i).ok_or("no such family")?;
        let unchecked = match f.status {
            Status::Decided { by, authority, .. } => {
                authority.check(by).and_then(|()| authority.run()).err()
            }
            Status::Agree | Status::Differs(_) => None,
        };
        if let Some(why) = unchecked {
            ok = false;
            println!("== {}: FAIL, unchecked: {why}", f.name);
            continue;
        }
        // A standard decides, and no engine follows it all through: its
        // test and its named cases' expected fields are the check.
        let alone = matches!(f.status, Status::Decided { by: [], .. }) && args.engines.is_none();
        let beside = match f.status {
            Status::Differs(_) => families::departures(f.name).map(|(beside, _)| beside),
            Status::Agree | Status::Decided { .. } => None,
        };
        let panel = match (args.engines.as_deref(), f.status) {
            (Some(list), _) => engines(list)?,
            (None, _) if beside.is_some() => engines(beside.unwrap_or("xterm"))?,
            (None, Status::Decided { by: [], .. }) => Vec::new(),
            (None, Status::Decided { by, .. }) => engines(&by.join(","))?,
            (None, Status::Agree | Status::Differs(_)) => engines("xterm")?,
        };
        if alone {
            println!("== {} (by its section and its test, which passes)", f.name);
        } else {
            println!("== {} (beside {})", f.name, names(&panel));
        }
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
                subject: FUX_VT,
            };
            let (outcome, failures, _) = case::judge_named(&case, &panel, i, name)?;
            let marks = marks(&outcome);
            if failures.is_empty() {
                println!("   ok    {name}   {marks}");
            } else {
                ok = false;
                println!("   FAIL  {name} on {}   {marks}", failures.join("; "));
                print!("{}", case::report(&case, &panel, &outcome));
            }
        }
        if alone {
            continue;
        }
        ok &= random_judged(args, i, &panel, count)?;
    }
    Ok(ok)
}

/// `count` random cases of family `i` (with text, unless it is text)
/// beside `panel`, cases xterm abstains from (SGR 58) drawn again, up to
/// ten times as many; the first that fails shown, shrunk. A family that
/// differs is judged by its departures (`families::DEPARTURES`), each
/// snippet a write of its own: a case passes that a departure covers, and
/// each departure's count is printed. Whether none failed, of some judged.
fn random_judged(args: &Args, i: usize, panel: &[usize], count: usize) -> Result<bool, String> {
    let f = FAMILIES.get(i).ok_or("no such family")?;
    let text = families::find("text").ok_or("no text family")?;
    let set: Vec<usize> = if i == text { vec![i] } else { vec![i, text] };
    let mut r = Rng::new(args.seed);
    let (mut judged, mut failed, mut unheard) = (0usize, 0usize, 0usize);
    let mut shown = false;
    // A family that differs: its departures from xterm, and how many
    // cases each covered.
    let departures = match f.status {
        Status::Differs(_) => families::departures(f.name).map(|(_, d)| d),
        Status::Agree | Status::Decided { .. } => None,
    };
    let mut made = vec![0usize; departures.map_or(0, <[_]>::len)];
    // Cases xterm abstains from (SGR 58) are drawn again, up to ten
    // times as many as asked for.
    while judged < count && judged.saturating_add(unheard) < count.saturating_mul(10) {
        let mut case = case::random(&mut r, &set, args.reflow);
        if departures.is_some() {
            case.steps = (case.steps.into_iter())
                .flat_map(|s| match s {
                    Step::Output(snippets) => snippets
                        .into_iter()
                        .map(|x| Step::Output(vec![x]))
                        .collect(),
                    other => vec![other],
                })
                .collect();
        }
        let outcome = case.run(panel)?;
        if outcome.verdicts.is_empty() {
            unheard = unheard.saturating_add(1);
            continue;
        }
        judged = judged.saturating_add(1);
        if outcome.agrees() {
            continue;
        }
        if let Some(departures) = departures
            && let Some(at) = covering(&case, &outcome, departures)
        {
            if let Some(n) = made.get_mut(at) {
                *n = n.saturating_add(1);
            }
            continue;
        }
        failed = failed.saturating_add(1);
        if !shown {
            shown = true;
            // Shrunk while it still differs where no departure covers.
            let small = case.shrink_while(4000, |c| {
                c.run(panel).is_ok_and(|o| {
                    !o.agrees() && departures.is_none_or(|d| covering(c, &o, d).is_none())
                })
            });
            print!("{}", case::report(&small, panel, &small.run(panel)?));
        }
    }
    for (d, n) in departures.unwrap_or_default().iter().zip(&made) {
        println!("   departs in {n} cases: {}", d.why);
    }
    println!(
        "   {failed} of {judged} random cases failed (seed {}); {unheard} more had no verdict",
        args.seed
    );
    Ok(failed == 0 && judged > 0)
}

/// The departure of `departures` that covers how `case` went (`outcome`):
/// one its output makes, whose fields name every difference.
fn covering(
    case: &Case,
    outcome: &case::Outcome,
    departures: &[families::Departure],
) -> Option<usize> {
    let bytes = case.output();
    departures.iter().position(|d| {
        (d.when)(&bytes, case.rows, case.cols)
            && (outcome.verdicts.iter())
                .flat_map(|v| &v.differences)
                .all(|diff| d.fields.iter().any(|g| families::glob(g, &diff.key)))
    })
}

fn replay(args: &Args) -> Result<bool, String> {
    let subject = subject(args)?;
    let panel = panel_beside(args, "panel", subject)?;
    let all = steps(&args.rest, usize::MAX)?;
    let mut case = Case {
        rows: args.size.0,
        cols: args.size.1,
        history: args.history,
        reflow: args.reflow,
        newline_before_resize: args.newline_before_resize,
        steps: Vec::new(),
        subject,
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

/// The children counted at once unless `--jobs` says: CARGO_BUILD_JOBS,
/// as a shared machine's limit sets it, else 4, as fux-bench takes
/// (speed-method.md: a count beside other work takes few cores).
pub fn few_jobs() -> usize {
    (std::env::var("CARGO_BUILD_JOBS").ok())
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(4)
}

/// The most data compare may hold, in bytes, unless
/// `FUX_VT_COMPARE_DATA_LIMIT` says otherwise (0: no limit of its own).
const DATA_LIMIT: u64 = 4 << 30;

/// Sets this process's data limit (`RLIMIT_DATA`, which its children
/// inherit) to [`DATA_LIMIT`] or `FUX_VT_COMPARE_DATA_LIMIT`, never above
/// the hard limit it has. An engine run in process that grows without end
/// (alacritty_terminal 0.26's resize loop took 55 GB on 2026-10-17) then
/// aborts with "memory allocation failed" instead of taking the machine.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "the data limit is set through libc's C interface, and only here"
)]
fn limit_data() -> Result<(), String> {
    let bytes = match std::env::var("FUX_VT_COMPARE_DATA_LIMIT") {
        Ok(text) => text
            .parse::<u64>()
            .map_err(|e| format!("FUX_VT_COMPARE_DATA_LIMIT={text:?}: {e}"))?,
        Err(_) => DATA_LIMIT,
    };
    if bytes == 0 {
        return Ok(());
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit for getrlimit to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_DATA, &raw mut limit) } != 0 {
        return Err(format!("getrlimit: {}", std::io::Error::last_os_error()));
    }
    let wanted = libc::rlim_t::try_from(bytes).unwrap_or(libc::RLIM_INFINITY);
    limit.rlim_cur = wanted.min(limit.rlim_max);
    // SAFETY: `limit` is a valid rlimit, its soft limit within its hard.
    if unsafe { libc::setrlimit(libc::RLIMIT_DATA, &raw const limit) } != 0 {
        return Err(format!("setrlimit: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn limit_data() -> Result<(), String> {
    Ok(())
}

fn main() -> ExitCode {
    if let Err(e) = limit_data() {
        eprintln!("fux-vt-compare: the data limit: {e}");
        return ExitCode::from(2);
    }
    // An engine's panic is caught and costs it its vote (`case::guarded`),
    // and reported there; the default hook would print each one again.
    std::panic::set_hook(Box::new(case::note_panic));
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some((first, rest)) = argv.split_first()
        && first == instructions::CHILD
    {
        return match instructions::child(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("fux-vt-compare: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some((first, rest)) = argv.split_first()
        && first == footprint::CHILD
    {
        return match footprint::child(rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("fux-vt-compare: {e}");
                ExitCode::FAILURE
            }
        };
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
    // `scoreboard` takes its own arguments (`--keep`).
    if let Some((first, rest)) = argv.split_first()
        && first == "scoreboard"
    {
        return match scoreboard::run(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("fux-vt-compare scoreboard: {e}");
                ExitCode::from(2)
            }
        };
    }
    if let Some((first, rest)) = argv.split_first()
        && first == "ledger"
    {
        return match ledger::run(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("fux-vt-compare ledger: {e}");
                ExitCode::from(2)
            }
        };
    }
    if let Some((first, rest)) = argv.split_first()
        && first == "lists"
    {
        return match lists::run(rest) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("fux-vt-compare lists: {e}");
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
    let parsed = parse().and_then(|args| {
        if args.subject.is_some()
            && !["run", "cases", "corpus", "replay"].contains(&args.command.as_str())
        {
            return Err(format!(
                "--subject: {} takes none (run, cases, corpus and replay do)",
                args.command
            ));
        }
        Ok(args)
    });
    let result = parsed.and_then(|args| match args.command.as_str() {
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
        #[cfg(feature = "engines")]
        "encoders" => encoders::run(args.seed, args.cases.unwrap_or(2000)),
        #[cfg(not(feature = "engines"))]
        "encoders" => Err("encoders compares with libghostty-vt, which is not built \
                           (--no-default-features)"
            .to_owned()),
        "corpus" => corpus::run(
            subject(&args)?,
            &panel_beside(&args, "xterm,panel", subject(&args)?)?,
            &args.rest,
            args.show,
            args.json.as_deref(),
            args.chunk,
        ),
        "bench" if args.instructions => instructions::run(
            &panel(&args, "in-process")?,
            &args.rest,
            args.mb,
            args.repeats,
            args.jobs.unwrap_or_else(few_jobs),
            args.json.as_deref(),
        ),
        "bench" if args.json.is_some() => bench::run_to(
            &panel(&args, "all")?,
            &args.rest,
            args.mb,
            args.json.as_deref(),
        ),
        "bench" => bench::run(&panel(&args, "all")?, &args.rest, args.mb),
        "footprint" => footprint::run(
            &panel(&args, "in-process")?,
            args.json.as_deref(),
            args.jobs.unwrap_or_else(few_jobs),
        ),
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
