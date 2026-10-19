//! `fux-bench`: fux's speed against another commit, in instructions
//! retired, on fixed workloads run in this process (`workloads`), each
//! counted in a child process of its own (`count`), REF's and the working
//! tree's built by one recipe from their trees (`git archive`, `against`);
//! and what a person feels through real
//! servers beside tmux, zellij and herdr (`feel`).
mod against;
mod calls;
mod census;
mod corpus;
mod count;
/// `bench/perf.sh`, the wrapper `count` runs perf through.
const PERF_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/perf.sh");
mod feel;
mod helpers;
mod info;
mod rng;
mod synthetic;
mod terminal;
mod workloads;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: fux-bench [--against REF] [--repeats N] [--jobs N] [--floors FILE | --floors-here FILE]
                 [--only TEXT]... [--json FILE]
       fux-bench --calibrate [--repeats N] [--jobs N] [--only TEXT]... [--json FILE]
       fux-bench list
       fux-bench run WORKLOAD [--baseline] [--corpus DIR] [--payloads DIR]
       fux-bench time [--corpus DIR] [--payloads DIR] [WORKLOAD...]
       fux-bench calls DIR
       fux-bench info [--json FILE]
       fux-bench feel [--muxes LIST] [--parts LIST] [--keys N] [--json FILE]

