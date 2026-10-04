//! `fux-vt-oracle`: the working tree's fux-vt beside fux-vt at the pinned
//! commit, over the corpus, random cases, resize-heavy streams, the limits
//! and the standalone types. Any difference fails, shrunk to the smallest
//! case that shows it, written down to replay.
use fux_vt_oracle::shrink::shrink;
use fux_vt_oracle::{Case, Count, Difference, cells, check, inputs, model::Setup};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: fux-vt-oracle [--seed N] [--cases N] [--streams N] [--cells N]
                     [--no-corpus] [--shrink SECONDS] [--threads N]
       fux-vt-oracle --replay FILE
       fux-vt-oracle --seeds DIR [--seed N] [--cases N] [--streams N]
       fux-vt-oracle --baseline

Runs the working tree's fux-vt beside fux-vt at the commit Cargo.lock pins
(diff/oracle.sh pins it, to the merge base with main unless told another),
comparing everything its API shows after every step: over every corpus
recording (as fux sets up a pane, and with every option off; each step in
pieces of up to 2048 bytes), --cases random cases (default 10000), --streams
resize-heavy streams with history full (default 50), the limits, and
--cells runs of the standalone types (default 300). A difference is shrunk
for up to --shrink seconds (default 60) and written to diff/target/oracle/
to replay with --replay. --seeds writes --cases random cases and --streams
resize streams to DIR instead, as --replay reads them, to seed the fuzz
target (diff/fuzz).";

/// The most bytes of a corpus recording given in one step.
const PIECE: usize = 2048;

struct Options {
    seed: u64,
    cases: usize,
    streams: usize,
    cells: usize,
    corpus: bool,
    shrink: Duration,
    threads: usize,
    /// Where to write seeds for the fuzz target, rather than run.
    seeds: Option<String>,
}

enum Asked {
    Run(Options),
    Seeds(String, Options),
    Replay(String),
    Print(String),
}

/// The commit `base-vt` is built from, as the lockfile says.
fn pinned() -> &'static str {
    include_str!("../../Cargo.lock")
        .lines()
        .filter(|line| line.starts_with("source = \"git+"))
        .find_map(|line| line.rsplit_once('#'))
        .map_or("(not found in Cargo.lock)", |(_, sha)| {
            sha.trim_end_matches('"')
        })
}

fn parse() -> Result<Asked, String> {
    let mut args = std::env::args().skip(1);
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let mut o = Options {
        seed: 1,
        cases: 10_000,
        streams: 50,
        cells: 300,
        corpus: true,
        shrink: Duration::from_secs(60),
        threads,
        seeds: None,
    };
    let number = |value: Option<String>, flag: &str| -> Result<usize, String> {
        value
            .ok_or(format!("{flag} needs a value"))?
            .parse()
            .map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => o.seed = u64::try_from(number(args.next(), &arg)?).unwrap_or(1),
            "--cases" => o.cases = number(args.next(), &arg)?,
            "--streams" => o.streams = number(args.next(), &arg)?,
            "--cells" => o.cells = number(args.next(), &arg)?,
            "--threads" => o.threads = number(args.next(), &arg)?.max(1),
            "--shrink" => {
                o.shrink =
                    Duration::from_secs(u64::try_from(number(args.next(), &arg)?).unwrap_or(60));
            }
            "--no-corpus" => o.corpus = false,
            "--seeds" => o.seeds = Some(args.next().ok_or("--seeds needs a directory")?),
            "--replay" => return Ok(Asked::Replay(args.next().ok_or("--replay needs a file")?)),
            "--baseline" => return Ok(Asked::Print(pinned().into())),
            "--help" | "-h" => return Ok(Asked::Print(USAGE.into())),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(match o.seeds.take() {
        Some(dir) => Asked::Seeds(dir, o),
        None => Asked::Run(o),
    })
}

/// Writes the random cases and resize streams to `dir` as `--replay`
/// reads them, which the fuzz target reads too: seeds that reach far more
/// than random bytes do. How many it wrote.
fn seeds(dir: &str, o: &Options) -> Result<bool, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    let cases = inputs::random_cases(o.seed ^ 0x5241_4e44, o.cases)
        .into_iter()
        .chain(inputs::resize_streams(o.seed ^ 0x5349_5a45, o.streams));
    let mut n = 0usize;
    for (i, case) in cases.enumerate() {
        let path = std::path::Path::new(dir).join(format!("seed-{}-{i}.case", o.seed));
        std::fs::write(&path, case.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
        n = n.saturating_add(1);
    }
    println!("fux-vt-oracle: {n} seeds in {dir}");
    Ok(true)
}

/// A case that differed: what it was, which, and how.
struct Failure {
    label: String,
    case: Case,
    difference: Difference,
}

/// Runs `jobs` cases on `threads` threads, stopping at the first that
/// differs. How much they compared, or the first that differed.
fn run_all(jobs: Vec<(String, Case)>, threads: usize) -> Result<Count, Box<Failure>> {
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let total = Mutex::new(Count::default());
    let failure: Mutex<Option<Box<Failure>>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads.min(jobs.len()).max(1) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((label, case)) = jobs.get(i) else {
                        return;
                    };
                    match check(case) {
                        Ok(count) => {
                            if let Ok(mut total) = total.lock() {
                                total.add(count);
                            }
                        }
                        Err(difference) => {
                            stop.store(true, Ordering::Relaxed);
                            if let Ok(mut failure) = failure.lock()
                                && failure.is_none()
                            {
                                *failure = Some(Box::new(Failure {
                                    label: label.clone(),
                                    case: case.clone(),
                                    difference,
                                }));
                            }
                        }
                    }
                }
            });
        }
    });
    match failure.into_inner().ok().flatten() {
        Some(f) => Err(f),
        None => Ok(total.into_inner().unwrap_or_default()),
    }
}

