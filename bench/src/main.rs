//! `fux-bench`: fux's speed against another commit, in instructions
//! retired, on fixed workloads run in this process (`workloads`), each
//! counted in a child process of its own (`count`), REF's built in a
//! temporary worktree (`against`); and what a person feels through real
//! servers beside tmux and zellij (`feel`).
mod against;
mod corpus;
mod count;
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
usage: fux-bench [--against REF] [--repeats N] [--jobs N] [--threshold PERCENT]
                 [--only TEXT]... [--json FILE]
       fux-bench list
       fux-bench run WORKLOAD [--baseline] [--corpus DIR]
       fux-bench time [--corpus DIR] [WORKLOAD...]
       fux-bench info [--json FILE]
       fux-bench feel [--muxes LIST] [--parts LIST] [--keys N] [--json FILE]

(default)  every workload on REF (default main), built in a temporary
           worktree, and on the working tree, alternating, in instructions
           retired; flags a workload more than PERCENT (default 3) above
           REF and beyond the noise of its repeats (default 5). Exit 1 if
           one is flagged. Run it built as fux is:
             cargo run --release --manifest-path bench/Cargo.toml -- --against main
           --only runs the workloads whose names contain TEXT.
list       the workloads.
run        one workload once (or its baseline), as `--against` counts it.
time       each workload's thread CPU time here, best of 3, and MB/s.
info       MB/s beside Ghostty and alacritty (fux-vt/compare's `run.sh
           bench`) and fux-diff's --speed: informational, not compared.
feel       real servers on sockets of their own, each with a client on a
           PTY: keystroke latency idle and beside a flooding pane, throughput
           to the final screen and the bytes sent, idle CPU and memory.
           LIST is joined by commas: muxes direct,fux,tmux,zellij (those
           installed), parts latency,throughput,footprint; N keys per
           latency run (default 2000). Wall time; reported, not gated.

The JSON goes to FILE, by default bench/target/fux-bench/against.json
(info.json for info, feel.json for feel).";

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
            "__launch" => return helpers::launch(rest),
            "__echo" => return helpers::echo(),
            "__flood" => return helpers::flood(),
            "__fill" => return helpers::fill(rest),
            "__serve" => return helpers::serve(rest),
            _ => {}
        }
    }
    let mut args = argv.into_iter().peekable();
    let command = match args.peek().map(String::as_str) {
        Some(c @ ("list" | "run" | "time" | "info" | "feel")) => {
            let c = c.to_owned();
            args.next();
            c
        }
        _ => "against".to_owned(),
    };
    let mut corpus = corpus::dir(&against::root());
    let mut options = against::Options {
        reference: "main".into(),
        repeats: 5,
        jobs: std::thread::available_parallelism().map_or(4, usize::from),
        threshold: 3.0,
        only: Vec::new(),
        json: results(match command.as_str() {
            "info" => "info.json",
            "feel" => "feel.json",
            _ => "against.json",
        }),
    };
    let mut feel = feel::Options {
        muxes: ["direct", "fux", "tmux", "zellij"]
            .map(str::to_owned)
            .to_vec(),
        parts: ["latency", "throughput", "footprint"]
            .map(str::to_owned)
            .to_vec(),
        keys: 2000,
        json: options.json.clone(),
    };
    let list = |text: String| text.split(',').map(str::to_owned).collect::<Vec<_>>();
    let mut baseline = false;
    let mut names = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--against" => options.reference = value(&mut args, &arg)?,
            "--repeats" => options.repeats = number(&value(&mut args, &arg)?, &arg)?,
            "--jobs" => options.jobs = number(&value(&mut args, &arg)?, &arg)?,
            "--threshold" => options.threshold = number(&value(&mut args, &arg)?, &arg)?,
            "--only" => options.only.push(value(&mut args, &arg)?),
            "--json" => options.json = value(&mut args, &arg)?.into(),
            "--corpus" => corpus = value(&mut args, &arg)?.into(),
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
            let done = workloads::run(name, &corpus, baseline)?;
            println!("{name} {} {} {}", done.units, done.frames, done.painted);
            Ok(true)
        }
        "time" => time(&corpus, &names),
        "info" => info::run(&options.json),
        "feel" => {
            feel.json = options.json;
            feel::run(&feel)
        }
        _ => {
            if !names.is_empty() {
                return Err(format!("unexpected {names:?}\n{USAGE}"));
            }
            against::run(&options)
        }
    }
}

/// Each workload's best thread CPU time of three, here.
fn time(corpus: &Path, names: &[String]) -> Result<bool, String> {
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
            workloads::run(&spec.name, corpus, true)?;
            let base = cpu()?.saturating_sub(base_start);
            let start = cpu()?;
            units = workloads::run(&spec.name, corpus, false)?.units;
            best = best.min(cpu()?.saturating_sub(start).saturating_sub(base));
        }
        let secs = best.as_secs_f64().max(1e-9);
        println!(
            "{:<26} {:>10.2} {:>10.1}",
            spec.name,
            secs * 1e3,
            (units as f64) / secs / 1e6
        );
    }
    Ok(true)
}