(default)  every workload on REF (default master) and on the working tree,
           both built by one recipe (git archive at paths of one length,
           one target dir), in N interleaved paired rounds (default 15),
           in instructions retired, judged as speed-method.md says: the
           median change of the rounds against each workload's floor from
           FILE (else max(0.25%, 3 x the run's spread)), and a sign test
           over the recordings. Exit 1 if a workload is slower past its
           floor or the recordings are broadly slower, 2 if a workload's
           floor is over 1% (the tool is broken for it); a noisy workload
           is listed, and judged inconclusive:
             cargo run --release --manifest-path bench/Cargo.toml -- --against master
           --only runs the workloads whose names contain TEXT.
--calibrate  an A/A run, the working tree built twice by the recipe: each
           workload's floor, max(0.25%, |change|, 3 x spread), for --floors.
list       the workloads.
run        one workload once (or its baseline), as `--against` counts it.
time       each workload's thread CPU time here, best of 3, and MB/s.
calls      the inputs fux-vt's bounds suite kept in DIR of its probes whose
           calls took long with its counters on, timed again without
           them: each one's longest call, best of 3 (worst-feed.sh).
info       MB/s beside Ghostty and alacritty (fux-vt/compare's `run.sh
           bench`) and fux-diff's --speed: informational, not compared.
feel       real servers on sockets of their own, each with a client on a
           PTY: keystroke latency idle and beside a flooding pane, throughput
           to the final screen and the bytes sent, idle CPU and memory.
           LIST is joined by commas: muxes direct,fux,tmux,zellij,herdr (those
           installed), parts latency,throughput,footprint; N keys per
           latency run (default 2000). Wall time; reported, not gated.

The JSON goes to FILE, by default in bench/target/fux-bench/:
against-REF-TREE.json by the two commits compared (calibration-TREE.json
for --calibrate), info.json for info, feel.json for feel.";

/// The threads a run counts on, and the jobs its cargo builds take,
/// unless `--jobs` says: CARGO_BUILD_JOBS, as a shared machine's limit sets
/// it, else 4, never every core (speed-method.md: a run beside other work
/// takes few).
pub fn default_jobs() -> usize {
    jobs_from(std::env::var("CARGO_BUILD_JOBS").ok().as_deref())
}

/// `default_jobs` from CARGO_BUILD_JOBS's value, if any.
fn jobs_from(var: Option<&str>) -> usize {
    var.and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4)
}

#[cfg(test)]
#[test]
fn jobs_are_cargo_build_jobs_or_four() {
    assert_eq!(jobs_from(Some("8")), 8);
    assert_eq!(jobs_from(None), 4);
    assert_eq!(jobs_from(Some("0")), 4);
    assert_eq!(jobs_from(Some("all")), 4);
}

/// Where results go unless `--json` says.
pub fn results(name: &str) -> PathBuf {
    against::root().join("bench/target/fux-bench").join(name)
}

/// Writes `value` to `path`, pretty, making its directory.
pub fn save_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The value after a flag.
fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or(format!("{flag} needs a value"))
}

fn number<T: std::str::FromStr>(text: &str, flag: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag}: {text:?} is not a number"))
}

/// A count of at least one: rounds or threads, which a run records as it
/// runs them.
fn count(text: &str, flag: &str) -> Result<usize, String> {
    number(text, flag)
        .ok()
        .filter(|&n: &usize| n > 0)
        .ok_or_else(|| format!("{flag}: {text:?} is not a count of one or more"))
}

#[cfg(test)]
#[test]
fn rounds_and_threads_are_one_or_more() {
    assert_eq!(count("15", "--repeats"), Ok(15));
    assert!(count("0", "--repeats").is_err());
    assert!(count("x", "--jobs").is_err());
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("fux-bench: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(argv: Vec<String>) -> Result<bool, String> {
    // The programs `feel` runs, with arguments of their own.
    if let Some((first, rest)) = argv.split_first() {
        match first.as_str() {
            "__echo" => return helpers::echo(),
            "__flood" => return helpers::flood(),
            "__fill" => return helpers::fill(rest),
            "__serve" => return helpers::serve(rest),
            _ => {}
        }
    }
    let mut args = argv.into_iter().peekable();
    let command = match args.peek().map(String::as_str) {
        Some(c @ ("list" | "run" | "time" | "calls" | "info" | "feel")) => {
            let c = c.to_owned();
            args.next();
            c
        }
        _ => "against".to_owned(),
    };
    let mut corpus = corpus::dir(&against::root());
    let mut payloads = census::data(&against::root());
    let mut options = against::Options {
        reference: "master".into(),
        repeats: 15,
        jobs: default_jobs(),
        floors: None,
        floors_here: false,
        calibrate: false,
        only: Vec::new(),
        // `--against`'s default names the two commits it compares, so a
        // run never overwrites another's (`against::Options::json`).
        json: match command.as_str() {
            "info" => Some(results("info.json")),
            "feel" => Some(results("feel.json")),
            _ => None,
        },
    };
    let mut feel = feel::Options {
        muxes: ["direct", "fux", "tmux", "zellij", "herdr"]
            .map(str::to_owned)
            .to_vec(),
        parts: ["latency", "throughput", "footprint"]
            .map(str::to_owned)
            .to_vec(),
        keys: 2000,
        json: results("feel.json"),
    };
    let list = |text: String| text.split(',').map(str::to_owned).collect::<Vec<_>>();
    let mut baseline = false;
    let mut against = false;
    let mut names = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--against" => {
                options.reference = value(&mut args, &arg)?;
                against = true;
            }
            "--repeats" => options.repeats = count(&value(&mut args, &arg)?, &arg)?,
            "--jobs" => options.jobs = count(&value(&mut args, &arg)?, &arg)?,
            "--floors" => options.floors = Some(value(&mut args, &arg)?.into()),
            "--floors-here" => {
                options.floors = Some(value(&mut args, &arg)?.into());
                options.floors_here = true;
            }
            "--calibrate" => options.calibrate = true,
            "--only" => options.only.push(value(&mut args, &arg)?),
            "--json" => options.json = Some(value(&mut args, &arg)?.into()),
            "--corpus" => corpus = value(&mut args, &arg)?.into(),
            "--payloads" => payloads = value(&mut args, &arg)?.into(),
            "--baseline" => baseline = true,
            "--muxes" => feel.muxes = list(value(&mut args, &arg)?),
            "--parts" => feel.parts = list(value(&mut args, &arg)?),
            "--keys" => feel.keys = number(&value(&mut args, &arg)?, &arg)?,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(true);
            }
            other if !other.starts_with('-') => names.push(other.to_owned()),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    match command.as_str() {
        "list" => {
            for spec in workloads::list(&corpus::recordings(&corpus)?) {
                println!("{:<26} {}", spec.name, spec.about);
            }
            Ok(true)
        }
        "run" => {
            let [name] = names.as_slice() else {
                return Err(format!("run takes one workload\n{USAGE}"));
            };
            let done = workloads::run(name, &corpus, &payloads, baseline)?;
            println!("{name} {} {} {}", done.units, done.frames, done.painted);
            Ok(true)
        }
        "time" => time(&corpus, &payloads, &names),
        "calls" => match names.as_slice() {
            [dir] => calls::run(Path::new(dir)),
            _ => Err(format!("calls takes one directory\n{USAGE}")),
        },
        "info" => info::run(&options.json.unwrap_or_else(|| results("info.json"))),
        "feel" => {
            feel.json = options.json.unwrap_or_else(|| results("feel.json"));
            feel::run(&feel)
        }
        _ => {
            if !names.is_empty() {
                return Err(format!("unexpected {names:?}\n{USAGE}"));
            }
            // A calibration is the working tree against itself: it takes
            // no REF and makes the floors, so it is judged by none.
            if options.calibrate && (against || options.floors.is_some()) {
                return Err(format!(
                    "--calibrate takes neither --against nor --floors\n{USAGE}"
                ));
            }
            against::run(&options)
        }
    }
}

/// Each workload's best thread CPU time of three, here.
fn time(corpus: &Path, payloads: &Path, names: &[String]) -> Result<bool, String> {
    let cpu = || fuxix::process::thread_cpu_time().map_err(|e| e.to_string());
    println!(
        "{:<26} {:>10} {:>10}   thread CPU, best of 3",
        "workload", "ms", "MB/s"
    );
    for spec in workloads::list(&corpus::recordings(corpus)?) {
        if !names.is_empty() && !names.iter().any(|n| spec.name.contains(n.as_str())) {
            continue;
        }
        let mut best = std::time::Duration::MAX;
        let mut units = 0;
        for _ in 0..3 {
            let base_start = cpu()?;
            workloads::run(&spec.name, corpus, payloads, true)?;
            let base = cpu()?.saturating_sub(base_start);
            let start = cpu()?;
            units = workloads::run(&spec.name, corpus, payloads, false)?.units;
            best = best.min(cpu()?.saturating_sub(start).saturating_sub(base));
        }
        let secs = best.as_secs_f64().max(1e-9);
        // MB/s only of a workload fed bytes: another's units (keys, waits)
        // are no bytes.
        let mbs = match workloads::fed(&spec.name).starts_with(workloads::NO_STREAM) {
            true => "-".to_owned(),
            false => format!("{:.1}", (units as f64) / secs / 1e6),
        };
        println!("{:<26} {:>10.2} {mbs:>10}", spec.name, secs * 1e3);
    }
    Ok(true)
}
