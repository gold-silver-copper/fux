//! `fux-diff`: fux run side by side with its last release. The same random
//! inputs go to both, and everything each gives back is compared: sessions
//! after every event, what each client is shown and painted, parsed
//! commands, layouts, frames, terminal screens, decoded input, copy mode,
//! words and config, and fuxix's calls. For a change meant to leave
//! behaviour as it was; any difference is reported with what led to it.
mod commands;
mod copy;
mod input;
mod layout;
mod protocol;
mod rng;
mod sessions;
mod system;
mod terminal;
mod text;

use rng::Rng;
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
usage: fux-diff [--seed N] [--scale N] [AREA...]
       fux-diff --list

Runs the current fux beside its last release (the baseline in Cargo.toml)
on the same random inputs, and compares what each gives back. With no AREA,
every area runs. --scale multiplies each area's number of cases (default 1).";

/// What a run of an area found alike: a line saying how much was compared.
pub type Outcome = Result<String, String>;

/// An area's run: its cases, from a seeded source, times a scale.
type Run = fn(&mut Rng, usize) -> Outcome;

/// The areas, each with its run and what it compares.
const AREAS: &[(&str, Run, &str)] = &[
    (
        "sessions",
        sessions::states,
        "whole sessions: views, modes, notices, focus, queued input, outbox, layout",
    ),
    (
        "screens",
        sessions::screens,
        "the screens sessions compose for each client, and the paints made of them",
    ),
    (
        "commands",
        commands::run,
        "command lines: commands, usage messages and labels",
    ),
    (
        "layout",
        layout::run,
        "layout trees: placements, resizes, splits and removals",
    ),
    (
        "protocol",
        protocol::run,
        "frames and byte streams: encoded bytes, decoded frames and errors",
    ),
    (
        "terminal",
        terminal::run,
        "fux-vt: screens, history, modes, replies and titles after random output",
    ),
    (
        "input",
        input::run,
        "decoded client input with its Escape deadline, encoded keys and pastes, input queues",
    ),
    (
        "copy",
        copy::run,
        "copy mode's searches and selections, and every error it can reach",
    ),
    (
        "text",
        text::run,
        "words, quoting and shell lines, key names, config lines",
    ),
    (
        "system",
        system::run,
        "fuxix's descriptor calls and errnos, and the config file's path",
    ),
];

struct Options {
    seed: u64,
    scale: usize,
    areas: Vec<String>,
}

/// What the command line asks for: a run, or text to print and stop, with
/// whether it was a mistake.
enum Asked {
    Run(Options),
    Print(String),
    Wrong(String),
}

fn options() -> Asked {
    match parse() {
        Ok(asked) => asked,
        Err(message) => Asked::Wrong(message),
    }
}

fn parse() -> Result<Asked, String> {
    let mut args = std::env::args().skip(1);
    let mut o = Options {
        seed: 1,
        scale: 1,
        areas: Vec::new(),
    };
    let number = |value: Option<String>, flag: &str| -> Result<u64, String> {
        value
            .ok_or(format!("{flag} needs a value"))?
            .parse()
            .map_err(|e| format!("{flag}: {e}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => o.seed = number(args.next(), &arg)?,
            "--scale" => {
                o.scale = usize::try_from(number(args.next(), &arg)?).map_err(|e| e.to_string())?
            }
            "--list" => {
                let lines: Vec<String> = AREAS
                    .iter()
                    .map(|(name, _, about)| format!("{name:<10} {about}"))
                    .collect();
                return Ok(Asked::Print(lines.join("\n")));
            }
            system::PROBE => return Ok(Asked::Print(system::probe())),
            "--help" | "-h" => return Ok(Asked::Print(USAGE.into())),
            area if AREAS.iter().any(|(name, _, _)| *name == area) => o.areas.push(arg),
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Asked::Run(o))
}

fn main() -> ExitCode {
    let o = match options() {
        Asked::Run(o) => o,
        Asked::Print(text) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        Asked::Wrong(message) => {
            eprintln!("fux-diff: {message}");
            return ExitCode::from(2);
        }
    };
    let mut failed = false;
    for (name, run, _) in AREAS {
        if !o.areas.is_empty() && !o.areas.iter().any(|a| a == name) {
            continue;
        }
        // Each area has a seed of its own, so running one alone replays it.
        let seed = o.seed ^ area_seed(name);
        let started = Instant::now();
        match run(&mut Rng::new(seed), o.scale) {
            Ok(summary) => println!(
                "{name:<10} alike: {summary} ({:.1} s)",
                started.elapsed().as_secs_f64()
            ),
            Err(difference) => {
                println!("{name:<10} DIFFERENT (--seed {}):\n{difference}", o.seed);
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// FNV-1a of an area's name.
fn area_seed(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A difference, saying what differed and how: `Err` unless the two are
/// equal.
pub fn same<T: PartialEq + std::fmt::Debug>(
    what: &str,
    baseline: T,
    current: T,
) -> Result<(), String> {
    if baseline == current {
        return Ok(());
    }
    Err(format!(
        "{what}\n  baseline: {baseline:?}\n  current:  {current:?}"
    ))
}

/// Two multi-line texts that should be equal: `Err` naming the first line
/// that differs.
pub fn same_lines(what: &str, baseline: &str, current: &str) -> Result<(), String> {
    if baseline == current {
        return Ok(());
    }
    let (mut a, mut b) = (baseline.lines(), current.lines());
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => {}
            (x, y) => {
                return Err(format!(
                    "{what}\n  baseline: {}\n  current:  {}",
                    x.unwrap_or("(nothing)"),
                    y.unwrap_or("(nothing)")
                ));
            }
        }
    }
}

/// `n` times `scale`, for a number of cases.
pub fn times(n: usize, scale: usize) -> usize {
    n.saturating_mul(scale)
}

/// Counts one more.
pub fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

/// Two byte strings that should be equal: `Err` saying where they part.
pub fn same_bytes(what: &str, baseline: &[u8], current: &[u8]) -> Result<(), String> {
    if baseline == current {
        return Ok(());
    }
    let at = baseline
        .iter()
        .zip(current)
        .take_while(|(a, b)| a == b)
        .count();
    let near = |b: &[u8]| {
        let from = at.saturating_sub(16);
        let to = at.saturating_add(16).min(b.len());
        format!(
            "{:?}",
            String::from_utf8_lossy(b.get(from..to).unwrap_or_default())
        )
    };
    Err(format!(
        "{what}: from byte {at} of {} and {}\n  baseline: {}\n  current:  {}",
        baseline.len(),
        current.len(),
        near(baseline),
        near(current)
    ))
}