/// Where differing cases are written.
fn out_dir() -> std::path::PathBuf {
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    oracle.parent().unwrap_or(oracle).join("target/oracle")
}

/// Shrinks a failure, writes it down and says how to replay it.
fn report(f: &Failure, budget: Duration) -> String {
    let started = Instant::now();
    let until = started.checked_add(budget).unwrap_or(started);
    let small = shrink(&f.case, &f.difference, until);
    let shrunk = check(&small).err();
    let mut out = format!(
        "{}: {}\n\nshrunk in {:.1} s from {} steps and {} bytes to {} steps and {} bytes",
        f.label,
        f.difference,
        started.elapsed().as_secs_f64(),
        f.case.steps.len(),
        f.case.bytes(),
        small.steps.len(),
        small.bytes(),
    );
    match &shrunk {
        Some(d) => out.push_str(&format!(":\n{d}\n")),
        None => out.push_str(" (the shrunk case no longer differs; the original is kept)\n"),
    }
    let kept = if shrunk.is_some() { &small } else { &f.case };
    let text = kept.to_text();
    if text.len() < 4_000 {
        out.push_str(&format!("\n{text}"));
    }
    let dir = out_dir();
    let name: String = f
        .label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = dir.join(format!("{name}.case"));
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &text)) {
        Ok(()) => out.push_str(&format!(
            "\nreplay: fux-vt-oracle --replay {}",
            path.display()
        )),
        Err(e) => out.push_str(&format!("\n(could not write {}: {e})", path.display())),
    }
    out
}

/// A part's line: what it compared and how long it took.
fn part(name: &str, started: Instant, result: Result<String, String>) -> bool {
    let seconds = started.elapsed().as_secs_f64();
    match result {
        Ok(summary) => {
            println!("{name:<11} alike: {summary} ({seconds:.1} s)");
            true
        }
        Err(report) => {
            println!("{name:<11} DIFFERENT ({seconds:.1} s):\n{report}");
            false
        }
    }
}

fn cases_part(name: &str, jobs: Vec<(String, Case)>, o: &Options) -> bool {
    let started = Instant::now();
    let n = jobs.len();
    let result = run_all(jobs, o.threads)
        .map(|count| format!("{n} cases: {count}"))
        .map_err(|f| {
            let found = started.elapsed().as_secs_f64();
            format!("found in {found:.1} s; {}", report(&f, o.shrink))
        });
    part(name, started, result)
}

fn run(o: &Options) -> Result<bool, String> {
    println!(
        "fux-vt-oracle: the working tree beside {} (seed {})",
        pinned(),
        o.seed
    );
    let mut alike = true;
    let started = Instant::now();
    let result = cells::check(o.seed, o.cells)
        .map(|edits| {
            format!(
                "constants, continues_cluster, {} runs and {edits} edits of Cells",
                o.cells
            )
        })
        .map_err(|d| d.to_string());
    alike &= part("standalone", started, result);
    if o.corpus {
        let mut jobs = Vec::new();
        for recording in inputs::corpus()? {
            jobs.push((
                format!("corpus {} as a pane", recording.name),
                recording.case(inputs::PANE_HISTORY, Setup::pane(), PIECE),
            ));
            jobs.push((
                format!("corpus {} with every option off", recording.name),
                recording.case(inputs::PANE_HISTORY, Setup::default(), PIECE),
            ));
        }
        alike &= cases_part("corpus", jobs, o);
    }
    let jobs = inputs::random_cases(o.seed ^ 0x5241_4e44, o.cases)
        .into_iter()
        .enumerate()
        .map(|(i, case)| (format!("random case {i} (seed {})", o.seed), case))
        .collect();
    alike &= cases_part("random", jobs, o);
    let jobs = inputs::resize_streams(o.seed ^ 0x5349_5a45, o.streams)
        .into_iter()
        .enumerate()
        .map(|(i, case)| (format!("resize stream {i} (seed {})", o.seed), case))
        .collect();
    alike &= cases_part("resizes", jobs, o);
    let jobs = inputs::limits_cases(o.seed ^ 0x4c49_4d49)
        .into_iter()
        .enumerate()
        .map(|(i, case)| (format!("limits {i}"), case))
        .collect();
    alike &= cases_part("limits", jobs, o);
    println!(
        "fux-vt-oracle: {} in {:.1} s",
        if alike { "alike" } else { "DIFFERENT" },
        started.elapsed().as_secs_f64()
    );
    Ok(alike)
}

fn replay(path: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let case = Case::from_text(&text)?;
    match check(&case) {
        Ok(count) => {
            println!("alike: {count}");
            Ok(true)
        }
        Err(d) => {
            println!("DIFFERENT: {d}");
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    let result = match parse() {
        Ok(Asked::Run(o)) => run(&o),
        Ok(Asked::Replay(path)) => replay(&path),
        Ok(Asked::Seeds(dir, o)) => seeds(&dir, &o),
        Ok(Asked::Print(text)) => {
            println!("{text}");
            Ok(true)
        }
        Err(message) => {
            eprintln!("fux-vt-oracle: {message}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("fux-vt-oracle: {message}");
            ExitCode::from(2)
        }
    }
}
